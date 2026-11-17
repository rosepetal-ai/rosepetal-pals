//! The DHT as a source of a service search's candidates (design §5.3): real
//! nodes on loopback, with no relay, the routers R1 and R2, the providers P1
//! and P2 of `vision.ocr`, which announce it and whose work a
//! [`CountingExecutor`] does, and A, the agent that searches, with no
//! contact but where a test says so.
//!
//! A search looks in the DHT behind the contacts: only when the node runs
//! the DHT, and the request names no peers and does not say `exclude_dht`.
//! What it finds there is checked as any peer is, and a provider without a
//! grant of the project gets neither the project nor the inventory ids.
//!
//! The tests wait for the state they need and never for time to pass: the
//! routers hold each provider's records, and A's own lookup finds them,
//! before A searches. A deadline only fails a test. The searches are
//! patient: only a broken test waits their limits out.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use a2a_server::AgentExecutor;
use common::{
    CountingExecutor, StubMode, TestNode, grant, key_in, now_unix, shut_down,
    spawn_node_in_configured, stub_card, stub_discovery_peer_with_card,
};
use futures::future::BoxFuture;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey};
use pals::config::{CONFIG_FILE, Config, DhtCfg, DhtMode, INVENTORY_FILE};
use pals::dht::swarm::STORE_LIMITS;
use pals::dht::transport::{ALPN_KAD, KadQuotas};
use pals::dht::{Dht, DhtLookup, DhtParts, DhtStatus, DhtTiming, PublishedRecords};
use pals::discovery::{DiscoveryHook, DiscoveryLimits};
use pals::node::NodeOptions;
use pals::outbox::AskRequest;
use pals_core::benchmark::Wanted;
use pals_core::card_jws::{CardHash, SignatureStatus};
use pals_core::clock::SystemClock;
use pals_core::discovery::query::{
    InventoryRequirements, OfferStatus, RequirementSet, Requirements, ServiceQuery,
};
use pals_core::discovery::selection::{Candidate, Coverage, SearchOutcome, SearchRequest, Stage};
use pals_core::discovery::wire::{self, ErrorCode, request, response};
use pals_core::discovery::{DISCOVERY_VERSION, server_limits};
use pals_core::grant::Operation::{TaskCancel, TaskRead, TaskSend};
use pals_core::grant::{format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{
    ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact, RECORD_TTL_S,
};
use pals_core::signed::Signed;
use pals_core::store::{RecordAccept, Store};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::sync::watch;

/// How long a test waits for the network to reach the state it needs: past
/// it, the test fails.
const CONVERGES: Duration = Duration::from_secs(60);

/// The pause between two lookups of [`wait_found`]: well under a router's
/// 600 streams a minute from one peer, beside the rest of the test's
/// traffic.
const LOOKUP_PAUSE: Duration = Duration::from_secs(2);

/// The capability P1 and P2 announce, and A looks for.
const CAPABILITY: &str = "vision.ocr";

/// P1's and P2's inventory: one public service of [`CAPABILITY`].
const OCR: &str = r#"
[[services]]
id = "ocr"
name = "OCR"
description = "Reads the text of an image"
capabilities = ["vision.ocr"]
visibility = "public"
"#;

/// How long the providers' offers last: far longer than any loaded host's
/// delay between an offer and the ask that names it.
const LONG_OFFER_MS: u32 = 300_000;

/// How long an ask waits for its answer: far more than any needs.
const WAIT: Duration = Duration::from_secs(120);

/// Every task operation.
const TASK_OPS: [pals_core::grant::Operation; 3] = [TaskSend, TaskRead, TaskCancel];

/// The configuration's default DHT times: nothing expires or is published
/// again while a test runs.
fn timing() -> DhtTiming {
    DhtTiming::from_config(&DhtCfg::default())
}

/// The limits of a search that waits for its candidates, and for its
/// lookup, as long as a loaded host may need.
fn patient() -> DiscoveryLimits {
    DiscoveryLimits {
        per_candidate_timeout: Duration::from_secs(60),
        total_budget: Duration::from_secs(300),
        ..DiscoveryLimits::default()
    }
}

/// A search for [`CAPABILITY`], and nothing else.
fn request() -> SearchRequest {
    SearchRequest {
        capability: CAPABILITY.to_owned(),
        ..SearchRequest::default()
    }
}

/// A node on `dir` with the DHT on, in `mode`, entering it through the
/// tickets of `bootstrap`, with the default DHT times, whose work
/// `executor` does, with `opts` and its configuration changed by `edit`
/// after the DHT's section is written.
async fn dht_node(
    dir: tempfile::TempDir,
    mode: DhtMode,
    bootstrap: &[&TestNode],
    executor: impl AgentExecutor,
    opts: NodeOptions,
    edit: impl FnOnce(&mut Config),
) -> TestNode {
    let tickets: Vec<String> = bootstrap.iter().map(|node| node.node.ticket()).collect();
    let opts = NodeOptions {
        dht_timing: Some(timing()),
        ..opts
    };
    spawn_node_in_configured(dir, executor, opts, |cfg| {
        cfg.dht = DhtCfg {
            enabled: true,
            mode,
            bootstrap: tickets,
            ..DhtCfg::default()
        };
        edit(cfg);
    })
    .await
}

/// `node`'s DHT.
fn dht(node: &TestNode) -> &Dht {
    node.node.dht().expect("the node's DHT is on")
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

/// How many values and provider records of `agent` a router's `status`
/// counts.
fn held(status: &DhtStatus, agent: AgentId) -> u32 {
    status.stored_by_agent.get(&agent).copied().unwrap_or(0)
}

/// Looks [`CAPABILITY`] up from `node`'s own DHT until the lookup finds
/// every one of `agents`, and returns that lookup; fails after
/// [`CONVERGES`].
async fn wait_found(node: &TestNode, agents: &[AgentId]) -> DhtLookup {
    let deadline = Instant::now() + CONVERGES;
    loop {
        let answer = tokio::time::Instant::now() + CONVERGES;
        let lookup = dht(node).providers(CAPABILITY, 8, answer).await;
        if let Ok(found) = &lookup
            && agents
                .iter()
                .all(|agent| found.found.iter().any(|f| f.agent_id == *agent))
        {
            return found.clone();
        }
        assert!(Instant::now() < deadline, "{agents:?} found: {lookup:?}");
        tokio::time::sleep(LOOKUP_PAUSE).await;
    }
}

/// The DHT of every test: two routers that know each other, and the two
/// providers, whose records both routers hold. Each node's work is its own
/// [`CountingExecutor`]'s.
struct Net {
    r1: TestNode,
    r2: TestNode,
    p1: TestNode,
    p2: TestNode,
    r1_exec: CountingExecutor,
    r2_exec: CountingExecutor,
    p1_exec: CountingExecutor,
    p2_exec: CountingExecutor,
}

/// A router entering through `bootstrap`, whose work `exec` does.
async fn router(bootstrap: &[&TestNode], exec: CountingExecutor) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    let opts = NodeOptions::default();
    dht_node(dir, DhtMode::Router, bootstrap, exec, opts, |_| {}).await
}

/// A DHT client with [`OCR`] that announces it, entering through
/// `routers`, with `opts`.
async fn provider(routers: &[&TestNode], exec: CountingExecutor, opts: NodeOptions) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), OCR).unwrap();
    dht_node(dir, DhtMode::Client, routers, exec, opts, |cfg| {
        cfg.dht.announce = true;
        cfg.discovery.offer_valid_for_ms = LONG_OFFER_MS;
    })
    .await
}

/// [`Net`], with P1 running with `p1_opts`.
async fn network(p1_opts: NodeOptions) -> Net {
    let execs: Vec<CountingExecutor> = (0..4)
        .map(|_| CountingExecutor::new(Duration::ZERO))
        .collect();
    let (r1_exec, r2_exec, p1_exec, p2_exec) = (
        execs[0].clone(),
        execs[1].clone(),
        execs[2].clone(),
        execs[3].clone(),
    );
    let r1 = router(&[], r1_exec.clone()).await;
    let r2 = router(&[&r1], r2_exec.clone()).await;
    wait_for(&r1, "R2 in R1's table", |s| s.routing_peers == 1).await;
    wait_for(&r2, "R1 in R2's table", |s| s.routing_peers == 1).await;
    let p1 = provider(&[&r1, &r2], p1_exec.clone(), p1_opts).await;
    let p2 = provider(&[&r1, &r2], p2_exec.clone(), NodeOptions::default()).await;
    // Each provider's contact, announce and provider record.
    for router in [&r1, &r2] {
        for p in [&p1, &p2] {
            wait_for(router, "a provider's records", |s| held(s, p.id()) == 3).await;
        }
    }
    Net {
        r1,
        r2,
        p1,
        p2,
        r1_exec,
        r2_exec,
        p1_exec,
        p2_exec,
    }
}

/// A, a DHT client entering through both routers, whose searches are
/// [`patient`] unless `opts` says otherwise, with its configuration changed
/// by `edit`; once its own lookup finds both providers.
async fn requester(net: &Net, opts: NodeOptions, edit: impl FnOnce(&mut Config)) -> TestNode {
    let opts = NodeOptions {
        search_limits: opts.search_limits.or(Some(patient())),
        ..opts
    };
    let dir = tempfile::tempdir().unwrap();
    let exec = CountingExecutor::new(Duration::ZERO);
    let a = dht_node(dir, DhtMode::Client, &[&net.r1, &net.r2], exec, opts, edit).await;
    wait_for(&a, "A entered the network", |s| s.routing_peers == 2).await;
    wait_found(&a, &[net.p1.id(), net.p2.id()]).await;
    a
}

/// [`requester`] with the default options and configuration.
async fn plain_requester(net: &Net) -> TestNode {
    requester(net, NodeOptions::default(), |_| {}).await
}

/// The candidate `outcome` ranks of `agent`.
fn ranked_as(outcome: &SearchOutcome, agent: AgentId) -> &Candidate {
    outcome
        .ranked
        .iter()
        .find(|candidate| candidate.agent_id == Some(agent))
        .unwrap_or_else(|| panic!("{agent} ranked: {outcome:#?}"))
}

/// Whether `outcome` names `agent` anywhere, ranked or discarded.
fn names(outcome: &SearchOutcome, agent: AgentId) -> bool {
    outcome
        .ranked
        .iter()
        .any(|candidate| candidate.agent_id == Some(agent))
        || outcome
            .discarded
            .iter()
            .any(|discarded| discarded.peer == agent.to_string())
}

/// Where `peer` was discarded, and why.
fn discard<'a>(outcome: &'a SearchOutcome, peer: &str) -> (Stage, &'a str) {
    let found = outcome
        .discarded
        .iter()
        .find(|d| d.peer == peer)
        .unwrap_or_else(|| panic!("{peer} discarded: {:#?}", outcome.discarded));
    (found.stage, found.reason.as_str())
}

/// The number of tasks `node` accepted, by its event log.
fn accepted(node: &TestNode) -> u64 {
    node.node.store.events_count("task.accepted").unwrap()
}

/// The direct address `node` is dialed at.
fn socket_of(node: &TestNode) -> SocketAddr {
    *node
        .node
        .addr()
        .ip_addrs()
        .next()
        .expect("a direct address")
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

/// `key`'s announce with `seq`, of the card `card_hash`, listing
/// `capabilities`, valid now.
fn announce(key: &AgentKey, seq: u64, card_hash: &CardHash, capabilities: &[&str]) -> Vec<u8> {
    let issued_at = now_unix() - 60;
    let doc = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: key.id(),
        seq,
        agent_card_hash: card_hash.clone(),
        capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
        discovery_alpn: "agentnet-discovery/0".to_owned(),
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(issued_at + RECORD_TTL_S).unwrap(),
    };
    Signed::issue(key, doc).unwrap().to_json()
}

/// Records a test publishes as it likes.
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
    _dht: Dht,
    _router: Router,
}

impl Publisher {
    /// Starts it on a loopback endpoint with `key`, entering through
    /// `routers`.
    async fn spawn(key: &AgentKey, records: Arc<TestRecords>, routers: &[&TestNode]) -> Self {
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
            timing: timing(),
            quotas: KadQuotas::default(),
            store_limits: STORE_LIMITS,
            clock: Arc::new(SystemClock),
            store: Arc::new(Store::open_in_memory(key.id()).unwrap()),
            published: Some(records),
        })
        .unwrap();
        let router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
        Self {
            _dht: dht,
            _router: router,
        }
    }
}

/// A stub mode that keeps every request in `kept` and refuses it.
fn refusing(kept: Arc<Mutex<Vec<wire::Request>>>) -> StubMode {
    StubMode::Record(
        kept,
        wire::Response {
            version: DISCOVERY_VERSION,
            limits: Some(server_limits()),
            contact_record: Vec::new(),
            announce_record: Vec::new(),
            result: Some(response::Result::Error(wire::Error::new(
                ErrorCode::NotFound,
                "not found",
            ))),
        },
    )
}

/// Keeps every request a node's discovery server reads, with who sent it.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<(AgentId, wire::Request)>>>);

impl DiscoveryHook for Recorder {
    fn before_answer(&self, peer: &AgentId, req: &wire::Request) -> BoxFuture<'static, ()> {
        self.0.lock().unwrap().push((*peer, req.clone()));
        Box::pin(async {})
    }
}

impl Recorder {
    /// The service queries `from` sent, as they arrived.
    fn queries_of(&self, from: AgentId) -> Vec<ServiceQuery> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(peer, _)| *peer == from)
            .filter_map(|(_, req)| match &req.op {
                Some(request::Op::ServiceQuery(query)) => {
                    Some(ServiceQuery::from_jcs(&query.query).unwrap())
                }
                _ => None,
            })
            .collect()
    }
}

/// Runs `pals --home <home> <args>`, off the async threads; its exit code,
/// stdout and stderr.
async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .arg("--home")
        .arg(home)
        .args(args);
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .expect("the command's thread");
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
    )
}

type Client = RunningService<RoleClient, ClientConfig>;

/// An MCP session with `node`'s gateway.
async fn mcp_client(node: &TestNode) -> Client {
    pals::install_crypto_provider();
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", node.gateway_url()));
    ClientConfig::default()
        .serve(StreamableHttpClientTransport::from_config(config))
        .await
        .expect("an MCP session with the gateway")
}

/// Calls `tool` with `args`.
async fn call(client: &Client, tool: &str, args: Value) -> CallToolResult {
    let args = args.as_object().cloned().expect("arguments are an object");
    client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await
        .unwrap_or_else(|err| panic!("{tool} got no result: {err}"))
}

/// The JSON a call that succeeded answered with.
fn ok_json(result: &CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let text = &result.content[0].as_text().expect("a text result").text;
    serde_json::from_str(text).expect("a JSON result")
}

#[tokio::test]
async fn a_provider_with_no_prior_contact_is_found() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    assert!(a.node.store.contacts().unwrap().is_empty());

    let outcome = a.node.searcher().search(request()).await.unwrap();
    let p1 = ranked_as(&outcome, net.p1.id());
    assert!(p1.from_dht, "{p1:#?}");
    assert!(!p1.grant_held, "{p1:#?}");
    assert_eq!(p1.peer, net.p1.id().to_string());
    assert_eq!(p1.skill_id.as_deref(), Some("ocr"));
    assert_eq!(
        p1.card_signature,
        SignatureStatus::Verified {
            kid: net.p1.id().to_string()
        }
    );
    // Its offer, verified as P1's for A, of the public scope, naming the
    // card read.
    let offer = p1.offer.as_ref().expect("an offer");
    assert_eq!(offer.agent_id, net.p1.id());
    assert_eq!(offer.requester, a.id());
    assert_eq!(offer.scope, "public");
    assert_eq!(offer.status, OfferStatus::Available);
    assert_eq!(offer.agent_card_hash, p1.card_hash);
    assert!(outcome.metrics.dht_requests > 0, "{:?}", outcome.metrics);
    assert_eq!(outcome.coverage, Coverage::Complete, "{outcome:#?}");
    // A still has no contact: the DHT found P1.
    assert!(a.node.store.contacts().unwrap().is_empty());
}

#[tokio::test]
async fn contacts_come_first_and_each_agent_once() {
    let net = network(NodeOptions::default()).await;
    let one = NodeOptions {
        search_limits: Some(DiscoveryLimits {
            max_candidates: 1,
            ..patient()
        }),
        ..NodeOptions::default()
    };
    let a1 = requester(&net, one, |_| {}).await;
    let a8 = plain_requester(&net).await;
    for a in [&a1, &a8] {
        a.add_contact("p2", &net.p2);
    }

    // With room for one, only the contact is evaluated, and the DHT's
    // provider is left out.
    let outcome = a1.node.searcher().search(request()).await.unwrap();
    assert_eq!(outcome.metrics.contacted, 1, "{outcome:#?}");
    assert_eq!(outcome.ranked.len(), 1, "{outcome:#?}");
    assert_eq!(outcome.ranked[0].peer, "p2");
    assert!(!outcome.ranked[0].from_dht);
    assert!(!names(&outcome, net.p1.id()), "{outcome:#?}");
    assert_eq!(
        outcome.coverage,
        Coverage::Partial {
            reasons: vec!["candidate limit".to_owned()]
        }
    );

    // With room for eight, P2 once, as the contact, and P1 from the DHT.
    let outcome = a8.node.searcher().search(request()).await.unwrap();
    let p2s: Vec<&Candidate> = outcome
        .ranked
        .iter()
        .filter(|candidate| candidate.agent_id == Some(net.p2.id()))
        .collect();
    assert_eq!(p2s.len(), 1, "{outcome:#?}");
    assert_eq!(p2s[0].peer, "p2");
    assert!(!p2s[0].from_dht);
    assert!(
        !outcome
            .discarded
            .iter()
            .any(|d| d.peer == net.p2.id().to_string() || d.peer == "p2"),
        "{outcome:#?}"
    );
    assert!(ranked_as(&outcome, net.p1.id()).from_dht);
    assert_eq!(outcome.metrics.contacted, 2, "{outcome:#?}");
}

#[tokio::test]
async fn exclude_dht_and_named_peers_never_touch_the_dht() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    a.add_contact("p2", &net.p2);
    // A search that may look in the DHT finds P1 there.
    let open = a.node.searcher().search(request()).await.unwrap();
    assert!(ranked_as(&open, net.p1.id()).from_dht);
    assert!(open.metrics.dht_requests > 0, "{:?}", open.metrics);

    let excluded = SearchRequest {
        exclude_dht: true,
        ..request()
    };
    let named = SearchRequest {
        peers: Some(vec!["p2".to_owned()]),
        ..request()
    };
    for req in [excluded, named] {
        let outcome = a.node.searcher().search(req).await.unwrap();
        assert_eq!(outcome.metrics.dht_requests, 0, "{:?}", outcome.metrics);
        assert!(!names(&outcome, net.p1.id()), "{outcome:#?}");
        let ranked: Vec<&str> = outcome.ranked.iter().map(|c| c.peer.as_str()).collect();
        assert_eq!(ranked, ["p2"], "{outcome:#?}");
        assert_eq!(outcome.coverage, Coverage::Complete, "{outcome:#?}");
    }
}

#[tokio::test]
async fn dht_without_routers_leaves_the_contacts_search_intact() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    a.add_contact("p2", &net.p2);
    let Net { r1, r2, p2, .. } = net;
    let TestNode { node: r1_node, .. } = r1;
    let TestNode { node: r2_node, .. } = r2;
    shut_down(r1_node).await;
    shut_down(r2_node).await;

    let outcome = a.node.searcher().search(request()).await.unwrap();
    let ranked: Vec<&str> = outcome.ranked.iter().map(|c| c.peer.as_str()).collect();
    assert_eq!(ranked, ["p2"], "{outcome:#?}");
    assert_eq!(outcome.ranked[0].agent_id, Some(p2.id()));
    assert_eq!(
        outcome.coverage,
        Coverage::Partial {
            reasons: vec!["dht unavailable".to_owned()]
        }
    );
}

#[tokio::test]
async fn a_tampered_card_is_discarded_even_with_a_valid_dht_announce() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    // The stub serves a card altered after it was signed. It is saved as a
    // contact of P2's, never of A's: A only finds it in the DHT.
    let key = AgentKey::generate();
    let mut altered = stub_card(&key, OCR);
    altered.json["description"] = json!("Reads any text, faster");
    let card_hash = altered.announce_hash.clone();
    let kept = Arc::new(Mutex::new(Vec::new()));
    let stub = stub_discovery_peer_with_card(
        &net.p2,
        "tampered",
        AgentKey::from_bytes(&key.to_bytes()),
        refusing(Arc::clone(&kept)),
        altered,
        false,
    )
    .await;
    let socket = *stub.addr().ip_addrs().next().expect("a direct address");
    // Its valid records, naming the card it signed, in the DHT.
    let records = TestRecords::new(
        contact(&key, 1, &[socket]),
        announce(&key, 1, &card_hash, &[CAPABILITY]),
        &[CAPABILITY],
    );
    let _publisher = Publisher::spawn(&key, records, &[&net.r1, &net.r2]).await;
    for router in [&net.r1, &net.r2] {
        wait_for(router, "the stub's records", |s| held(s, stub.id) == 3).await;
    }
    wait_found(&a, &[net.p1.id(), net.p2.id(), stub.id]).await;

    let outcome = a.node.searcher().search(request()).await.unwrap();
    let (stage, why) = discard(&outcome, &stub.id.to_string());
    assert_eq!(stage, Stage::Card);
    assert!(why.contains("invalid"), "{why}");
    assert!(
        !outcome
            .ranked
            .iter()
            .any(|candidate| candidate.agent_id == Some(stub.id))
    );
    // Never asked for an offer.
    assert!(kept.lock().unwrap().is_empty());
    assert!(ranked_as(&outcome, net.p1.id()).from_dht);
}

#[tokio::test]
async fn a_dht_record_that_goes_back_is_discarded() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    let p1 = net.p1.id();
    // A knows a contact of P1's newer than the one the DHT serves (the
    // brief's 3 against 2): one more than whatever seq the DHT serves now,
    // since a real node's first seq is not fixed.
    let lookup = wait_found(&a, &[p1]).await;
    let served = lookup
        .found
        .iter()
        .find(|found| found.agent_id == p1)
        .expect("P1 found")
        .contact
        .doc
        .seq;
    let newer = contact(
        &key_in(net.p1.dir.path()),
        served + 1,
        &[socket_of(&net.p1)],
    );
    let expires_at = Signed::<ContactRecord>::from_json(&newer)
        .unwrap()
        .doc
        .expires_at;
    let accepted = a
        .node
        .store
        .peer_record_accept(
            &p1,
            "contact",
            served + 1,
            &newer,
            parse_timestamp(&expires_at).unwrap(),
            now_unix(),
        )
        .unwrap();
    assert_eq!(accepted, RecordAccept::New);

    let outcome = a.node.searcher().search(request()).await.unwrap();
    assert_eq!(
        discard(&outcome, &p1.to_string()),
        (Stage::Card, "record regression")
    );
    assert!(!outcome.ranked.iter().any(|c| c.agent_id == Some(p1)));
    assert!(ranked_as(&outcome, net.p2.id()).from_dht);
}

#[tokio::test]
async fn inventory_ids_never_go_to_a_dht_candidate_without_a_grant() {
    let hook = Recorder::default();
    let opts = NodeOptions {
        discovery_hook: Some(Arc::new(hook.clone())),
        ..NodeOptions::default()
    };
    let net = network(opts).await;
    let a = plain_requester(&net).await;
    // For the project `demo`, preferring inventory ids: a preference never
    // discards, so P1 stays in the ranking.
    let req = SearchRequest {
        project: Some("demo".to_owned()),
        requirements: Requirements {
            inventory: Some(InventoryRequirements {
                required: RequirementSet::default(),
                preferred: RequirementSet {
                    mcp_integration_ids: vec!["example:repo-reader".to_owned()],
                    information_source_ids: vec!["project:repo-controller".to_owned()],
                    ..RequirementSet::default()
                },
            }),
            ..Requirements::default()
        },
        ..request()
    };
    let outcome = a.node.searcher().search(req).await.unwrap();
    let p1 = ranked_as(&outcome, net.p1.id());
    assert!(!p1.grant_held);
    assert!(p1.from_dht);

    let queries = hook.queries_of(a.id());
    assert!(!queries.is_empty(), "P1 was asked");
    for query in queries {
        assert_eq!(query.capability, CAPABILITY);
        assert_eq!(query.project, None, "{query:?}");
        assert_eq!(query.requirements.inventory, None, "{query:?}");
        let jcs = String::from_utf8(query.to_jcs()).unwrap();
        for withheld in ["example:repo-reader", "project:repo-controller", "demo"] {
            assert!(!jcs.contains(withheld), "{withheld}: {jcs}");
        }
    }
}

#[tokio::test]
async fn an_alias_searches_its_capability() {
    let net = network(NodeOptions::default()).await;
    let a = requester(&net, NodeOptions::default(), |cfg| {
        cfg.discovery.aliases = BTreeMap::from([("ocr".to_owned(), CAPABILITY.to_owned())]);
    })
    .await;
    let req = SearchRequest {
        capability: "ocr".to_owned(),
        ..SearchRequest::default()
    };
    let outcome = a.node.searcher().search(req).await.unwrap();
    let p1 = ranked_as(&outcome, net.p1.id());
    assert!(p1.from_dht);
    assert_eq!(p1.skill_id.as_deref(), Some("ocr"));
    assert!(ranked_as(&outcome, net.p2.id()).from_dht);
}

#[test]
fn aliases_to_invalid_capabilities_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let load = |text: &str| {
        std::fs::write(&path, text).unwrap();
        Config::load(&path)
    };
    let cfg =
        load("[discovery.aliases]\nocr = \"vision.ocr\"\nplates = \"example:metal-plates\"\n")
            .unwrap();
    assert_eq!(
        cfg.discovery.aliases,
        BTreeMap::from([
            ("ocr".to_owned(), "vision.ocr".to_owned()),
            ("plates".to_owned(), "example:metal-plates".to_owned()),
        ])
    );
    // Without any, none.
    assert!(load("").unwrap().discovery.aliases.is_empty());
    for invalid in ["Vision OCR", "agentnet:ocr", "", "ocr:"] {
        let err = load(&format!("[discovery.aliases]\nocr = {invalid:?}\n")).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("discovery.aliases"), "{invalid:?}: {err}");
    }
    // 256 at most.
    let many = |n: usize| -> String {
        (0..n)
            .map(|i| format!("alias-{i} = \"vision.ocr\"\n"))
            .collect()
    };
    let cfg = load(&format!("[discovery.aliases]\n{}", many(256))).unwrap();
    assert_eq!(cfg.discovery.aliases.len(), 256);
    let err = load(&format!("[discovery.aliases]\n{}", many(257))).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("discovery.aliases"), "{err}");
}

#[tokio::test]
async fn a_dht_candidate_is_asked_once_a_grant_is_held() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    a.hold(&grant(&net.p1, &a, &TASK_OPS, 3600));

    let chosen = a
        .node
        .searcher()
        .choose_and_ask(request(), "hola".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.agent_id, Some(net.p1.id()), "{chosen:#?}");
    assert!(chosen.candidate.from_dht);
    assert!(chosen.candidate.grant_held);
    assert_eq!(chosen.outcome.state, "TASK_STATE_COMPLETED", "{chosen:#?}");
    assert_eq!(chosen.outcome.answer.as_deref(), Some("echo: hola"));
    assert_eq!(net.p1_exec.runs(), 1);
    assert_eq!(net.p2_exec.runs(), 0);
    // P1 is still no contact of A's.
    assert!(a.node.store.contact_by_id(&net.p1.id()).unwrap().is_none());
    assert!(a.node.store.contacts().unwrap().is_empty());

    // As `pals ask <id>` does, from A started again with the DHT off and no
    // relay: its endpoint has never dialed P1, and the gateway finds P1 at
    // the newest contact record A keeps of it.
    let TestNode {
        node,
        dir,
        downloads: _,
    } = a;
    shut_down(node).await;
    let a = spawn_node_in_configured(
        dir,
        CountingExecutor::new(Duration::ZERO),
        NodeOptions::default(),
        |_| {},
    )
    .await;
    assert!(a.node.dht().is_none());
    let asked = AskRequest {
        peer: net.p1.id().to_string(),
        question: "otra vez".to_owned(),
        attachments: Vec::new(),
        wait: WAIT,
        offer: None,
    };
    let outcome = a.node.outbox().ask(asked).await.unwrap();
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED", "{outcome:#?}");
    assert_eq!(outcome.answer.as_deref(), Some("echo: otra vez"));
    assert_eq!(net.p1_exec.runs(), 2);
}

#[tokio::test]
async fn querying_the_dht_runs_no_work() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    // The broadest searches: with and without a project, preferences and
    // benchmarks wanted, again and again.
    let wanted = Wanted {
        profile: "service.task".to_owned(),
        profile_version: "1".to_owned(),
        config: json!({"task": "ocr"}),
        model_revision: None,
        route: None,
        peer_pair: None,
    };
    let broad = SearchRequest {
        summary: Some("Read the text of a scanned invoice".to_owned()),
        project: Some("demo".to_owned()),
        requirements: Requirements {
            inventory: Some(InventoryRequirements {
                required: RequirementSet::default(),
                preferred: RequirementSet {
                    model_ids: vec!["example:reader".to_owned()],
                    ..RequirementSet::default()
                },
            }),
            ..Requirements::default()
        },
        benchmark: Some(wanted),
        ..request()
    };
    for round in 0..20 {
        let req = if round % 2 == 0 {
            request()
        } else {
            broad.clone()
        };
        let outcome = a.node.searcher().search(req).await.unwrap();
        assert!(!outcome.ranked.is_empty(), "{outcome:#?}");
        assert!(outcome.metrics.dht_requests > 0, "{:?}", outcome.metrics);
    }
    for exec in [&net.p1_exec, &net.p2_exec, &net.r1_exec, &net.r2_exec] {
        assert_eq!(exec.runs(), 0);
    }
    for node in [&net.p1, &net.p2, &net.r1, &net.r2] {
        assert_eq!(accepted(node), 0);
    }
}

#[tokio::test]
async fn discover_cli_and_mcp_take_exclude_dht() {
    let net = network(NodeOptions::default()).await;
    let a = plain_requester(&net).await;
    let p1 = net.p1.id().to_string();
    // The command reaches A's own gateway.
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        a.dir.path().join(CONFIG_FILE),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    let home = a.dir.path();
    let (code, stdout, stderr) = pals(home, &["discover", "--capability", CAPABILITY]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let line = stdout
        .lines()
        .find(|line| line.contains(&format!(". {p1} (agent {p1})")))
        .unwrap_or_else(|| panic!("P1 ranked: {stdout}"));
    assert!(
        line.contains("no grant held, found in the DHT, card"),
        "{line}"
    );
    let (code, stdout, stderr) =
        pals(home, &["discover", "--capability", CAPABILITY, "--no-dht"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(!stdout.contains(&p1), "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[..3], ["ranked:", "discarded:", "coverage: complete"]);

    let client = mcp_client(&a).await;
    let listed = client.list_all_tools().await.expect("the tool list");
    let mut names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "pals_ask",
            "pals_cancel",
            "pals_contacts",
            "pals_discover",
            "pals_fetch",
            "pals_inventory",
            "pals_task",
            "pals_whoami"
        ]
    );
    let discover = listed
        .iter()
        .find(|tool| tool.name == "pals_discover")
        .unwrap();
    let schema = Value::Object((*discover.input_schema).clone());
    let properties: BTreeSet<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(properties.contains("exclude_dht"), "{schema}");
    assert!(
        !schema["required"].to_string().contains("exclude_dht"),
        "{schema}"
    );
    let description = discover.description.as_deref().unwrap_or_default();
    assert!(description.contains("DHT routers"), "{description}");

    let open = ok_json(&call(&client, "pals_discover", json!({"capability": CAPABILITY})).await);
    let found = open["ranked"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["agent_id"] == p1.as_str())
        .unwrap_or_else(|| panic!("P1 ranked: {open}"));
    assert_eq!(found["from_dht"], true, "{found}");
    assert!(
        open["metrics"]["dht_requests"].as_u64().unwrap() > 0,
        "{open}"
    );
    let excluded = ok_json(
        &call(
            &client,
            "pals_discover",
            json!({"capability": CAPABILITY, "exclude_dht": true}),
        )
        .await,
    );
    assert_eq!(excluded["ranked"], json!([]), "{excluded}");
    assert_eq!(excluded["metrics"]["dht_requests"], 0, "{excluded}");
    client.cancel().await.expect("close the session");
}
