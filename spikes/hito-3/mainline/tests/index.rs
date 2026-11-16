//! The capability index this candidate defines over a private Mainline DHT:
//! what a router takes into it, without network, and real nodes on
//! loopback, three routers that join through explicit bootstrap only, a
//! provider and a client.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use n0_mainline::{MutableItem, SigningKey};
use pals_core::identity::AgentKey;
use spike_common::records::{BAD_ANNOUNCE, BAD_CONTACT, EXPIRED};
use spike_common::{
    Backend, BackendStats, NodeConfig, Role, issue_announce, issue_contact, tamper_signature,
    verify,
};
use spike_mainline::Mainline;
use spike_mainline::index::{self, Kind};
use spike_mainline::node::Mode;

const OCR: &str = "agentnet:capability:v1:vision.ocr";
const DOCS: &str = "agentnet:capability:v1:knowledge.documentation";
const RUST: &str = "agentnet:capability:v1:code.rust.review";
/// What the routers may take to store what a put sent them, and their
/// probes to see it.
const STORE_DEADLINE: Duration = Duration::from_secs(20);
/// How long a test watches for something that must not happen: three of the
/// routers' probe rounds.
const ABSENCE: Duration = Duration::from_secs(3);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn key(seed: u8) -> AgentKey {
    AgentKey::from_bytes(&[seed; 32])
}

fn contact_addr(seed: u8) -> SocketAddr {
    SocketAddr::from(([192, 0, 2, seed], 4433))
}

fn config(name: &str, role: Role, listen: SocketAddr, bootstrap: &[String]) -> NodeConfig {
    NodeConfig {
        name: name.to_owned(),
        role,
        listen,
        bootstrap: bootstrap.to_vec(),
        ttl: Duration::from_secs(120),
        republish: Duration::from_secs(40),
    }
}

async fn node(name: &str, role: Role, seed: u8, bootstrap: &[String]) -> Mainline {
    let cfg = config(name, role, "127.0.0.1:0".parse().unwrap(), bootstrap);
    Mainline::start(&cfg, &key(seed)).await.unwrap()
}

/// Three routers: the first joins through no one, the other two through it.
async fn testnet() -> (Vec<Mainline>, String) {
    let first = node("r1", Role::Router, 0x11, &[]).await;
    let bootstrap = first.stats().bootstrap_addr.expect("a router names itself");
    let mut routers = vec![first];
    for (name, seed) in [("r2", 0x12), ("r3", 0x13)] {
        routers.push(node(name, Role::Router, seed, std::slice::from_ref(&bootstrap)).await);
    }
    (routers, bootstrap)
}

/// Announces `capabilities` with records issued now; with `rogue`, the
/// contact's signature does not verify.
async fn announce(
    node: &mut Mainline,
    seed: u8,
    capabilities: &[&str],
    rogue: bool,
) -> anyhow::Result<()> {
    let key = key(seed);
    let capabilities: Vec<String> = capabilities.iter().map(|c| c.to_string()).collect();
    let seq = now() as u64;
    let contact = issue_contact(&key, &[contact_addr(seed)], seq, now()).unwrap();
    let contact = if rogue {
        tamper_signature(&contact)
    } else {
        contact
    };
    let announce = issue_announce(&key, &capabilities, seq, now()).unwrap();
    node.announce(&capabilities, &contact, &announce).await
}

/// Waits until every router's stats satisfy `ok`.
async fn until_routers(routers: &[Mainline], what: &str, ok: impl Fn(&BackendStats) -> bool) {
    let deadline = Instant::now() + STORE_DEADLINE;
    loop {
        let stats: Vec<BackendStats> = routers.iter().map(Backend::stats).collect();
        if stats.iter().all(&ok) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} never held: {stats:#?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[test]
fn the_infohash_of_a_capability_is_the_sha1_of_its_string() {
    assert_eq!(
        index::capability_hash(OCR).as_bytes(),
        &[
            0x7e, 0x5f, 0x0a, 0xc2, 0xa5, 0xfe, 0x7a, 0x58, 0x9c, 0xc1, 0x26, 0x55, 0xac, 0xad,
            0xc6, 0xe2, 0xd4, 0x1d, 0xdc, 0xec
        ]
    );
}

#[test]
fn a_router_takes_only_an_agents_own_records() {
    let (agent, other) = (key(1), key(2));
    let contact = issue_contact(&agent, &[contact_addr(1)], 7, now()).unwrap();
    let announce = issue_announce(&agent, &[OCR.to_owned()], 7, now()).unwrap();
    let k = *agent.id().as_bytes();
    let (contact_json, announce_json) = (contact.to_json(), announce.to_json());
    let check = |key: &[u8; 32], salt: &[u8], value: &[u8], seq: i64| {
        index::check_item(key, Some(salt), value, seq, now())
    };

    let taken = check(&k, Kind::Contact.salt(), &contact_json, 7).unwrap();
    assert_eq!((taken.agent, taken.kind), (agent.id(), Kind::Contact));
    let taken = check(&k, Kind::Announce.salt(), &announce_json, 7).unwrap();
    assert_eq!((taken.agent, taken.kind), (agent.id(), Kind::Announce));
    assert_eq!(taken.capabilities, [index::capability_hash(OCR)]);

    // Under another salt, or none, it is not this index's.
    assert_eq!(
        check(&k, b"other", &contact_json, 7).unwrap_err(),
        index::UNKNOWN_KEY
    );
    assert_eq!(
        index::check_item(&k, None, &contact_json, 7, now()).unwrap_err(),
        index::UNKNOWN_KEY
    );
    // A record under the salt of the other kind does not parse as it.
    assert_eq!(
        check(&k, Kind::Announce.salt(), &contact_json, 7).unwrap_err(),
        BAD_ANNOUNCE
    );
    // Another agent's key: the item is not the record's agent's.
    let other_k = *other.id().as_bytes();
    assert_eq!(
        check(&other_k, Kind::Contact.salt(), &contact_json, 7).unwrap_err(),
        index::WRONG_KEY
    );
    // The rogue's contact (scenario S5).
    let tampered = tamper_signature(&contact).to_json();
    assert_eq!(
        check(&k, Kind::Contact.salt(), &tampered, 7).unwrap_err(),
        BAD_CONTACT
    );
    // BEP 44's seq is the record's, so that the newest item is the newest record.
    assert_eq!(
        check(&k, Kind::Contact.salt(), &contact_json, 8).unwrap_err(),
        index::SEQ_MISMATCH
    );
    // A record past its validity window, issued two days ago for one.
    let old = issue_contact(&agent, &[contact_addr(1)], 7, now() - 2 * 86_400).unwrap();
    assert_eq!(
        check(&k, Kind::Contact.salt(), &old.to_json(), 7).unwrap_err(),
        EXPIRED
    );
    // A key that is not an Ed25519 point is no agent's.
    let not_a_point = (0..=u8::MAX)
        .map(|b| [b; 32])
        .find(|k| index::agent_of(k).is_err())
        .expect("some byte repeated is no curve point");
    assert_eq!(
        check(&not_a_point, Kind::Contact.salt(), &contact_json, 7).unwrap_err(),
        index::BAD_KEY
    );
    assert_eq!(
        index::check_provider(&not_a_point, &index::capability_hash(OCR), None).unwrap_err(),
        index::BAD_KEY
    );

    // A signed peer of a capability the stored announce does not list.
    let ocr = index::capability_hash(OCR);
    let docs = index::capability_hash(DOCS);
    assert_eq!(
        index::check_provider(&k, &ocr, Some(&[ocr])).unwrap(),
        agent.id()
    );
    assert_eq!(index::check_provider(&k, &ocr, None).unwrap(), agent.id());
    assert_eq!(
        index::check_provider(&k, &docs, Some(&[ocr])).unwrap_err(),
        index::CAPABILITY_NOT_ANNOUNCED
    );
}

#[test]
fn an_item_holds_at_most_bep_44s_1000_bytes() {
    let agent = key(1);
    let signer = SigningKey::from_bytes(&agent.to_bytes());
    let contact = issue_contact(&agent, &[contact_addr(1)], 7, now()).unwrap();
    let item = index::item(&signer, Kind::Contact, &contact.to_json(), 7).unwrap();
    assert_eq!(item.key(), agent.id().as_bytes());
    assert_eq!(item.seq(), 7);
    assert_eq!(item.salt(), Some(Kind::Contact.salt()));
    assert_eq!(
        *item.target(),
        MutableItem::target_from_key(agent.id().as_bytes(), Some(Kind::Contact.salt()))
    );
    // A contact always fits: eight IPv6 addresses, as many as pals-core
    // allows, are under 800 bytes.
    let wide: Vec<SocketAddr> = (0..8u16)
        .map(|i| {
            format!("[2001:db8:ffff:ffff:ffff:ffff:ffff:{i:x}]:4433")
                .parse()
                .unwrap()
        })
        .collect();
    let contact = issue_contact(&agent, &wide, 7, now()).unwrap();
    assert!(index::item(&signer, Kind::Contact, &contact.to_json(), 7).is_ok());
    // An announce of twelve capabilities as long as the common
    // vocabulary's does not, though pals-core's 4 KiB take it.
    let many: Vec<String> = (0..12).map(|i| format!("{DOCS}{i:02}")).collect();
    let announce = issue_announce(&agent, &many, 7, now()).unwrap();
    assert!(announce.to_json().len() > index::MAX_VALUE_BYTES);
    assert!(index::item(&signer, Kind::Announce, &announce.to_json(), 7).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn announced_capability_is_found() {
    let (_routers, bootstrap) = testnet().await;
    let boot = std::slice::from_ref(&bootstrap);
    let mut provider = node("p1", Role::Provider, 0x21, boot).await;
    announce(&mut provider, 0x21, &[OCR, DOCS], false)
        .await
        .unwrap();
    let mut client = node("c1", Role::Client, 0x31, boot).await;

    let found = client.search(OCR, 8).await.unwrap();
    assert_eq!(found.candidates.len(), 1, "{found:?}");
    let raw = found.candidates[0].clone();
    assert!(raw.found_ms.is_some());
    let candidate = verify(OCR, raw, now());
    assert!(candidate.valid, "{candidate:?}");
    assert_eq!(candidate.agent_id, Some(key(0x21).id()));

    // A capability no one announced finds no one.
    assert!(client.search(RUST, 8).await.unwrap().candidates.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn found_entries_lead_to_a_verified_contact() {
    let (routers, bootstrap) = testnet().await;
    let boot = std::slice::from_ref(&bootstrap);
    let mut honest = node("p1", Role::Provider, 0x21, boot).await;
    announce(&mut honest, 0x21, &[OCR], false).await.unwrap();
    let mut rogue = node("rogue", Role::Provider, 0x66, boot).await;
    // The routers refuse its contact, so the announce reports it.
    assert!(announce(&mut rogue, 0x66, &[OCR], true).await.is_err());

    let (honest_id, rogue_id) = (key(0x21).id(), key(0x66).id());
    until_routers(&routers, "the routers hold what they took", |s| {
        let held = |id| s.stored_by_agent.get(id).copied().unwrap_or_default();
        let (h, r) = (held(&honest_id), held(&rogue_id));
        (h.contact_records, h.announce_records, h.provider_records) == (1, 1, 1)
            && (r.contact_records, r.announce_records, r.provider_records) == (0, 1, 1)
            && s.records_stored == 3
            && s.provider_records_stored == 2
    })
    .await;
    let rejected: u64 = routers.iter().map(|r| r.stats().records_rejected).sum();
    assert!(rejected > 0, "no router counted the rogue's contact");
    // The routers probe the refused contact's target too: what they report
    // there is what the library holds, and it must stay nothing, with every
    // stored record attributed.
    let watched = Instant::now();
    while watched.elapsed() < ABSENCE {
        for stats in routers.iter().map(Backend::stats) {
            let rogue_held = stats.stored_by_agent.get(&rogue_id).copied();
            assert_eq!(
                rogue_held.unwrap_or_default().contact_records,
                0,
                "{stats:#?}"
            );
            let attributed: u64 = stats
                .stored_by_agent
                .values()
                .map(|r| r.contact_records + r.announce_records + r.other_records)
                .sum();
            assert_eq!(stats.records_stored, attributed, "{stats:#?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let mut client = node("c1", Role::Client, 0x31, boot).await;
    let found = client.search(OCR, 8).await.unwrap();
    let mut checked: Vec<_> = found
        .candidates
        .into_iter()
        .map(|raw| verify(OCR, raw, now()))
        .collect();
    checked.sort_by_key(|c| !c.valid);
    assert_eq!(checked.len(), 2, "{checked:?}");
    assert!(checked[0].valid);
    assert_eq!(checked[0].agent_id, Some(honest_id));
    assert_eq!(checked[0].direct_addrs, [contact_addr(0x21).to_string()]);
    assert_eq!(checked[1].agent_id, Some(rogue_id));
    assert_eq!(checked[1].reason, Some(BAD_CONTACT));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_without_bootstrap_finds_nothing() {
    let (_routers, bootstrap) = testnet().await;
    let mut provider = node("p1", Role::Provider, 0x21, std::slice::from_ref(&bootstrap)).await;
    announce(&mut provider, 0x21, &[OCR], false).await.unwrap();

    let mut lone = node("c0", Role::Client, 0x30, &[]).await;
    // A client that joined, beside it, does know the routers once its stats
    // are read again, every second.
    let joined = node("c1", Role::Client, 0x31, std::slice::from_ref(&bootstrap)).await;
    let deadline = Instant::now() + STORE_DEADLINE;
    while joined.stats().routing_peers == 0 {
        assert!(
            Instant::now() < deadline,
            "the joined client never knew a router"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(lone.search(OCR, 8).await.unwrap().candidates.is_empty());
    let stats = lone.stats();
    assert_eq!(stats.routing_peers, 0, "{stats:?}");
    assert_eq!(stats.bootstrap_addr, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_router_gets_the_records_again() {
    // A router on fixed ports, as in the harness: Iroh on `listen`, the DHT
    // on the next one.
    let listen = loop {
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if port < u16::MAX && std::net::UdpSocket::bind(("127.0.0.1", port + 1)).is_ok() {
            break SocketAddr::from(([127, 0, 0, 1], port));
        }
    };
    let cfg = config("r1", Role::Router, listen, &[]);
    let router = Mainline::start(&cfg, &key(0x11)).await.unwrap();
    let bootstrap = router.stats().bootstrap_addr.unwrap();
    let mut provider = node("p1", Role::Provider, 0x21, std::slice::from_ref(&bootstrap)).await;
    let key21 = key(0x21);
    let seq = now() as u64;
    let contact = issue_contact(&key21, &[contact_addr(0x21)], seq, now()).unwrap();
    let announce = issue_announce(&key21, &[OCR.to_owned()], seq, now()).unwrap();
    let capabilities = [OCR.to_owned()];
    provider
        .announce(&capabilities, &contact, &announce)
        .await
        .unwrap();
    let held = |s: &BackendStats| s.records_stored == 2 && s.provider_records_stored == 1;
    until_routers(std::slice::from_ref(&router), "the router holds p1", held).await;

    // The router restarts on the same ports, with nothing stored, a new node
    // id and a new token secret. The provider's next publication, as a
    // republication would, reaches it.
    drop(router);
    let deadline = Instant::now() + STORE_DEADLINE;
    let router = loop {
        match Mainline::start(&cfg, &key(0x11)).await {
            Ok(router) => break router,
            Err(error) if Instant::now() < deadline => {
                eprintln!("the router's ports are not free yet: {error:#}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => panic!("the router did not start again: {error:#}"),
        }
    };
    assert_eq!(router.stats().bootstrap_addr, Some(bootstrap));
    provider
        .announce(&capabilities, &contact, &announce)
        .await
        .unwrap();
    until_routers(
        std::slice::from_ref(&router),
        "the restarted router holds p1",
        held,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_and_clients_stay_in_client_mode() {
    let (routers, bootstrap) = testnet().await;
    let boot = std::slice::from_ref(&bootstrap);
    let mut provider = node("p1", Role::Provider, 0x21, boot).await;
    announce(&mut provider, 0x21, &[OCR], false).await.unwrap();
    let mut client = node("c1", Role::Client, 0x31, boot).await;
    assert_eq!(client.search(OCR, 8).await.unwrap().candidates.len(), 1);
    // The routers' answers tell each node its address. A client that
    // confirmed it with a ping to itself would become a server at its next
    // 15-minute refresh; n0-mainline 0.7.2 never sends that ping. Time for
    // it to arrive, if it did.
    tokio::time::sleep(Duration::from_secs(3)).await;

    for router in &routers {
        assert!(router.mode().await.unwrap().server);
    }
    let client_mode = Mode {
        server: false,
        reachable: false,
    };
    assert_eq!(provider.mode().await.unwrap(), client_mode);
    assert_eq!(client.mode().await.unwrap(), client_mode);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_router_refuses_what_the_index_does_not_use() {
    use serde_bencode::value::Value;
    use spike_mainline::krpc::Dict;

    let router = node("r1", Role::Router, 0x11, &[]).await;
    let at = router.stats().bootstrap_addr.unwrap();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let bytes = |n: usize, b: u8| Value::Bytes(vec![b; n]);
    let query = |method: &str, args: Vec<(&str, Value)>| {
        let args: Dict = args
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect();
        let message: Dict = [
            (b"t".to_vec(), Value::Bytes(b"aa".to_vec())),
            (b"y".to_vec(), Value::Bytes(b"q".to_vec())),
            (b"q".to_vec(), Value::Bytes(method.as_bytes().to_vec())),
            (b"a".to_vec(), Value::Dict(args)),
        ]
        .into();
        serde_bencode::to_bytes(&Value::Dict(message)).unwrap()
    };
    // Each query, and the reason the router's KRPC error must give.
    let refused = [
        // An immutable item: the index has none.
        (
            query(
                "put",
                vec![
                    ("id", bytes(20, 1)),
                    ("target", bytes(20, 2)),
                    ("token", bytes(4, 3)),
                    ("v", bytes(5, 4)),
                ],
            ),
            index::UNKNOWN_KEY,
        ),
        // A BEP 5 peer: the index is of signed peers.
        (
            query(
                "announce_peer",
                vec![
                    ("id", bytes(20, 1)),
                    ("info_hash", bytes(20, 2)),
                    ("port", Value::Int(4433)),
                    ("token", bytes(4, 3)),
                ],
            ),
            index::UNKNOWN_KEY,
        ),
        // A mutable put whose key is not 32 bytes: it cannot be read, so it
        // is refused rather than let through.
        (
            query(
                "put",
                vec![
                    ("id", bytes(20, 1)),
                    ("target", bytes(20, 2)),
                    ("token", bytes(4, 3)),
                    ("k", bytes(5, 4)),
                    ("v", bytes(5, 5)),
                    ("seq", Value::Int(1)),
                    ("sig", bytes(64, 6)),
                ],
            ),
            index::MALFORMED,
        ),
        // A signed peer whose infohash is not 20 bytes.
        (
            query(
                "announce_signed_peer",
                vec![
                    ("id", bytes(20, 1)),
                    ("info_hash", bytes(3, 2)),
                    ("token", bytes(4, 3)),
                    ("k", bytes(32, 4)),
                    ("t", Value::Int(1)),
                    ("sig", bytes(64, 6)),
                ],
            ),
            index::MALFORMED,
        ),
    ];
    for (query, reason) in &refused {
        socket.send_to(query, &at).unwrap();
        let mut answer = [0u8; 1500];
        let (n, _) = socket.recv_from(&mut answer).expect("the router answers");
        let Value::Dict(answer) = serde_bencode::from_bytes::<Value>(&answer[..n]).unwrap() else {
            panic!("not a dictionary");
        };
        assert_eq!(
            answer.get(b"y".as_slice()),
            Some(&Value::Bytes(b"e".to_vec()))
        );
        let expected = Value::List(vec![
            Value::Int(203),
            Value::Bytes(format!("agentnet: {reason}").into_bytes()),
        ]);
        assert_eq!(answer.get(b"e".as_slice()), Some(&expected));
    }
    let stats = router.stats();
    assert_eq!(stats.records_rejected, refused.len() as u64);
    assert_eq!(stats.records_stored, 0);
}
