//! What a node publishes in the DHT and how another finds it: real nodes on
//! loopback, with no relay, the routers R1 and R2, the providers P1 (with
//! `vision.ocr` and `knowledge.documentation` public) and P2 (with
//! `code.rust.review` public), and C, a client that looks them up.
//!
//! A node with `[dht] announce` publishes in rounds: when it enters the
//! network, every `republish`, every time one of its records is issued again
//! and, after a round that reached no router, after `retry_after`. The
//! routers keep what they get for `record_ttl`, with `libp2p-kad`'s own
//! expiry. Where a test needs records the node never issues (a newer contact,
//! a forged signature), a [`Dht`] of its own publishes a [`TestRecords`].
//!
//! The tests wait for the state they need and never for time to pass; a
//! deadline only fails them. What follows `libp2p-kad`'s times runs with
//! short ones (`record_ttl` 6 s and `republish` 2 s), except where a test
//! says otherwise.

mod common;

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use common::{
    TestNode, loopback_endpoint, now_unix, shut_down, spawn_dht_node, spawn_dht_node_in,
    spawn_dht_node_with,
};
use iroh::endpoint::{Connection, PortmapperConfig, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
use libp2p::kad::store::MemoryStoreConfig;
use pals::config::{CONFIG_FILE, DhtCfg, DhtMode, INVENTORY_FILE};
use pals::dht::keys::peer_id;
use pals::dht::swarm::STORE_LIMITS;
use pals::dht::transport::{ALPN_KAD, KadQuotas, located_nowhere};
use pals::dht::{
    Dht, DhtLookup, DhtParts, DhtStatus, DhtTiming, PublicationStatus, PublishedRecords,
    QUERY_TIMEOUT,
};
use pals::node::NodeOptions;
use pals_core::card_jws::CardHash;
use pals_core::clock::{Clock, ManualClock, SystemClock};
use pals_core::grant::format_timestamp;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{
    ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact,
    MAX_ANNOUNCED_CAPABILITIES, RECORD_TTL_S,
};
use pals_core::signed::Signed;
use pals_core::store::Store;
use tokio::sync::watch;

/// How long a test waits for the network to reach the state it needs: past
/// it, the test fails.
const CONVERGES: Duration = Duration::from_secs(60);

/// The pause between two lookups of [`wait_for_lookup`]: well under a
/// router's 600 streams a minute from one peer, beside the rest of the
/// test's traffic.
const LOOKUP_PAUSE: Duration = Duration::from_secs(2);

/// P1's inventory: two public services.
const P1_INVENTORY: &str = r#"
[[services]]
id = "ocr"
name = "OCR"
description = "Reads the text of an image"
capabilities = ["vision.ocr"]
visibility = "public"

[[services]]
id = "docs"
name = "Documentation"
description = "Answers from the documentation"
capabilities = ["knowledge.documentation"]
visibility = "public"
"#;

/// P2's inventory: one public service.
const P2_INVENTORY: &str = r#"
[[services]]
id = "review"
name = "Rust review"
description = "Reviews Rust code"
capabilities = ["code.rust.review"]
visibility = "public"
"#;

/// A service of the project `demo` and a private one, which P1 also has in
/// [`only_public_capabilities_reach_the_dht`].
const HIDDEN_SERVICES: &str = r#"
[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["code.rust.review"]
visibility = { project = ["demo"] }

[[services]]
id = "transcriber"
name = "Transcriber"
description = "Transcribes meetings"
capabilities = ["audio.transcribe"]
"#;

/// P1's public capabilities, in the inventory's order.
const P1_CAPABILITIES: [&str; 2] = ["vision.ocr", "knowledge.documentation"];

/// Short DHT times: records live 6 s, published again every 2 s.
fn short() -> DhtTiming {
    DhtTiming {
        record_ttl: Duration::from_secs(6),
        republish: Duration::from_secs(2),
        ..DhtTiming::from_config(&DhtCfg::default())
    }
}

/// The configuration's default DHT times: nothing expires or is published
/// again while a test runs.
fn long() -> DhtTiming {
    DhtTiming::from_config(&DhtCfg::default())
}

/// A node's options with the DHT's times `timing`.
fn with_timing(timing: DhtTiming) -> NodeOptions {
    NodeOptions {
        dht_timing: Some(timing),
        ..NodeOptions::default()
    }
}

/// `node`'s DHT.
fn dht(node: &TestNode) -> &Dht {
    node.node.dht().expect("the node's DHT is on")
}

/// `node`'s DHT status now.
async fn status(node: &TestNode) -> DhtStatus {
    dht(node).status().await.expect("the node's DHT runs")
}

/// Polls `dht`'s status until `done`, and returns it; fails, saying `what`,
/// after [`CONVERGES`].
async fn wait_for_dht(dht: &Dht, what: &str, done: impl Fn(&DhtStatus) -> bool) -> DhtStatus {
    let deadline = Instant::now() + CONVERGES;
    loop {
        let now = dht.status().await.expect("the DHT runs");
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "{what}: {now:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// [`wait_for_dht`] on `node`'s DHT.
async fn wait_for(node: &TestNode, what: &str, done: impl Fn(&DhtStatus) -> bool) -> DhtStatus {
    wait_for_dht(dht(node), what, done).await
}

/// Waits until `dht`'s publication is as `done` wants it.
async fn wait_for_publication(
    dht: &Dht,
    what: &str,
    done: impl Fn(&PublicationStatus) -> bool,
) -> PublicationStatus {
    let status = wait_for_dht(dht, what, |s| s.publication.as_ref().is_some_and(&done)).await;
    status.publication.expect("it publishes")
}

/// How many values and provider records of `agent` a router's `status`
/// counts.
fn held(status: &DhtStatus, agent: AgentId) -> u32 {
    status.stored_by_agent.get(&agent).copied().unwrap_or(0)
}

/// The agents whose provider records of `capability` `router` holds.
async fn holders(router: &TestNode, capability: &str) -> Vec<AgentId> {
    dht(router)
        .held_providers(capability)
        .await
        .unwrap()
        .into_iter()
        .map(|(agent, _)| agent)
        .collect()
}

/// The agents a lookup found, in its order.
fn agents(lookup: &DhtLookup) -> Vec<AgentId> {
    lookup.found.iter().map(|found| found.agent_id).collect()
}

/// `node`'s lookup of `capability`, of up to `limit` providers, with
/// [`CONVERGES`] to answer.
async fn lookup(node: &TestNode, capability: &str, limit: usize) -> DhtLookup {
    let deadline = tokio::time::Instant::now() + CONVERGES;
    dht(node)
        .providers(capability, limit, deadline)
        .await
        .expect("a router answers")
}

/// Looks `capability` up from `node` until a lookup is as `done` wants it,
/// and returns that one; fails, saying `what`, after [`CONVERGES`]. A lookup
/// opens three streams at each router, and a router takes 600 a minute from
/// one peer: the tests wait for the routers' state first, and the lookups
/// here go [`LOOKUP_PAUSE`] apart.
async fn wait_for_lookup(
    node: &TestNode,
    capability: &str,
    what: &str,
    done: impl Fn(&DhtLookup) -> bool,
) -> DhtLookup {
    let deadline = Instant::now() + CONVERGES;
    loop {
        let now = lookup(node, capability, 8).await;
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "{what}: {now:?}");
        tokio::time::sleep(LOOKUP_PAUSE).await;
    }
}

/// Two routers that know each other, with `timing`.
async fn routers(timing: DhtTiming) -> (TestNode, TestNode) {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], timing).await;
    wait_for(&r1, "R2 in R1's table", |s| s.routing_peers == 1).await;
    wait_for(&r2, "R1 in R2's table", |s| s.routing_peers == 1).await;
    (r1, r2)
}

/// A fresh data directory whose inventory is `inventory`.
fn with_inventory(inventory: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    dir
}

/// A DHT client with `inventory` that announces it, entering through
/// `routers`, with `timing`.
async fn spawn_provider(inventory: &str, routers: &[&TestNode], timing: DhtTiming) -> TestNode {
    spawn_dht_node_in(
        with_inventory(inventory),
        DhtMode::Client,
        routers,
        with_timing(timing),
        |cfg| cfg.dht.announce = true,
    )
    .await
}

/// A UDP port of 127.0.0.1 that was free a moment ago.
fn free_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A router on `listen`, entering through `bootstrap`, with `opts`.
async fn router_on(
    dir: tempfile::TempDir,
    listen: SocketAddr,
    bootstrap: &[&TestNode],
    opts: NodeOptions,
) -> TestNode {
    spawn_dht_node_in(dir, DhtMode::Router, bootstrap, opts, |cfg| {
        cfg.net.listen = Some(listen);
    })
    .await
}

/// `key`'s contact with `seq`, valid now, naming `direct`.
fn contact(key: &AgentKey, seq: u64, direct: &[SocketAddr]) -> Vec<u8> {
    let issued_at = now_unix() - 60;
    let doc = ContactRecord {
        schema: CONTACT_SCHEMA.to_owned(),
        agent_id: key.id(),
        iroh: IrohContact {
            endpoint_id: key.id(),
            relay_url: None,
            direct_addrs: direct.iter().map(ToString::to_string).collect(),
        },
        seq,
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(issued_at + RECORD_TTL_S).unwrap(),
    };
    Signed::issue(key, doc).unwrap().to_json()
}

/// `key`'s announce with `seq`, listing `capabilities`, valid now.
fn announce(key: &AgentKey, seq: u64, capabilities: &[&str]) -> Vec<u8> {
    let issued_at = now_unix() - 60;
    let doc = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: key.id(),
        seq,
        agent_card_hash: format!("blake3:{}", "ab".repeat(32))
            .parse::<CardHash>()
            .unwrap(),
        capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
        discovery_alpn: "agentnet-discovery/0".to_owned(),
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(issued_at + RECORD_TTL_S).unwrap(),
    };
    Signed::issue(key, doc).unwrap().to_json()
}

/// Where a [`TestRecords`] says its agent is.
fn somewhere() -> SocketAddr {
    SocketAddr::from(([192, 0, 2, 10], 4433))
}

/// Records a test publishes as it likes: what [`TestRecords::set`] gives,
/// each time a new change.
struct TestRecords {
    current: Mutex<(Vec<u8>, Vec<u8>)>,
    capabilities: Vec<String>,
    changes: watch::Sender<u64>,
}

impl TestRecords {
    fn new(contact: Vec<u8>, announce: Vec<u8>, capabilities: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new((contact, announce)),
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            changes: watch::channel(0).0,
        })
    }

    /// `key`'s contact and announce of `seq` 1, listing `capabilities`.
    fn of(key: &AgentKey, capabilities: &[&str]) -> Arc<Self> {
        Self::new(
            contact(key, 1, &[somewhere()]),
            announce(key, 1, capabilities),
            capabilities,
        )
    }

    /// Puts `contact` in force, as a record issued again.
    fn set_contact(&self, contact: Vec<u8>) {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0 = contact;
        self.bump();
    }

    /// Says the records changed.
    fn bump(&self) {
        self.changes.send_modify(|n| *n += 1);
    }
}

impl PublishedRecords for TestRecords {
    fn current(&self) -> (Vec<u8>, Vec<u8>) {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn capabilities(&self) -> Vec<String> {
        self.capabilities.clone()
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

/// A DHT client of its own, with no node around it, that publishes
/// `records` under `key`.
struct Publisher {
    dht: Dht,
    _router: Router,
}

impl Publisher {
    /// Starts it on a loopback endpoint with `key`, entering through
    /// `routers`, with `timing` and `clock`.
    async fn spawn(
        key: &AgentKey,
        records: Arc<TestRecords>,
        routers: &[&TestNode],
        timing: DhtTiming,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let cfg = DhtCfg {
            enabled: true,
            mode: DhtMode::Client,
            bootstrap: routers.iter().map(|router| router.node.ticket()).collect(),
            announce: true,
            ..DhtCfg::default()
        };
        let (dht, listener) = Dht::spawn(DhtParts {
            endpoint: endpoint.clone(),
            key: Arc::new(AgentKey::from_bytes(&key.to_bytes())),
            cfg,
            timing,
            quotas: KadQuotas::default(),
            store_limits: STORE_LIMITS,
            clock,
            store: Arc::new(Store::open_in_memory(key.id()).unwrap()),
            published: Some(records),
        })
        .unwrap();
        let router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
        Self {
            dht,
            _router: router,
        }
    }
}

#[tokio::test]
async fn a_published_capability_is_found() {
    let (r1, r2) = routers(short()).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1, &r2], short()).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], short()).await;
    for router in [&r1, &r2] {
        wait_for(router, "P1's records", |s| held(s, p1.id()) == 4).await;
    }

    let found = wait_for_lookup(&c, "vision.ocr", "P1 found", |l| !l.found.is_empty()).await;
    assert_eq!(agents(&found), [p1.id()], "{found:?}");
    let candidate = &found.found[0];
    // What C keeps is P1's signed records, checked.
    assert_eq!(
        Signed::<ContactRecord>::from_json(&candidate.contact_json).unwrap(),
        candidate.contact
    );
    assert_eq!(
        Signed::<AnnounceRecord>::from_json(&candidate.announce_json).unwrap(),
        candidate.announce
    );
    assert_eq!(candidate.contact.doc.agent_id, p1.id());
    let p1_addrs: Vec<String> = p1.node.addr().ip_addrs().map(ToString::to_string).collect();
    assert!(
        candidate
            .contact
            .doc
            .iroh
            .direct_addrs
            .iter()
            .any(|addr| p1_addrs.contains(addr)),
        "{:?} names none of {p1_addrs:?}",
        candidate.contact.doc.iroh
    );
    assert_eq!(candidate.announce_json, p1.node.records().current().1);
    assert_eq!(candidate.announce.doc.capabilities, P1_CAPABILITIES);
    assert!(found.requests > 0, "{found:?}");
    assert!(!found.cut, "{found:?}");
    assert!(found.discarded.is_empty(), "{found:?}");
}

#[tokio::test]
async fn only_public_capabilities_reach_the_dht() {
    let (r1, r2) = routers(short()).await;
    let inventory = format!("{P1_INVENTORY}{HIDDEN_SERVICES}");
    let p1 = spawn_provider(&inventory, &[&r1, &r2], short()).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], short()).await;

    // Its contact, its announce and a provider record of each public
    // capability, and nothing it ever tried beyond them.
    for router in [&r1, &r2] {
        let now = wait_for(router, "P1's records", |s| held(s, p1.id()) == 4).await;
        assert_eq!(now.rejected["capability_not_announced"], 0, "{now:?}");
        assert_eq!(holders(router, "vision.ocr").await, [p1.id()]);
        assert!(holders(router, "code.rust.review").await.is_empty());
        assert!(holders(router, "audio.transcribe").await.is_empty());
    }
    let found = wait_for_lookup(&c, "vision.ocr", "P1 found", |l| !l.found.is_empty()).await;
    assert_eq!(found.found[0].announce.doc.capabilities, P1_CAPABILITIES);
    for hidden in ["code.rust.review", "audio.transcribe"] {
        let none = lookup(&c, hidden, 8).await;
        assert!(
            none.found.is_empty() && none.discarded.is_empty(),
            "{none:?}"
        );
    }
    let published = status(&p1).await.publication.expect("P1 publishes");
    assert_eq!(published.capabilities, P1_CAPABILITIES);
}

#[tokio::test]
async fn nothing_is_published_without_announce() {
    let (r1, r2) = routers(short()).await;
    let quiet = spawn_dht_node_in(
        with_inventory(P1_INVENTORY),
        DhtMode::Client,
        &[&r1, &r2],
        with_timing(short()),
        |_| {},
    )
    .await;
    wait_for(&quiet, "the quiet one entered", |s| s.routing_peers == 2).await;
    // One that announces, entering after it, as the yardstick.
    let p2 = spawn_provider(P2_INVENTORY, &[&r1, &r2], short()).await;
    for router in [&r1, &r2] {
        let now = wait_for(router, "P2's records", |s| held(s, p2.id()) == 3).await;
        assert!(!now.stored_by_agent.contains_key(&quiet.id()), "{now:?}");
    }
    assert_eq!(status(&quiet).await.publication, None);
}

#[tokio::test]
async fn a_client_providers_record_carries_no_ip_address() {
    let (r1, r2) = routers(short()).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1, &r2], short()).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], short()).await;
    wait_for(&r1, "P1's records", |s| held(s, p1.id()) == 4).await;

    // The client form of the vector, and nothing else.
    let held_by_r1 = dht(&r1).held_providers("vision.ocr").await.unwrap();
    let client_form = located_nowhere(&peer_id(&p1.id()));
    assert_eq!(held_by_r1, [(p1.id(), vec![client_form.clone()])]);
    assert_eq!(
        client_form.to_string(),
        format!("/p2p/{}", peer_id(&p1.id()))
    );
    let found = wait_for_lookup(&c, "vision.ocr", "P1 found", |l| !l.found.is_empty()).await;
    assert_eq!(agents(&found), [p1.id()]);
}

#[tokio::test]
async fn concurrent_lookups_and_a_round_all_finish() {
    // Four lookups and a round want about ten requests at once at each
    // router, which takes 8 streams at a time from one peer: P2 keeps its
    // own within the routers' quotas, and none is refused.
    let (r1, r2) = routers(short()).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1, &r2], short()).await;
    for router in [&r1, &r2] {
        wait_for(router, "P1's records", |s| held(s, p1.id()) == 4).await;
    }
    // P2 publishes with a clock of the test's, so that each of its rounds
    // says when it ended, and only when its records change.
    let key = AgentKey::generate();
    let start = now_unix();
    let clock = Arc::new(ManualClock::new(start));
    let records = TestRecords::of(&key, &["code.rust.review"]);
    let timing = DhtTiming {
        republish: Duration::from_secs(3600),
        ..short()
    };
    let p2 = Publisher::spawn(
        &key,
        Arc::clone(&records),
        &[&r1, &r2],
        timing,
        Arc::clone(&clock) as _,
    )
    .await;
    wait_for_publication(&p2.dht, "P2's first round", |p| {
        p.last_round_unix == Some(start) && p.last_error.is_none()
    })
    .await;

    // Four lookups of P2's at once, two that stop at the first provider and
    // two that do not, and a round of P2's forced in the middle of them.
    clock.advance(1);
    let deadline = tokio::time::Instant::now() + CONVERGES;
    let lookups = [1, 8, 1, 8].map(|limit| p2.dht.providers("vision.ocr", limit, deadline));
    let (found, ()) = tokio::join!(futures::future::join_all(lookups), async { records.bump() });
    let found: Vec<DhtLookup> = found
        .into_iter()
        .map(|lookup| lookup.expect("a router answers"))
        .collect();
    for lookup in &found {
        assert_eq!(agents(lookup), [p1.id()], "{lookup:?}");
        assert!(!lookup.cut, "{lookup:?}");
        // Each lookup counts its own requests, and none of another's.
        assert!(lookup.requests > 0, "{lookup:?}");
        assert_eq!(lookup.requests, found[0].requests, "{found:?}");
    }
    let round = wait_for_publication(&p2.dht, "the forced round", |p| {
        p.last_round_unix == Some(start + 1)
    })
    .await;
    assert_eq!(round.last_error, None, "{round:?}");
    assert!(round.failed_capabilities.is_empty(), "{round:?}");
    for router in [&r1, &r2] {
        let now = status(router).await;
        assert_eq!(now.refused.streams, 0, "{now:?}");
    }
}

#[tokio::test]
async fn a_restarted_router_gets_the_announce_before_the_providers() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], short()).await;
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
    let r2 = router_on(
        tempfile::tempdir().unwrap(),
        listen,
        &[&r1],
        with_timing(short()),
    )
    .await;
    wait_for(&r2, "R1 in R2's table", |s| s.routing_peers == 1).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1, &r2], short()).await;
    wait_for(&r2, "P1's records", |s| held(s, p1.id()) == 4).await;

    // R2 starts again, empty, where it was.
    let TestNode { node, dir, .. } = r2;
    shut_down(node).await;
    let r2 = router_on(dir, listen, &[&r1], with_timing(short())).await;
    let now = wait_for(&r2, "P1's records again", |s| held(s, p1.id()) == 4).await;
    assert_eq!(now.rejected["provider_without_announce"], 0, "{now:?}");
    let mut held_by_r2 = holders(&r2, "vision.ocr").await;
    held_by_r2.extend(holders(&r2, "knowledge.documentation").await);
    assert_eq!(held_by_r2, [p1.id(), p1.id()]);
}

#[tokio::test]
async fn an_address_change_republishes_the_contact_at_once() {
    let (r1, r2) = routers(long()).await;
    let key = AgentKey::generate();
    let records = TestRecords::of(&key, &["vision.ocr"]);
    // Rounds every hour: only a change publishes again within the test.
    let timing = DhtTiming {
        republish: Duration::from_secs(3600),
        ..long()
    };
    let _p = Publisher::spawn(
        &key,
        Arc::clone(&records),
        &[&r1, &r2],
        timing,
        Arc::new(SystemClock),
    )
    .await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], long()).await;
    for router in [&r1, &r2] {
        wait_for(router, "its records", |s| held(s, key.id()) == 3).await;
    }
    let seq_found = |lookup: &DhtLookup| {
        lookup
            .found
            .iter()
            .find(|found| found.agent_id == key.id())
            .map(|found| found.contact.doc.seq)
    };
    wait_for_lookup(&c, "vision.ocr", "the contact of seq 1", |l| {
        seq_found(l) == Some(1)
    })
    .await;

    // The agent's addresses change: it issues a contact of seq 2.
    let moved = SocketAddr::from(([192, 0, 2, 11], 4433));
    records.set_contact(contact(&key, 2, &[moved]));
    let found = wait_for_lookup(&c, "vision.ocr", "the contact of seq 2", |l| {
        seq_found(l) == Some(2)
    })
    .await;
    let candidate = &found.found[0];
    assert_eq!(candidate.contact.doc.iroh.direct_addrs, [moved.to_string()]);
    // Each router holds it, not only one that a lookup reached.
    for router in [&r1, &r2] {
        wait_for_held_contact(router, key.id(), 2).await;
    }
}

/// Waits until `router` holds `agent`'s contact of `seq`; fails after
/// [`CONVERGES`].
async fn wait_for_held_contact(router: &TestNode, agent: AgentId, seq: u64) {
    let deadline = Instant::now() + CONVERGES;
    loop {
        let (contact, _) = dht(router).held_records(&agent).await.unwrap();
        let held = contact
            .and_then(|json| Signed::<ContactRecord>::from_json(&json).ok())
            .map(|contact| contact.doc.seq);
        if held == Some(seq) {
            return;
        }
        assert!(Instant::now() < deadline, "the router holds {held:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn records_expire_at_the_routers() {
    let (r1, r2) = routers(short()).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1, &r2], short()).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], short()).await;
    for router in [&r1, &r2] {
        wait_for(router, "P1's records", |s| held(s, p1.id()) == 4).await;
    }
    wait_for_lookup(&c, "vision.ocr", "P1 found", |l| !l.found.is_empty()).await;

    // P1 stops and tells no one; its records' life at the routers runs out.
    let p1_id = p1.id();
    let TestNode { node, .. } = p1;
    shut_down(node).await;
    for router in [&r1, &r2] {
        let now = wait_for(router, "P1's records expired", |s| {
            !s.stored_by_agent.contains_key(&p1_id)
        })
        .await;
        assert_eq!(now.records_stored, 0, "{now:?}");
        assert_eq!(now.provider_records_stored, 0, "{now:?}");
    }
    let gone = lookup(&c, "vision.ocr", 8).await;
    assert!(
        gone.found.is_empty() && gone.discarded.is_empty(),
        "{gone:?}"
    );
}

#[tokio::test]
async fn a_router_back_after_a_stop_is_dialed_again() {
    // Nothing is published again nor replicated while the test runs: R1's
    // incoming connections are C's.
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
    let r1 = router_on(
        tempfile::tempdir().unwrap(),
        listen,
        &[],
        with_timing(long()),
    )
    .await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], long()).await;
    wait_for(&r1, "R2 in R1's table", |s| s.routing_peers == 1).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1, &r2], long()).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], long()).await;
    for router in [&r1, &r2] {
        wait_for(router, "P1's records", |s| held(s, p1.id()) == 4).await;
    }
    wait_for_lookup(&c, "vision.ocr", "P1 found", |l| !l.found.is_empty()).await;

    // R1 stops: C still finds P1, through R2.
    let TestNode { node, dir, .. } = r1;
    shut_down(node).await;
    wait_for_lookup(&c, "vision.ocr", "P1 found without R1", |l| {
        agents(l) == [p1.id()]
    })
    .await;

    // R1 is back where it was: C's next lookup dials it again.
    let r1 = router_on(dir, listen, &[&r2], with_timing(long())).await;
    let before = status(&r1).await.connections_accepted;
    wait_for_lookup(&c, "vision.ocr", "P1 found", |l| agents(l) == [p1.id()]).await;
    wait_for(&r1, "C dialed R1 again", |s| {
        s.connections_accepted > before
    })
    .await;
}

/// Two public services: `vision.ocr`, then `code.rust.review`.
const OCR_AND_REVIEW: &str = r#"
[[services]]
id = "ocr"
name = "OCR"
description = "Reads the text of an image"
capabilities = ["vision.ocr"]
visibility = "public"

[[services]]
id = "review"
name = "Rust review"
description = "Reviews Rust code"
capabilities = ["code.rust.review"]
visibility = "public"
"#;

/// A router with no other, that announces [`OCR_AND_REVIEW`] and has room
/// for the providers of one key.
async fn crowded_router() -> TestNode {
    let opts = NodeOptions {
        kad_store: MemoryStoreConfig {
            max_provided_keys: 1,
            ..STORE_LIMITS
        },
        ..with_timing(short())
    };
    spawn_dht_node_in(
        with_inventory(OCR_AND_REVIEW),
        DhtMode::Router,
        &[],
        opts,
        |cfg| cfg.dht.announce = true,
    )
    .await
}

#[tokio::test]
async fn a_capability_that_does_not_fit_fails_alone() {
    let r3 = crowded_router().await;
    let c = spawn_dht_node(DhtMode::Client, &[&r3], short()).await;
    let published = wait_for_publication(dht(&r3), "R3's first round", |p| {
        p.last_round_unix.is_some()
    })
    .await;
    assert_eq!(published.capabilities, ["vision.ocr", "code.rust.review"]);
    assert_eq!(published.failed_capabilities, ["code.rust.review"]);
    let found = wait_for_lookup(&c, "vision.ocr", "R3 found", |l| !l.found.is_empty()).await;
    assert_eq!(agents(&found), [r3.id()]);
}

#[tokio::test]
async fn a_round_that_reaches_no_router_is_retried() {
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
    let r1 = router_on(
        tempfile::tempdir().unwrap(),
        listen,
        &[],
        with_timing(long()),
    )
    .await;
    let ticket = r1.node.ticket();
    let TestNode { node, dir, .. } = r1;
    shut_down(node).await;

    // P1 starts with R1 down, and publishes again only after a round that
    // reached no router.
    let timing = DhtTiming {
        republish: Duration::from_secs(3600),
        retry_after: Duration::from_secs(1),
        ..short()
    };
    let p1 = spawn_dht_node_in(
        with_inventory(P1_INVENTORY),
        DhtMode::Client,
        &[],
        with_timing(timing),
        |cfg| {
            cfg.dht.bootstrap = vec![ticket];
            cfg.dht.announce = true;
        },
    )
    .await;
    wait_for_publication(dht(&p1), "a round that reached no router", |p| {
        p.last_error.is_some()
    })
    .await;

    // R1 is back where it was, and gets P1's records.
    let r1 = router_on(dir, listen, &[], with_timing(long())).await;
    wait_for(&r1, "P1's records", |s| held(s, p1.id()) == 4).await;
    wait_for_publication(dht(&p1), "a round that reached R1", |p| {
        p.last_error.is_none()
    })
    .await;
}

#[tokio::test]
async fn a_full_router_store_is_counted() {
    // Room for two values: one agent's contact and announce.
    let opts = NodeOptions {
        kad_store: MemoryStoreConfig {
            max_records: 2,
            ..STORE_LIMITS
        },
        ..with_timing(long())
    };
    let r1 = spawn_dht_node_with(DhtMode::Router, &[], opts, |_| {}).await;
    let p1 = spawn_provider(P1_INVENTORY, &[&r1], long()).await;
    wait_for(&r1, "P1's records", |s| held(s, p1.id()) == 4).await;

    let p2 = spawn_provider(P2_INVENTORY, &[&r1], long()).await;
    let now = wait_for(&r1, "P2's records refused", |s| {
        s.rejected["store_full"] >= 1
    })
    .await;
    assert_eq!(held(&now, p1.id()), 4, "{now:?}");
    assert_eq!(held(&now, p2.id()), 0, "{now:?}");
}

#[tokio::test]
async fn expired_records_make_room_in_a_full_store() {
    // Room for one agent's contact and announce and the providers of one
    // key; what expires stays there, since R1 replicates nothing within
    // the test.
    let opts = NodeOptions {
        kad_store: MemoryStoreConfig {
            max_records: 2,
            max_provided_keys: 1,
            ..STORE_LIMITS
        },
        ..with_timing(DhtTiming {
            republish: Duration::from_secs(3600),
            ..short()
        })
    };
    let r1 = spawn_dht_node_with(DhtMode::Router, &[], opts, |_| {}).await;
    let p2 = spawn_provider(P2_INVENTORY, &[&r1], short()).await;
    wait_for(&r1, "P2's records", |s| held(s, p2.id()) == 3).await;
    let TestNode { node, .. } = p2;
    shut_down(node).await;
    wait_for(&r1, "P2's records expired", |s| {
        s.records_stored == 0 && s.provider_records_stored == 0
    })
    .await;

    // A store full of expired records takes a new one.
    let ocr_only = r#"
[[services]]
id = "ocr"
name = "OCR"
description = "Reads the text of an image"
capabilities = ["vision.ocr"]
visibility = "public"
"#;
    let p3 = spawn_provider(ocr_only, &[&r1], short()).await;
    let now = wait_for(&r1, "P3's records", |s| held(s, p3.id()) == 3).await;
    assert_eq!(now.rejected["store_full"], 0, "{now:?}");
}

/// A router that takes `agentnet-kad/0` connections and never says a word.
#[derive(Debug, Clone)]
struct Mute;

impl ProtocolHandler for Mute {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        connection.closed().await;
        Ok(())
    }
}

#[tokio::test]
async fn a_lookup_ends_at_its_deadline() {
    let mute = loopback_endpoint().await;
    let ticket = EndpointTicket::new(EndpointAddr::from_parts(
        mute.id(),
        mute.bound_sockets().into_iter().map(TransportAddr::Ip),
    ))
    .to_string();
    let _served = Router::builder(mute).accept(ALPN_KAD, Mute).spawn();
    let c = spawn_dht_node_with(DhtMode::Client, &[], with_timing(short()), |cfg| {
        cfg.dht.bootstrap = vec![ticket];
    })
    .await;

    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let cut = dht(&c)
        .providers("vision.ocr", 8, deadline)
        .await
        .expect("a lookup cut at its deadline");
    assert!(cut.cut, "{cut:?}");
    assert!(cut.found.is_empty(), "{cut:?}");
    assert!(cut.requests >= 1, "the mute router was asked: {cut:?}");
    // Its deadline, not the end of its queries.
    assert!(started.elapsed() < QUERY_TIMEOUT, "{:?}", started.elapsed());
}

#[tokio::test]
async fn a_provider_with_a_tampered_contact_is_discarded() {
    let (r1, r2) = routers(short()).await;
    let key = AgentKey::generate();
    let mut tampered =
        Signed::<ContactRecord>::from_json(&contact(&key, 1, &[somewhere()])).unwrap();
    let first = tampered.signature.remove(0);
    tampered
        .signature
        .insert(0, if first == 'A' { 'B' } else { 'A' });
    let tampered = tampered.to_json();
    assert!(Signed::<ContactRecord>::from_json(&tampered).is_err());
    let records = TestRecords::new(
        tampered,
        announce(&key, 1, &["vision.ocr"]),
        &["vision.ocr"],
    );
    let _t = Publisher::spawn(&key, records, &[&r1, &r2], short(), Arc::new(SystemClock)).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1, &r2], short()).await;

    // The routers refuse its contact, and keep its announce and its
    // provider record.
    for router in [&r1, &r2] {
        wait_for(router, "the contact refused", |s| {
            s.rejected["bad_contact"] >= 1 && held(s, key.id()) == 2
        })
        .await;
    }
    let found = wait_for_lookup(&c, "vision.ocr", "it discarded", |l| {
        l.discarded.iter().any(|(agent, _)| *agent == key.id())
    })
    .await;
    assert!(found.found.is_empty(), "{found:?}");
    assert_eq!(
        found.discarded,
        [(key.id(), "no valid contact".to_owned())],
        "{found:?}"
    );
}

#[tokio::test]
async fn a_round_of_32_capabilities_fits_the_routers_quotas() {
    // The most capabilities an announce lists: some 130 streams to each
    // router in one round, against routers with the default quotas.
    let (r1, r2) = routers(long()).await;
    let capabilities: Vec<String> = (0..MAX_ANNOUNCED_CAPABILITIES)
        .map(|n| format!("test.capability-{n:02}"))
        .collect();
    let listed: Vec<&str> = capabilities.iter().map(String::as_str).collect();
    let key = AgentKey::generate();
    let timing = DhtTiming {
        republish: Duration::from_secs(3600),
        ..long()
    };
    let p = Publisher::spawn(
        &key,
        TestRecords::of(&key, &listed),
        &[&r1, &r2],
        timing,
        Arc::new(SystemClock),
    )
    .await;
    let round = wait_for_publication(&p.dht, "P's round", |p| p.last_round_unix.is_some()).await;
    assert_eq!(round.last_error, None, "{round:?}");
    assert!(round.failed_capabilities.is_empty(), "{round:?}");
    for router in [&r1, &r2] {
        let now = status(router).await;
        assert_eq!(now.refused.streams, 0, "{now:?}");
        let now = wait_for(router, "P's provider records", |s| {
            held(s, key.id()) == 2 + 32
        })
        .await;
        assert_eq!(now.refused.streams, 0, "{now:?}");
        for capability in &capabilities {
            assert_eq!(
                holders(router, capability).await,
                [key.id()],
                "{capability}"
            );
        }
    }
}

/// Runs `pals --home <home> <args>`, off the async threads: its exit code,
/// stdout and stderr.
async fn pals(home: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .env_remove("RUST_LOG")
        .arg("--home")
        .arg(home)
        .args(args);
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .unwrap();
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Points a configuration in `node`'s data directory at its gateway, so
/// that the `pals` command finds its key and its running daemon.
fn cli_home(node: &TestNode) -> &std::path::Path {
    let listen = node.gateway_url().replace("http://", "");
    std::fs::write(
        node.dir.path().join(CONFIG_FILE),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    node.dir.path()
}

#[tokio::test]
async fn pals_dht_status_prints_the_publication() {
    // A round that falls short both ways: no router took it, and one of
    // its capabilities did not fit.
    let r3 = crowded_router().await;
    wait_for_publication(dht(&r3), "R3's first round", |p| {
        p.last_round_unix.is_some()
    })
    .await;
    let (code, out, err) = pals(cli_home(&r3), &["dht", "status"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("publication: 2 capabilities, last round at 20"),
        "{out}"
    );
    assert!(out.contains("  failed: code.rust.review\n"), "{out}");
    assert!(
        out.contains("  last error: no DHT router answered\n"),
        "{out}"
    );
}
