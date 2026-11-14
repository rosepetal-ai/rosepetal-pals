//! Shared helpers for the integration tests: Iroh endpoints on loopback with
//! no relay, an echo agent served through the tunnel, a client that goes
//! through the production `PeerHttp`, complete PALS nodes ([`TestNode`])
//! with the grants that let them talk to each other, and an executor that
//! counts its runs ([`CountingExecutor`]).

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
use axum::routing::post;
use futures::StreamExt;
use futures::stream::BoxStream;
use http_body_util::BodyExt;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode};
use pals::config::{AgentCfg, Config, GatewayCfg, KEY_FILE, LimitsCfg, NetCfg, WorkerCfg};
use pals::net::{ALPN_A2A_HTTP, PEER_HEADER, PeerHttp, TunnelLimits, TunnelServer};
use pals::node::Node;
use pals::worker::echo::EchoExecutor;
use pals_core::grant::{GRANT_SCHEMA, Grant, Operation, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use serde_json::{Value, json};

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
    async fn read(response: Response<Body>) -> Self {
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
    let server_endpoint = loopback_endpoint().await;
    let socket = *server_endpoint
        .bound_sockets()
        .first()
        .expect("the endpoint is bound to a socket");
    let addr = EndpointAddr::new(server_endpoint.id()).with_ip_addr(socket);
    let router = Router::builder(server_endpoint)
        .accept(
            ALPN_A2A_HTTP,
            TunnelServer::new(app, TunnelLimits::default()),
        )
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

async fn loopback_endpoint() -> Endpoint {
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

/// A complete PALS node on loopback with no relay, and the data directory it
/// runs on, removed when this value is dropped.
pub struct TestNode {
    pub node: Node,
    pub dir: tempfile::TempDir,
}

/// The configuration a [`TestNode`] runs with on `data_dir`: the defaults,
/// `worker`, and no relay, so the node binds to 127.0.0.1 only.
pub fn test_config(data_dir: &Path, worker: WorkerCfg) -> Config {
    Config {
        data_dir: data_dir.to_path_buf(),
        agent: AgentCfg::default(),
        gateway: GatewayCfg::default(),
        worker,
        projects: BTreeMap::new(),
        limits: LimitsCfg::default(),
        net: NetCfg { relay: false },
        trusted_issuers: Vec::new(),
    }
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
    let mut cfg = test_config(dir.path(), worker);
    edit(&mut cfg);
    let node = Node::spawn(cfg).await.expect("spawn the node");
    TestNode { node, dir }
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
    let cfg = test_config(dir.path(), WorkerCfg::Echo);
    let node = Node::spawn_with_executor(cfg, executor)
        .await
        .expect("spawn the node");
    TestNode { node, dir }
}

/// The echo executor, counting how many times it runs, that takes `delay`
/// between its first event (the task in `WORKING`) and the rest. Clones
/// share the count.
#[derive(Clone)]
pub struct CountingExecutor {
    runs: Arc<AtomicUsize>,
    delay: Duration,
}

impl CountingExecutor {
    pub fn new(delay: Duration) -> Self {
        Self {
            runs: Arc::new(AtomicUsize::new(0)),
            delay,
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
        self.runs.fetch_add(1, Ordering::SeqCst);
        let delay = self.delay;
        Box::pin(
            EchoExecutor
                .execute(ctx)
                .enumerate()
                .then(move |(n, event)| async move {
                    if n == 1 {
                        tokio::time::sleep(delay).await;
                    }
                    event
                }),
        )
    }

    fn cancel(&self, ctx: ExecutorContext) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        EchoExecutor.cancel(ctx)
    }
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

    /// `GET path` to `to`, presenting `grant` when given.
    pub async fn get(
        &self,
        to: &TestNode,
        path: &str,
        grant: Option<&SignedGrant>,
    ) -> TestResponse {
        let bearer = grant.map(bearer);
        let headers: Vec<(&str, &str)> = bearer
            .iter()
            .map(|value| ("authorization", value.as_str()))
            .collect();
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
        let bearer = grant.map(bearer);
        let mut headers = vec![("a2a-version", "1.0")];
        if let Some(value) = &bearer {
            headers.push(("authorization", value.as_str()));
        }
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
