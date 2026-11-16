//! What a router accepts under each key, now that the `PeerId` is the
//! agent's own key, first without a network, then over real swarms on
//! Iroh connections on loopback. Adapted from kad-libp2p's
//! `tests/link.rs`, without its identity link.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use libp2p::kad::RecordKey;
use libp2p::multiaddr::Protocol;
use pals_core::identity::AgentKey;
use sha2::{Digest, Sha256};
use spike_common::{BackendStats, Role, issue_announce, issue_contact, tamper_signature, verify};
use spike_kad_iroh::behaviour::{self, Handle, Settings};
use spike_kad_iroh::bind_endpoint;
use spike_kad_iroh::transport::{self, ALPN_KAD};
use spike_kad_iroh::validate::{self, Verdict};

const OCR: &str = "agentnet:capability:v1:vision.ocr";
const REVIEW: &str = "agentnet:capability:v1:code.rust.review";
const NOW: i64 = 1_800_000_000;

fn agent(seed: u8) -> AgentKey {
    AgentKey::from_bytes(&[seed; 32])
}

/// `agent`'s contact of `seq`, issued at [`NOW`].
fn contact_of(agent: &AgentKey, seq: u64) -> Vec<u8> {
    let addr: SocketAddr = "192.0.2.10:4433".parse().unwrap();
    issue_contact(agent, &[addr], seq, NOW).unwrap().to_json()
}

fn announce_of(agent: &AgentKey, capabilities: &[&str]) -> Vec<u8> {
    let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
    issue_announce(agent, &capabilities, 1, NOW)
        .unwrap()
        .to_json()
}

/// What a router holds, by key.
fn held(records: &[(RecordKey, Vec<u8>)]) -> impl Fn(&RecordKey) -> Option<Vec<u8>> {
    let map: HashMap<RecordKey, Vec<u8>> = records.iter().cloned().collect();
    move |key| map.get(key).cloned()
}

#[test]
fn the_peer_id_is_the_agent() {
    let a = agent(1);
    let peer = validate::peer_of(&a.id());
    assert_eq!(validate::agent_of(&peer), Some(a.id()));
    assert_ne!(peer, validate::peer_of(&agent(2).id()));
}

#[test]
fn a_contact_is_stored_only_under_its_own_agent() {
    let (a, b) = (agent(1), agent(2));
    let (p, q) = (validate::peer_of(&a.id()), validate::peer_of(&b.id()));
    let value = contact_of(&a, 1);
    assert_eq!(
        validate::check_put(&validate::contact_key(&p), &value, held(&[]), NOW),
        Verdict::Store
    );
    // A's contact, signed by A, under B's key.
    assert_eq!(
        validate::check_put(&validate::contact_key(&q), &value, held(&[]), NOW),
        Verdict::Reject("wrong_key")
    );
    // Not one of the two kinds of value a router stores.
    let other = RecordKey::new(&format!("/agentnet/link/{p}"));
    assert_eq!(
        validate::check_put(&other, &value, held(&[]), NOW),
        Verdict::Reject("unknown_key")
    );
}

#[test]
fn a_tampered_contact_is_rejected() {
    // What `--rogue` publishes.
    let a = agent(1);
    let addr: SocketAddr = "192.0.2.10:4433".parse().unwrap();
    let contact = tamper_signature(&issue_contact(&a, &[addr], 1, NOW).unwrap());
    let key = validate::contact_key(&validate::peer_of(&a.id()));
    assert_eq!(
        validate::check_put(&key, &contact.to_json(), held(&[]), NOW),
        Verdict::Reject("bad_contact")
    );
}

#[test]
fn an_announce_is_stored_only_under_its_own_agent() {
    let (a, b) = (agent(1), agent(2));
    let p = validate::peer_of(&a.id());
    let key = validate::announce_key(&p);
    assert_eq!(
        validate::check_put(&key, &announce_of(&a, &[OCR]), held(&[]), NOW),
        Verdict::Store
    );
    assert_eq!(
        validate::check_put(&key, &announce_of(&b, &[OCR]), held(&[]), NOW),
        Verdict::Reject("wrong_key")
    );
    assert_eq!(
        validate::check_put(&key, b"{}", held(&[]), NOW),
        Verdict::Reject("bad_announce")
    );
}

#[test]
fn a_router_keeps_the_newest_contact() {
    let a = agent(1);
    let key = validate::contact_key(&validate::peer_of(&a.id()));
    let records = [(key.clone(), contact_of(&a, 2))];
    assert_eq!(
        validate::check_put(&key, &contact_of(&a, 1), held(&records), NOW),
        Verdict::Stale
    );
    assert_eq!(
        validate::check_put(&key, &contact_of(&a, 3), held(&records), NOW),
        Verdict::Store
    );
}

#[test]
fn provider_key_is_the_sha256_multihash_of_the_capability() {
    let mut expected = vec![0x12, 0x20];
    expected.extend_from_slice(&Sha256::digest(OCR.as_bytes()));
    assert_eq!(validate::provider_key(OCR).to_vec(), expected);
}

#[test]
fn a_provider_record_is_checked_against_the_announce_held() {
    let a = agent(1);
    let p = validate::peer_of(&a.id());
    let records = [(validate::announce_key(&p), announce_of(&a, &[OCR]))];
    let (ocr, review) = (validate::provider_key(OCR), validate::provider_key(REVIEW));
    assert_eq!(
        validate::check_provider(&ocr, &p, held(&records), NOW),
        Verdict::Store
    );
    assert_eq!(
        validate::check_provider(&review, &p, held(&records), NOW),
        Verdict::Reject("capability_not_announced")
    );
    // With no announce held, only the key's form can be checked.
    assert_eq!(
        validate::check_provider(&review, &p, held(&[]), NOW),
        Verdict::Store
    );
    assert_eq!(
        validate::check_provider(&RecordKey::new(&OCR), &p, held(&[]), NOW),
        Verdict::Reject("bad_provider_key")
    );
}

/// A node of `role` on loopback, joined through `bootstrap`.
async fn node(role: Role, agent: &AgentKey, bootstrap: &[String]) -> Handle {
    let settings = Settings {
        role,
        listen: "127.0.0.1:0".parse().unwrap(),
        bootstrap: bootstrap.to_vec(),
        ttl: Duration::from_secs(120),
        republish: Duration::from_secs(40),
    };
    behaviour::spawn(settings, agent).await.unwrap()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `agent`'s announcement of [`OCR`] now, its contact tampered with when
/// `rogue`, as `run_node` makes it.
async fn announce(node: &Handle, agent: &AgentKey, rogue: bool) {
    let now = now_unix();
    let addr: SocketAddr = "127.0.0.1:4433".parse().unwrap();
    let contact = issue_contact(agent, &[addr], 1, now).unwrap();
    let contact = if rogue {
        tamper_signature(&contact)
    } else {
        contact
    };
    let announce = issue_announce(agent, &[OCR.to_owned()], 1, now).unwrap();
    node.announce(contact.to_json(), announce.to_json(), vec![OCR.to_owned()])
        .await
        .unwrap();
}

/// Polls `check` on the routers' stats until it holds, for a minute at
/// most.
async fn until_routers(routers: &[&Handle], check: impl Fn(&[BackendStats]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let stats: Vec<BackendStats> = routers.iter().map(|r| r.stats()).collect();
        if check(&stats) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the routers never got there: {stats:#?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn routers_store_an_honest_announcement_and_reject_a_tampered_contact() {
    let (honest, rogue) = (agent(11), agent(12));
    let r1 = node(Role::Router, &agent(1), &[]).await;
    let r1_addr = r1.stats().bootstrap_addr.expect("a router names itself");
    assert!(
        r1_addr.ends_with(&format!(
            "/quic-v1/p2p/{}",
            validate::peer_of(&agent(1).id())
        )),
        "{r1_addr}"
    );
    let r2 = node(Role::Router, &agent(2), std::slice::from_ref(&r1_addr)).await;
    let bootstrap = [r1_addr];
    let p = node(Role::Provider, &honest, &bootstrap).await;
    let bad = node(Role::Provider, &rogue, &bootstrap).await;
    let c = node(Role::Client, &agent(21), &bootstrap).await;
    assert_eq!(
        c.stats().bootstrap_addr,
        None,
        "a client is no bootstrap node"
    );
    announce(&p, &honest, false).await;
    announce(&bad, &rogue, true).await;

    let routers = [&r1, &r2];
    until_routers(&routers, |stats| {
        let of = |s: &BackendStats, a: &AgentKey| s.stored_by_agent.get(&a.id()).copied();
        stats.iter().all(|s| s.records_rejected > 0)
            && stats.iter().all(|s| {
                of(s, &honest).is_some_and(|r| {
                    (r.contact_records, r.announce_records, r.provider_records) == (1, 1, 1)
                })
            })
    })
    .await;
    for stats in routers.iter().map(|r| r.stats()) {
        let rogue_contacts = stats
            .stored_by_agent
            .get(&rogue.id())
            .map_or(0, |r| r.contact_records);
        assert_eq!(rogue_contacts, 0, "a router stored the tampered contact");
        // Every value stored is someone's: S5 counts the rest as the rogue's
        // contact.
        let attributed: u64 = stats
            .stored_by_agent
            .values()
            .map(|r| r.contact_records + r.announce_records + r.other_records)
            .sum();
        assert_eq!(attributed, stats.records_stored);
    }

    let now = now_unix();
    let found = c.search(OCR.to_owned(), 8).await.unwrap();
    let candidates: Vec<_> = found
        .candidates
        .into_iter()
        .map(|raw| (raw.found_ms, verify(OCR, raw, now)))
        .collect();
    let valid: Vec<_> = candidates.iter().filter(|(_, c)| c.valid).collect();
    assert_eq!(valid.len(), 1, "{candidates:#?}");
    assert_eq!(valid[0].1.agent_id, Some(honest.id()));
    assert!(
        valid[0].0.is_some(),
        "the backend says when it had the records"
    );
    assert!(
        candidates
            .iter()
            .all(|(_, c)| c.valid || c.reason == Some("bad_contact")),
        "{candidates:#?}"
    );
    // The two routers asked for the providers, and both asked for each
    // candidate's contact and announce: 2 + 2 × 2 × 2. The denied dials to
    // the two providers reach no network and do not count.
    assert_eq!(found.requests, Some(10));

    // The search reached the providers through their contacts, and dialed
    // none of them: only the two routers.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(c.stats().connections, 2);
}

/// An Iroh endpoint that accepts connections of [`ALPN_KAD`] and then says
/// nothing: every Kademlia request to it hangs until libp2p gives up.
#[derive(Debug, Clone)]
struct Silent;

impl ProtocolHandler for Silent {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        connection.closed().await;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_announce_answers_within_the_control_api_time() {
    // Its only router never answers, so the puts fail slowly and a retry is
    // still running at the announce's deadline: the announce must answer
    // all the same, within the 30 s the control API waits for it.
    let silent = agent(41);
    let endpoint = bind_endpoint("127.0.0.1:0".parse().unwrap(), &silent)
        .await
        .unwrap();
    let addr = transport::quic_addr(endpoint.bound_sockets()[0])
        .with(Protocol::P2p(validate::peer_of(&silent.id())));
    let _router = Router::builder(endpoint).accept(ALPN_KAD, Silent).spawn();
    let provider = agent(42);
    let p = node(Role::Provider, &provider, &[addr.to_string()]).await;

    let now = now_unix();
    let addr: SocketAddr = "127.0.0.1:4433".parse().unwrap();
    let contact = issue_contact(&provider, &[addr], 1, now).unwrap();
    let announce = issue_announce(&provider, &[OCR.to_owned()], 1, now).unwrap();
    let started = Instant::now();
    let result = p
        .announce(contact.to_json(), announce.to_json(), vec![OCR.to_owned()])
        .await;
    let took = started.elapsed();
    assert!(result.is_err(), "the records reached no router");
    // The control API's contract.
    assert!(took < Duration::from_secs(30), "answered after {took:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_with_no_bootstrap_finds_no_one() {
    // Scenario S0: nothing but `--bootstrap` brings a peer, and it is empty.
    let lone = node(Role::Client, &agent(31), &[]).await;
    let found = lone.search(OCR.to_owned(), 8).await.unwrap();
    assert!(found.candidates.is_empty());
    let stats = lone.stats();
    assert_eq!((stats.routing_peers, stats.connections), (0, 0));
}
