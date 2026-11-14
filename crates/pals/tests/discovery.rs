//! `agentnet-discovery/0` between real nodes: its own ALPN, framed protobuf,
//! version negotiation, limits, quotas, the inventory views a grant gives,
//! the signed records every answer carries, and what answering never does:
//! run work, or reach another peer.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    CountingExecutor, StubMode, TestNode, gateway_request, grant, issue_grant, key_in, now_unix,
    spawn_node, spawn_node_in_with_options, spawn_node_with_options, stub_discovery_peer,
};
use futures::future::BoxFuture;
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use pals::a2a::IDEMPOTENCY_EXTENSION;
use pals::config::{INVENTORY_FILE, WorkerCfg};
use pals::discovery::{DiscoveryError, DiscoveryHook, Quotas, Reply};
use pals::node::NodeOptions;
use pals::worker::echo::EchoExecutor;
use pals_core::card_jws::parse_and_verify;
use pals_core::clock::{Clock, ManualClock};
use pals_core::discovery::wire::{
    self, ErrorCode, GetBenchmarkReport, GetInventory, Request, Response, request, response,
};
use pals_core::discovery::{
    ALPN_DISCOVERY, MAX_RESPONSE_FRAME_BYTES, decode, encode_frame, frame_len, server_limits,
};
use pals_core::grant::Operation::{BlobFetch, TaskRead, TaskSend};
use pals_core::grant::{SignedGrant, format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::INVENTORY_EXTENSION;
use pals_core::inventory::view::{INVENTORY_SECTIONS, ViewError, ViewPage, verify_page};
use pals_core::records::{ContactRecord, RECORD_RENEW_BEFORE_S, RECORD_TTL_S};
use pals_core::signed::Signed;
use pals_core::store::RecordAccept;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};

/// A plugin everyone sees, one the project `demo` sees, one only its task
/// senders see, and one installed but never offered.
const INVENTORY: &str = r#"
[[plugins]]
id = "example:public-tool"
description = "A plugin anyone may know of"
provenance = "operator"
visibility = "public"

[[plugins]]
id = "example:demo-tool"
description = "A plugin of the demo project"
provenance = "operator"
visibility = { project = ["demo"] }

[[plugins]]
id = "example:demo-task-tool"
description = "A plugin for the tasks of the demo project"
provenance = "operator"
visibility = { task = ["demo"] }

[[plugins]]
id = "example:secret-tool"
description = "Installed, never offered"
provenance = "operator"
"#;

/// How long a call may take: far more than any answer needs, even on a
/// loaded host; only a broken test waits it out.
const CALL: Duration = Duration::from_secs(30);

const WELL_KNOWN: &str = "/.well-known/agent-card.json";

/// A node whose inventory is `inventory`, whose work `executor` does, with
/// `opts`.
async fn provider_with(
    inventory: &str,
    executor: impl a2a_server::AgentExecutor,
    opts: NodeOptions,
) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    spawn_node_in_with_options(dir, executor, opts).await
}

/// [`provider_with`] the echo executor.
async fn provider(inventory: &str, opts: NodeOptions) -> TestNode {
    provider_with(inventory, EchoExecutor, opts).await
}

/// `GetInventory` of `sections` (all when empty) from `cursor` (none when
/// empty), presenting `grant` when given.
fn get_inventory(sections: &[&str], cursor: &str, grant: Option<&SignedGrant>) -> Request {
    Request {
        version: 0,
        max_response_bytes: 0,
        grant_token: grant.map(SignedGrant::to_token).unwrap_or_default(),
        op: Some(request::Op::GetInventory(GetInventory {
            sections: sections.iter().map(|s| (*s).to_owned()).collect(),
            cursor: cursor.to_owned(),
            page_entries: 0,
        })),
    }
}

/// `req` from `from` to `to`, over `from`'s discovery client.
async fn call(from: &TestNode, to: &TestNode, req: Request) -> Result<Reply, DiscoveryError> {
    from.node
        .discovery()
        .call(&to.id(), to.node.addr(), req, CALL)
        .await
}

/// The signed page a reply carries.
fn page_of(reply: &Reply) -> &[u8] {
    match &reply.response.result {
        Some(response::Result::InventoryPage(page)) => page,
        other => panic!("an inventory page, not {other:?}"),
    }
}

/// The page of `reply`, verified as `from`'s page for `audience`.
fn verified(reply: &Reply, from: &TestNode, audience: &TestNode) -> ViewPage {
    verify_page(page_of(reply), &from.id(), Some(&audience.id()), now_unix()).unwrap()
}

fn ids(page: &ViewPage) -> Vec<&str> {
    page.entries.iter().map(|entry| entry.id.as_str()).collect()
}

/// The error a peer answered with.
fn remote(result: Result<Reply, DiscoveryError>) -> wire::Error {
    match result {
        Err(DiscoveryError::Remote(error)) => error,
        Err(other) => panic!("a refusal from the peer, not {other}"),
        Ok(reply) => panic!("a refusal from the peer, not {:?}", reply.response.result),
    }
}

/// A stream to `to` on the discovery ALPN from a fresh endpoint, written to
/// by hand: the endpoint and connection, which keep it open, and its halves.
async fn raw_stream(to: &TestNode) -> (Endpoint, Connection, SendStream, RecvStream) {
    let endpoint = common::loopback_endpoint().await;
    let connection = endpoint
        .connect(to.node.addr(), ALPN_DISCOVERY)
        .await
        .expect("connect on the discovery ALPN");
    let (send, recv) = connection.open_bi().await.expect("open a stream");
    (endpoint, connection, send, recv)
}

/// The response frame `recv` carries, read whole.
async fn read_response(recv: &mut RecvStream) -> Response {
    let mut prefix = [0u8; 4];
    recv.read_exact(&mut prefix).await.expect("a length prefix");
    let len = frame_len(prefix, MAX_RESPONSE_FRAME_BYTES).expect("a bounded response");
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("the response body");
    decode::<Response>(&body).expect("a response")
}

/// The error `response` carries.
fn error_of(response: &Response) -> &wire::Error {
    match &response.result {
        Some(response::Result::Error(error)) => error,
        other => panic!("an error, not {other:?}"),
    }
}

/// `frame` sent alone on a new stream of a fresh endpoint to `to`, and the
/// response it gets.
async fn raw_exchange(to: &TestNode, frame: &[u8]) -> Response {
    let (_endpoint, _connection, mut send, mut recv) = raw_stream(to).await;
    send.write_all(frame).await.expect("send the frame");
    send.finish().expect("finish the request");
    read_response(&mut recv).await
}

/// A discovery hook that holds every request it sees until released, and
/// counts them as they arrive. Clones share the count and the gate.
#[derive(Clone)]
struct HoldingHook {
    arrived: Arc<watch::Sender<usize>>,
    /// Closed, it lets every request go on.
    gate: Arc<Semaphore>,
}

impl HoldingHook {
    fn new() -> Self {
        Self {
            arrived: Arc::new(watch::Sender::new(0)),
            gate: Arc::new(Semaphore::new(0)),
        }
    }

    /// Waits until `count` requests are held, for up to 60 s.
    async fn wait_for(&self, count: usize) {
        let mut arrived = self.arrived.subscribe();
        tokio::time::timeout(Duration::from_secs(60), arrived.wait_for(|n| *n >= count))
            .await
            .expect("the requests reached the hook in time")
            .unwrap();
    }

    fn release(&self) {
        self.gate.close();
    }
}

impl DiscoveryHook for HoldingHook {
    fn before_answer(&self, _peer: &AgentId, _req: &Request) -> BoxFuture<'static, ()> {
        let (arrived, gate) = (Arc::clone(&self.arrived), Arc::clone(&self.gate));
        Box::pin(async move {
            arrived.send_modify(|n| *n += 1);
            // Closed once released: no permit is ever given.
            let _ = gate.acquire().await;
        })
    }
}

/// `req` from `from` to `to` on a task of its own.
fn spawn_call(
    from: &TestNode,
    to: &TestNode,
    req: Request,
) -> tokio::task::JoinHandle<Result<Reply, DiscoveryError>> {
    let client = from.node.discovery().clone();
    let (peer, addr) = (to.id(), to.node.addr());
    tokio::spawn(async move { client.call(&peer, addr, req, CALL).await })
}

#[tokio::test]
async fn public_inventory_over_the_discovery_alpn() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;

    let reply = call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
    assert_eq!(reply.response.version, 0);
    assert_eq!(reply.response.limits, Some(server_limits()));
    assert!(reply.bytes_sent > 4 && reply.bytes_received > page_of(&reply).len() as u64);
    let page = verified(&reply, &b, &a);
    assert_eq!(page.agent_id, b.id());
    assert_eq!(page.scope, "public");
    assert_eq!(page.audience, None);
    assert_eq!(page.seq, 1);
    assert_eq!(page.agent_card_hash, b.node.cards().public().hash);
    assert_eq!(page.sections, INVENTORY_SECTIONS.to_vec());
    assert_eq!(ids(&page), ["example:public-tool"]);
    // A public page names nobody: it verifies for anybody.
    verify_page(page_of(&reply), &b.id(), None, now_unix()).unwrap();

    // Sections asked for, in any order and repeated: those only, in order.
    let reply = call(
        &a,
        &b,
        get_inventory(&["plugins", "services", "plugins"], "", None),
    )
    .await
    .unwrap();
    let page = verified(&reply, &b, &a);
    assert_eq!(
        serde_json::to_value(&page.sections).unwrap(),
        json!(["services", "plugins"])
    );
    assert_eq!(ids(&page), ["example:public-tool"]);
    let reply = call(&a, &b, get_inventory(&["services"], "", None))
        .await
        .unwrap();
    assert!(verified(&reply, &b, &a).entries.is_empty());
}

#[tokio::test]
async fn grant_holder_gets_its_project_view_and_others_do_not() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let c = spawn_node(WorkerCfg::Echo).await;

    // A grant to read the project's tasks: the project's view, for a.
    let read = grant(&b, &a, &[TaskRead], 3600);
    let reply = call(&a, &b, get_inventory(&[], "", Some(&read)))
        .await
        .unwrap();
    let project = verified(&reply, &b, &a);
    assert_eq!(project.scope, "project:demo");
    assert_eq!(project.audience, Some(a.id()));
    assert_eq!(ids(&project), ["example:public-tool", "example:demo-tool"]);
    assert!(project.entries.iter().all(|e| !e.dimensions.authorized));
    // Signed for a only.
    assert_eq!(
        verify_page(page_of(&reply), &b.id(), Some(&c.id()), now_unix()),
        Err(ViewError::WrongAudience)
    );

    // A grant to send it tasks: the task's view, authorized.
    let send = grant(&b, &a, &[TaskSend], 3600);
    let reply = call(&a, &b, get_inventory(&[], "", Some(&send)))
        .await
        .unwrap();
    let task = verified(&reply, &b, &a);
    assert_eq!(task.scope, "task:demo");
    assert_eq!(
        ids(&task),
        [
            "example:public-tool",
            "example:demo-tool",
            "example:demo-task-tool"
        ]
    );
    assert!(task.entries.iter().all(|e| e.dimensions.authorized));

    // A grant that allows nothing on skill:ask only lowers the scope.
    let blobs = grant(&b, &a, &[BlobFetch], 3600);
    let reply = call(&a, &b, get_inventory(&[], "", Some(&blobs)))
        .await
        .unwrap();
    assert_eq!(verified(&reply, &b, &a).scope, "public");

    // Others do not: without a grant, the public view.
    let reply = call(&c, &b, get_inventory(&[], "", None)).await.unwrap();
    let public = verified(&reply, &b, &c);
    assert_eq!(public.scope, "public");
    assert_eq!(ids(&public), ["example:public-tool"]);
    // Nor with a's grant: it is a's, and the connection is c's.
    let error = remote(call(&c, &b, get_inventory(&[], "", Some(&read))).await);
    assert_eq!(error.code(), ErrorCode::Unauthenticated);
    assert_eq!(error.message, "wrong_subject");
    // The private entry shows to nobody.
    for page in [&project, &task, &public] {
        assert!(!ids(page).contains(&"example:secret-tool"));
    }
}

#[tokio::test]
async fn invalid_grant_is_unauthenticated_and_says_why() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;

    let expired = grant(&b, &a, &[TaskSend], -3600);
    let revoked = grant(&b, &a, &[TaskSend], 3600);
    assert!(
        b.node
            .store
            .revoke(&revoked.grant.grant_id, now_unix())
            .unwrap()
    );
    let now = now_unix();
    let stranger = issue_grant(
        &AgentKey::generate(),
        a.id(),
        b.id(),
        &[TaskSend],
        now,
        now + 3600,
    );
    let for_another = grant(&b, &a, &[TaskSend], 3600);
    let elsewhere = issue_grant(
        &key_in(b.dir.path()),
        a.id(),
        AgentKey::generate().id(),
        &[TaskSend],
        now,
        now + 3600,
    );
    for (token, reason) in [
        (expired.to_token(), "expired"),
        (revoked.to_token(), "revoked"),
        (stranger.to_token(), "issuer_not_accepted"),
        (elsewhere.to_token(), "wrong_audience"),
        ("not a token".to_owned(), "malformed"),
    ] {
        let mut req = get_inventory(&[], "", None);
        req.grant_token = token;
        let error = remote(call(&a, &b, req).await);
        assert_eq!(error.code(), ErrorCode::Unauthenticated, "{reason}");
        assert_eq!(error.message, reason);
    }
    // The valid one still works.
    let reply = call(&a, &b, get_inventory(&[], "", Some(&for_another)))
        .await
        .unwrap();
    assert_eq!(verified(&reply, &b, &a).scope, "task:demo");
}

#[tokio::test]
async fn unsupported_version_lists_the_supported_ones() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    for version in [1, 7, u32::MAX] {
        let mut req = get_inventory(&[], "", None);
        req.version = version;
        let error = remote(call(&a, &b, req).await);
        assert_eq!(error.code(), ErrorCode::UnsupportedVersion, "{version}");
        assert_eq!(error.supported_versions, [0]);
    }
    // The refusal still says what the server takes, in version 0.
    let mut req = get_inventory(&[], "", None);
    req.version = 1;
    let response = raw_exchange(&b, &encode_frame(&req)).await;
    assert_eq!(response.version, 0);
    assert_eq!(response.limits, Some(server_limits()));
}

#[tokio::test]
async fn oversized_request_frame_is_refused_without_reading_it_all() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;

    // A gibibyte announced, a kibibyte sent, and the stream left open: the
    // answer comes from the prefix alone.
    let (_endpoint, _connection, mut send, mut recv) = raw_stream(&b).await;
    send.write_all(&(1u32 << 30).to_be_bytes()).await.unwrap();
    // The server may already have stopped reading.
    let _ = send.write_all(&[0u8; 1024]).await;
    let response = read_response(&mut recv).await;
    let error = error_of(&response);
    assert_eq!(error.code(), ErrorCode::TooLarge, "{error:?}");
    assert!(error.message.len() <= 256);

    // One byte over the limit, too.
    let mut frame = (20 * 1024 + 1u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&[0u8; 16]);
    let response = raw_exchange(&b, &frame).await;
    assert_eq!(error_of(&response).code(), ErrorCode::TooLarge);

    // The node goes on serving.
    call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
}

#[tokio::test]
async fn slow_request_times_out_and_the_node_keeps_serving() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;

    // Half a length prefix, and then nothing.
    let (_endpoint, _connection, mut send, mut recv) = raw_stream(&b).await;
    send.write_all(&[0, 0]).await.unwrap();
    // Others are served meanwhile.
    call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
    // The slow stream is answered once its time is up.
    let response = read_response(&mut recv).await;
    let error = error_of(&response);
    assert_eq!(error.code(), ErrorCode::BadRequest, "{error:?}");
    assert_eq!(error.message, "request timed out");
    // And the node still serves.
    call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
}

#[tokio::test]
async fn garbage_frame_is_bad_request() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let request = encode_frame(&get_inventory(&[], "", None));

    let mut not_protobuf = 8u32.to_be_bytes().to_vec();
    not_protobuf.extend_from_slice(&[0xff; 8]);
    let no_operation = encode_frame(&Request::default());
    let two_frames = [request.clone(), request.clone()].concat();
    // Nine bytes announced, fewer sent before the stream ends.
    let mut cut_short = 9u32.to_be_bytes().to_vec();
    cut_short.extend_from_slice(&request[4..]);
    assert!(cut_short.len() < 4 + 9);
    for (name, frame) in [
        ("not protobuf", not_protobuf),
        ("no operation", no_operation),
        ("two frames", two_frames),
        ("cut short", cut_short),
        ("empty", Vec::new()),
    ] {
        let response = raw_exchange(&b, &frame).await;
        let error = error_of(&response);
        assert_eq!(error.code(), ErrorCode::BadRequest, "{name}: {error:?}");
    }
    // The same request whole is answered.
    let response = raw_exchange(&b, &request).await;
    assert!(matches!(
        response.result,
        Some(response::Result::InventoryPage(_))
    ));
    call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
}

#[tokio::test]
async fn error_messages_are_at_most_256_bytes() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let long = |c: char| c.to_string().repeat(4096);

    // An unknown section of 4 KiB: refused, and not echoed.
    let error = remote(call(&a, &b, get_inventory(&[&long('x')], "", None)).await);
    assert_eq!(error.code(), ErrorCode::BadRequest);
    assert!(error.message.len() <= 256, "{}", error.message.len());
    assert!(!error.message.contains("xxxx"), "{}", error.message);

    // Nor any other refusal echoes what was sent.
    let mut bad_version = get_inventory(&[], "", None);
    bad_version.version = 2;
    let mut bad_token = get_inventory(&[], "", None);
    bad_token.grant_token = long('t');
    let bad_cursor = get_inventory(&[], &long('c'), None);
    let mut no_op = get_inventory(&[], "", None);
    no_op.op = None;
    let mut benchmark = get_inventory(&[], "", None);
    benchmark.op = Some(request::Op::GetBenchmarkReport(GetBenchmarkReport {
        benchmark_id: long('b'),
    }));
    for (req, code, echo) in [
        (bad_version, ErrorCode::UnsupportedVersion, None),
        (bad_token, ErrorCode::Unauthenticated, Some("tttt")),
        (bad_cursor, ErrorCode::InvalidCursor, Some("cccc")),
        (no_op, ErrorCode::BadRequest, None),
        (benchmark, ErrorCode::UnsupportedOperation, Some("bbbb")),
    ] {
        let error = remote(call(&a, &b, req).await);
        assert_eq!(error.code(), code);
        assert!(
            !error.message.is_empty() && error.message.len() <= 256,
            "{code:?}"
        );
        if let Some(echo) = echo {
            assert!(!error.message.contains(echo), "{}", error.message);
        }
    }
}

#[tokio::test]
async fn per_peer_quota_refuses_the_61st_request_in_a_minute() {
    // The provider's clock stands still unless the test moves it.
    let clock = Arc::new(ManualClock::new(now_unix()));
    let opts = NodeOptions {
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        ..NodeOptions::default()
    };
    let b = provider(INVENTORY, opts).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let c = spawn_node(WorkerCfg::Echo).await;

    for n in 0..60 {
        call(&a, &b, get_inventory(&["plugins"], "", None))
            .await
            .unwrap_or_else(|err| panic!("request {n}: {err}"));
    }
    let error = remote(call(&a, &b, get_inventory(&["plugins"], "", None)).await);
    assert_eq!(error.code(), ErrorCode::RateLimited);
    assert!((1..=60_000).contains(&error.retry_after_ms), "{error:?}");
    // Another peer has a minute of its own.
    call(&c, &b, get_inventory(&["plugins"], "", None))
        .await
        .unwrap();

    // A minute later, a is served again.
    clock.advance(60);
    call(&a, &b, get_inventory(&["plugins"], "", None))
        .await
        .unwrap();
}

#[tokio::test]
async fn more_than_4_streams_per_peer_at_once_are_refused() {
    let hook = HoldingHook::new();
    let opts = NodeOptions {
        discovery_hook: Some(Arc::new(hook.clone())),
        ..NodeOptions::default()
    };
    let b = provider(INVENTORY, opts).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let c = spawn_node(WorkerCfg::Echo).await;

    let held: Vec<_> = (0..4)
        .map(|_| spawn_call(&a, &b, get_inventory(&[], "", None)))
        .collect();
    hook.wait_for(4).await;
    // A fifth stream of a's while four are open: refused, not queued.
    let error = remote(call(&a, &b, get_inventory(&[], "", None)).await);
    assert_eq!(error.code(), ErrorCode::RateLimited);
    assert!(error.retry_after_ms > 0);
    // Another peer is not held back by a's streams.
    let other = spawn_call(&c, &b, get_inventory(&[], "", None));
    hook.wait_for(5).await;

    hook.release();
    for call in held {
        call.await.unwrap().unwrap();
    }
    other.await.unwrap().unwrap();
    // With its streams done, a is served again.
    call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
}

#[tokio::test]
async fn global_stream_cap_refuses_beyond_it() {
    let hook = HoldingHook::new();
    let opts = NodeOptions {
        discovery_hook: Some(Arc::new(hook.clone())),
        quotas: Quotas {
            global_concurrent: 2,
            ..Quotas::default()
        },
        ..NodeOptions::default()
    };
    let b = provider(INVENTORY, opts).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let d = spawn_node(WorkerCfg::Echo).await;

    let held = [
        spawn_call(&a, &b, get_inventory(&[], "", None)),
        spawn_call(&c, &b, get_inventory(&[], "", None)),
    ];
    hook.wait_for(2).await;
    // A third peer, within its own quota but beyond the node's.
    let error = remote(call(&d, &b, get_inventory(&[], "", None)).await);
    assert_eq!(error.code(), ErrorCode::RateLimited);
    // Nor does a get a second stream.
    let error = remote(call(&a, &b, get_inventory(&[], "", None)).await);
    assert_eq!(error.code(), ErrorCode::RateLimited);

    hook.release();
    for call in held {
        call.await.unwrap().unwrap();
    }
    call(&d, &b, get_inventory(&[], "", None)).await.unwrap();
}

#[tokio::test]
async fn card_declares_the_inventory_extension_once_it_is_served() {
    let b = spawn_node(WorkerCfg::Echo).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let card = a.get(&b, WELL_KNOWN, None).await.json;

    let extensions = card["capabilities"]["extensions"].as_array().unwrap();
    let uris: Vec<&str> = extensions
        .iter()
        .map(|extension| extension["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, [IDEMPOTENCY_EXTENSION, INVENTORY_EXTENSION]);
    // Neither is required: a request without them is served as in
    // milestone 1.
    for extension in extensions {
        assert!(extension.get("required").is_none(), "{extension}");
    }
    assert_eq!(
        extensions[1]["params"],
        json!({"profile": "agentnet.inventory/1", "discoveryAlpn": "agentnet-discovery/0"})
    );
    assert!(
        extensions[1]["description"]
            .as_str()
            .is_some_and(|d| !d.is_empty())
    );
    // Still signed by b.
    parse_and_verify(&serde_json::to_vec(&card).unwrap(), &b.id()).unwrap();

    // The ALPN it names is the one served.
    let alpn = extensions[1]["params"]["discoveryAlpn"].as_str().unwrap();
    assert_eq!(alpn.as_bytes(), ALPN_DISCOVERY);
    call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
}

#[tokio::test]
async fn discovery_never_runs_work() {
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_with(INVENTORY, exec.clone(), NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let send = grant(&b, &a, &[TaskSend], 3600);
    let expired = grant(&b, &a, &[TaskSend], -3600);

    let mut requests = Vec::new();
    for round in 0..5 {
        let cursor = format!("not-a-cursor-{round}");
        let mut bad_version = get_inventory(&[], "", None);
        bad_version.version = 1;
        let mut query = get_inventory(&[], "", Some(&send));
        query.op = Some(request::Op::ServiceQuery(wire::ServiceQuery {
            query: br#"{"capability":"code.review","summary":"run this"}"#.to_vec(),
        }));
        let mut report = get_inventory(&[], "", Some(&send));
        report.op = Some(request::Op::GetBenchmarkReport(GetBenchmarkReport {
            benchmark_id: "b-1".to_owned(),
        }));
        let mut no_op = get_inventory(&[], "", None);
        no_op.op = None;
        let mut small = get_inventory(&["plugins"], "", Some(&send));
        if let Some(request::Op::GetInventory(get)) = &mut small.op {
            get.page_entries = 1;
        }
        requests.extend([
            get_inventory(&[], "", None),
            get_inventory(&[], "", Some(&send)),
            get_inventory(&["mcp", "services"], "", Some(&expired)),
            bad_version,
            query,
            report,
            no_op,
            get_inventory(&["tasks"], "", None),
            get_inventory(&[], &cursor, Some(&send)),
            small,
        ]);
    }
    assert_eq!(requests.len(), 50);
    let mut answered = 0;
    for req in requests {
        match call(&a, &b, req).await {
            Ok(_) | Err(DiscoveryError::Remote(_)) => answered += 1,
            Err(other) => panic!("every request is answered: {other}"),
        }
    }
    assert_eq!(answered, 50);
    assert_eq!(exec.runs(), 0);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

#[tokio::test]
async fn provider_never_forwards_a_query() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    // C is one of b's contacts, with a grant b holds.
    let carol = stub_discovery_peer(&b, "carol", StubMode::Silent).await;
    let send = grant(&b, &a, &[TaskSend], 3600);

    for grant in [None, Some(&send)] {
        let mut query = get_inventory(&[], "", grant);
        query.op = Some(request::Op::ServiceQuery(wire::ServiceQuery {
            query: br#"{"capability":"code.review","peers":["carol"]}"#.to_vec(),
        }));
        let mut report = get_inventory(&[], "", grant);
        report.op = Some(request::Op::GetBenchmarkReport(GetBenchmarkReport {
            benchmark_id: "carol".to_owned(),
        }));
        for req in [get_inventory(&[], "", grant), query, report] {
            match call(&a, &b, req).await {
                Ok(_) | Err(DiscoveryError::Remote(_)) => {}
                Err(other) => panic!("b answers itself: {other}"),
            }
        }
    }
    assert_eq!(carol.connections(), 0);
    assert_eq!(carol.requests(), 0);

    // The stub does count what reaches it.
    let probe = {
        let client = a.node.discovery().clone();
        let (id, addr) = (carol.id, carol.addr());
        tokio::spawn(async move {
            client
                .call(&id, addr, get_inventory(&[], "", None), CALL)
                .await
        })
    };
    carol.wait_for_requests(1).await;
    assert_eq!(carol.connections(), 1);
    probe.abort();
}

#[tokio::test]
async fn every_response_carries_the_signed_records_and_a_regression_is_refused() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let (contact, announce) = b.node.records().current();

    // An answer: the records b has in force, as a has verified them.
    let reply = call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
    assert_eq!(reply.response.contact_record, contact);
    assert_eq!(reply.response.announce_record, announce);
    assert_eq!(reply.contact.to_json(), contact);
    assert_eq!(reply.announce.to_json(), announce);
    assert_eq!(reply.contact.doc.agent_id, b.id());
    assert_eq!(
        reply.announce.doc.agent_card_hash,
        b.node.cards().public().hash
    );
    assert_eq!(reply.announce.doc.discovery_alpn, "agentnet-discovery/0");

    // A refusal, of the server or of the answer, carries them too.
    let mut bad_version = get_inventory(&[], "", None);
    bad_version.version = 3;
    let mut too_large = (1u32 << 30).to_be_bytes().to_vec();
    too_large.extend_from_slice(&[0u8; 8]);
    for frame in [
        encode_frame(&bad_version),
        encode_frame(&Request::default()),
        vec![0, 0, 0, 2, 0xff, 0xff],
        too_large,
    ] {
        let response = raw_exchange(&b, &frame).await;
        assert!(matches!(response.result, Some(response::Result::Error(_))));
        assert_eq!(response.contact_record, contact);
        assert_eq!(response.announce_record, announce);
    }

    // a knows a newer contact record of b's than the one b serves: b's
    // answer goes back, and is refused whole.
    let now = now_unix();
    let newer = ContactRecord {
        seq: 1000,
        ..reply.contact.doc.clone()
    };
    let newer = Signed::issue(&key_in(b.dir.path()), newer).unwrap();
    let expires_at = parse_timestamp(&newer.doc.expires_at).unwrap();
    assert_eq!(
        a.node
            .store
            .peer_record_accept(&b.id(), "contact", 1000, &newer.to_json(), expires_at, now)
            .unwrap(),
        RecordAccept::New
    );
    assert_eq!(
        call(&a, &b, get_inventory(&[], "", None))
            .await
            .unwrap_err(),
        DiscoveryError::Regression { known_seq: 1000 }
    );
}

#[tokio::test]
async fn answers_carry_the_records_renewed_after_start() {
    // Both clocks stand still unless the test moves them.
    let start = now_unix();
    let clock_a = Arc::new(ManualClock::new(start));
    let clock_b = Arc::new(ManualClock::new(start));
    let b = provider(
        INVENTORY,
        NodeOptions {
            clock: Arc::clone(&clock_b) as Arc<dyn Clock>,
            ..NodeOptions::default()
        },
    )
    .await;
    let a = spawn_node_with_options(
        EchoExecutor,
        NodeOptions {
            clock: Arc::clone(&clock_a) as Arc<dyn Clock>,
            ..NodeOptions::default()
        },
    )
    .await;
    let first = call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
    assert_eq!(first.contact.doc.seq, 1);

    // Half a day later, b's renewal step issues its records again.
    let later = start + RECORD_TTL_S - RECORD_RENEW_BEFORE_S + 1;
    clock_a.set(later);
    clock_b.set(later);
    assert!(b.node.records().renew(later).unwrap());

    // The next answer carries them: the ones in force, not those b
    // started with.
    let second = call(&a, &b, get_inventory(&[], "", None)).await.unwrap();
    let (contact, announce) = b.node.records().current();
    assert_eq!(second.contact.to_json(), contact);
    assert_eq!(second.announce.to_json(), announce);
    assert_eq!(second.contact.doc.seq, 2);
    assert_eq!(second.announce.doc.seq, 2);
    assert_eq!(
        second.contact.doc.issued_at,
        format_timestamp(later).unwrap()
    );
}

#[tokio::test]
async fn private_inventory_cached_for_one_requester_is_not_given_to_another() {
    let b = provider(INVENTORY, NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let c = spawn_node(WorkerCfg::Echo).await;

    // a's project page, which b keeps.
    let read = grant(&b, &a, &[TaskRead], 3600);
    let private = call(&a, &b, get_inventory(&[], "", Some(&read)))
        .await
        .unwrap();
    assert_eq!(verified(&private, &b, &a).scope, "project:demo");
    let again = call(&a, &b, get_inventory(&[], "", Some(&read)))
        .await
        .unwrap();
    assert_eq!(page_of(&again), page_of(&private), "served from b's cache");

    // The same request from c, without a grant: the public page, never a's.
    for _ in 0..2 {
        let reply = call(&c, &b, get_inventory(&[], "", None)).await.unwrap();
        assert_ne!(page_of(&reply), page_of(&private));
        let public = verified(&reply, &b, &c);
        assert_eq!((public.scope.as_str(), public.audience), ("public", None));
        assert_eq!(ids(&public), ["example:public-tool"]);
    }
    // Nor is a's page any good to c.
    assert_eq!(
        verify_page(page_of(&private), &b.id(), Some(&c.id()), now_unix()),
        Err(ViewError::WrongAudience)
    );

    // a's gateway keeps each page under the grant it asked with: a newer
    // grant's request does not get the page an older one got.
    a.add_contact("bob", &b);
    a.hold(&read);
    let gateway = a.gateway_url();
    let path = "/discovery/inventory/bob";
    let scope_of = |page: &Value| page["view"]["scope"].as_str().unwrap().to_owned();
    let res = gateway_request(&gateway, "GET", path, &[], None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(scope_of(&res.json), "project:demo");
    let res = gateway_request(&gateway, "GET", path, &[], None).await;
    assert_eq!(scope_of(&res.json), "project:demo");
    let now = now_unix();
    let newer = issue_grant(
        &key_in(b.dir.path()),
        a.id(),
        b.id(),
        &[TaskSend],
        now + 1,
        now + 3600,
    );
    b.node.store.grant_issued_put(&newer).unwrap();
    a.hold(&newer);
    let res = gateway_request(&gateway, "GET", path, &[], None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(scope_of(&res.json), "task:demo");
}

/// `count` public plugins, `example:plugin-<n>`.
fn plugins(count: usize) -> String {
    (0..count)
        .map(|n| {
            format!(
                "[[plugins]]\nid = \"example:plugin-{n:03}\"\ndescription = \"Plugin {n}\"\n\
                 provenance = \"operator\"\nvisibility = \"public\"\n\n"
            )
        })
        .collect()
}

/// Runs `pals --home <home> <args>` off the async threads; its exit code,
/// stdout and stderr.
async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
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

/// The pages `pals inventory get` printed, each with its `verified:` line.
fn printed_pages(stdout: &str) -> Vec<(Value, String)> {
    let mut pages = Vec::new();
    let mut json = String::new();
    for line in stdout.lines() {
        if line.starts_with("verified: ") {
            pages.push((serde_json::from_str(&json).unwrap(), line.to_owned()));
            json.clear();
        } else {
            json.push_str(line);
            json.push('\n');
        }
    }
    assert!(json.trim().is_empty(), "nothing after the last page");
    pages
}

#[tokio::test]
async fn inventory_get_cli_prints_the_verified_pages() {
    let b = provider(&plugins(120), NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    // The CLI finds the running daemon by its configuration.
    let home = a.dir.path().to_owned();
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    let expected: Vec<String> = (0..120).map(|n| format!("example:plugin-{n:03}")).collect();
    let verified_line = format!("verified: {} public seq 1", b.id());

    // The first page, with the cursor of the next.
    let (code, stdout, stderr) = pals(&home, &["inventory", "get", "bob"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let pages = printed_pages(&stdout);
    assert_eq!(pages.len(), 1, "{stdout}");
    let (page, verified) = &pages[0];
    assert_eq!(verified, &verified_line);
    assert_eq!(page["view"]["entries"].as_array().unwrap().len(), 100);
    assert_eq!(page["view"]["truncated_by"], "page_entries");
    assert!(page["view"]["next_cursor"].is_string());
    // As b signed it.
    let bytes = pals_core::jcs::canonical(page);
    verify_page(&bytes, &b.id(), Some(&a.id()), now_unix()).unwrap();

    // Every page, following the cursors, of the sections asked for.
    let (code, stdout, stderr) = pals(
        &home,
        &["inventory", "get", "bob", "--sections", "plugins", "--all"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let pages = printed_pages(&stdout);
    assert_eq!(pages.len(), 2, "{stdout}");
    let mut seen = Vec::new();
    for (page, verified) in &pages {
        assert_eq!(verified, &verified_line);
        assert_eq!(page["view"]["sections"], json!(["plugins"]));
        for entry in page["view"]["entries"].as_array().unwrap() {
            seen.push(entry["id"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(seen, expected);
    assert!(pages[1].0["view"].get("next_cursor").is_none());

    // An unknown peer, or section, is said so.
    let (code, _, stderr) = pals(&home, &["inventory", "get", "nobody"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown contact"), "{stderr}");
    let (code, _, stderr) = pals(&home, &["inventory", "get", "bob", "--sections", "tasks"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown section"), "{stderr}");
}
