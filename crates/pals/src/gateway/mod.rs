//! The local gateway: the standard A2A entry to every peer.
//!
//! It listens on loopback only. A stock A2A client reaches any PALS peer at
//! `<gateway>/peers/<peer>`, where `<peer>` is a contact name or an agent id,
//! with the standard bindings:
//!
//! | Route | Behavior |
//! |-|-|
//! | `GET /healthz` | `200 {"agent_id", "name", "ticket"}` |
//! | `GET /peers/{peer}/.well-known/agent-card.json` | the peer's card, as [`gateway_view`] |
//! | `POST /peers/{peer}/jsonrpc` | the peer's `/jsonrpc` |
//! | any method, `/peers/{peer}/rest/{*rest}` | the peer's `/rest/{rest}` |
//!
//! A request to a peer travels through [`PeerHttp`], never to the node's own
//! app: method, path after `/peers/{peer}` and query exactly as received,
//! end-to-end headers and the body, streamed; the response streams back the
//! same way, Server-Sent Events included. The gateway drops the hop-by-hop
//! headers, `Host` and any `x-agentnet-*` header the client sent, which only
//! the peer's tunnel sets. When the request carries no `Authorization`, the
//! gateway presents the newest unexpired grant it holds for the peer, and
//! answers `401 {"error":"no grant held for <agent_id>"}` itself, without
//! contacting the peer, when it holds none; a client's own `Authorization`
//! passes intact.
//!
//! A `<peer>` that is neither a contact nor an agent id gets
//! `404 {"error":"unknown peer"}`; a peer that cannot be reached, within the
//! 10 s that [`PeerHttp`] allows for dialing, `502 {"error":"peer
//! unreachable"}`; and a broken exchange `502 {"error":"peer protocol
//! error"}`.
//!
//! Every request, to any route, first passes the local guard: one with an
//! `Origin` header (a web page) or with a `Host` other than `127.0.0.1`,
//! `localhost` or `[::1]` with the gateway's port (DNS rebinding) gets
//! `403`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use a2a::AgentCard;
use a2a_server::WELL_KNOWN_AGENT_CARD_PATH;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use http::header::{AUTHORIZATION, CONNECTION, HOST, HeaderMap, HeaderName, HeaderValue, ORIGIN};
use http::{StatusCode, Uri};
use http_body_util::{BodyExt, Limited};
use iroh::{Endpoint, EndpointAddr};
use pals_core::identity::AgentId;
use pals_core::store::Store;
use serde_json::json;

use crate::a2a::gateway_view;
use crate::net::{PeerHttp, TunnelError, peer_addr};
use crate::node::ticket_of;

/// The A2A protocol version the gateway asks a peer's card for.
const A2A_VERSION: &str = "1.0";
/// The largest Agent Card the gateway reads from a peer.
const MAX_CARD_BYTES: usize = 1024 * 1024;
/// How long fetching a peer's card may take in all, dialing included.
const CARD_TIMEOUT: Duration = Duration::from_secs(20);
/// The prefix of the headers only PALS sets.
const OWN_HEADER_PREFIX: &str = "x-agentnet-";

/// Headers that describe one connection, not the request (RFC 9110 §7.6.1),
/// and are never forwarded, in either direction.
const HOP_BY_HOP: [&str; 10] = [
    "connection",
    "expect",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// What the gateway works with: this agent, its store (contacts and held
/// grants), its client for peers and the address it listens on.
///
/// Cloning is cheap.
#[derive(Clone)]
pub struct GatewayState {
    inner: Arc<Inner>,
}

struct Inner {
    id: AgentId,
    name: String,
    store: Arc<Store>,
    peer_http: PeerHttp,
    /// This agent's endpoint, whose ticket `/healthz` reports.
    endpoint: Endpoint,
    /// Where the gateway listens.
    local_addr: SocketAddr,
}

impl GatewayState {
    /// The gateway of agent `id`, called `name`, with `store` and
    /// `peer_http`, `endpoint` being the agent's own, listening on
    /// `local_addr`.
    pub fn new(
        id: AgentId,
        name: String,
        store: Arc<Store>,
        peer_http: PeerHttp,
        endpoint: Endpoint,
        local_addr: SocketAddr,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                id,
                name,
                store,
                peer_http,
                endpoint,
                local_addr,
            }),
        }
    }

    /// `http://<address the gateway listens on>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.inner.local_addr)
    }

    /// The agent `peer` names, a contact name or an agent id, and where to
    /// dial it; `None` when it names no one.
    async fn resolve(&self, peer: String) -> Result<Option<Target>, StoreUnavailable> {
        let store = Arc::clone(&self.inner.store);
        blocking(move || {
            let (id, contact) = match peer.parse::<AgentId>() {
                Ok(id) => (id, store.contact_by_id(&id)?),
                Err(_) => match store.contact_by_name(&peer)? {
                    Some(contact) => (contact.agent_id, Some(contact)),
                    None => return Ok(None),
                },
            };
            let ticket = contact.and_then(|contact| contact.ticket);
            let addr = peer_addr(&id, ticket.as_deref());
            Ok(Some(Target { id, addr }))
        })
        .await
    }

    /// `Bearer <token>` of the newest unexpired grant held for `peer`.
    async fn held_bearer(&self, peer: AgentId) -> Result<Option<HeaderValue>, StoreUnavailable> {
        let store = Arc::clone(&self.inner.store);
        let held = blocking(move || {
            let now = chrono::Utc::now().timestamp();
            Ok(store.grants_held_for(&peer, now)?.into_iter().next())
        })
        .await?;
        Ok(held.map(|g| {
            HeaderValue::from_str(&format!("Bearer {}", g.to_token()))
                .expect("a base64url token is a valid header value")
        }))
    }
}

/// A peer, resolved.
struct Target {
    id: AgentId,
    addr: EndpointAddr,
}

/// The gateway's routes, behind the local guard.
pub fn router(state: GatewayState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/peers/{peer}/.well-known/agent-card.json", get(peer_card))
        .route("/peers/{peer}/jsonrpc", post(forward_jsonrpc))
        .route("/peers/{peer}/rest/{*rest}", any(forward_rest))
        .layer(middleware::from_fn_with_state(state.clone(), local_guard))
        .with_state(state)
}

/// Refuses requests from web pages and from names other than loopback's.
async fn local_guard(State(state): State<GatewayState>, request: Request, next: Next) -> Response {
    if request.headers().contains_key(ORIGIN) {
        return error(StatusCode::FORBIDDEN, "requests from web pages are refused");
    }
    if !is_local_host(
        request.headers(),
        request.uri(),
        state.inner.local_addr.port(),
    ) {
        return error(StatusCode::FORBIDDEN, "host not allowed");
    }
    next.run(request).await
}

/// Whether the request names the gateway by a loopback name and its port:
/// exactly one `Host` header, or an authority in the request target, and
/// every one present is `127.0.0.1:<port>`, `localhost:<port>` or
/// `[::1]:<port>`.
fn is_local_host(headers: &HeaderMap, uri: &Uri, port: u16) -> bool {
    let allowed = |host: &str| {
        let host = host.to_ascii_lowercase();
        [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]
        .contains(&host)
    };
    let mut hosts = headers.get_all(HOST).iter();
    let host = match (hosts.next(), hosts.next()) {
        (Some(host), None) => match host.to_str() {
            Ok(host) => Some(host),
            Err(_) => return false,
        },
        (None, _) => None,
        (Some(_), Some(_)) => return false,
    };
    let authority = uri.authority().map(|authority| authority.as_str());
    match (host, authority) {
        (None, None) => false,
        (host, authority) => host.is_none_or(allowed) && authority.is_none_or(allowed),
    }
}

async fn healthz(State(state): State<GatewayState>) -> Response {
    let inner = &state.inner;
    Json(json!({
        "agent_id": inner.id.to_string(),
        "name": inner.name,
        "ticket": ticket_of(&inner.endpoint),
    }))
    .into_response()
}

/// The peer's card as [`gateway_view`], fetched from the peer.
///
/// The card comes from the endpoint whose id is the peer's: the connection
/// is dialed to that id, and Iroh's handshake proves the key at the other
/// end.
async fn peer_card(State(state): State<GatewayState>, Path(peer): Path<String>) -> Response {
    let target = match state.resolve(peer).await {
        Ok(Some(target)) => target,
        Ok(None) => return unknown_peer(),
        Err(failure) => return failure.into_response(),
    };
    let request = http::Request::get(WELL_KNOWN_AGENT_CARD_PATH)
        .header("a2a-version", A2A_VERSION)
        .body(Body::empty())
        .expect("a valid request");
    let fetch = async {
        let response = state.inner.peer_http.send(target.addr, request).await?;
        if !response.status().is_success() {
            return Err(TunnelError::Protocol(format!(
                "the card request got {}",
                response.status()
            )));
        }
        let body = Limited::new(response.into_body(), MAX_CARD_BYTES)
            .collect()
            .await
            .map_err(|error| TunnelError::Protocol(error.to_string()))?
            .to_bytes();
        serde_json::from_slice::<AgentCard>(&body)
            .map_err(|error| TunnelError::Protocol(format!("not an Agent Card: {error}")))
    };
    let card = match tokio::time::timeout(CARD_TIMEOUT, fetch).await {
        Ok(Ok(card)) => card,
        Ok(Err(error)) => return tunnel_failure(&target.id, &error),
        Err(_) => return tunnel_failure(&target.id, &TunnelError::Timeout),
    };
    Json(gateway_view(&card, &state.base_url(), &target.id)).into_response()
}

async fn forward_jsonrpc(
    State(state): State<GatewayState>,
    Path(peer): Path<String>,
    request: Request,
) -> Response {
    forward(state, peer, request).await
}

async fn forward_rest(
    State(state): State<GatewayState>,
    Path((peer, _rest)): Path<(String, String)>,
    request: Request,
) -> Response {
    forward(state, peer, request).await
}

/// Forwards `request`, for `/peers/<peer>/<rest>`, to `/<rest>` of `peer`.
async fn forward(state: GatewayState, peer: String, request: Request) -> Response {
    let Some(target_uri) = peer_target(request.uri()) else {
        return error(StatusCode::BAD_REQUEST, "invalid request target");
    };
    let target = match state.resolve(peer).await {
        Ok(Some(target)) => target,
        Ok(None) => return unknown_peer(),
        Err(failure) => return failure.into_response(),
    };
    let (parts, body) = request.into_parts();
    let mut headers = outgoing_headers(&parts.headers);
    if !headers.contains_key(AUTHORIZATION) {
        match state.held_bearer(target.id).await {
            Ok(Some(bearer)) => {
                headers.insert(AUTHORIZATION, bearer);
            }
            Ok(None) => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    format!("no grant held for {}", target.id),
                );
            }
            Err(failure) => return failure.into_response(),
        }
    }
    let mut outgoing = http::Request::builder()
        .method(parts.method)
        .uri(target_uri)
        .body(body)
        .expect("a method and URI taken from a valid request");
    *outgoing.headers_mut() = headers;

    match state.inner.peer_http.send(target.addr, outgoing).await {
        Ok(response) => {
            let (mut parts, body) = response.into_parts();
            parts.headers = end_to_end(&parts.headers);
            Response::from_parts(parts, body)
        }
        Err(error) => tunnel_failure(&target.id, &error),
    }
}

/// The path and query that a request for `/peers/<peer>/<rest>` is forwarded
/// with: `/<rest>` and the query, both exactly as received, still
/// percent-encoded.
fn peer_target(uri: &Uri) -> Option<Uri> {
    let after_peers = uri.path().strip_prefix("/peers/")?;
    let rest = &after_peers[after_peers.find('/')?..];
    let target = match uri.query() {
        Some(query) => format!("{rest}?{query}"),
        None => rest.to_owned(),
    };
    target.parse().ok()
}

/// The headers of a client's request that go on to the peer: the
/// [end-to-end](end_to_end) ones, but for the `x-agentnet-*` headers that
/// only the peer's tunnel and authentication set.
fn outgoing_headers(incoming: &HeaderMap) -> HeaderMap {
    let mut headers = end_to_end(incoming);
    headers.retain_names(|name| !name.as_str().starts_with(OWN_HEADER_PREFIX));
    headers
}

/// `headers` without the hop-by-hop ones: [`HOP_BY_HOP`], those that
/// `Connection` names, and `Host`.
fn end_to_end(headers: &HeaderMap) -> HeaderMap {
    let named_by_connection: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    let mut kept = headers.clone();
    kept.retain_names(|name| {
        name != HOST && !HOP_BY_HOP.contains(&name.as_str()) && !named_by_connection.contains(name)
    });
    kept
}

/// `HeaderMap::retain` for whole names: drops every value of the names
/// `keep` refuses.
trait RetainNames {
    fn retain_names(&mut self, keep: impl FnMut(&HeaderName) -> bool);
}

impl RetainNames for HeaderMap {
    fn retain_names(&mut self, mut keep: impl FnMut(&HeaderName) -> bool) {
        let dropped: Vec<HeaderName> = self.keys().filter(|name| !keep(name)).cloned().collect();
        for name in dropped {
            self.remove(name);
        }
    }
}

/// The answer to a request the tunnel could not carry to `peer`.
fn tunnel_failure(peer: &AgentId, failure: &TunnelError) -> Response {
    tracing::debug!(%peer, error = %failure, "a gateway request did not reach the peer");
    match failure {
        TunnelError::Unreachable(_) | TunnelError::Timeout => {
            error(StatusCode::BAD_GATEWAY, "peer unreachable")
        }
        TunnelError::Protocol(_) => error(StatusCode::BAD_GATEWAY, "peer protocol error"),
    }
}

fn unknown_peer() -> Response {
    error(StatusCode::NOT_FOUND, "unknown peer")
}

/// `status` with `{"error": message}`.
fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// The store could not be read; the gateway answers `500`.
struct StoreUnavailable;

impl IntoResponse for StoreUnavailable {
    fn into_response(self) -> Response {
        error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

/// Runs a store call off the async threads, logging a failure.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, StoreUnavailable> {
    let failed = |message: String| {
        tracing::warn!(error = %message, "the gateway cannot read the store");
        StoreUnavailable
    };
    match tokio::task::spawn_blocking(call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(failed(format!("{err:#}"))),
        Err(err) => Err(failed(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(uri: &str) -> Option<String> {
        peer_target(&uri.parse().unwrap()).map(|uri| uri.to_string())
    }

    #[test]
    fn target_keeps_the_rest_of_the_path_and_the_query_as_sent() {
        assert_eq!(target("/peers/bob/jsonrpc").as_deref(), Some("/jsonrpc"));
        assert_eq!(
            target("/peers/bob/rest/message:send").as_deref(),
            Some("/rest/message:send")
        );
        assert_eq!(
            target("/peers/b%2Fo/rest/tasks/a%2Fb%3Acancel?historyLength=0&x=%20").as_deref(),
            Some("/rest/tasks/a%2Fb%3Acancel?historyLength=0&x=%20")
        );
        assert_eq!(
            target("/peers/bob/rest/tasks/t1?").as_deref(),
            Some("/rest/tasks/t1?")
        );
        assert_eq!(target("/peers/bob"), None);
        assert_eq!(target("/healthz"), None);
    }

    #[test]
    fn only_loopback_names_with_the_port_are_local() {
        let local = |hosts: &[&str], uri: &str| {
            let mut headers = HeaderMap::new();
            for host in hosts {
                headers.append(HOST, HeaderValue::from_str(host).unwrap());
            }
            is_local_host(&headers, &uri.parse().unwrap(), 7420)
        };
        for host in [
            "127.0.0.1:7420",
            "localhost:7420",
            "LocalHost:7420",
            "[::1]:7420",
        ] {
            assert!(local(&[host], "/healthz"), "{host}");
        }
        for host in [
            "evil.example:7420",
            "127.0.0.1:7421",
            "127.0.0.1",
            "localhost",
            "127.0.0.2:7420",
            "localhost.evil.example:7420",
            "",
        ] {
            assert!(!local(&[host], "/healthz"), "{host}");
        }
        // Two Host headers, or none and no authority, are refused.
        assert!(!local(&["127.0.0.1:7420", "127.0.0.1:7420"], "/healthz"));
        assert!(!local(&[], "/healthz"));
        // An authority in the request target must be local too.
        assert!(local(&[], "http://127.0.0.1:7420/healthz"));
        assert!(!local(
            &["127.0.0.1:7420"],
            "http://evil.example:7420/healthz"
        ));
    }

    #[test]
    fn end_to_end_drops_hop_by_hop_and_connection_named_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "127.0.0.1:7420"),
            ("connection", "keep-alive, x-private"),
            ("x-private", "1"),
            ("keep-alive", "timeout=5"),
            ("transfer-encoding", "chunked"),
            ("te", "trailers"),
            ("upgrade", "h2c"),
            ("authorization", "Bearer t"),
            ("a2a-version", "1.0"),
            ("content-type", "application/json"),
            ("accept", "text/event-stream"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        let kept = end_to_end(&headers);
        let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["a2a-version", "accept", "authorization", "content-type"]
        );
    }

    #[test]
    fn a_peer_out_of_reach_or_broken_exchange_is_a_bad_gateway() {
        let peer = pals_core::identity::AgentKey::generate().id();
        for (failure, message) in [
            (
                TunnelError::Unreachable("gone".to_owned()),
                "peer unreachable",
            ),
            (TunnelError::Timeout, "peer unreachable"),
            (
                TunnelError::Protocol("garbled".to_owned()),
                "peer protocol error",
            ),
        ] {
            let response = tunnel_failure(&peer, &failure);
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let body = futures::executor::block_on(response.into_body().collect())
                .unwrap()
                .to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body, json!({ "error": message }));
        }
    }

    #[test]
    fn outgoing_headers_drop_pals_own_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-agentnet-peer", "a forged peer"),
            ("x-agentnet-project", "a forged project"),
            ("x-agentnet-grant-id", "g-forged"),
            ("x-agentnet-peer", "another"),
            ("x-other", "kept"),
            ("authorization", "Bearer t"),
            ("connection", "close"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        let kept = outgoing_headers(&headers);
        let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["authorization", "x-other"]);
        assert_eq!(kept["authorization"], "Bearer t");
    }
}
