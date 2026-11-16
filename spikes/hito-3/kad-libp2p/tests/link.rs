//! The identity link `agentnet.spike-kadlink/0` and what a router accepts
//! under each key: the checks of spec §6.2 and of scenario S5, first
//! without a network, then over real swarms on loopback.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use libp2p::PeerId;
use libp2p::kad::RecordKey;
use pals_core::grant::format_timestamp;
use pals_core::identity::AgentKey;
use sha2::{Digest, Sha256};
use spike_common::{BackendStats, Role, issue_announce, issue_contact, tamper_signature, verify};
use spike_kad_libp2p::behaviour::{self, Handle, Settings};
use spike_kad_libp2p::link::{self, LINK_SCHEMA, LinkDoc, LinkError};
use spike_kad_libp2p::validate::{self, Verdict};

const OCR: &str = "agentnet:capability:v1:vision.ocr";
const REVIEW: &str = "agentnet:capability:v1:code.rust.review";
const NOW: i64 = 1_800_000_000;

fn agent(seed: u8) -> AgentKey {
    AgentKey::from_bytes(&[seed; 32])
}

fn peer_of(agent: &AgentKey) -> PeerId {
    link::libp2p_keypair(agent).public().to_peer_id()
}

/// `agent`'s link with a contact of `seq`, issued at [`NOW`].
fn link_of(agent: &AgentKey, seq: u64) -> Vec<u8> {
    let addr: SocketAddr = "192.0.2.10:4433".parse().unwrap();
    let contact = issue_contact(agent, &[addr], seq, NOW).unwrap();
    link::issue(&link::libp2p_keypair(agent), agent, &contact, NOW).unwrap()
}

/// The document of a link `link_of(agent, 1)` would carry.
fn doc_of(agent: &AgentKey) -> LinkDoc {
    let addr: SocketAddr = "192.0.2.10:4433".parse().unwrap();
    let contact = issue_contact(agent, &[addr], 1, NOW).unwrap();
    LinkDoc {
        schema: LINK_SCHEMA.to_owned(),
        peer_id: peer_of(agent).to_base58(),
        agent_id: agent.id(),
        contact: serde_json::from_slice(&contact.to_json()).unwrap(),
        issued_at: format_timestamp(NOW).unwrap(),
        expires_at: format_timestamp(NOW + 3600).unwrap(),
    }
}

/// What a router holds, by key.
fn held(records: &[(RecordKey, Vec<u8>)]) -> impl Fn(&RecordKey) -> Option<Vec<u8>> {
    let map: HashMap<RecordKey, Vec<u8>> = records.iter().cloned().collect();
    move |key| map.get(key).cloned()
}

#[test]
fn link_verifies_with_both_keys() {
    let a = agent(1);
    let verified = link::verify(&link_of(&a, 1), NOW + 60).unwrap();
    assert_eq!(verified.peer_id, peer_of(&a));
    assert_eq!(verified.agent_id, a.id());
    assert_eq!(verified.contact.doc.agent_id, a.id());
    assert_eq!(verified.contact.doc.seq, 1);
}

#[test]
fn link_rejects_a_peer_id_not_matching_its_key() {
    let (a, b) = (agent(1), agent(2));
    // A's document, naming B's PeerId, signed by A's own libp2p key: the
    // key in the envelope is not the one the PeerId names.
    let doc = LinkDoc {
        peer_id: peer_of(&b).to_base58(),
        ..doc_of(&a)
    };
    let value = link::sign(&link::libp2p_keypair(&a), &a, &doc).unwrap();
    assert_eq!(link::verify(&value, NOW), Err(LinkError::PeerIdMismatch));
}

#[test]
fn link_rejects_a_bad_agent_signature() {
    let (a, b) = (agent(1), agent(2));
    // A's document, signed by A's libp2p key but by B's agent key.
    let value = link::sign(&link::libp2p_keypair(&a), &b, &doc_of(&a)).unwrap();
    assert_eq!(link::verify(&value, NOW), Err(LinkError::BadAgentSignature));
}

#[test]
fn link_rejects_a_contact_whose_signature_does_not_verify() {
    // What `--rogue` publishes: the link's own signatures are good.
    let a = agent(1);
    let addr: SocketAddr = "192.0.2.10:4433".parse().unwrap();
    let contact = tamper_signature(&issue_contact(&a, &[addr], 1, NOW).unwrap());
    let value = link::issue(&link::libp2p_keypair(&a), &a, &contact, NOW).unwrap();
    assert_eq!(link::verify(&value, NOW), Err(LinkError::BadContact));
    let p = peer_of(&a);
    assert_eq!(
        validate::check_put(&validate::link_key(&p), &value, held(&[]), NOW),
        Verdict::Reject("bad_contact")
    );
}

#[test]
fn validator_rejects_a_record_under_the_wrong_key() {
    let (a, b) = (agent(1), agent(2));
    let value = link_of(&a, 1);
    let (p, q) = (peer_of(&a), peer_of(&b));
    assert_eq!(
        validate::check_put(&validate::link_key(&q), &value, held(&[]), NOW),
        Verdict::Reject("wrong_key")
    );
    assert_eq!(
        validate::check_put(&validate::link_key(&p), &value, held(&[]), NOW),
        Verdict::Store(Some(a.id()))
    );
    // Not one of the two kinds of value a router stores.
    let other = RecordKey::new(&format!("/agentnet/other/{p}"));
    assert_eq!(
        validate::check_put(&other, &value, held(&[]), NOW),
        Verdict::Reject("unknown_key")
    );
}

#[test]
fn validator_keeps_the_newest_link() {
    let a = agent(1);
    let key = validate::link_key(&peer_of(&a));
    let records = [(key.clone(), link_of(&a, 2))];
    assert_eq!(
        validate::check_put(&key, &link_of(&a, 1), held(&records), NOW),
        Verdict::Stale
    );
    assert_eq!(
        validate::check_put(&key, &link_of(&a, 3), held(&records), NOW),
        Verdict::Store(Some(a.id()))
    );
}

#[test]
fn validator_checks_an_announce_against_the_link_it_holds() {
    let (a, b) = (agent(1), agent(2));
    let p = peer_of(&a);
    let key = validate::announce_key(&p);
    let links = [(validate::link_key(&p), link_of(&a, 1))];
    let own = issue_announce(&a, &[OCR.to_owned()], 1, NOW)
        .unwrap()
        .to_json();
    let foreign = issue_announce(&b, &[OCR.to_owned()], 1, NOW)
        .unwrap()
        .to_json();
    assert_eq!(
        validate::check_put(&key, &own, held(&links), NOW),
        Verdict::Store(Some(a.id()))
    );
    // B's announce under A's PeerId, which A's link binds to A.
    assert_eq!(
        validate::check_put(&key, &foreign, held(&links), NOW),
        Verdict::Reject("agent_mismatch")
    );
    // With no link held, the binding cannot be checked: the announce is
    // stored on its own signature.
    assert_eq!(
        validate::check_put(&key, &foreign, held(&[]), NOW),
        Verdict::Store(Some(b.id()))
    );
}

#[test]
fn provider_key_is_the_sha256_multihash_of_the_capability() {
    let mut expected = vec![0x12, 0x20];
    expected.extend_from_slice(&Sha256::digest(OCR.as_bytes()));
    assert_eq!(validate::provider_key(OCR).to_vec(), expected);
}

#[test]
fn validator_checks_a_provider_record_against_the_announce_it_holds() {
    let a = agent(1);
    let p = peer_of(&a);
    let announce = issue_announce(&a, &[OCR.to_owned()], 1, NOW)
        .unwrap()
        .to_json();
    let records = [(validate::announce_key(&p), announce)];
    let ocr = validate::provider_key(OCR);
    let review = validate::provider_key(REVIEW);
    assert_eq!(
        validate::check_provider(&ocr, &p, held(&records), NOW),
        Verdict::Store(None)
    );
    assert_eq!(
        validate::check_provider(&review, &p, held(&records), NOW),
        Verdict::Reject("capability_not_announced")
    );
    // With no announce held, only the key's form can be checked.
    assert_eq!(
        validate::check_provider(&review, &p, held(&[]), NOW),
        Verdict::Store(None)
    );
    assert_eq!(
        validate::check_provider(&RecordKey::new(&OCR), &p, held(&[]), NOW),
        Verdict::Reject("bad_provider_key")
    );
}

#[test]
fn the_libp2p_key_derives_from_the_agent_seed() {
    let a = agent(1);
    assert_eq!(peer_of(&a), peer_of(&agent(1)));
    assert_ne!(peer_of(&a), peer_of(&agent(2)));
    // Domain separated: not the agent's own Ed25519 key.
    let libp2p_public = link::libp2p_keypair(&a).public();
    let ed25519 = libp2p_public.try_into_ed25519().unwrap();
    assert_ne!(ed25519.to_bytes(), *a.id().as_bytes());
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
    behaviour::spawn(settings, link::libp2p_keypair(agent))
        .await
        .unwrap()
}

/// `agent`'s announcement of [`OCR`] now, its contact tampered with when
/// `rogue`, as `run_node` makes it.
async fn announce(node: &Handle, agent: &AgentKey, rogue: bool) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let addr: SocketAddr = "127.0.0.1:4433".parse().unwrap();
    let contact = issue_contact(agent, &[addr], 1, now).unwrap();
    let contact = if rogue {
        tamper_signature(&contact)
    } else {
        contact
    };
    let announce = issue_announce(agent, &[OCR.to_owned()], 1, now).unwrap();
    let link = link::issue(&link::libp2p_keypair(agent), agent, &contact, now).unwrap();
    node.announce(link, announce.to_json(), vec![OCR.to_owned()])
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
async fn routers_store_an_honest_announcement_and_reject_a_tampered_link() {
    let (honest, rogue) = (agent(11), agent(12));
    let r1 = node(Role::Router, &agent(1), &[]).await;
    let r1_addr = r1.stats().bootstrap_addr.expect("a router names itself");
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
        stats.iter().any(|s| s.records_rejected > 0)
            && stats.iter().any(|s| {
                of(s, &honest).is_some_and(|r| r.contact_records == 1 && r.provider_records == 1)
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

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
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
    assert!(found.requests.is_some_and(|n| n > 0));

    // The search reached the providers through their links, and dialed
    // none of them: only the two routers. A provider can answer no
    // Kademlia request, and one on another network would hold the search
    // until libp2p's QUIC handshake gives up (5 s).
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(c.stats().connections, 2);
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
