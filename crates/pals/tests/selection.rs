//! Choosing a provider among several before delegating (design §8, §14):
//! cards validated first, then offers asked for within the limits of §7.3,
//! the candidates that cannot serve discarded with their reason and the
//! rest ordered; the task sent to the one chosen with the standard A2A
//! bindings, naming its offer, which the provider checks again; and none of
//! it running any work until then.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{
    BUSY, CountingExecutor, IDEMPOTENCY_EXTENSION, StubAnswer, StubAsked, StubMode, StubPeer,
    TestNode, echo_app_with_card, echo_card_at, grant, issue_grant, key_in, now_unix, serve_at,
    shut_down, spawn_node, spawn_node_in_configured, spawn_node_with, stub_card,
    stub_discovery_peer_with_card, stub_response,
};
use futures::future::BoxFuture;
use pals::config::{Config, INVENTORY_FILE, WorkerCfg};
use pals::discovery::{DiscoveryHook, DiscoveryLimits, SearchError, Searcher};
use pals::inventory::InventoryState;
use pals::node::NodeOptions;
use pals::outbox::{AskError, AskRequest, OfferLookupError, received_offer_ref};
use pals_core::benchmark::{
    BENCHMARK_SCHEMA, BenchmarkReport, Environment, MeasurementKind, MetricResult, Procedure,
    Provenance, Subject, Wanted,
};
use pals_core::card_jws::{CanonicalForm, CardHash, SignatureStatus, canonical_payload};
use pals_core::clock::ManualClock;
use pals_core::discovery::matching::Evaluation;
use pals_core::discovery::query::{
    Conditions, InventoryRequirements, OfferParts, OfferRef, OfferStatus, RequirementSet,
    Requirements, ServiceOffer, ServiceQuery, sign_offer,
};
use pals_core::discovery::selection::{Coverage, SearchOutcome, SearchRequest, Stage};
use pals_core::discovery::wire::{self, ErrorCode, request, response};
use pals_core::discovery::{DISCOVERY_VERSION, Load, server_limits};
use pals_core::grant::Operation::{BlobFetch, TaskCancel, TaskRead, TaskSend};
use pals_core::grant::{GRANT_SCHEMA, Grant, SignedGrant, format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::{
    AdapterMethod, INVENTORY_EXTENSION, Observation, ObservationSource, OperationalStatus,
    Visibility, missing_tools_detail,
};
use pals_core::jcs;
use pals_core::signed::Signed;
use pals_core::store::A2aContact;
use serde_json::{Value, json};

/// The capability every candidate is asked for.
const CAPABILITY: &str = "code.rust.review";
/// The MCP connection the review needs.
const MCP: &str = "example:repo-reader";
/// How long an ask waits for its answer: far more than any needs.
const WAIT: Duration = Duration::from_secs(120);
/// How long a call made directly may take, on a loaded host.
const CALL: Duration = Duration::from_secs(30);
/// Every task operation.
const TASK_OPS: [pals_core::grant::Operation; 3] = [TaskSend, TaskRead, TaskCancel];

/// The review service of the project `demo`, its MCP connection and the
/// host it runs on, which offers `ram_mib` to a task; it declares the
/// external cloud it uses, `cloud`, and its times, `times`.
fn inventory(cloud: bool, ram_mib: u64, times: (u64, u64)) -> String {
    format!(
        r#"
[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["{CAPABILITY}"]
input_modes = ["text/plain"]
output_modes = ["text/plain"]
external_cloud_used = {cloud}
estimated_start_delay_ms = {}
estimated_duration_ms = {}
visibility = {{ project = ["demo"] }}

[[mcp]]
id = "{MCP}"
description = "Reads the project repository"
provenance = "operator"
visibility = {{ project = ["demo"] }}
server = "@modelcontextprotocol/server-filesystem"
tools = [{{ name = "read_file" }}]
offered_tools = ["read_file"]

[[models]]
id = "example:coder"
description = "The coding model"
provenance = "operator"
visibility = {{ project = ["demo"] }}
provider = "Example"
model_id = "example-coder"
revision = "2026-10-01"
role = "primary"
execution = {{ kind = "local" }}

[hardware]
id = "example:host"
description = "Build host"
provenance = "operator"
visibility = {{ project = ["demo"] }}
ram_mib = 65536
offered = {{ ram_mib = {ram_mib} }}
"#,
        times.0, times.1
    )
}

/// A compatible provider's inventory, with these times.
fn good(times: (u64, u64)) -> String {
    inventory(false, 8192, times)
}

/// The review service shown to everyone, with these times.
fn public_service(times: (u64, u64)) -> String {
    format!(
        r#"
[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["{CAPABILITY}"]
input_modes = ["text/plain"]
output_modes = ["text/plain"]
estimated_start_delay_ms = {}
estimated_duration_ms = {}
visibility = "public"
"#,
        times.0, times.1
    )
}

fn ts(unix_s: i64) -> String {
    format_timestamp(unix_s).unwrap()
}

/// An adapter's observation of [`MCP`], in force for half an hour of now.
fn observed(status: OperationalStatus, detail: Option<String>) -> BTreeMap<String, Observation> {
    let now = now_unix();
    BTreeMap::from([(
        MCP.to_owned(),
        Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status,
            observed_at: ts(now),
            expires_at: ts(now + 1800),
            detail,
            tools: Vec::new(),
        },
    )])
}

/// No external cloud, the MCP connection, and 4 GiB of RAM offered.
fn requirements() -> Requirements {
    Requirements {
        external_cloud_allowed: Some(false),
        inventory: Some(InventoryRequirements {
            required: RequirementSet {
                mcp_integration_ids: vec![MCP.to_owned()],
                min_offered_ram_mib: Some(4096),
                ..RequirementSet::default()
            },
            preferred: RequirementSet::default(),
        }),
        ..Requirements::default()
    }
}

/// A review of text for the project `demo`, with no requirements.
fn plain_request() -> SearchRequest {
    SearchRequest {
        capability: CAPABILITY.to_owned(),
        input_media_types: vec!["text/plain".to_owned()],
        output_media_types: vec!["text/plain".to_owned()],
        project: Some("demo".to_owned()),
        ..SearchRequest::default()
    }
}

/// [`plain_request`] with [`requirements`].
fn request() -> SearchRequest {
    SearchRequest {
        requirements: requirements(),
        ..plain_request()
    }
}

/// The limits of a search that waits for its candidates as long as a
/// loaded host may need: only a broken test waits them out.
fn patient() -> DiscoveryLimits {
    DiscoveryLimits {
        per_candidate_timeout: Duration::from_secs(60),
        total_budget: Duration::from_secs(300),
        ..DiscoveryLimits::default()
    }
}

/// How long the offers of a provider last when its test is not about
/// expiry: far longer than any loaded host's delays between an offer and
/// the ask that names it, on the system's clock.
const LONG_OFFER_MS: u32 = 300_000;

/// A provider's configuration with [`LONG_OFFER_MS`] offers.
fn long_offers(cfg: &mut Config) {
    cfg.discovery.offer_valid_for_ms = LONG_OFFER_MS;
}

/// The requester: an echo node with `opts`, whose searches are
/// [`patient`] unless `opts` says otherwise, and its configuration changed
/// by `edit`.
async fn requester_with(opts: NodeOptions, edit: impl FnOnce(&mut Config)) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    let opts = NodeOptions {
        search_limits: opts.search_limits.or(Some(patient())),
        ..opts
    };
    spawn_node_in_configured(dir, CountingExecutor::new(Duration::ZERO), opts, edit).await
}

async fn requester() -> TestNode {
    requester_with(NodeOptions::default(), |_| {}).await
}

/// A provider with `inventory`, whose work `executor` does, with `opts`
/// and its configuration changed by `edit`, saved in `a` as the contact
/// `name`, with a grant of it to `a` for every task operation in `demo`
/// when `with_grant`.
async fn provider_for(
    a: &TestNode,
    name: &str,
    inventory: &str,
    executor: impl a2a_server::AgentExecutor,
    opts: NodeOptions,
    edit: impl FnOnce(&mut Config),
    with_grant: bool,
) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    let node = spawn_node_in_configured(dir, executor, opts, edit).await;
    a.add_contact(name, &node);
    if with_grant {
        a.hold(&grant(&node, a, &TASK_OPS, 3600));
    }
    node
}

/// [`provider_for`] with the default options and configuration, a grant,
/// and the MCP connection observed `status`.
async fn provider(
    a: &TestNode,
    name: &str,
    inventory: &str,
    executor: impl a2a_server::AgentExecutor,
    status: OperationalStatus,
) -> TestNode {
    let node = provider_for(
        a,
        name,
        inventory,
        executor,
        NodeOptions::default(),
        long_offers,
        true,
    )
    .await;
    let detail =
        (status == OperationalStatus::Failed).then(|| missing_tools_detail(&["read_file"]));
    node.node
        .inventory()
        .replace_observations(observed(status, detail));
    node
}

/// A worker that reports itself full, whatever it runs.
fn full() -> NodeOptions {
    NodeOptions {
        load: Some(Arc::new(Load {
            running: 1,
            queued: 0,
            max_concurrent: 1,
            max_queue: 0,
        })),
        ..NodeOptions::default()
    }
}

fn ranked(outcome: &SearchOutcome) -> Vec<&str> {
    outcome.ranked.iter().map(|c| c.peer.as_str()).collect()
}

/// The reason `peer` was discarded, and where.
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

#[tokio::test]
async fn chooses_among_three_candidates() {
    let a = requester().await;
    let bob = CountingExecutor::busy_while_held();
    let (carol, dave, erin, gina) = (
        CountingExecutor::new(Duration::ZERO),
        CountingExecutor::new(Duration::ZERO),
        CountingExecutor::new(Duration::ZERO),
        CountingExecutor::new(Duration::ZERO),
    );
    let ok = OperationalStatus::Ok;
    let _b = provider(&a, "bob", &good((100, 1000)), bob.clone(), ok).await;
    // Faster than b, but busy.
    let c = provider_for(
        &a,
        "carol",
        &good((10, 100)),
        carol.clone(),
        full(),
        long_offers,
        true,
    )
    .await;
    c.node.inventory().replace_observations(observed(ok, None));
    let _d = provider(&a, "dave", &inventory(true, 8192, (1, 1)), dave.clone(), ok).await;
    let failed = OperationalStatus::Failed;
    let _e = provider(&a, "erin", &good((1, 1)), erin.clone(), failed).await;
    let _g = provider(
        &a,
        "gina",
        &inventory(false, 1024, (1, 1)),
        gina.clone(),
        ok,
    )
    .await;

    let outcome = a.node.searcher().search(request()).await.unwrap();
    // Every candidate was asked, and none ran anything.
    assert_eq!(outcome.metrics.contacted, 5, "{outcome:#?}");
    assert!(outcome.metrics.messages >= 3 * 5, "{:?}", outcome.metrics);
    assert!(outcome.metrics.bytes_sent > 0 && outcome.metrics.bytes_received > 0);
    assert!(outcome.metrics.time_to_first_valid_ms.is_some());
    assert_eq!(outcome.coverage, Coverage::Complete);
    // b first; c, compatible but busy, after it.
    assert_eq!(ranked(&outcome), ["bob", "carol"], "{outcome:#?}");
    let first = &outcome.ranked[0];
    assert_eq!(first.skill_id.as_deref(), Some("repo-review"));
    assert!(first.grant_held);
    assert_eq!(
        first.card_signature,
        SignatureStatus::Verified {
            kid: first.agent_id.unwrap().to_string()
        }
    );
    let offer = first.offer.as_ref().unwrap();
    assert_eq!(offer.status, OfferStatus::Available);
    assert_eq!(offer.scope, "task:demo");
    assert_eq!(
        outcome.ranked[1].offer.as_ref().unwrap().status,
        OfferStatus::Busy
    );
    // d, e and g, with why.
    assert_eq!(
        discard(&outcome, "dave"),
        (Stage::Offer, "external_cloud_allowed not met")
    );
    assert_eq!(
        discard(&outcome, "erin"),
        (
            Stage::Offer,
            "mcp_integration:example:repo-reader not operational"
        )
    );
    assert_eq!(
        discard(&outcome, "gina"),
        (Stage::Offer, "min_offered_ram_mib not met")
    );
    assert_eq!(outcome.discarded.len(), 3);
    for exec in [&bob, &carol, &dave, &erin, &gina] {
        assert_eq!(exec.runs(), 0);
    }

    // Asked, b takes it, and no one else runs anything.
    let chosen = a
        .node
        .searcher()
        .choose_and_ask(request(), "review src/lib.rs".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.peer, "bob");
    assert_eq!(chosen.outcome.state, "TASK_STATE_COMPLETED", "{chosen:#?}");
    assert_eq!(
        chosen.outcome.answer.as_deref(),
        Some("echo: review src/lib.rs")
    );
    assert!(chosen.tried.is_empty(), "{:?}", chosen.tried);
    assert!(chosen.discarded.is_empty());
    assert_eq!(bob.runs(), 1);
    for exec in [&carol, &dave, &erin, &gina] {
        assert_eq!(exec.runs(), 0);
    }
}

#[tokio::test]
async fn requirements_on_integrations_and_resources_are_checked() {
    let a = requester().await;
    let ok = OperationalStatus::Ok;
    let exec = CountingExecutor::new(Duration::ZERO);
    let _b = provider(&a, "bob", &good((1, 1)), exec.clone(), ok).await;
    let failed = OperationalStatus::Failed;
    let _e = provider(&a, "erin", &good((1, 1)), exec.clone(), failed).await;
    let _g = provider(
        &a,
        "gina",
        &inventory(false, 1024, (1, 1)),
        exec.clone(),
        ok,
    )
    .await;
    // Configured, observed in force by nobody: unknown, never met.
    let _h = provider(
        &a,
        "hugo",
        &good((1, 1)),
        exec.clone(),
        OperationalStatus::Unknown,
    )
    .await;

    let outcome = a.node.searcher().search(request()).await.unwrap();
    assert_eq!(ranked(&outcome), ["bob"], "{outcome:#?}");
    let (stage, erin) = discard(&outcome, "erin");
    assert_eq!(stage, Stage::Offer);
    assert!(
        erin.contains("mcp") && erin.contains("not operational"),
        "{erin}"
    );
    let (_, gina) = discard(&outcome, "gina");
    assert!(gina.contains("min_offered_ram_mib"), "{gina}");
    let (_, hugo) = discard(&outcome, "hugo");
    assert_eq!(hugo, "mcp_integration:example:repo-reader unknown");
    // b meets them as its offer says, on an observation and a declaration.
    let results = &outcome.ranked[0]
        .offer
        .as_ref()
        .unwrap()
        .requirement_results;
    let named: Vec<&str> = results.iter().map(|r| r.requirement.as_str()).collect();
    assert_eq!(
        named,
        [
            "input_media_type:text/plain",
            "output_media_type:text/plain",
            "external_cloud_allowed",
            "mcp_integration:example:repo-reader",
            "min_offered_ram_mib"
        ]
    );
    assert_eq!(exec.runs(), 0);
}

/// What a stub answers a service query with, keeping every request in
/// `kept`: an offer `available` and compatible, with no results, for the
/// card `card_hash`, signed by the stub for whoever asks.
fn offering(kept: Arc<Mutex<Vec<wire::Request>>>, card_hash: CardHash) -> StubAnswer {
    Arc::new(move |asked: &StubAsked<'_>| {
        let Some(req) = asked.request else {
            return Vec::new();
        };
        kept.lock().unwrap().push(req.clone());
        let Some(request::Op::ServiceQuery(query)) = &req.op else {
            let refused = wire::Error::new(ErrorCode::NotFound, "not found");
            return stub_response(asked, response::Result::Error(refused));
        };
        let parsed = ServiceQuery::from_jcs(&query.query).unwrap();
        let offer = stub_offer(
            asked.key,
            asked.requester,
            &parsed,
            &card_hash,
            now_unix(),
            15_000,
        );
        let signed = sign_offer(asked.key, offer).unwrap();
        stub_response(asked, response::Result::ServiceOffer(signed.to_json()))
    })
}

/// `provider`'s offer to `requester` for `query`, available and compatible
/// with no results, naming the card `card_hash`, issued at `now` and valid
/// `valid_ms`.
fn stub_offer(
    provider: &AgentKey,
    requester: AgentId,
    query: &ServiceQuery,
    card_hash: &CardHash,
    now: i64,
    valid_ms: u32,
) -> ServiceOffer {
    let evaluation = Evaluation {
        skill_id: Some("repo-review".to_owned()),
        status: OfferStatus::Available,
        compatible: true,
        conditions: Conditions {
            external_cloud_used: None,
            processing_region: None,
            authorization_required: false,
        },
        requirement_results: Vec::new(),
        unknown_requirements: Vec::new(),
        estimated_start_delay_ms: None,
        estimated_duration_ms: None,
        limitations: Vec::new(),
    };
    let parts = OfferParts {
        offer_id: format!("offer-{}", query.query_id),
        agent_id: provider.id(),
        requester,
        query_id: query.query_id.clone(),
        query_hash: query.hash(),
        agent_card_hash: card_hash.clone(),
        scope: "public".to_owned(),
        inventory_revision: 1,
        valid_for_ms: valid_ms,
        now_unix: now,
    };
    ServiceOffer::new(parts, evaluation).unwrap()
}

/// A stub `name` serving a valid signed card of the public review service,
/// answering as `mode` says, with a grant when `with_grant`.
async fn card_stub(a: &TestNode, name: &str, mode: StubMode, with_grant: bool) -> StubPeer {
    let key = AgentKey::generate();
    let card = stub_card(&key, &public_service((1, 1)));
    stub_discovery_peer_with_card(a, name, key, mode, card, with_grant).await
}

/// What a stub refuses every request with, quickly.
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

#[tokio::test]
async fn cards_are_validated() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let _b = provider(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        OperationalStatus::Ok,
    )
    .await;
    // h serves a card altered after it was signed.
    let h_key = AgentKey::generate();
    let mut altered = stub_card(&h_key, &public_service((1, 1)));
    altered.json["description"] = json!("Reviews any repository, faster");
    let h_kept = Arc::new(Mutex::new(Vec::new()));
    let _h = stub_discovery_peer_with_card(
        &a,
        "hank",
        h_key,
        refusing(Arc::clone(&h_kept)),
        altered,
        true,
    )
    .await;
    // i serves a valid card of its own, and an offer that names it, but
    // its announce record names another card.
    let i_key = AgentKey::generate();
    let mut card = stub_card(&i_key, &public_service((1, 1)));
    let served = card.announce_hash.clone();
    card.announce_hash = format!("blake3:{}", "e".repeat(64)).parse().unwrap();
    let i_kept = Arc::new(Mutex::new(Vec::new()));
    let answer = offering(Arc::clone(&i_kept), served);
    let _i =
        stub_discovery_peer_with_card(&a, "ivy", i_key, StubMode::Answer(answer), card, true).await;

    let outcome = a.node.searcher().search(plain_request()).await.unwrap();
    assert_eq!(ranked(&outcome), ["bob"], "{outcome:#?}");
    let (stage, hank) = discard(&outcome, "hank");
    assert_eq!(stage, Stage::Card);
    assert!(hank.contains("invalid"), "{hank}");
    // Never asked.
    assert!(h_kept.lock().unwrap().is_empty());
    let (stage, ivy) = discard(&outcome, "ivy");
    assert_eq!(stage, Stage::Card);
    assert_eq!(ivy, "the card does not match its announce record");
    assert_eq!(i_kept.lock().unwrap().len(), 1);
    assert_eq!(exec.runs(), 0);
}

#[tokio::test]
async fn querying_runs_no_work() {
    let a = requester().await;
    let ok = OperationalStatus::Ok;
    let execs: Vec<CountingExecutor> = (0..5)
        .map(|_| CountingExecutor::new(Duration::ZERO))
        .collect();
    let inventories = [
        good((1, 1)),
        good((2, 2)),
        inventory(true, 8192, (1, 1)),
        good((1, 1)),
        inventory(false, 1024, (1, 1)),
    ];
    let names = ["bob", "carol", "dave", "erin", "gina"];
    let mut nodes = Vec::new();
    for ((name, inventory), exec) in names.iter().zip(&inventories).zip(&execs) {
        let status = if *name == "erin" {
            OperationalStatus::Failed
        } else {
            ok
        };
        nodes.push(provider(&a, name, inventory, exec.clone(), status).await);
    }
    // The broadest searches: with and without requirements, benchmarks
    // wanted, again and again.
    let wanted = Wanted {
        profile: "service.task".to_owned(),
        profile_version: "1".to_owned(),
        config: json!({"task": "review"}),
        model_revision: None,
        route: None,
        peer_pair: None,
    };
    for round in 0..3 {
        let mut req = if round % 2 == 0 {
            request()
        } else {
            plain_request()
        };
        req.summary = Some("Run `cargo test` and tell me what fails".to_owned());
        req.benchmark = Some(wanted.clone());
        let outcome = a.node.searcher().search(req).await.unwrap();
        assert!(!outcome.ranked.is_empty(), "{outcome:#?}");
    }
    for (node, exec) in nodes.iter().zip(&execs) {
        assert_eq!(exec.runs(), 0);
        assert_eq!(accepted(node), 0);
        assert!(common::tasks_in(node, a2a::TaskState::Completed).is_empty());
    }
}

#[tokio::test]
async fn incompatible_descriptor_starts_no_task() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    // Its card takes text, not Rust sources only.
    let b = provider(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        OperationalStatus::Ok,
    )
    .await;
    // Its offer says it uses an external cloud.
    let d = provider(
        &a,
        "dave",
        &inventory(true, 8192, (1, 1)),
        exec.clone(),
        OperationalStatus::Ok,
    )
    .await;
    let mut req = request();
    req.input_media_types = vec!["application/x-rust-crate".to_owned()];
    let err = a
        .node
        .searcher()
        .choose_and_ask(req.clone(), "review this crate".to_owned(), WAIT)
        .await
        .unwrap_err();
    let SearchError::NoProvider { tried, .. } = &err else {
        panic!("no provider, not {err}");
    };
    assert!(tried.is_empty(), "{tried:?}");
    let outcome = a.node.searcher().search(req).await.unwrap();
    assert!(outcome.ranked.is_empty());
    assert_eq!(
        discard(&outcome, "bob"),
        (
            Stage::Card,
            "input media type application/x-rust-crate not accepted"
        )
    );
    assert_eq!(discard(&outcome, "dave").0, Stage::Card);
    // The compatible descriptor, an incompatible offer: no task either.
    let err = a
        .node
        .searcher()
        .choose_and_ask(
            SearchRequest {
                peers: Some(vec!["dave".to_owned()]),
                ..request()
            },
            "review".to_owned(),
            WAIT,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SearchError::NoProvider { .. }), "{err}");
    assert_eq!(exec.runs(), 0);
    for node in [&b, &d] {
        assert_eq!(accepted(node), 0);
    }
    assert!(a.node.store.outbox_all().unwrap().is_empty());
}

/// Counts the service queries a provider answers and, at the first,
/// moves the requester's clock `by` seconds on.
struct AdvanceAtFirstQuery {
    queries: Arc<AtomicUsize>,
    clock: Arc<ManualClock>,
    by: i64,
}

impl DiscoveryHook for AdvanceAtFirstQuery {
    fn before_answer(&self, _peer: &AgentId, req: &wire::Request) -> BoxFuture<'static, ()> {
        if matches!(req.op, Some(request::Op::ServiceQuery(_)))
            && self.queries.fetch_add(1, Ordering::SeqCst) == 0
        {
            self.clock.advance(self.by);
        }
        Box::pin(async {})
    }
}

#[tokio::test]
async fn expired_offer_is_renewed_before_asking() {
    let now = now_unix();
    let a_clock = Arc::new(ManualClock::new(now));
    let a = requester_with(
        NodeOptions {
            clock: Arc::clone(&a_clock) as _,
            ..NodeOptions::default()
        },
        |_| {},
    )
    .await;
    let queries = Arc::new(AtomicUsize::new(0));
    let hook = AdvanceAtFirstQuery {
        queries: Arc::clone(&queries),
        clock: Arc::clone(&a_clock),
        by: 2,
    };
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        NodeOptions {
            clock: Arc::new(ManualClock::new(now)),
            discovery_hook: Some(Arc::new(hook)),
            ..NodeOptions::default()
        },
        |cfg| cfg.discovery.offer_valid_for_ms = 1000,
        true,
    )
    .await;

    // The offer b makes lasts a second; by a's clock two have passed when
    // it is to be used: it is renewed, once, and then asked with.
    let chosen = a
        .node
        .searcher()
        .choose_and_ask(plain_request(), "hello".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.peer, "bob");
    assert_eq!(chosen.outcome.state, "TASK_STATE_COMPLETED", "{chosen:#?}");
    assert_eq!(queries.load(Ordering::SeqCst), 2);
    assert_eq!(exec.runs(), 1);
    // The message named the renewed offer, and b ran it once.
    let rows = a.node.store.outbox_all().unwrap();
    assert_eq!(rows.len(), 1);
    let sent: Value = serde_json::from_str(&rows[0].request_json).unwrap();
    let named = &sent["message"]["metadata"][INVENTORY_EXTENSION];
    let offer = chosen.candidate.offer.as_ref().unwrap();
    assert_eq!(named["offerId"], json!(offer.offer_id));
    assert!(
        b.node
            .store
            .offer_issued(&offer.offer_id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn busy_provider_at_acceptance_is_skipped_for_the_next() {
    let a = requester().await;
    let bob = CountingExecutor::busy_while_held();
    let carol = CountingExecutor::new(Duration::ZERO);
    let ok = OperationalStatus::Ok;
    // b's offer says available, and it is first; by the time the task
    // arrives, it has no place.
    let _b = provider(&a, "bob", &good((10, 100)), bob.clone(), ok).await;
    let _c = provider(&a, "carol", &good((10, 900)), carol.clone(), ok).await;
    bob.hold();
    let chosen = a
        .node
        .searcher()
        .choose_and_ask(request(), "review".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.peer, "carol");
    assert_eq!(chosen.outcome.state, "TASK_STATE_COMPLETED", "{chosen:#?}");
    assert_eq!(chosen.tried, [("bob".to_owned(), BUSY.to_owned())]);
    assert_eq!(bob.runs(), 0);
    assert_eq!(carol.runs(), 1);
}

/// `params` of a `SendMessage` with `message_id` that names `offer`.
fn naming(message_id: &str, text: &str, offer: &OfferRef) -> Value {
    json!({"message": {
        "messageId": message_id,
        "role": "ROLE_USER",
        "parts": [{"text": text}],
        "metadata": { INVENTORY_EXTENSION: offer.to_value() },
    }})
}

/// The `A2A-Extensions` of a task that names an offer.
fn both_extensions() -> String {
    format!("{IDEMPOTENCY_EXTENSION}, {INVENTORY_EXTENSION}")
}

/// `SendMessage` from `a` to `b` with `params`, presenting `grant`,
/// activating both extensions.
async fn send_naming(
    a: &TestNode,
    b: &TestNode,
    grant: Option<&SignedGrant>,
    params: Value,
) -> common::TestResponse {
    let extensions = both_extensions();
    a.rpc_with(
        b,
        grant,
        "SendMessage",
        params,
        &[("a2a-extensions", extensions.as_str())],
    )
    .await
}

/// The JSON-RPC error `res` carries: its code and message.
fn rpc_error(res: &common::TestResponse) -> (i64, String) {
    assert_eq!(res.status, 200, "{}", res.json);
    let error = &res.json["error"];
    assert!(error.is_object(), "an error, not {}", res.json);
    (
        error["code"].as_i64().unwrap(),
        error["message"].as_str().unwrap().to_owned(),
    )
}

/// `INVALID_PARAMS` of JSON-RPC.
const INVALID_PARAMS: i64 = -32602;

/// A query of `from`'s for the review service, by itself.
fn review_query(id: &str) -> ServiceQuery {
    plain_request().query(id, 3000, true)
}

/// `from`'s offer from `to` for [`review_query`], presenting `grant`.
async fn offer_from(
    from: &TestNode,
    to: &TestNode,
    id: &str,
    grant: Option<&SignedGrant>,
) -> ServiceOffer {
    from.node
        .discovery()
        .query(&to.id(), to.node.addr(), &review_query(id), grant, CALL)
        .await
        .unwrap()
        .0
        .doc
}

#[tokio::test]
async fn provider_refuses_an_expired_stale_or_foreign_offer_without_running() {
    let b_clock = Arc::new(ManualClock::new(now_unix()));
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        NodeOptions {
            clock: Arc::clone(&b_clock) as _,
            ..NodeOptions::default()
        },
        |_| {},
        false,
    )
    .await;
    let send = grant(&b, &a, &TASK_OPS, 3600);
    a.hold(&send);
    let c = spawn_node(WorkerCfg::Echo).await;
    let to_c = grant(&b, &c, &TASK_OPS, 3600);

    // An offer b never issued.
    let mut unknown = OfferRef::of(&offer_from(&a, &b, "q-1", Some(&send)).await);
    unknown.offer_id = "offer-never-issued".to_owned();
    let res = send_naming(&a, &b, Some(&send), naming("m-1", "x", &unknown)).await;
    assert_eq!(
        rpc_error(&res),
        (INVALID_PARAMS, "offer_unknown".to_owned())
    );
    // c's offer, named by a.
    let foreign = offer_from(&c, &b, "q-2", Some(&to_c)).await;
    let res = send_naming(
        &a,
        &b,
        Some(&send),
        naming("m-2", "x", &OfferRef::of(&foreign)),
    )
    .await;
    assert_eq!(
        rpc_error(&res),
        (INVALID_PARAMS, "offer_unknown".to_owned())
    );
    // a's, naming another card than the one it was made for.
    let mut other_card = OfferRef::of(&offer_from(&a, &b, "q-3", Some(&send)).await);
    other_card.agent_card_hash = format!("blake3:{}", "f".repeat(64)).parse().unwrap();
    let res = send_naming(&a, &b, Some(&send), naming("m-3", "x", &other_card)).await;
    assert_eq!(
        rpc_error(&res),
        (INVALID_PARAMS, "offer_unknown".to_owned())
    );
    // An entry that is no reference.
    let mut params = naming("m-4", "x", &other_card);
    params["message"]["metadata"][INVENTORY_EXTENSION] = json!({"offerId": 7});
    let res = send_naming(&a, &b, Some(&send), params).await;
    assert_eq!(
        rpc_error(&res),
        (INVALID_PARAMS, "offer_unknown".to_owned())
    );

    // Expired, by b's own clock.
    let offer = offer_from(&a, &b, "q-5", Some(&send)).await;
    let expires_at = parse_timestamp(&offer.expires_at).unwrap();
    b_clock.set(expires_at);
    let res = send_naming(
        &a,
        &b,
        Some(&send),
        naming("m-5", "x", &OfferRef::of(&offer)),
    )
    .await;
    assert_eq!(
        rpc_error(&res),
        (INVALID_PARAMS, "offer_expired".to_owned())
    );
    b_clock.set(expires_at - 15);

    // Stale: what the scope shows changed since, an observation's times.
    let offer = offer_from(&a, &b, "q-6", Some(&send)).await;
    b.node
        .inventory()
        .replace_observations(observed(OperationalStatus::Ok, None));
    let res = send_naming(
        &a,
        &b,
        Some(&send),
        naming("m-6", "x", &OfferRef::of(&offer)),
    )
    .await;
    assert_eq!(rpc_error(&res), (INVALID_PARAMS, "offer_stale".to_owned()));
    assert_eq!(exec.runs(), 0);
    assert_eq!(accepted(&b), 0);

    // Refused before running, the messageId is free again: the same one
    // with an offer in force runs.
    let fresh = offer_from(&a, &b, "q-7", Some(&send)).await;
    let res = send_naming(
        &a,
        &b,
        Some(&send),
        naming("m-6", "y", &OfferRef::of(&fresh)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let task = &res.json["result"]["task"];
    assert!(task["id"].is_string(), "{}", res.json);
    assert_eq!(exec.runs(), 1);
    // The response names both extensions it activated.
    let echoed = res.header("a2a-extensions");
    assert!(echoed.contains(IDEMPOTENCY_EXTENSION), "{echoed}");
    assert!(echoed.contains(INVENTORY_EXTENSION), "{echoed}");
}

#[tokio::test]
async fn retry_of_a_bound_request_after_its_offer_expired_attaches() {
    let b_clock = Arc::new(ManualClock::new(now_unix()));
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        NodeOptions {
            clock: Arc::clone(&b_clock) as _,
            ..NodeOptions::default()
        },
        |_| {},
        false,
    )
    .await;
    let send = grant(&b, &a, &TASK_OPS, 3600);
    a.hold(&send);
    let offer = offer_from(&a, &b, "q-1", Some(&send)).await;
    let asked = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "bob".to_owned(),
            question: "once".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: Some(OfferRef::of(&offer)),
        })
        .await
        .unwrap();
    assert_eq!(asked.state, "TASK_STATE_COMPLETED");
    let task_id = asked.task_id.clone().unwrap();
    assert_eq!(exec.runs(), 1);

    // The offer expires; the very request, sent again, finds its task.
    b_clock.advance(3600);
    let row = a.node.store.outbox_get(&asked.message_id).unwrap().unwrap();
    let params: Value = serde_json::from_str(&row.request_json).unwrap();
    assert_eq!(
        params["message"]["metadata"][INVENTORY_EXTENSION],
        OfferRef::of(&offer).to_value()
    );
    let res = send_naming(&a, &b, Some(&send), params).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["result"]["task"]["id"],
        json!(task_id),
        "{}",
        res.json
    );
    assert_eq!(exec.runs(), 1);
}

#[tokio::test]
async fn model_change_requires_a_new_offer() {
    let a = requester().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), good((1, 1))).unwrap();
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = spawn_node_in_configured(dir, exec.clone(), NodeOptions::default(), long_offers).await;
    a.add_contact("bob", &b);
    let send = grant(&b, &a, &TASK_OPS, 3600);
    a.hold(&send);
    let offer = offer_from(&a, &b, "q-1", Some(&send)).await;

    // b restarts with another revision of its model.
    let TestNode { node, dir, .. } = b;
    shut_down(node).await;
    let edited = good((1, 1)).replace("2026-10-01", "2026-11-01");
    std::fs::write(dir.path().join(INVENTORY_FILE), edited).unwrap();
    let b = spawn_node_in_configured(dir, exec.clone(), NodeOptions::default(), long_offers).await;
    a.add_contact("bob", &b);
    // It keeps the offer, whose scope's view moved on.
    let kept = b.node.store.offer_issued(&offer.offer_id).unwrap().unwrap();
    assert_eq!(kept.doc, offer);
    let res = send_naming(
        &a,
        &b,
        Some(&send),
        naming("m-1", "x", &OfferRef::of(&offer)),
    )
    .await;
    assert_eq!(rpc_error(&res), (INVALID_PARAMS, "offer_stale".to_owned()));
    // Asked with it, the outbox is refused just so, and nothing runs.
    let refused = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "bob".to_owned(),
            question: "x".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: Some(OfferRef::of(&offer)),
        })
        .await
        .unwrap_err();
    let AskError::Rejected(reason) = &refused else {
        panic!("refused, not {refused}");
    };
    assert!(reason.contains("offer_stale"), "{reason}");
    assert_eq!(exec.runs(), 0);
    // A new offer is accepted.
    let renewed = offer_from(&a, &b, "q-2", Some(&send)).await;
    assert_ne!(renewed.inventory_revision, offer.inventory_revision);
    let res = send_naming(
        &a,
        &b,
        Some(&send),
        naming("m-2", "x", &OfferRef::of(&renewed)),
    )
    .await;
    assert!(res.json["result"]["task"]["id"].is_string(), "{}", res.json);
    assert_eq!(exec.runs(), 1);
}

#[tokio::test]
async fn an_offer_never_replaces_the_grant() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &public_service((1, 1)),
        exec.clone(),
        NodeOptions::default(),
        long_offers,
        false,
    )
    .await;
    // An offer in force, asked for without a grant.
    let offer = offer_from(&a, &b, "q-1", None).await;
    assert_eq!(offer.status, OfferStatus::Available);
    let reference = OfferRef::of(&offer);
    let res = send_naming(&a, &b, None, naming("m-1", "x", &reference)).await;
    assert_eq!(res.status, 401, "{}", res.json);
    // A grant that allows no task is no better.
    let blobs = grant(&b, &a, &[BlobFetch], 3600);
    let res = send_naming(&a, &b, Some(&blobs), naming("m-2", "x", &reference)).await;
    assert_eq!(res.status, 403, "{}", res.json);
    assert_eq!(exec.runs(), 0);
    assert_eq!(accepted(&b), 0);
}

#[tokio::test]
async fn candidate_without_a_grant_is_not_asked() {
    let a = requester().await;
    let bob = CountingExecutor::new(Duration::ZERO);
    let carol = CountingExecutor::new(Duration::ZERO);
    // b is the better one, and a holds no grant of b's.
    let _b = provider_for(
        &a,
        "bob",
        &public_service((1, 1)),
        bob.clone(),
        NodeOptions::default(),
        long_offers,
        false,
    )
    .await;
    let _c = provider_for(
        &a,
        "carol",
        &public_service((100, 1000)),
        carol.clone(),
        NodeOptions::default(),
        long_offers,
        true,
    )
    .await;
    // The search ranks it, saying a holds no grant, so that one can be
    // asked for.
    let outcome = a.node.searcher().search(plain_request()).await.unwrap();
    assert_eq!(ranked(&outcome), ["bob", "carol"], "{outcome:#?}");
    assert!(!outcome.ranked[0].grant_held);
    assert!(outcome.ranked[1].grant_held);
    assert_eq!(outcome.ranked[0].offer.as_ref().unwrap().scope, "public");

    let chosen = a
        .node
        .searcher()
        .choose_and_ask(plain_request(), "hello".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.peer, "carol");
    assert_eq!(chosen.outcome.state, "TASK_STATE_COMPLETED");
    assert_eq!(chosen.discarded.len(), 1);
    let not_asked = &chosen.discarded[0];
    assert_eq!(
        (
            not_asked.peer.as_str(),
            not_asked.stage,
            not_asked.reason.as_str()
        ),
        ("bob", Stage::Offer, "no grant held")
    );
    assert!(chosen.tried.is_empty());
    assert_eq!(bob.runs(), 0);
    assert_eq!(carol.runs(), 1);
}

/// `measurer`'s report `id` of `kind` about the review service under
/// `service.task` 1 with the configuration `config`, measured an hour ago
/// and in force for a day.
fn review_report(
    measurer: &AgentKey,
    id: &str,
    kind: MeasurementKind,
    config: Value,
) -> Signed<BenchmarkReport> {
    let measured_at = now_unix() - 3600;
    let doc = BenchmarkReport {
        schema: BENCHMARK_SCHEMA.to_owned(),
        benchmark_id: id.to_owned(),
        subject: Subject::Service("repo-review".to_owned()),
        measurement_kind: kind,
        procedure: Procedure {
            profile: "service.task".to_owned(),
            profile_version: "1".to_owned(),
            runner: None,
            config,
            inputs_hash: None,
        },
        environment: Environment::default(),
        measured_at: ts(measured_at),
        duration_ms: 60_000,
        samples: 20,
        expires_at: ts(measured_at + 86_400),
        results: vec![MetricResult {
            metric: "total_time".to_owned(),
            unit: "ms".to_owned(),
            statistic: "p50".to_owned(),
            value: 812.0,
            errors: None,
            uncertainty: None,
        }],
        provenance: Provenance {
            measured_by: measurer.id(),
            method: "pals-bench".to_owned(),
        },
        evidence: None,
    };
    Signed::issue(measurer, doc).unwrap()
}

#[tokio::test]
async fn benchmark_ranking_uses_only_comparable_reports() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let ok = OperationalStatus::Ok;
    let b = provider(&a, "bob", &good((100, 1000)), exec.clone(), ok).await;
    let c = provider(&a, "carol", &good((10, 100)), exec.clone(), ok).await;
    let review = json!({"task": "review", "files": 20});
    let usable = review_report(
        &key_in(b.dir.path()),
        "bench-b",
        MeasurementKind::OwnMeasurement,
        review.clone(),
    );
    let other = review_report(
        &key_in(c.dir.path()),
        "bench-c",
        MeasurementKind::OwnMeasurement,
        json!({"task": "review", "files": 2000}),
    );
    for (node, report) in [(&b, &usable), (&c, &other)] {
        node.node
            .store
            .benchmark_put(report, &Visibility::Public, now_unix())
            .unwrap();
    }

    // Without benchmarks wanted, c is faster.
    let plain = a.node.searcher().search(request()).await.unwrap();
    assert_eq!(ranked(&plain), ["carol", "bob"], "{plain:#?}");
    assert!(plain.ranked.iter().all(|c| c.benchmark.is_none()));
    // Wanted, b's report stands for it, and c's measured something else.
    let wanted = SearchRequest {
        benchmark: Some(Wanted {
            profile: "service.task".to_owned(),
            profile_version: "1".to_owned(),
            config: json!({"files": 20, "task": "review"}),
            model_revision: None,
            route: None,
            peer_pair: None,
        }),
        ..request()
    };
    let outcome = a.node.searcher().search(wanted).await.unwrap();
    assert_eq!(ranked(&outcome), ["bob", "carol"], "{outcome:#?}");
    assert_eq!(
        outcome.ranked[0].benchmark,
        Some(("own_measurement".to_owned(), 812.0))
    );
    assert_eq!(outcome.ranked[1].benchmark, None);
    // One GetBenchmarkReport each, counted.
    assert_eq!(outcome.metrics.messages, plain.metrics.messages + 2);
    assert!(outcome.metrics.bytes_received > plain.metrics.bytes_received);
    assert_eq!(exec.runs(), 0);
}

#[tokio::test]
async fn more_than_8_contacts_stop_at_8_with_partial_coverage() {
    let a = requester().await;
    let kept = Arc::new(Mutex::new(Vec::new()));
    let mut stubs = Vec::new();
    for n in 0..10 {
        let name = format!("s{n:02}");
        stubs.push(card_stub(&a, &name, refusing(Arc::clone(&kept)), true).await);
    }
    let outcome = a.node.searcher().search(plain_request()).await.unwrap();
    assert_eq!(outcome.metrics.contacted, 8, "{outcome:#?}");
    assert_eq!(
        outcome.coverage,
        Coverage::Partial {
            reasons: vec!["candidate limit".to_owned()]
        }
    );
    assert_eq!(kept.lock().unwrap().len(), 8);
    // By name, the first eight: the last two were never asked.
    let asked: Vec<usize> = stubs.iter().map(StubPeer::requests).collect();
    assert_eq!(asked, [1, 1, 1, 1, 1, 1, 1, 1, 0, 0]);
    assert_eq!(outcome.discarded.len(), 8);
}

/// Waits until `stubs` have read `count` requests between them.
async fn wait_for_held(stubs: &[StubPeer], count: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while stubs.iter().map(StubPeer::requests).sum::<usize>() < count {
        assert!(
            std::time::Instant::now() < deadline,
            "the stubs read {} requests, not {count}",
            stubs.iter().map(StubPeer::requests).sum::<usize>()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wakes the requests `release` holds until `done` ends.
async fn releasing<T>(release: &tokio::sync::Notify, done: impl Future<Output = T>) -> T {
    tokio::pin!(done);
    loop {
        release.notify_waiters();
        tokio::select! {
            out = &mut done => return out,
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

#[tokio::test]
async fn never_more_than_3_queries_in_flight_across_searches() {
    let a = requester().await;
    let release = Arc::new(tokio::sync::Notify::new());
    let mut stubs = Vec::new();
    for n in 0..5 {
        let mode = StubMode::Hold(Arc::clone(&release));
        stubs.push(card_stub(&a, &format!("h{n}"), mode, true).await);
    }
    let searcher = a.node.searcher().clone();
    let first = tokio::spawn({
        let searcher = searcher.clone();
        async move { searcher.search(plain_request()).await }
    });
    let second = tokio::spawn({
        let searcher = searcher.clone();
        async move { searcher.search(plain_request()).await }
    });
    // Ten queries to make between the two searches: three at a time, for
    // the whole daemon.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while searcher.searches_running() < 2 {
        assert!(std::time::Instant::now() < deadline, "two searches running");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    wait_for_held(&stubs, 3).await;
    assert_eq!(stubs.iter().map(StubPeer::requests).sum::<usize>(), 3);
    assert_eq!(searcher.searches_running(), 2);
    let (first, second) = releasing(&release, async { (first.await, second.await) }).await;
    for outcome in [first.unwrap().unwrap(), second.unwrap().unwrap()] {
        assert_eq!(outcome.metrics.contacted, 5, "{outcome:#?}");
        assert!(outcome.ranked.is_empty());
    }
    assert_eq!(stubs.iter().map(StubPeer::requests).sum::<usize>(), 10);
}

#[tokio::test]
async fn slow_candidate_is_cut_and_the_budget_ends_the_search() {
    // A second per candidate and two and a half in all, as `[discovery]`
    // says, for eight that never answer, three at a time.
    let a = spawn_node_in_configured(
        tempfile::tempdir().unwrap(),
        CountingExecutor::new(Duration::ZERO),
        NodeOptions::default(),
        |cfg| {
            cfg.discovery.per_candidate_timeout_ms = 1000;
            cfg.discovery.total_budget_ms = 2500;
        },
    )
    .await;
    let mut stubs = Vec::new();
    for n in 0..8 {
        stubs.push(card_stub(&a, &format!("q{n}"), StubMode::Silent, true).await);
    }
    let outcome = a.node.searcher().search(plain_request()).await.unwrap();
    assert!(outcome.ranked.is_empty());
    assert_eq!(outcome.discarded.len(), 8, "{outcome:#?}");
    let reasons: Vec<&str> = outcome
        .discarded
        .iter()
        .map(|d| d.reason.as_str())
        .collect();
    assert!(reasons.contains(&"timeout"), "{reasons:?}");
    assert!(reasons.contains(&"time budget"), "{reasons:?}");
    assert!(
        reasons
            .iter()
            .all(|r| *r == "timeout" || *r == "time budget"),
        "{reasons:?}"
    );
    let Coverage::Partial { reasons } = &outcome.coverage else {
        panic!("partial coverage, not {:?}", outcome.coverage);
    };
    assert_eq!(reasons, &["time budget".to_owned()]);
}

/// A JWS protected header for `kid` with its JWK Set at `jku`.
fn header(kid: &str, jku: &str) -> Value {
    json!({"alg": "EdDSA", "jku": jku, "kid": kid, "typ": "JOSE"})
}

/// `card` signed by `key` over its JCS as is, with `protected`.
fn sign_card(card: &Value, key: &AgentKey, protected: &Value) -> Value {
    let protected = URL_SAFE_NO_PAD.encode(jcs::canonical(protected));
    let payload = canonical_payload(card, CanonicalForm::Raw).unwrap();
    let input = format!("{protected}.{}", URL_SAFE_NO_PAD.encode(&payload));
    let signature = URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()));
    let mut signed = card.clone();
    signed["signatures"] = json!([{"protected": protected, "signature": signature}]);
    signed
}

/// The stock echo agent's card at `origin`, its skill tagged with the
/// capability.
fn reviewing_card(origin: &str) -> Value {
    let mut card = serde_json::to_value(echo_card_at(origin)).unwrap();
    card["skills"][0]["tags"] = json!(["echo", format!("agentnet:capability:v1:{CAPABILITY}")]);
    card
}

/// Saves the agent at `origin` in `a` as the plain contact `name`.
fn add_plain(a: &TestNode, name: &str, origin: &str) {
    a.node
        .store
        .a2a_contact_put(&A2aContact {
            name: name.to_owned(),
            card_url: format!("{origin}/.well-known/agent-card.json"),
            origin: origin.to_owned(),
            bearer_env: None,
            added_at: now_unix(),
        })
        .unwrap();
}

#[tokio::test]
async fn plain_contact_is_a_card_only_candidate_only_when_verified_and_without_requirements() {
    let a = requester().await;
    // f signs its card with an OKP key of a JWK Set of its own origin.
    let key = AgentKey::generate();
    let jwks = json!({"keys": [{
        "kty": "OKP", "crv": "Ed25519", "kid": "k1", "alg": "EdDSA", "use": "sig",
        "x": URL_SAFE_NO_PAD.encode(key.id().as_bytes()),
    }]});
    let (f_origin, _f) = serve_at(|origin| {
        let jku = format!("{origin}/jwks.json");
        let card = sign_card(&reviewing_card(origin), &key, &header("k1", &jku));
        echo_app_with_card(card).route(
            "/jwks.json",
            axum::routing::get(move || {
                let jwks = jwks.clone();
                async move { axum::Json(jwks) }
            }),
        )
    })
    .await;
    // f2 does not sign it.
    let (f2_origin, _f2) = serve_at(|origin| echo_app_with_card(reviewing_card(origin))).await;
    add_plain(&a, "fred", &f_origin);
    add_plain(&a, "fred2", &f2_origin);

    let outcome = a.node.searcher().search(plain_request()).await.unwrap();
    assert_eq!(ranked(&outcome), ["fred"], "{outcome:#?}");
    let fred = &outcome.ranked[0];
    assert_eq!(fred.offer, None);
    assert_eq!(fred.agent_id, None);
    assert_eq!(fred.skill_id.as_deref(), Some("echo"));
    assert_eq!(
        fred.card_signature,
        SignatureStatus::Verified {
            kid: "k1".to_owned()
        }
    );
    assert_eq!(
        discard(&outcome, "fred2"),
        (Stage::Card, "card not verified")
    );
    // With requirements on the inventory, no card confirms them.
    let outcome = a.node.searcher().search(request()).await.unwrap();
    assert!(outcome.ranked.is_empty(), "{outcome:#?}");
    assert_eq!(
        discard(&outcome, "fred"),
        (
            Stage::Card,
            "a plain A2A agent cannot confirm the requirements"
        )
    );
    // Chosen, f answers.
    let chosen = a
        .node
        .searcher()
        .choose_and_ask(plain_request(), "hi".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.peer, "fred");
    assert_eq!(chosen.outcome.answer.as_deref(), Some("echo: hi"));
    // f2 is still asked by its name.
    let asked = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "fred2".to_owned(),
            question: "hi".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: None,
        })
        .await
        .unwrap();
    assert_eq!(asked.state, "TASK_STATE_COMPLETED");
    assert_eq!(asked.answer.as_deref(), Some("echo: hi"));
}

/// The query `req` carries.
fn query_of(req: &wire::Request) -> ServiceQuery {
    let Some(request::Op::ServiceQuery(query)) = &req.op else {
        panic!("a service query, not {:?}", req.op);
    };
    ServiceQuery::from_jcs(&query.query).unwrap()
}

#[tokio::test]
async fn inventory_requirement_ids_go_only_to_authorized_candidates() {
    let a = requester().await;
    // Two stubs that offer, signing for whoever asks: one a holds a grant
    // of for `demo`, one it holds none of.
    let mut kept = Vec::new();
    let mut stubs = Vec::new();
    for (name, with_grant) in [("granted", true), ("stranger", false)] {
        let key = AgentKey::generate();
        let card = stub_card(&key, &public_service((1, 1)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answer = offering(Arc::clone(&requests), card.announce_hash.clone());
        let stub = stub_discovery_peer_with_card(
            &a,
            name,
            key,
            StubMode::Answer(answer),
            card,
            with_grant,
        )
        .await;
        kept.push(requests);
        stubs.push(stub);
    }
    let req = SearchRequest {
        requirements: Requirements {
            inventory: Some(InventoryRequirements {
                required: RequirementSet {
                    mcp_integration_ids: vec![MCP.to_owned()],
                    ..RequirementSet::default()
                },
                preferred: RequirementSet {
                    information_source_ids: vec!["project:repo-controller".to_owned()],
                    ..RequirementSet::default()
                },
            }),
            ..Requirements::default()
        },
        input_media_types: Vec::new(),
        output_media_types: Vec::new(),
        ..plain_request()
    };
    let outcome = a.node.searcher().search(req).await.unwrap();

    let granted = kept[0].lock().unwrap().clone();
    assert_eq!(granted.len(), 1);
    assert!(!granted[0].grant_token.is_empty());
    let inventory = query_of(&granted[0]).requirements.inventory.unwrap();
    assert_eq!(inventory.required.mcp_integration_ids, [MCP]);
    assert_eq!(
        inventory.preferred.information_source_ids,
        ["project:repo-controller"]
    );
    assert_eq!(query_of(&granted[0]).project.as_deref(), Some("demo"));
    let stranger = kept[1].lock().unwrap().clone();
    assert_eq!(stranger.len(), 1);
    assert!(stranger[0].grant_token.is_empty());
    let query = query_of(&stranger[0]);
    assert_eq!(query.requirements.inventory, None);
    // Nor which project it is for.
    assert_eq!(query.project, None);
    let bytes = String::from_utf8(query.to_jcs()).unwrap();
    for id in [
        MCP,
        "project:repo-controller",
        "repo-reader",
        "repo-controller",
    ] {
        assert!(!bytes.contains(id), "{id} in {bytes}");
    }
    // Neither says it has the connection: the one with the grant was asked
    // and did not answer it; the other was never asked, and its unknown
    // requirement discards it all the same.
    assert!(outcome.ranked.is_empty(), "{outcome:#?}");
    assert_eq!(
        discard(&outcome, "granted"),
        (Stage::Offer, "mcp_integration:example:repo-reader unknown")
    );
    assert_eq!(
        discard(&outcome, "stranger"),
        (
            Stage::Offer,
            "mcp_integration:example:repo-reader unknown: inventory ids go only to peers \
             with a grant for the project"
        )
    );
}

#[tokio::test]
async fn repeated_broad_searches_keep_the_aggregate_limits() {
    let a = requester().await;
    let release = Arc::new(tokio::sync::Notify::new());
    let mut held = Vec::new();
    for n in 0..3 {
        let mode = StubMode::Hold(Arc::clone(&release));
        held.push(card_stub(&a, &format!("h{n}"), mode, true).await);
    }
    let kept = Arc::new(Mutex::new(Vec::new()));
    let mut quick = Vec::new();
    for n in 0..2 {
        quick.push(card_stub(&a, &format!("r{n}"), refusing(Arc::clone(&kept)), true).await);
    }
    let searcher: Searcher = a.node.searcher().clone();
    let on_held = SearchRequest {
        peers: Some(vec!["h0".to_owned(), "h1".to_owned(), "h2".to_owned()]),
        ..plain_request()
    };
    // Four searches at once, held at the stubs, three queries at a time.
    let running: Vec<_> = (0..4)
        .map(|_| {
            let (searcher, req) = (searcher.clone(), on_held.clone());
            tokio::spawn(async move { searcher.search(req).await })
        })
        .collect();
    wait_for_held(&held, 3).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while searcher.searches_running() < 4 {
        assert!(
            std::time::Instant::now() < deadline,
            "four searches running"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A fifth is refused, at once, before it asks anyone.
    let fifth = searcher.search(plain_request()).await.unwrap_err();
    assert!(matches!(fifth, SearchError::TooManySearches), "{fifth}");
    assert_eq!(fifth.to_string(), "too many searches");
    let ended = releasing(&release, futures::future::join_all(running)).await;
    for outcome in ended {
        assert_eq!(outcome.unwrap().unwrap().metrics.contacted, 3);
    }
    assert_eq!(held.iter().map(StubPeer::requests).sum::<usize>(), 12);
    assert!(kept.lock().unwrap().is_empty(), "the fifth asked no one");

    // Twenty more, one after another: each within the limits, and no more
    // than one query a candidate.
    let on_quick = SearchRequest {
        peers: Some(vec!["r0".to_owned(), "r1".to_owned()]),
        ..plain_request()
    };
    for _ in 0..20 {
        let outcome = searcher.search(on_quick.clone()).await.unwrap();
        assert_eq!(outcome.metrics.contacted, 2);
        assert_eq!(outcome.coverage, Coverage::Complete);
    }
    assert_eq!(kept.lock().unwrap().len(), 40);
    assert_eq!(searcher.searches_running(), 0);
    assert_eq!(held.iter().map(StubPeer::requests).sum::<usize>(), 12);
}

#[tokio::test]
async fn tck_style_message_without_offer_is_unchanged() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &public_service((1, 1)),
        exec.clone(),
        NodeOptions::default(),
        long_offers,
        false,
    )
    .await;
    let send = grant(&b, &a, &TASK_OPS, 3600);
    // A stock client's message: no extension, as in milestone 1.
    let res = a
        .rpc(
            &b,
            Some(&send),
            "SendMessage",
            common::message_params("hi", None),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["result"]["task"]["status"]["state"],
        json!("TASK_STATE_COMPLETED"),
        "{}",
        res.json
    );
    assert_eq!(res.header("a2a-extensions"), "");
    // An offer named without the extension activated is only metadata.
    let unknown = OfferRef {
        offer_id: "offer-never-issued".to_owned(),
        agent_card_hash: b.node.cards().public().hash.clone(),
        query_id: "q-1".to_owned(),
    };
    let res = a
        .rpc(
            &b,
            Some(&send),
            "SendMessage",
            naming("m-1", "hi", &unknown),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert!(res.json["result"]["task"]["id"].is_string(), "{}", res.json);
    // Idempotency alone is echoed alone.
    let res = a
        .rpc_idempotent(
            &b,
            Some(&send),
            "SendMessage",
            common::message_params("hi", None),
        )
        .await;
    assert_eq!(res.header("a2a-extensions"), IDEMPOTENCY_EXTENSION);
    assert_eq!(exec.runs(), 3);
}

/// Writes a configuration in `a`'s data directory, so that the `pals`
/// command finds its key and its store there.
fn cli_home(a: &TestNode) -> &Path {
    std::fs::write(a.dir.path().join("config.toml"), "[net]\nrelay = false\n").unwrap();
    a.dir.path()
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

#[tokio::test]
async fn ask_with_an_expired_offer_sends_nothing() {
    let a = spawn_node_with(WorkerCfg::Echo, |_| {}).await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &public_service((1, 1)),
        exec.clone(),
        NodeOptions::default(),
        |_| {},
        true,
    )
    .await;
    let c = provider_for(
        &a,
        "carol",
        &public_service((1, 1)),
        exec.clone(),
        NodeOptions::default(),
        |_| {},
        true,
    )
    .await;
    // Two providers, one offer id: b's expired an hour ago, c's in force.
    let query = review_query("q-shared");
    let now = now_unix();
    let shared = |provider: &TestNode, issued: i64| {
        let key = key_in(provider.dir.path());
        let hash = provider.node.cards().public().hash.clone();
        let mut offer = stub_offer(&key, a.id(), &query, &hash, issued, 15_000);
        offer.offer_id = "offer-shared".to_owned();
        let signed = sign_offer(&key, offer).unwrap();
        a.node.store.offer_received_put(&signed, now).unwrap();
        signed
    };
    let _expired = shared(&b, now - 3600);
    let in_force = shared(&c, now);

    let home = cli_home(&a);
    let (code, stdout, stderr) =
        pals(home, &["ask", "bob", "hello", "--offer", "offer-shared"]).await;
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(
        stderr.contains("offer expired: run pals discover again"),
        "{stderr}"
    );
    assert!(a.node.store.outbox_all().unwrap().is_empty());
    assert_eq!(exec.runs(), 0);
    // Looked up by provider and id: c's is another offer, in force.
    let store = &a.node.store;
    assert_eq!(
        received_offer_ref(store, "bob", "offer-shared", now),
        Err(OfferLookupError::Expired)
    );
    assert_eq!(
        received_offer_ref(store, "carol", "offer-shared", now),
        Ok(OfferRef::of(&in_force.doc))
    );
    assert_eq!(
        received_offer_ref(store, "carol", "offer-other", now),
        Err(OfferLookupError::Unknown)
    );
    let (code, _, stderr) = pals(home, &["ask", "carol", "hello", "--offer", "offer-other"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("no offer offer-other"), "{stderr}");
}

#[tokio::test]
async fn discover_cli_prints_the_ranking_and_asks_the_best() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let ok = OperationalStatus::Ok;
    let _b = provider(&a, "bob", &good((1, 1)), exec.clone(), ok).await;
    let _d = provider(&a, "dave", &inventory(true, 8192, (1, 1)), exec.clone(), ok).await;
    // The command reaches a's own gateway.
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        a.dir.path().join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    let file = a.dir.path().join("req.json");
    std::fs::write(&file, serde_json::to_vec(&requirements()).unwrap()).unwrap();
    let search = [
        "discover",
        "--capability",
        CAPABILITY,
        "--input",
        "text/plain",
        "--output",
        "text/plain",
        "--requirements-file",
        file.to_str().unwrap(),
        "--project",
        "demo",
    ];
    let (code, stdout, stderr) = pals(a.dir.path(), &search).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "ranked:", "{stdout}");
    assert!(lines[1].starts_with("  1. bob (agent "), "{stdout}");
    assert!(
        lines[1].contains(": available, skill repo-review"),
        "{stdout}"
    );
    assert!(lines[1].contains("grant held"), "{stdout}");
    assert_eq!(lines[2], "discarded:");
    assert_eq!(lines[3], "  dave (offer): external_cloud_allowed not met");
    assert_eq!(lines[4], "coverage: complete");
    assert!(
        lines[5].starts_with("metrics: contacted 2, messages 6,"),
        "{stdout}"
    );
    assert_eq!(exec.runs(), 0);

    // With a question, the best takes it.
    let mut asking = search.to_vec();
    asking.extend(["--ask", "hello", "--wait", "120s"]);
    let (code, stdout, stderr) = pals(a.dir.path(), &asking).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("asked: bob\necho: hello\n"), "{stdout}");
    assert_eq!(exec.runs(), 1);
    // A search that makes no query is refused before anyone is asked.
    let (code, _, stderr) = pals(a.dir.path(), &["discover", "--capability", "a b"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("invalid capability"), "{stderr}");
}

#[tokio::test]
async fn ask_names_the_offer_discover_printed() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let _b = provider(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        OperationalStatus::Ok,
    )
    .await;
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        a.dir.path().join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    let file = a.dir.path().join("req.json");
    std::fs::write(&file, serde_json::to_vec(&requirements()).unwrap()).unwrap();
    let search = [
        "discover",
        "--capability",
        CAPABILITY,
        "--requirements-file",
        file.to_str().unwrap(),
        "--project",
        "demo",
    ];
    let (code, stdout, stderr) = pals(a.dir.path(), &search).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    // The ranking names each candidate's offer and when it expires: what
    // `pals ask --offer` takes, and when it stops taking it.
    let line = stdout
        .lines()
        .find(|line| line.starts_with("  1. bob "))
        .unwrap_or_else(|| panic!("{stdout}"));
    let (_, offer) = line
        .rsplit_once(", offer ")
        .unwrap_or_else(|| panic!("no offer named: {line}"));
    let (offer_id, until) = offer
        .split_once(" until ")
        .unwrap_or_else(|| panic!("no expiry named: {line}"));
    assert!(offer_id.starts_with("offer-"), "{line}");
    assert!(parse_timestamp(until).is_some(), "{line}");
    assert_eq!(exec.runs(), 0);

    let (code, stdout, stderr) = pals(
        a.dir.path(),
        &["ask", "bob", "hello", "--offer", offer_id, "--wait", "120s"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stdout, "echo: hello\n");
    assert_eq!(exec.runs(), 1);
    let rows = a.node.store.outbox_all().unwrap();
    let sent: Value = serde_json::from_str(&rows[0].request_json).unwrap();
    assert_eq!(
        sent["message"]["metadata"][INVENTORY_EXTENSION]["offerId"],
        json!(offer_id)
    );
}

/// Waits up to 60 s for `condition`, looked at every 10 ms.
async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn refusal_once_the_task_exists_stops_the_choice() {
    let a = requester().await;
    let bob = CountingExecutor::gated();
    let carol = CountingExecutor::new(Duration::ZERO);
    let defaults = NodeOptions::default;
    let b = provider_for(
        &a,
        "bob",
        &public_service((1, 1)),
        bob.clone(),
        defaults(),
        long_offers,
        true,
    )
    .await;
    let _c = provider_for(
        &a,
        "carol",
        &public_service((100, 1000)),
        carol.clone(),
        defaults(),
        long_offers,
        true,
    )
    .await;
    let searcher = a.node.searcher().clone();
    let choice = tokio::spawn(async move {
        searcher
            .choose_and_ask(plain_request(), "a long review".to_owned(), WAIT)
            .await
    });
    // b runs it, and a knows its task.
    wait_until("a knows b's task", || {
        a.node
            .store
            .outbox_all()
            .unwrap()
            .iter()
            .any(|row| row.task_id.is_some())
    })
    .await;
    assert_eq!(bob.runs(), 1);
    // Then b takes a's grant no more, the stream breaks, and the follow-up
    // call is refused.
    for held in a.node.store.grants_held_for(&b.id(), now_unix()).unwrap() {
        b.node
            .store
            .revoke(&held.grant.grant_id, now_unix())
            .unwrap();
    }
    a.node.close_peer_connections().await;
    let err = choice.await.unwrap().unwrap_err();
    let SearchError::Ask {
        peer, error, tried, ..
    } = &err
    else {
        panic!("the ask stops, not {err}");
    };
    assert_eq!(peer, "bob");
    assert!(matches!(error, AskError::Rejected(_)), "{error}");
    assert!(tried.is_empty(), "{tried:?}");
    // c is never asked: b's task may still run.
    assert_eq!(carol.runs(), 0);
    assert_eq!(a.node.store.outbox_all().unwrap().len(), 1);
    bob.release();
}

/// Counts the service queries and benchmark report requests a provider
/// answers, and, at the first report requested, changes what its scope
/// shows: between the search and the ask.
struct StaleAtTheReport {
    queries: Arc<AtomicUsize>,
    reports: Arc<AtomicUsize>,
    inventory: Arc<OnceLock<InventoryState>>,
}

impl DiscoveryHook for StaleAtTheReport {
    fn before_answer(&self, _peer: &AgentId, req: &wire::Request) -> BoxFuture<'static, ()> {
        match req.op {
            Some(request::Op::ServiceQuery(_)) => {
                self.queries.fetch_add(1, Ordering::SeqCst);
            }
            Some(request::Op::GetBenchmarkReport(_))
                if self.reports.fetch_add(1, Ordering::SeqCst) == 0 =>
            {
                let inventory = self.inventory.get().expect("the provider's inventory");
                inventory.replace_observations(observed(OperationalStatus::Ok, None));
            }
            _ => {}
        }
        Box::pin(async {})
    }
}

#[tokio::test]
async fn stale_offer_at_the_ask_is_renewed_once() {
    let a = requester().await;
    let (queries, reports) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let inventory = Arc::new(OnceLock::new());
    let hook = StaleAtTheReport {
        queries: Arc::clone(&queries),
        reports: Arc::clone(&reports),
        inventory: Arc::clone(&inventory),
    };
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &good((1, 1)),
        exec.clone(),
        NodeOptions {
            discovery_hook: Some(Arc::new(hook)),
            ..NodeOptions::default()
        },
        long_offers,
        true,
    )
    .await;
    assert!(inventory.set(b.node.inventory().clone()).is_ok());
    let review = json!({"task": "review"});
    let report = review_report(
        &key_in(b.dir.path()),
        "bench-b",
        MeasurementKind::OwnMeasurement,
        review.clone(),
    );
    b.node
        .store
        .benchmark_put(&report, &Visibility::Public, now_unix())
        .unwrap();
    // The search reads b's offer and report; by then the scope's view has
    // moved, and the offer is stale when the task arrives.
    let req = SearchRequest {
        benchmark: Some(Wanted {
            profile: "service.task".to_owned(),
            profile_version: "1".to_owned(),
            config: review,
            model_revision: None,
            route: None,
            peer_pair: None,
        }),
        ..plain_request()
    };
    let chosen = a
        .node
        .searcher()
        .choose_and_ask(req, "review".to_owned(), WAIT)
        .await
        .unwrap();
    assert_eq!(chosen.candidate.peer, "bob");
    assert_eq!(chosen.outcome.state, "TASK_STATE_COMPLETED", "{chosen:#?}");
    assert!(chosen.tried.is_empty(), "{:?}", chosen.tried);
    // The search's query and the renewal: three requests to b's
    // discovery in all, and one run.
    assert_eq!(queries.load(Ordering::SeqCst), 2);
    assert_eq!(reports.load(Ordering::SeqCst), 1);
    assert_eq!(exec.runs(), 1);
    // The first question was refused as stale, the second ran.
    let rows = a.node.store.outbox_all().unwrap();
    let states: Vec<&str> = rows.iter().map(|row| row.state.as_str()).collect();
    assert_eq!(states, ["refused", "TASK_STATE_COMPLETED"]);
}

#[tokio::test]
async fn unreachable_provider_stops_the_choice() {
    let a = requester().await;
    // b offers, and then takes no task: its binding answers 503.
    let key = AgentKey::generate();
    let mut card = stub_card(&key, &public_service((1, 1)));
    card.tasks_unavailable = true;
    let kept = Arc::new(Mutex::new(Vec::new()));
    let answer = offering(Arc::clone(&kept), card.announce_hash.clone());
    let _b =
        stub_discovery_peer_with_card(&a, "bob", key, StubMode::Answer(answer), card, true).await;
    // c is busy, so after b, and would take it.
    let carol = CountingExecutor::new(Duration::ZERO);
    let _c = provider_for(
        &a,
        "carol",
        &public_service((1, 1)),
        carol.clone(),
        full(),
        long_offers,
        true,
    )
    .await;
    let req = SearchRequest {
        capability: CAPABILITY.to_owned(),
        project: Some("demo".to_owned()),
        ..SearchRequest::default()
    };
    let outcome = a.node.searcher().search(req.clone()).await.unwrap();
    assert_eq!(ranked(&outcome), ["bob", "carol"], "{outcome:#?}");
    let err = a
        .node
        .searcher()
        .choose_and_ask(req, "hello".to_owned(), WAIT)
        .await
        .unwrap_err();
    let SearchError::Ask { peer, error, .. } = &err else {
        panic!("the ask stops, not {err}");
    };
    assert_eq!(peer, "bob");
    assert!(matches!(error, AskError::Unreachable { .. }), "{error}");
    // The question waits for b, kept; c runs nothing.
    assert_eq!(carol.runs(), 0);
    let rows = a.node.store.outbox_all().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "pending");
}

/// `issuer`'s grant to `subject` of every task operation on `skill:ask` of
/// `project`, against `audience`, issued at `issued_at` for an hour.
fn project_grant(
    issuer: &AgentKey,
    subject: AgentId,
    audience: AgentId,
    project: &str,
    issued_at: i64,
) -> SignedGrant {
    let grant = Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: format!("g-{project}-{issued_at}"),
        issuer: issuer.id(),
        subject,
        audience,
        project: project.to_owned(),
        resources: vec!["skill:ask".to_owned()],
        operations: TASK_OPS.to_vec(),
        issued_at: ts(issued_at),
        expires_at: ts(issued_at + 3600),
    };
    SignedGrant::issue(issuer, grant).unwrap()
}

#[tokio::test]
async fn offer_runs_only_under_a_grant_of_its_scope() {
    let a = requester().await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_for(
        &a,
        "bob",
        &public_service((1, 1)),
        exec.clone(),
        NodeOptions::default(),
        long_offers,
        false,
    )
    .await;
    let key = key_in(b.dir.path());
    let now = now_unix();
    // Two grants of b's: demo's, and a newer one of another project, the
    // one the gateway would present by itself.
    let demo = issue_grant(&key, a.id(), b.id(), &TASK_OPS, now - 60, now + 3600);
    let other = project_grant(&key, a.id(), b.id(), "other", now);
    for held in [&demo, &other] {
        b.node.store.grant_issued_put(held).unwrap();
        a.hold(held);
    }
    let offer = offer_from(&a, &b, "q-1", Some(&demo)).await;
    assert_eq!(offer.scope, "task:demo");
    // Under a grant of another scope, the offer is stale, and nothing runs.
    let res = send_naming(
        &a,
        &b,
        Some(&other),
        naming("m-1", "x", &OfferRef::of(&offer)),
    )
    .await;
    assert_eq!(rpc_error(&res), (INVALID_PARAMS, "offer_stale".to_owned()));
    // An offer made without a grant is the public scope's.
    let public = offer_from(&a, &b, "q-2", None).await;
    assert_eq!(public.scope, "public");
    let res = send_naming(
        &a,
        &b,
        Some(&demo),
        naming("m-2", "x", &OfferRef::of(&public)),
    )
    .await;
    assert_eq!(rpc_error(&res), (INVALID_PARAMS, "offer_stale".to_owned()));
    assert_eq!(exec.runs(), 0);
    // Asked with the offer, the outbox presents the grant of its scope,
    // not the newest.
    let asked = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "bob".to_owned(),
            question: "under demo".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: Some(OfferRef::of(&offer)),
        })
        .await
        .unwrap();
    assert_eq!(asked.state, "TASK_STATE_COMPLETED");
    assert_eq!(exec.runs(), 1);
    // Without an offer, as in milestone 1: the gateway's grant, the newest.
    let plain = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "bob".to_owned(),
            question: "under any".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: None,
        })
        .await
        .unwrap();
    assert_eq!(plain.state, "TASK_STATE_COMPLETED");
    assert_eq!(exec.runs(), 2);
}
