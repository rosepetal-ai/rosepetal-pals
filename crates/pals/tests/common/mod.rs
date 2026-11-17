//! Shared helpers for the integration tests: Iroh endpoints on loopback with
//! no relay, an echo agent served through the tunnel, a client that goes
//! through the production `PeerHttp`, complete PALS nodes ([`TestNode`])
//! with the grants that let them talk to each other, an executor that counts
//! its runs and can be gated or made busy ([`CountingExecutor`]), nodes whose
//! worker reports a fixed load ([`spawn_node_with_load`]) or with their
//! configuration and options both set ([`spawn_node_in_configured`]), peers
//! that speak only the discovery protocol, as the tests script them
//! ([`stub_discovery_peer`]), and serve a signed card too
//! ([`stub_discovery_peer_with_card`]), stock A2A agents with
//! no PALS behind them, served over plain HTTP on loopback ([`echo_app_at`],
//! [`serve_at`]), a plain HTTP client for a node's loopback gateway
//! ([`gateway_request`]), and nodes with the DHT on, entering it through
//! other nodes' tickets ([`spawn_dht_node`]), with what the DHT's tests
//! share: waiting on a node's DHT ([`wait_for`]), signed records
//! ([`contact`], [`announce`]), a DHT client of its own that publishes
//! records a test chose ([`Publisher`], [`TestRecords`]), and the `pals`
//! command ([`pals`]).

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use a2a::{
    A2AError, AgentCapabilities, AgentCard, AgentInterface, AgentSkill, StreamResponse,
    TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC,
};
use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::rest::rest_router;
use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, InMemoryTaskStore, StaticAgentCard,
};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, Response, header};
use axum::routing::{get, post};
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use http_body_util::BodyExt;
use hyper::client::conn::http1 as client_http1;
use hyper_util::rt::TokioIo;
use iroh::endpoint::{Connection, PortmapperConfig, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use pals::config::{
    AgentCfg, Config, DhtCfg, DhtMode, DiscoveryCfg, GatewayCfg, INVENTORY_FILE, InventoryCfg,
    KEY_FILE, LimitsCfg, McpCfg, NetCfg, WorkerCfg,
};
use pals::dht::swarm::STORE_LIMITS;
use pals::dht::transport::{ALPN_KAD, KadQuotas};
use pals::dht::{Dht, DhtParts, DhtStatus, DhtTiming, PublishedRecords};
use pals::net::{ALPN_A2A_HTTP, PEER_HEADER, PeerHttp, TunnelLimits, TunnelServer, agent_id_of};
use pals::node::{Node, NodeOptions};
use pals::worker::echo::EchoExecutor;
use pals_core::card_jws::CardHash;
use pals_core::clock::Clock;
use pals_core::discovery::{
    ALPN_DISCOVERY, DISCOVERY_VERSION, LoadProbe, MAX_REQUEST_FRAME_BYTES, decode, encode_frame,
    frame_len, server_limits, wire,
};
use pals_core::grant::{GRANT_SCHEMA, Grant, Operation, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{
    ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact, RECORD_TTL_S,
};
use pals_core::signed::Signed;
use pals_core::store::Contact;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Notify, watch};

/// The URI of PALS's idempotency extension, which a request activates with
/// `A2A-Extensions`. A literal, so that the tests pin the wire value.
pub const IDEMPOTENCY_EXTENSION: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/extensions/idempotency/v1";

/// The echo agent as a standard A2A v1.0 server: a static card, the JSON-RPC
/// binding at `/jsonrpc` and the HTTP+JSON binding at `/rest`.
pub fn echo_app(agent_name: &str) -> axum::Router {
    let capabilities = AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        ..AgentCapabilities::default()
    };
    let card = AgentCard {
        name: agent_name.to_owned(),
        description: "Replies with the text it receives".to_owned(),
        version: "0.1.0".to_owned(),
        // The tunnel has no HTTP origin; the `.invalid` TLD (RFC 2606) keeps
        // these placeholder URLs from ever resolving.
        supported_interfaces: vec![
            AgentInterface::new("http://agent.invalid/jsonrpc", TRANSPORT_PROTOCOL_JSONRPC),
            AgentInterface::new("http://agent.invalid/rest", TRANSPORT_PROTOCOL_HTTP_JSON),
        ],
        capabilities: capabilities.clone(),
        default_input_modes: vec!["text/plain".to_owned()],
        default_output_modes: vec!["text/plain".to_owned()],
        skills: vec![AgentSkill {
            id: "echo".to_owned(),
            name: "Echo".to_owned(),
            description: "Replies with `echo: <text>`".to_owned(),
            tags: vec!["echo".to_owned()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        }],
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    };
    let handler = Arc::new(
        DefaultRequestHandler::new(EchoExecutor, InMemoryTaskStore::new())
            .with_capabilities(capabilities),
    );
    axum::Router::new()
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(card))))
        .nest("/jsonrpc", jsonrpc_router(Arc::clone(&handler)))
        .nest("/rest", rest_router(handler))
}

/// The echo agent's card, its JSON-RPC and HTTP+JSON interfaces at `origin`
/// (`http://127.0.0.1:<port>`), as a stock A2A agent reached over HTTP
/// declares them.
pub fn echo_card_at(origin: &str) -> AgentCard {
    AgentCard {
        name: "plain echo".to_owned(),
        description: "Replies with the text it receives".to_owned(),
        version: "0.1.0".to_owned(),
        supported_interfaces: vec![
            AgentInterface::new(format!("{origin}/jsonrpc"), TRANSPORT_PROTOCOL_JSONRPC),
            AgentInterface::new(format!("{origin}/rest"), TRANSPORT_PROTOCOL_HTTP_JSON),
        ],
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            ..AgentCapabilities::default()
        },
        default_input_modes: vec!["text/plain".to_owned()],
        default_output_modes: vec!["text/plain".to_owned()],
        skills: vec![AgentSkill {
            id: "echo".to_owned(),
            name: "Echo".to_owned(),
            description: "Replies with `echo: <text>`".to_owned(),
            tags: vec!["echo".to_owned()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        }],
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    }
}

/// The echo agent as a stock A2A v1.0 server that serves `card`, exactly as
/// given, at `/.well-known/agent-card.json`, and its JSON-RPC binding at
/// `/jsonrpc` and its HTTP+JSON binding at `/rest`, whatever the card says.
pub fn echo_app_with_card(card: Value) -> axum::Router {
    let capabilities = AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        ..AgentCapabilities::default()
    };
    let handler = Arc::new(
        DefaultRequestHandler::new(EchoExecutor, InMemoryTaskStore::new())
            .with_capabilities(capabilities),
    );
    axum::Router::new()
        .route(
            "/.well-known/agent-card.json",
            get(move || {
                let card = card.clone();
                async move { Json(card) }
            }),
        )
        .nest("/jsonrpc", jsonrpc_router(Arc::clone(&handler)))
        .nest("/rest", rest_router(handler))
}

/// A stock A2A agent with no PALS behind it, the echo agent, served over
/// HTTP on a new port of 127.0.0.1, whose card declares the origin it
/// listens at, `http://127.0.0.1:<port>` ([`echo_app`] declares
/// `agent.invalid`, for the tunnel): that origin, and the server.
pub async fn echo_app_at() -> (String, ServerHandle) {
    serve_at(|origin| {
        echo_app_with_card(serde_json::to_value(echo_card_at(origin)).expect("a card serializes"))
    })
    .await
}

/// A request a [`ServerHandle`] received.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub method: String,
    /// The path and the query, as sent.
    pub uri: String,
    pub headers: HeaderMap,
}

/// An HTTP server on 127.0.0.1 ([`serve_at`]) that records every request it
/// receives, and stops when dropped.
pub struct ServerHandle {
    origin: String,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves over HTTP, on a new port of 127.0.0.1, the app `build` makes for
/// the origin it listens at: the listener is bound first, so that the app,
/// its card, can name the origin it is reached at. That origin,
/// `http://127.0.0.1:<port>`, and the server.
pub async fn serve_at(build: impl FnOnce(&str) -> axum::Router) -> (String, ServerHandle) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind a loopback port");
    let origin = format!("http://{}", listener.local_addr().expect("a local address"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let app = build(&origin).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().unwrap().push(SeenRequest {
                    method: request.method().to_string(),
                    uri: request.uri().to_string(),
                    headers: request.headers().clone(),
                });
                next.run(request).await
            }
        },
    ));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (origin.clone(), ServerHandle { origin, seen, task })
}

/// An app that answers `POST /peer` with every `x-agentnet-peer` value the
/// request carried when it reached the app: `{"peer": ["<value>", ...]}`.
pub fn peer_header_app() -> axum::Router {
    axum::Router::new().route(
        "/peer",
        post(|headers: HeaderMap| async move {
            let seen: Vec<String> = headers
                .get_all(PEER_HEADER)
                .iter()
                .map(|value| value.to_str().unwrap_or("<not ascii>").to_owned())
                .collect();
            Json(json!({ "peer": seen }))
        }),
    )
}

/// The serving side of a pair: an Iroh router with `TunnelServer` on
/// `ALPN_A2A_HTTP`, and the address a client dials.
pub struct TestServer {
    pub addr: EndpointAddr,
    /// Keeps accepting connections until dropped.
    router: Router,
}

/// The requesting side of a pair.
pub struct TestClient {
    pub endpoint: Endpoint,
    pub http: PeerHttp,
}

/// A response read to the end.
pub struct TestResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// The body parsed as JSON, or `Value::Null` when it is not JSON.
    pub json: Value,
}

impl TestResponse {
    /// Reads `response` to the end.
    pub async fn read(response: Response<Body>) -> Self {
        let (parts, body) = response.into_parts();
        let body = body
            .collect()
            .await
            .expect("read the response body")
            .to_bytes();
        let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
        Self {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
            json,
        }
    }

    /// The header's first value, or `""` when it is absent or not ASCII.
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }
}

impl TestClient {
    /// Sends `request` to `server` through `PeerHttp` and returns the
    /// response with its body still streaming.
    pub async fn send(&self, server: &TestServer, request: Request<Body>) -> Response<Body> {
        self.http
            .send(server.addr.clone(), request)
            .await
            .expect("tunnel request")
    }

    pub async fn get(&self, server: &TestServer, path: &str) -> TestResponse {
        let request = Request::get(path)
            .body(Body::empty())
            .expect("valid request");
        TestResponse::read(self.send(server, request).await).await
    }

    pub async fn post_json(
        &self,
        server: &TestServer,
        path: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> TestResponse {
        let mut request = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let body = serde_json::to_vec(body).expect("serializable body");
        let request = request.body(Body::from(body)).expect("valid request");
        TestResponse::read(self.send(server, request).await).await
    }
}

/// Two Iroh endpoints on 127.0.0.1 with no relay; the first serves `app`
/// through `TunnelServer` with the default limits.
pub async fn spawn_pair(app: axum::Router) -> (TestServer, TestClient) {
    spawn_pair_with(app, TunnelLimits::default()).await
}

/// [`spawn_pair`], the server with `limits`.
pub async fn spawn_pair_with(app: axum::Router, limits: TunnelLimits) -> (TestServer, TestClient) {
    let server_endpoint = loopback_endpoint().await;
    let socket = *server_endpoint
        .bound_sockets()
        .first()
        .expect("the endpoint is bound to a socket");
    let addr = EndpointAddr::new(server_endpoint.id()).with_ip_addr(socket);
    let router = Router::builder(server_endpoint)
        .accept(ALPN_A2A_HTTP, TunnelServer::new(app, limits))
        .spawn();

    let client_endpoint = loopback_endpoint().await;
    let http = PeerHttp::new(client_endpoint.clone());
    (
        TestServer { addr, router },
        TestClient {
            endpoint: client_endpoint,
            http,
        },
    )
}

/// `spawn_pair` serving `echo_app("echo")`.
pub async fn spawn_echo_pair() -> (TestServer, TestClient) {
    spawn_pair(echo_app("echo")).await
}

/// An Iroh endpoint on 127.0.0.1 with no relay and a random key.
pub async fn loopback_endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("loopback bind address")
        .bind()
        .await
        .expect("bind the Iroh endpoint")
}

/// A complete PALS node on loopback with no relay, the data directory it
/// runs on and the directory its download directory is created in, both
/// removed when this value is dropped.
pub struct TestNode {
    pub node: Node,
    pub dir: tempfile::TempDir,
    /// Holds the node's download directory, [`TestNode::download_dir`],
    /// which `pals_fetch` creates on first use.
    pub downloads: tempfile::TempDir,
}

/// The configuration a [`TestNode`] runs with on `data_dir`: the defaults,
/// `worker`, no relay, so the node binds to 127.0.0.1 only, the gateway on
/// a free port of 127.0.0.1, `pals_fetch` writing to
/// `<temp dir>/pals-test-downloads` (a [`TestNode`] has its own), and the
/// inventory read from `inventory.toml` in `data_dir`, empty unless a test
/// writes it there first.
pub fn test_config(data_dir: &Path, worker: WorkerCfg) -> Config {
    Config {
        data_dir: data_dir.to_path_buf(),
        agent: AgentCfg::default(),
        gateway: GatewayCfg {
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        },
        worker,
        projects: BTreeMap::new(),
        limits: LimitsCfg::default(),
        net: NetCfg {
            relay: false,
            listen: None,
        },
        mcp: McpCfg {
            download_dir: Some(std::env::temp_dir().join("pals-test-downloads")),
        },
        inventory: InventoryCfg {
            file: data_dir.join(INVENTORY_FILE),
            ..InventoryCfg::default()
        },
        discovery: DiscoveryCfg::default(),
        dht: DhtCfg::default(),
        trusted_issuers: Vec::new(),
    }
}

/// The download directory of a node whose downloads are kept in
/// `downloads`: a directory in it that does not exist yet.
fn download_dir_in(downloads: &tempfile::TempDir) -> std::path::PathBuf {
    downloads.path().join("pals")
}

/// A node with `worker` on a fresh data directory.
pub async fn spawn_node(worker: WorkerCfg) -> TestNode {
    spawn_node_with(worker, |_| {}).await
}

/// [`spawn_node`] with its configuration changed by `edit` first.
pub async fn spawn_node_with(worker: WorkerCfg, edit: impl FnOnce(&mut Config)) -> TestNode {
    let dir = tempfile::tempdir().expect("create a data directory");
    spawn_node_in(dir, worker, edit).await
}

/// A node on `dir`, which may already hold a key and a store.
pub async fn spawn_node_in(
    dir: tempfile::TempDir,
    worker: WorkerCfg,
    edit: impl FnOnce(&mut Config),
) -> TestNode {
    let downloads = tempfile::tempdir().expect("create a downloads directory");
    let mut cfg = test_config(dir.path(), worker);
    cfg.mcp.download_dir = Some(download_dir_in(&downloads));
    edit(&mut cfg);
    let node = Node::spawn(cfg).await.expect("spawn the node");
    TestNode {
        node,
        dir,
        downloads,
    }
}

/// A node whose work `executor` does, on a fresh data directory.
pub async fn spawn_node_with_executor(executor: impl AgentExecutor) -> TestNode {
    let dir = tempfile::tempdir().expect("create a data directory");
    spawn_node_in_with_executor(dir, executor).await
}

/// A node whose work `executor` does, on `dir`, which may already hold a key
/// and a store.
pub async fn spawn_node_in_with_executor(
    dir: tempfile::TempDir,
    executor: impl AgentExecutor,
) -> TestNode {
    spawn_node_in_with_options(dir, executor, NodeOptions::default()).await
}

/// A node whose work `executor` does, with `opts` (its clock, its load, a
/// hook in its discovery server, its discovery quotas), on a fresh data
/// directory.
pub async fn spawn_node_with_options(executor: impl AgentExecutor, opts: NodeOptions) -> TestNode {
    let dir = tempfile::tempdir().expect("create a data directory");
    spawn_node_in_with_options(dir, executor, opts).await
}

/// [`spawn_node_with_options`] on `dir`, which may already hold a key, a
/// store and an inventory.
pub async fn spawn_node_in_with_options(
    dir: tempfile::TempDir,
    executor: impl AgentExecutor,
    opts: NodeOptions,
) -> TestNode {
    spawn_node_in_configured(dir, executor, opts, |_| {}).await
}

/// [`spawn_node_in_with_options`], with its configuration changed by `edit`
/// first.
pub async fn spawn_node_in_configured(
    dir: tempfile::TempDir,
    executor: impl AgentExecutor,
    opts: NodeOptions,
    edit: impl FnOnce(&mut Config),
) -> TestNode {
    let downloads = tempfile::tempdir().expect("create a downloads directory");
    let mut cfg = test_config(dir.path(), WorkerCfg::Echo);
    cfg.mcp.download_dir = Some(download_dir_in(&downloads));
    edit(&mut cfg);
    let node = Node::spawn_with_options(cfg, executor, opts)
        .await
        .expect("spawn the node");
    TestNode {
        node,
        dir,
        downloads,
    }
}

/// A node with the DHT on, in `mode`, entering it through the tickets of
/// `bootstrap` ([`Node::ticket`]), on loopback with no relay, with the DHT's
/// times `timing` and an echo worker, on a fresh data directory.
pub async fn spawn_dht_node(mode: DhtMode, bootstrap: &[&TestNode], timing: DhtTiming) -> TestNode {
    let opts = NodeOptions {
        dht_timing: Some(timing),
        ..NodeOptions::default()
    };
    spawn_dht_node_with(mode, bootstrap, opts, |_| {}).await
}

/// [`spawn_dht_node`], with `opts` (its DHT's times among them) and its
/// configuration changed by `edit` after the DHT's section is written.
pub async fn spawn_dht_node_with(
    mode: DhtMode,
    bootstrap: &[&TestNode],
    opts: NodeOptions,
    edit: impl FnOnce(&mut Config),
) -> TestNode {
    let dir = tempfile::tempdir().expect("create a data directory");
    spawn_dht_node_in(dir, mode, bootstrap, opts, edit).await
}

/// [`spawn_dht_node_with`] on `dir`, which may already hold a key and a
/// store.
pub async fn spawn_dht_node_in(
    dir: tempfile::TempDir,
    mode: DhtMode,
    bootstrap: &[&TestNode],
    opts: NodeOptions,
    edit: impl FnOnce(&mut Config),
) -> TestNode {
    let tickets: Vec<String> = bootstrap.iter().map(|node| node.node.ticket()).collect();
    spawn_node_in_configured(dir, EchoExecutor, opts, |cfg| {
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

/// A node whose work `executor` does and whose worker reports `load` to the
/// service queries it answers, on a fresh data directory.
pub async fn spawn_node_with_load(
    executor: impl AgentExecutor,
    load: Arc<dyn LoadProbe>,
) -> TestNode {
    let dir = tempfile::tempdir().expect("create a data directory");
    spawn_node_in_with_load(dir, executor, load).await
}

/// [`spawn_node_with_load`] on `dir`, which may already hold a key, a store
/// and an inventory.
pub async fn spawn_node_in_with_load(
    dir: tempfile::TempDir,
    executor: impl AgentExecutor,
    load: Arc<dyn LoadProbe>,
) -> TestNode {
    let opts = NodeOptions {
        load: Some(load),
        ..NodeOptions::default()
    };
    spawn_node_in_with_options(dir, executor, opts).await
}

/// Shuts `node` down and waits until nothing holds its store any more, as
/// when its process exits, so that a node spawned on its data directory
/// next opens the database alone.
pub async fn shut_down(node: Node) {
    let store = Arc::downgrade(&node.store);
    node.shutdown().await.expect("shut the node down");
    wait_for_store_to_close(&store).await;
}

/// Waits up to 30 s for every handle on `store` to be gone.
pub async fn wait_for_store_to_close(store: &std::sync::Weak<pals_core::store::Store>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while store.strong_count() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the store outlived its node: {} handles",
            store.strong_count()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The text of the error a [`CountingExecutor::failing`] run ends with.
pub const WORKER_CRASHED: &str = "the worker crashed";

/// The echo executor, counting how many times it runs, that takes `delay`
/// between its first event (the task in `WORKING`) and the rest, or, when
/// [gated](CountingExecutor::gated), waits there until the test
/// [releases](CountingExecutor::release) it; or, [busy while
/// held](CountingExecutor::busy_while_held), refuses every task while the
/// test [holds](CountingExecutor::hold) it. Clones share the count, the
/// gate and the hold.
#[derive(Clone)]
pub struct CountingExecutor {
    runs: Arc<AtomicUsize>,
    delay: Duration,
    fail: bool,
    /// Closed, it lets every run go on.
    gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Set, every task is `REJECTED` as `busy` without running.
    busy: Option<Arc<std::sync::atomic::AtomicBool>>,
}

/// The reason a [`CountingExecutor::busy_while_held`] refuses with, the
/// full worker's (`WorkerExecutor`).
pub const BUSY: &str = "busy";

impl CountingExecutor {
    pub fn new(delay: Duration) -> Self {
        Self {
            runs: Arc::new(AtomicUsize::new(0)),
            delay,
            fail: false,
            gate: None,
            busy: None,
        }
    }

    /// An executor that, while [held](CountingExecutor::hold), ends every
    /// task `REJECTED` with the reason [`BUSY`] right after its first event,
    /// as a worker with every place taken does, without counting a run;
    /// and, while [released](CountingExecutor::release), as at first,
    /// completes them.
    pub fn busy_while_held() -> Self {
        Self {
            busy: Some(Arc::new(std::sync::atomic::AtomicBool::new(false))),
            ..Self::new(Duration::ZERO)
        }
    }

    /// Makes a [busy while held](CountingExecutor::busy_while_held)
    /// executor refuse every task from now on.
    pub fn hold(&self) {
        if let Some(busy) = &self.busy {
            busy.store(true, Ordering::SeqCst);
        }
    }

    /// An executor whose runs, once the task is `WORKING`, wait until
    /// [`CountingExecutor::release`].
    pub fn gated() -> Self {
        Self {
            gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
            ..Self::new(Duration::ZERO)
        }
    }

    /// Lets every run of a [gated](CountingExecutor::gated) executor go on,
    /// those waiting and those to come; makes a [busy while
    /// held](CountingExecutor::busy_while_held) one take tasks again.
    pub fn release(&self) {
        if let Some(gate) = &self.gate {
            gate.close();
        }
        if let Some(busy) = &self.busy {
            busy.store(false, Ordering::SeqCst);
        }
    }

    /// An executor whose runs fail right after their first event: the task
    /// in `WORKING`, then an error, [`WORKER_CRASHED`].
    pub fn failing() -> Self {
        Self {
            fail: true,
            ..Self::new(Duration::ZERO)
        }
    }

    /// How many times `execute` has been called.
    pub fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

impl AgentExecutor for CountingExecutor {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        if self
            .busy
            .as_ref()
            .is_some_and(|busy| busy.load(Ordering::SeqCst))
        {
            let (task_id, context_id) = ctx.task_info();
            let mut reason = a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text(BUSY)]);
            reason.task_id = Some(task_id.clone());
            reason.context_id = Some(context_id.clone());
            let rejected = StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
                task_id,
                context_id,
                status: a2a::TaskStatus {
                    state: a2a::TaskState::Rejected,
                    message: Some(reason),
                    timestamp: None,
                },
                metadata: None,
            });
            return Box::pin(
                EchoExecutor
                    .execute(ctx)
                    .take(1)
                    .chain(stream::iter([Ok(rejected)])),
            );
        }
        self.runs.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            let crashed = Err(A2AError::internal(WORKER_CRASHED));
            return Box::pin(
                EchoExecutor
                    .execute(ctx)
                    .take(1)
                    .chain(stream::iter([crashed])),
            );
        }
        let (delay, gate) = (self.delay, self.gate.clone());
        Box::pin(
            EchoExecutor
                .execute(ctx)
                .enumerate()
                .then(move |(n, event)| {
                    let gate = gate.clone();
                    async move {
                        if n == 1 {
                            if let Some(gate) = gate {
                                // Closed once released: no permit is ever
                                // given.
                                let _ = gate.acquire().await;
                            }
                            tokio::time::sleep(delay).await;
                        }
                        event
                    }
                }),
        )
    }

    fn cancel(&self, ctx: ExecutorContext) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        EchoExecutor.cancel(ctx)
    }
}

/// A stub peer with `key`'s identity, serving `app` through the tunnel as a
/// node would, saved in `a` as contact `name` with its address, and a grant
/// of it to send, read and cancel tasks that `a` holds. It keeps running
/// until the returned router drops.
pub async fn stub_peer(a: &TestNode, name: &str, key: &AgentKey, app: axum::Router) -> Router {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("loopback bind address")
        .bind()
        .await
        .expect("bind the stub's endpoint");
    let socket = *endpoint.bound_sockets().first().expect("a bound socket");
    let ticket = EndpointTicket::new(EndpointAddr::new(endpoint.id()).with_ip_addr(socket));
    let now = now_unix();
    let ops = [
        Operation::TaskSend,
        Operation::TaskRead,
        Operation::TaskCancel,
    ];
    a.hold(&issue_grant(key, a.id(), key.id(), &ops, now, now + 3600));
    a.node
        .store
        .contact_put(&Contact {
            name: name.to_owned(),
            agent_id: key.id(),
            ticket: Some(ticket.to_string()),
            added_at: now,
        })
        .expect("save the stub");
    Router::builder(endpoint)
        .accept(
            ALPN_A2A_HTTP,
            TunnelServer::new(app, TunnelLimits::default()),
        )
        .spawn()
}

/// What a [`stub_discovery_peer`] does with each request it reads.
pub enum StubMode {
    /// Holds the stream until the test notifies (`notify_waiters`, for every
    /// stream held then, or `notify_one`), then ends it without an answer.
    Hold(Arc<Notify>),
    /// Keeps the request and answers the response given. Its contact and
    /// announce records, when left empty, are the stub's own, valid now.
    Record(Arc<Mutex<Vec<wire::Request>>>, wire::Response),
    /// Answers each request with the bytes the function makes of it: all
    /// that the stub sends on the stream before it ends it, frames and
    /// anything else ([`stub_response`] for one response frame).
    Answer(StubAnswer),
    /// Reads the request and never answers.
    Silent,
}

/// What a [`StubMode::Answer`] stub sends on a stream, from what it was
/// asked.
pub type StubAnswer = Arc<dyn Fn(&StubAsked<'_>) -> Vec<u8> + Send + Sync>;

/// What a [`StubMode::Answer`] stub answers from: its own key and signed
/// records, the agent whose connection asks, and the request, when it is
/// one.
pub struct StubAsked<'a> {
    pub key: &'a AgentKey,
    /// The stub's signed contact and announce records, valid now.
    pub records: &'a (Vec<u8>, Vec<u8>),
    pub requester: AgentId,
    pub request: Option<&'a wire::Request>,
}

/// One response frame of `asked`'s stub, with `result`, its limits and its
/// own records.
pub fn stub_response(asked: &StubAsked<'_>, result: wire::response::Result) -> Vec<u8> {
    encode_frame(&wire::Response {
        version: DISCOVERY_VERSION,
        limits: Some(server_limits()),
        contact_record: asked.records.0.clone(),
        announce_record: asked.records.1.clone(),
        result: Some(result),
    })
}

/// A peer that serves only `agentnet-discovery/0`, as the test scripts it
/// ([`StubMode`]). It never serves the A2A binding: a connection on
/// [`ALPN_A2A_HTTP`] is only counted and closed. It runs until dropped.
pub struct StubPeer {
    pub key: AgentKey,
    pub id: AgentId,
    addr: EndpointAddr,
    seen: Arc<StubSeen>,
    /// Keeps accepting connections until dropped.
    router: Router,
}

/// What a stub peer has seen.
#[derive(Debug)]
struct StubSeen {
    /// Connections accepted, on either ALPN.
    connections: AtomicUsize,
    /// Requests read whole.
    requests: watch::Sender<usize>,
}

impl StubPeer {
    /// Where the stub is dialed.
    pub fn addr(&self) -> EndpointAddr {
        self.addr.clone()
    }

    /// The connections made to the stub so far, on either ALPN.
    pub fn connections(&self) -> usize {
        self.seen.connections.load(Ordering::SeqCst)
    }

    /// The requests the stub has read whole so far.
    pub fn requests(&self) -> usize {
        *self.seen.requests.borrow()
    }

    /// Waits until the stub has read `count` requests, for up to 60 s.
    pub async fn wait_for_requests(&self, count: usize) {
        let mut seen = self.seen.requests.subscribe();
        tokio::time::timeout(Duration::from_secs(60), seen.wait_for(|n| *n >= count))
            .await
            .expect("the stub peer read the requests in time")
            .expect("the stub peer is alive");
    }
}

/// The stub's discovery side.
#[derive(Clone)]
struct StubDiscovery {
    mode: Arc<StubMode>,
    seen: Arc<StubSeen>,
    key: Arc<AgentKey>,
    /// The stub's own signed contact and announce records.
    records: Arc<(Vec<u8>, Vec<u8>)>,
}

impl std::fmt::Debug for StubDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StubDiscovery")
    }
}

impl ProtocolHandler for StubDiscovery {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.seen.connections.fetch_add(1, Ordering::SeqCst);
        let requester = agent_id_of(&connection.remote_id());
        let mut streams = tokio::task::JoinSet::new();
        while let Ok((send, recv)) = connection.accept_bi().await {
            let stub = self.clone();
            streams.spawn(async move { stub.serve(requester, send, recv).await });
        }
        Ok(())
    }
}

impl StubDiscovery {
    async fn serve(&self, requester: AgentId, mut send: SendStream, mut recv: RecvStream) {
        let mut prefix = [0u8; 4];
        if recv.read_exact(&mut prefix).await.is_err() {
            return;
        }
        let Ok(len) = frame_len(prefix, MAX_REQUEST_FRAME_BYTES) else {
            return;
        };
        let mut body = vec![0u8; len];
        if recv.read_exact(&mut body).await.is_err() {
            return;
        }
        let request = decode::<wire::Request>(&body);
        self.seen.requests.send_modify(|n| *n += 1);
        match self.mode.as_ref() {
            StubMode::Hold(release) => {
                release.notified().await;
                let _ = send.finish();
            }
            StubMode::Record(kept, response) => {
                if let Ok(request) = request {
                    kept.lock().unwrap().push(request);
                }
                let mut response = response.clone();
                if response.contact_record.is_empty() {
                    response.contact_record = self.records.0.clone();
                }
                if response.announce_record.is_empty() {
                    response.announce_record = self.records.1.clone();
                }
                let _ = send.write_all(&encode_frame(&response)).await;
                let _ = send.finish();
                // Until the requester has read it all.
                let _ = send.stopped().await;
            }
            StubMode::Answer(answer) => {
                let asked = StubAsked {
                    key: &self.key,
                    records: &self.records,
                    requester,
                    request: request.as_ref().ok(),
                };
                let _ = send.write_all(&answer(&asked)).await;
                let _ = send.finish();
                let _ = send.stopped().await;
            }
            StubMode::Silent => std::future::pending::<()>().await,
        }
    }
}

/// Counts the connections made on the A2A binding, and closes them.
#[derive(Clone, Debug)]
struct StubRefusal(Arc<StubSeen>);

impl ProtocolHandler for StubRefusal {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.0.connections.fetch_add(1, Ordering::SeqCst);
        connection.close(0u32.into(), b"not served");
        Ok(())
    }
}

/// A stub peer that serves only `agentnet-discovery/0` as `mode` says, saved
/// in `a` as contact `name` with its address, and a grant of it to send,
/// read and cancel tasks that `a` holds, as [`stub_peer`] does.
pub async fn stub_discovery_peer(a: &TestNode, name: &str, mode: StubMode) -> StubPeer {
    stub_discovery_peer_with_key(a, name, AgentKey::generate(), mode).await
}

/// [`stub_discovery_peer`] with `key`'s identity, so that the test can sign
/// what the stub answers before it runs.
pub async fn stub_discovery_peer_with_key(
    a: &TestNode,
    name: &str,
    key: AgentKey,
    mode: StubMode,
) -> StubPeer {
    spawn_stub(a, name, key, mode, None, true).await
}

/// What a stub peer serves on the A2A binding: a card, and the hash its
/// announce record names.
#[derive(Debug, Clone)]
pub struct StubCard {
    /// Served, exactly as given, at `/.well-known/agent-card.json` and at
    /// `/rest/extendedAgentCard`.
    pub json: Value,
    /// The `agent_card_hash` of the stub's announce record.
    pub announce_hash: pals_core::card_jws::CardHash,
    /// Every other request on the A2A binding, a task included, is
    /// answered `503`, as by an agent that cannot take any: the gateway
    /// passes it on, and an ask counts it as a peer out of reach.
    pub tasks_unavailable: bool,
}

/// The public card of an agent with `key`'s identity and the inventory
/// `inventory_toml`, signed by `key` as a node signs its own, with the
/// announce record naming it as a node's does.
pub fn stub_card(key: &AgentKey, inventory_toml: &str) -> StubCard {
    let inventory = pals_core::inventory::Inventory::from_toml(inventory_toml)
        .expect("a valid inventory for the stub");
    let cfg = test_config(Path::new("/nonexistent-stub"), WorkerCfg::Echo);
    let signer =
        pals_core::card_jws::CardSigner::new(Arc::new(AgentKey::from_bytes(&key.to_bytes())));
    let cards = pals::a2a::CardSet::build(&cfg, &signer, &inventory).expect("sign the stub's card");
    StubCard {
        json: cards.public().json.clone(),
        announce_hash: cards.public().hash.clone(),
        tasks_unavailable: false,
    }
}

/// A stub peer that serves `agentnet-discovery/0` as `mode` says and, on
/// the A2A binding, only `card` (public and extended): a candidate a search
/// reads the card of and then asks. Saved in `a` as contact `name` with its
/// address, and, when `grant`, with a grant of it to send, read and cancel
/// tasks that `a` holds, as [`stub_peer`] does.
pub async fn stub_discovery_peer_with_card(
    a: &TestNode,
    name: &str,
    key: AgentKey,
    mode: StubMode,
    card: StubCard,
    grant: bool,
) -> StubPeer {
    spawn_stub(a, name, key, mode, Some(card), grant).await
}

/// The stub of [`stub_discovery_peer_with_key`], serving `card` on the A2A
/// binding when given, and only counting and closing connections there
/// otherwise, with a grant held in `a` when `grant`.
async fn spawn_stub(
    a: &TestNode,
    name: &str,
    key: AgentKey,
    mode: StubMode,
    card: Option<StubCard>,
    grant: bool,
) -> StubPeer {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("loopback bind address")
        .bind()
        .await
        .expect("bind the stub's endpoint");
    let socket = *endpoint.bound_sockets().first().expect("a bound socket");
    let addr = EndpointAddr::new(endpoint.id()).with_ip_addr(socket);
    let id = key.id();
    let now = now_unix();
    let contact = ContactRecord {
        schema: CONTACT_SCHEMA.to_owned(),
        agent_id: id,
        iroh: IrohContact {
            endpoint_id: id,
            relay_url: None,
            direct_addrs: vec![socket.to_string()],
        },
        seq: 1,
        issued_at: format_timestamp(now).expect("a timestamp"),
        expires_at: format_timestamp(now + RECORD_TTL_S).expect("a timestamp"),
    };
    let announce = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: id,
        seq: 1,
        agent_card_hash: card.as_ref().map_or_else(
            || {
                format!("blake3:{}", "0".repeat(64))
                    .parse()
                    .expect("a card hash")
            },
            |card| card.announce_hash.clone(),
        ),
        capabilities: Vec::new(),
        discovery_alpn: "agentnet-discovery/0".to_owned(),
        issued_at: format_timestamp(now).expect("a timestamp"),
        expires_at: format_timestamp(now + RECORD_TTL_S).expect("a timestamp"),
    };
    let records = (
        Signed::issue(&key, contact)
            .expect("sign the stub's contact")
            .to_json(),
        Signed::issue(&key, announce)
            .expect("sign the stub's announce")
            .to_json(),
    );
    let seen = Arc::new(StubSeen {
        connections: AtomicUsize::new(0),
        requests: watch::Sender::new(0),
    });
    let discovery = StubDiscovery {
        mode: Arc::new(mode),
        seen: Arc::clone(&seen),
        key: Arc::new(AgentKey::from_bytes(&key.to_bytes())),
        records: Arc::new(records),
    };
    let ops = [
        Operation::TaskSend,
        Operation::TaskRead,
        Operation::TaskCancel,
    ];
    if grant {
        a.hold(&issue_grant(&key, a.id(), id, &ops, now, now + 3600));
    }
    a.node
        .store
        .contact_put(&Contact {
            name: name.to_owned(),
            agent_id: id,
            ticket: Some(EndpointTicket::new(addr.clone()).to_string()),
            added_at: now,
        })
        .expect("save the stub");
    let router = Router::builder(endpoint).accept(ALPN_DISCOVERY, discovery);
    let router = match card {
        Some(card) => {
            let json = Arc::new(card.json);
            let served = move || {
                let json = Arc::clone(&json);
                async move { Json((*json).clone()) }
            };
            let app = axum::Router::new()
                .route("/.well-known/agent-card.json", get(served.clone()))
                .route("/rest/extendedAgentCard", get(served));
            let app = if card.tasks_unavailable {
                app.fallback(|| async {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": "unavailable"})),
                    )
                })
            } else {
                app
            };
            router.accept(
                ALPN_A2A_HTTP,
                TunnelServer::new(app, TunnelLimits::default()),
            )
        }
        None => router.accept(ALPN_A2A_HTTP, StubRefusal(Arc::clone(&seen))),
    }
    .spawn();
    // Its connections come from whoever dials it; this one is its own.
    debug_assert_eq!(agent_id_of(&router.endpoint().id()), id);
    StubPeer {
        key,
        id,
        addr,
        seen,
        router,
    }
}

/// The tasks `node` serves that are in `state`, as its store has them.
pub fn tasks_in(node: &TestNode, state: a2a::TaskState) -> Vec<a2a::Task> {
    let request = a2a::ListTasksRequest {
        context_id: None,
        status: Some(state),
        page_size: Some(100),
        page_token: None,
        history_length: Some(0),
        status_timestamp_after: None,
        include_artifacts: None,
        tenant: None,
    };
    node.node
        .store
        .tasks_list(&request)
        .expect("list the tasks")
        .tasks
}

/// The key of the agent whose data directory is `dir`, created if missing.
pub fn key_in(dir: &Path) -> AgentKey {
    AgentKey::load_or_create(&dir.join(KEY_FILE)).expect("load the agent key")
}

/// Unix seconds now.
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A grant of `issuer_key` that lets `subject` perform `ops` on `skill:ask`
/// of project `demo` against `audience`, valid from `issued_at` to
/// `expires_at`. Not stored anywhere.
pub fn issue_grant(
    issuer_key: &AgentKey,
    subject: AgentId,
    audience: AgentId,
    ops: &[Operation],
    issued_at: i64,
    expires_at: i64,
) -> SignedGrant {
    static NEXT_GRANT: AtomicU64 = AtomicU64::new(1);
    let grant = Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: format!("g-{}", NEXT_GRANT.fetch_add(1, Ordering::Relaxed)),
        issuer: issuer_key.id(),
        subject,
        audience,
        project: "demo".to_owned(),
        resources: vec!["skill:ask".to_owned()],
        operations: ops.to_vec(),
        issued_at: format_timestamp(issued_at).expect("a timestamp"),
        expires_at: format_timestamp(expires_at).expect("a timestamp"),
    };
    SignedGrant::issue(issuer_key, grant).expect("issue the grant")
}

/// A grant of `issuer` that lets `subject` perform `ops` on `skill:ask` of
/// project `demo` against the issuer, from now for `ttl_s` seconds (expired
/// already when negative), recorded as issued in the issuer's store.
pub fn grant(issuer: &TestNode, subject: &TestNode, ops: &[Operation], ttl_s: i64) -> SignedGrant {
    let now = now_unix();
    let g = issue_grant(
        &key_in(issuer.dir.path()),
        subject.id(),
        issuer.id(),
        ops,
        now,
        now + ttl_s,
    );
    issuer
        .node
        .store
        .grant_issued_put(&g)
        .expect("store the issued grant");
    g
}

/// A JSON-RPC 2.0 request with id 1.
pub fn rpc_request(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

/// `params` of a `SendMessage` or `SendStreamingMessage` with one text part
/// and a fresh `messageId`, continuing `task_id` when given.
pub fn message_params(text: &str, task_id: Option<&str>) -> Value {
    static NEXT_MESSAGE: AtomicU64 = AtomicU64::new(1);
    let mut message = json!({
        "messageId": format!("m-{}", NEXT_MESSAGE.fetch_add(1, Ordering::Relaxed)),
        "role": "ROLE_USER",
        "parts": [{"text": text}],
    });
    if let Some(task_id) = task_id {
        message["taskId"] = json!(task_id);
    }
    json!({ "message": message })
}

impl TestNode {
    pub fn id(&self) -> AgentId {
        self.node.id
    }

    /// The only directory this node's `pals_fetch` writes to, which it
    /// creates on first use.
    pub fn download_dir(&self) -> std::path::PathBuf {
        download_dir_in(&self.downloads)
    }

    /// The URL of this node's loopback gateway, `http://127.0.0.1:<port>`.
    pub fn gateway_url(&self) -> String {
        self.node.gateway_url()
    }

    /// Saves `peer` as the contact `name`, with its ticket, as `pals contact
    /// add` does.
    pub fn add_contact(&self, name: &str, peer: &TestNode) {
        let contact = Contact {
            name: name.to_owned(),
            agent_id: peer.id(),
            ticket: Some(peer.node.ticket()),
            added_at: now_unix(),
        };
        self.node
            .store
            .contact_put(&contact)
            .expect("save the contact");
    }

    /// Keeps `g`, a grant issued to this node, as `pals grant import` does.
    pub fn hold(&self, g: &SignedGrant) {
        self.node
            .store
            .grant_held_put(g)
            .expect("store the held grant");
    }

    /// Sends `request` from this node to `to` through this node's
    /// `PeerHttp`, and returns the response with its body still streaming.
    pub async fn send(&self, to: &TestNode, request: Request<Body>) -> Response<Body> {
        self.node
            .peer_http
            .send(to.node.addr(), request)
            .await
            .expect("tunnel request")
    }

    /// `method path` to `to` with `headers` and an optional JSON body, read
    /// to the end.
    pub async fn request(
        &self,
        to: &TestNode,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> TestResponse {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(body).expect("serializable body"))
            }
            None => Body::empty(),
        };
        let request = request.body(body).expect("valid request");
        TestResponse::read(self.send(to, request).await).await
    }

    /// `GET path` to `to` with `A2A-Version: 1.0`, presenting `grant` when
    /// given.
    pub async fn get(
        &self,
        to: &TestNode,
        path: &str,
        grant: Option<&SignedGrant>,
    ) -> TestResponse {
        let bearer = grant.map(bearer);
        let mut headers = vec![("a2a-version", "1.0")];
        if let Some(value) = &bearer {
            headers.push(("authorization", value.as_str()));
        }
        self.request(to, "GET", path, &headers, None).await
    }

    /// A JSON-RPC call to `to` at `/jsonrpc` with `A2A-Version: 1.0`,
    /// presenting `grant` when given.
    pub async fn rpc(
        &self,
        to: &TestNode,
        grant: Option<&SignedGrant>,
        method: &str,
        params: Value,
    ) -> TestResponse {
        self.rpc_with(to, grant, method, params, &[]).await
    }

    /// [`TestNode::rpc`], activating the idempotency extension with
    /// `A2A-Extensions`.
    pub async fn rpc_idempotent(
        &self,
        to: &TestNode,
        grant: Option<&SignedGrant>,
        method: &str,
        params: Value,
    ) -> TestResponse {
        let extension = [("a2a-extensions", IDEMPOTENCY_EXTENSION)];
        self.rpc_with(to, grant, method, params, &extension).await
    }

    /// [`TestNode::rpc`] with `extra` headers.
    pub async fn rpc_with(
        &self,
        to: &TestNode,
        grant: Option<&SignedGrant>,
        method: &str,
        params: Value,
        extra: &[(&str, &str)],
    ) -> TestResponse {
        let bearer = grant.map(bearer);
        let mut headers = vec![("a2a-version", "1.0")];
        if let Some(value) = &bearer {
            headers.push(("authorization", value.as_str()));
        }
        headers.extend_from_slice(extra);
        let body = rpc_request(method, params);
        self.request(to, "POST", "/jsonrpc", &headers, Some(&body))
            .await
    }

    /// Sends `text` to `to` as a new task with `SendMessage` and returns the
    /// task it answered with.
    pub async fn ask(&self, to: &TestNode, grant: &SignedGrant, text: &str) -> Value {
        let res = self
            .rpc(to, Some(grant), "SendMessage", message_params(text, None))
            .await;
        assert_eq!(res.status, 200, "{}", res.json);
        let task = res.json["result"]["task"].clone();
        assert!(task["id"].is_string(), "{}", res.json);
        task
    }
}

/// The `Authorization` value that presents `grant`.
pub fn bearer(grant: &SignedGrant) -> String {
    format!("Bearer {}", grant.to_token())
}

/// The `result` of every JSON-RPC response in the `data:` lines of an SSE
/// body, in order; a response that carries an `error` instead fails the test.
pub fn sse_results(res: &TestResponse) -> Vec<Value> {
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    String::from_utf8_lossy(&res.body)
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| {
            let response: Value = serde_json::from_str(data.trim()).expect("an SSE event is JSON");
            assert!(
                response.get("error").is_none(),
                "an SSE event is an error: {response}"
            );
            response["result"].clone()
        })
        .collect()
}

/// Sends `method path` to the gateway at `base` (`http://<host>:<port>`) over
/// a new TCP connection, with `headers` and an optional JSON body, and
/// returns the response with its body still streaming. `Host` is the base's
/// `<host>:<port>` unless `headers` sets one.
pub async fn gateway_send(
    base: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> Response<Body> {
    let body = body.map(|body| {
        (
            "application/json",
            serde_json::to_vec(body).expect("serializable body"),
        )
    });
    gateway_send_raw(base, method, path, headers, body).await
}

/// [`gateway_send`] with a body of any type: `(content type, bytes)`.
pub async fn gateway_send_raw(
    base: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, Vec<u8>)>,
) -> Response<Body> {
    let authority = base.strip_prefix("http://").expect("an http:// base URL");
    let stream = TcpStream::connect(authority)
        .await
        .expect("connect to the gateway");
    let (mut sender, connection) = client_http1::handshake(TokioIo::new(stream))
        .await
        .expect("HTTP/1.1 handshake with the gateway");
    tokio::spawn(connection);
    let mut request = Request::builder().method(method).uri(path);
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        request = request.header(header::HOST, authority);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = match body {
        Some((content_type, bytes)) => {
            request = request.header(header::CONTENT_TYPE, content_type);
            Body::from(bytes)
        }
        None => Body::empty(),
    };
    let request = request.body(body).expect("valid request");
    sender
        .send_request(request)
        .await
        .expect("a response from the gateway")
        .map(Body::new)
}

/// [`gateway_send`], with the response read to the end.
pub async fn gateway_request(
    base: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> TestResponse {
    TestResponse::read(gateway_send(base, method, path, headers, body).await).await
}

// --- The DHT: what dht_publish and dht_search share ------------------------

/// How long a DHT test waits for the network to reach the state it needs:
/// past it, the test fails.
pub const CONVERGES: Duration = Duration::from_secs(60);

/// The pause between two lookups a test repeats until one finds what it
/// needs: well under a router's 600 streams a minute from one peer, beside
/// the rest of the test's traffic.
pub const LOOKUP_PAUSE: Duration = Duration::from_secs(2);

/// `node`'s DHT.
pub fn dht(node: &TestNode) -> &Dht {
    node.node.dht().expect("the node's DHT is on")
}

/// Polls `dht`'s status until `done`, and returns it; fails, saying `what`,
/// after [`CONVERGES`].
pub async fn wait_for_dht(dht: &Dht, what: &str, done: impl Fn(&DhtStatus) -> bool) -> DhtStatus {
    let deadline = std::time::Instant::now() + CONVERGES;
    loop {
        let now = dht.status().await.expect("the DHT runs");
        if done(&now) {
            return now;
        }
        assert!(std::time::Instant::now() < deadline, "{what}: {now:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// [`wait_for_dht`] on `node`'s DHT.
pub async fn wait_for(node: &TestNode, what: &str, done: impl Fn(&DhtStatus) -> bool) -> DhtStatus {
    wait_for_dht(dht(node), what, done).await
}

/// How many values and provider records of `agent` a router's `status`
/// counts.
pub fn held(status: &DhtStatus, agent: AgentId) -> u32 {
    status.stored_by_agent.get(&agent).copied().unwrap_or(0)
}

/// `key`'s contact with `seq`, valid now, naming `direct`.
pub fn contact(key: &AgentKey, seq: u64, direct: &[SocketAddr]) -> Vec<u8> {
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

/// The card hash of an announce whose card no test reads.
pub fn any_card_hash() -> CardHash {
    format!("blake3:{}", "ab".repeat(32))
        .parse::<CardHash>()
        .unwrap()
}

/// `key`'s announce with `seq`, of the card `card_hash`, listing
/// `capabilities`, valid now.
pub fn announce(key: &AgentKey, seq: u64, card_hash: &CardHash, capabilities: &[&str]) -> Vec<u8> {
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

/// Where a [`TestRecords::of`] says its agent is.
pub fn somewhere() -> SocketAddr {
    SocketAddr::from(([192, 0, 2, 10], 4433))
}

/// Records a test publishes as it likes: those it gives, and each
/// [`TestRecords::set_contact`] a new change.
pub struct TestRecords {
    current: Mutex<(Vec<u8>, Vec<u8>)>,
    capabilities: Vec<String>,
    changes: watch::Sender<u64>,
}

impl TestRecords {
    pub fn new(contact: Vec<u8>, announce: Vec<u8>, capabilities: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            current: Mutex::new((contact, announce)),
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            changes: watch::channel(0).0,
        })
    }

    /// `key`'s contact and announce of `seq` 1, listing `capabilities`.
    pub fn of(key: &AgentKey, capabilities: &[&str]) -> Arc<Self> {
        Self::new(
            contact(key, 1, &[somewhere()]),
            announce(key, 1, &any_card_hash(), capabilities),
            capabilities,
        )
    }

    /// Puts `contact` in force, as a record issued again.
    pub fn set_contact(&self, contact: Vec<u8>) {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0 = contact;
        self.bump();
    }

    /// Says the records changed.
    pub fn bump(&self) {
        self.changes.send_modify(|n| *n += 1);
    }
}

impl PublishedRecords for TestRecords {
    fn current(&self) -> (Vec<u8>, Vec<u8>) {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
pub struct Publisher {
    pub dht: Dht,
    _router: Router,
}

impl Publisher {
    /// Starts it on a loopback endpoint with `key`, entering through
    /// `routers`, with `timing` and `clock`.
    pub async fn spawn(
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
            store: Arc::new(pals_core::store::Store::open_in_memory(key.id()).unwrap()),
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

/// Runs `pals --home <home> <args>`, off the async threads: its exit code,
/// stdout and stderr.
pub async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
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
