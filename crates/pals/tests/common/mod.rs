//! Shared helpers for the integration tests: Iroh endpoints on loopback with
//! no relay, an echo agent served through the tunnel, and a client that goes
//! through the production `PeerHttp`.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::net::Ipv4Addr;
use std::sync::Arc;

use a2a::{
    AgentCapabilities, AgentCard, AgentInterface, AgentSkill, TRANSPORT_PROTOCOL_HTTP_JSON,
    TRANSPORT_PROTOCOL_JSONRPC,
};
use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::rest::rest_router;
use a2a_server::{DefaultRequestHandler, InMemoryTaskStore, StaticAgentCard};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, Response, header};
use axum::routing::post;
use http_body_util::BodyExt;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode};
use pals::net::{ALPN_A2A_HTTP, PEER_HEADER, PeerHttp, TunnelLimits, TunnelServer};
use pals::worker::echo::EchoExecutor;
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
