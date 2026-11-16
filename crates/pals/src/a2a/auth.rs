//! Grant authentication in front of the SDK's routers.
//!
//! Every request but the public Agent Card must present a grant as
//! `Authorization: Bearer <token>`. The grant is checked with
//! [`SignedGrant::check`] against the request: this agent as audience, the
//! peer that the tunnel authenticated ([`PEER_HEADER`]) as subject, this
//! agent or one of its trusted issuers as issuer, and the operation of the
//! route ([`classify`]) on `skill:ask`. A grant that is itself unusable gets
//! `401` with a `WWW-Authenticate` challenge; a valid grant that does not
//! cover the request gets `403`.
//!
//! Headers named `x-agentnet-*` are PALS's own: the layer drops any the
//! request carries, except [`PEER_HEADER`] that the tunnel set, and sets
//! [`GRANT_ID_HEADER`] and [`PROJECT_HEADER`] from the accepted grant. The
//! project of a request is the project of its grant.

use std::sync::Arc;

use a2a::methods;
use a2a_server::WELL_KNOWN_AGENT_CARD_PATH;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use pals_core::grant::{GrantCheck, GrantError, Operation, SignedGrant};
use pals_core::identity::AgentId;
use pals_core::store::Store;
use serde_json::json;

use crate::net::PEER_HEADER;

/// The id of the grant a request was accepted with.
pub const GRANT_ID_HEADER: &str = "x-agentnet-grant-id";
/// The project of the grant a request was accepted with.
pub const PROJECT_HEADER: &str = "x-agentnet-project";

/// Where the JSON-RPC binding is served.
pub(crate) const JSONRPC_PATH: &str = "/jsonrpc";
/// Where the HTTP+JSON binding is served.
pub(crate) const REST_PATH: &str = "/rest";

/// The resource every A2A operation of milestone 1 is checked against.
pub(crate) const ASK_RESOURCE: &str = "skill:ask";
/// The prefix of the headers only PALS sets.
const OWN_HEADER_PREFIX: &str = "x-agentnet-";

/// What a request needs to reach the A2A server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteClass {
    /// Nothing: the Agent Card.
    Public,
    /// A grant that allows this operation on `skill:ask`.
    Op(Operation),
    /// A valid grant, whatever it allows: push notification configs, the
    /// extended card, and anything the SDK does not serve, which it then
    /// refuses.
    AuthOnly,
}

/// The class of a request to the node's app, from its method, its path and,
/// for the JSON-RPC binding, the `method` of its JSON-RPC body (`None` when
/// the body is not a JSON-RPC request).
///
/// It mirrors the routes of `a2a-server-lf` 0.5.3 as the node mounts them:
/// the card at [`WELL_KNOWN_AGENT_CARD_PATH`], JSON-RPC at `/jsonrpc` and
/// HTTP+JSON at `/rest`. `HEAD` is classified as `GET`, as axum routes it.
/// Every request the SDK would dispatch to a task operation is classified
/// with that operation; whatever is not recognized is
/// [`RouteClass::AuthOnly`].
pub fn classify(method: &Method, path: &str, jsonrpc_method: Option<&str>) -> RouteClass {
    if is_get(method) && path == WELL_KNOWN_AGENT_CARD_PATH {
        return RouteClass::Public;
    }
    if is_jsonrpc_path(path) {
        return match jsonrpc_method {
            Some(name) if *method == Method::POST => jsonrpc_class(name),
            _ => RouteClass::AuthOnly,
        };
    }
    match path
        .strip_prefix(REST_PATH)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(rest) => rest_class(method, rest),
        None => RouteClass::AuthOnly,
    }
}

fn is_get(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

/// Whether `path` is under the JSON-RPC binding, whose operation is in the
/// body.
fn is_jsonrpc_path(path: &str) -> bool {
    path.strip_prefix(JSONRPC_PATH)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn jsonrpc_class(name: &str) -> RouteClass {
    match name {
        methods::SEND_MESSAGE | methods::SEND_STREAMING_MESSAGE => {
            RouteClass::Op(Operation::TaskSend)
        }
        methods::GET_TASK | methods::LIST_TASKS | methods::SUBSCRIBE_TO_TASK => {
            RouteClass::Op(Operation::TaskRead)
        }
        methods::CANCEL_TASK => RouteClass::Op(Operation::TaskCancel),
        _ => RouteClass::AuthOnly,
    }
}

/// The class of an HTTP+JSON request, `rest` being its path after `/rest/`.
///
/// The router matches literal segments as sent and percent-decodes a path
/// parameter before the handler reads it; so does this.
fn rest_class(method: &Method, rest: &str) -> RouteClass {
    let get = is_get(method);
    let post = *method == Method::POST;
    let segments: Vec<&str> = rest.split('/').collect();
    let op = match segments.as_slice() {
        ["message:send" | "message:stream"] | ["message", "send" | "stream"] if post => {
            Operation::TaskSend
        }
        // ListTasks.
        ["tasks"] if get => Operation::TaskRead,
        // GetTask, or SubscribeToTask when the id ends in `:subscribe`.
        ["tasks", _] if get => Operation::TaskRead,
        // CancelTask or SubscribeToTask by the suffix of the decoded id, in
        // the SDK's order; any other action is refused by the SDK.
        ["tasks", id] if post => match percent_decoded(id) {
            Some(id) if id.ends_with(":cancel") => Operation::TaskCancel,
            Some(id) if id.ends_with(":subscribe") => Operation::TaskRead,
            _ => return RouteClass::AuthOnly,
        },
        ["tasks", _, "cancel"] if post => Operation::TaskCancel,
        ["tasks", _, "subscribe"] if get || post => Operation::TaskRead,
        _ => return RouteClass::AuthOnly,
    };
    RouteClass::Op(op)
}

/// A path parameter as axum decodes it; `None` when it is not UTF-8, which
/// axum refuses before any handler runs.
fn percent_decoded(segment: &str) -> Option<String> {
    percent_encoding::percent_decode_str(segment)
        .decode_utf8()
        .ok()
        .map(Into::into)
}

/// Checks the grant of every request but the card, before the SDK's routers.
#[derive(Clone)]
pub(crate) struct Authenticator {
    audience: AgentId,
    accepted_issuers: Arc<[AgentId]>,
    store: Arc<Store>,
    max_body_bytes: usize,
}

impl Authenticator {
    /// Accepts grants for `audience` issued by `audience` itself or by one of
    /// `trusted_issuers`, revoked as `store` says. A JSON-RPC body is read up
    /// to `max_body_bytes`.
    pub(crate) fn new(
        audience: AgentId,
        trusted_issuers: &[AgentId],
        store: Arc<Store>,
        max_body_bytes: usize,
    ) -> Self {
        let accepted_issuers = std::iter::once(audience)
            .chain(trusted_issuers.iter().copied())
            .collect();
        Self {
            audience,
            accepted_issuers,
            store,
            max_body_bytes,
        }
    }

    /// `app` behind this authenticator: every request, routed or not.
    pub(crate) fn guard(self, app: Router) -> Router {
        app.layer(middleware::from_fn_with_state(self, authenticate))
    }

    /// The grant that lets the request with `headers` through as `class`.
    async fn accept(&self, headers: &HeaderMap, class: RouteClass) -> Result<Accepted, Refusal> {
        let subject = headers
            .get(PEER_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<AgentId>().ok())
            .ok_or(Refusal::Unauthenticated("unknown_peer"))?;
        let token = bearer_token(headers).ok_or(Refusal::Unauthenticated("missing_token"))?;
        let grant = SignedGrant::from_token(token).map_err(Refusal::from)?;

        let store = Arc::clone(&self.store);
        let grant_id = grant.grant.grant_id.clone();
        let revoked = tokio::task::spawn_blocking(move || store.is_revoked(&grant_id))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|revoked| revoked)
            .map_err(|err| {
                tracing::error!(error = %format!("{err:#}"), "cannot read revocations");
                Refusal::Internal
            })?;

        // A route that needs no particular operation is checked as a read,
        // and then only the grant's own validity counts.
        let (operation, any_operation) = match class {
            RouteClass::Op(operation) => (operation, false),
            RouteClass::AuthOnly | RouteClass::Public => (Operation::TaskRead, true),
        };
        let check = GrantCheck {
            now_unix: chrono::Utc::now().timestamp(),
            audience: &self.audience,
            subject: &subject,
            accepted_issuers: &self.accepted_issuers,
            operation,
            resource: ASK_RESOURCE,
            revoked,
        };
        match grant.check(&check) {
            Ok(()) => {}
            Err(err) if any_operation && !err.is_authentication() => {}
            Err(err) => return Err(err.into()),
        }

        // Ids and project names a header cannot carry make the grant
        // unusable here.
        let header = |text: &str| {
            HeaderValue::from_str(text).map_err(|_| Refusal::Unauthenticated("malformed"))
        };
        Ok(Accepted {
            grant_id: header(&grant.grant.grant_id)?,
            project: header(&grant.grant.project)?,
        })
    }
}

/// What an accepted grant adds to the request.
struct Accepted {
    grant_id: HeaderValue,
    project: HeaderValue,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// 401: no usable grant; the reason in snake case.
    Unauthenticated(&'static str),
    /// 403: a valid grant that does not cover the request.
    Forbidden(&'static str),
    /// 500: the store could not be read.
    Internal,
}

impl From<GrantError> for Refusal {
    fn from(err: GrantError) -> Self {
        let reason = match err {
            GrantError::Malformed => "malformed",
            GrantError::TooLarge => "too_large",
            GrantError::UnknownSchema => "unknown_schema",
            GrantError::BadSignature => "bad_signature",
            GrantError::IssuerNotAccepted => "issuer_not_accepted",
            GrantError::WrongSubject => "wrong_subject",
            GrantError::WrongAudience => "wrong_audience",
            GrantError::NotYetValid => "not_yet_valid",
            GrantError::Expired => "expired",
            GrantError::Revoked => "revoked",
            GrantError::OperationNotGranted => "operation_not_granted",
            GrantError::ResourceNotGranted => "resource_not_granted",
        };
        if err.is_authentication() {
            Self::Unauthenticated(reason)
        } else {
            Self::Forbidden(reason)
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthenticated(reason) => {
                let challenge = format!(
                    r#"Bearer realm="agentnet", error="invalid_token", error_description="{reason}""#
                );
                let challenge =
                    HeaderValue::from_str(&challenge).expect("the reasons are header-safe");
                (
                    StatusCode::UNAUTHORIZED,
                    [(WWW_AUTHENTICATE, challenge)],
                    Json(json!({ "error": reason })),
                )
                    .into_response()
            }
            Self::Forbidden(reason) => {
                (StatusCode::FORBIDDEN, Json(json!({ "error": reason }))).into_response()
            }
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal_error" })),
            )
                .into_response(),
        }
    }
}

/// The token of the request's only `Authorization` header, if it is
/// `Bearer <token>`.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return None;
    };
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    let token = token.trim_matches(' ');
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && !token.contains(' '))
        .then_some(token)
}

/// Drops the `x-agentnet-*` headers a client sent, keeping the peer id the
/// tunnel set.
fn drop_own_headers(headers: &mut HeaderMap) {
    let sent: Vec<HeaderName> = headers
        .keys()
        .filter(|name| name.as_str().starts_with(OWN_HEADER_PREFIX) && *name != PEER_HEADER)
        .cloned()
        .collect();
    for name in sent {
        headers.remove(name);
    }
}

async fn authenticate(State(auth): State<Authenticator>, request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();
    drop_own_headers(&mut parts.headers);

    // The JSON-RPC operation is in the body: read it, and hand the same
    // bytes on.
    let (class, body) = if is_jsonrpc_path(parts.uri.path()) {
        let bytes = match read_body(body, auth.max_body_bytes).await {
            Ok(bytes) => bytes,
            Err(refusal) => return refusal.into_response(),
        };
        let jsonrpc_method = serde_json::from_slice::<a2a::JsonRpcRequest>(&bytes)
            .ok()
            .map(|request| request.method);
        let class = classify(&parts.method, parts.uri.path(), jsonrpc_method.as_deref());
        (class, Body::from(bytes))
    } else {
        (classify(&parts.method, parts.uri.path(), None), body)
    };

    if class != RouteClass::Public {
        match auth.accept(&parts.headers, class).await {
            Ok(accepted) => {
                parts.headers.insert(GRANT_ID_HEADER, accepted.grant_id);
                parts.headers.insert(PROJECT_HEADER, accepted.project);
            }
            Err(refusal) => {
                tracing::debug!(?refusal, path = parts.uri.path(), "request refused");
                return refusal.into_response();
            }
        }
    }
    next.run(Request::from_parts(parts, body)).await
}

/// The whole body, at most `max_body_bytes` long.
async fn read_body(body: Body, max_body_bytes: usize) -> Result<Bytes, (StatusCode, String)> {
    match Limited::new(body, max_body_bytes).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(err) if err.is::<LengthLimitError>() => Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("request body exceeds {max_body_bytes} bytes"),
        )),
        Err(err) => {
            tracing::debug!(error = %err, "unreadable request body");
            Err((
                StatusCode::BAD_REQUEST,
                "unreadable request body".to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::routing::{get, post};
    use pals_core::grant::{GRANT_SCHEMA, Grant, format_timestamp};
    use pals_core::identity::AgentKey;
    use tower::ServiceExt;

    use super::*;

    fn op(operation: Operation) -> RouteClass {
        RouteClass::Op(operation)
    }

    #[test]
    fn json_rpc_methods_are_classified_by_operation() {
        let post = Method::POST;
        for (name, class) in [
            ("SendMessage", op(Operation::TaskSend)),
            ("SendStreamingMessage", op(Operation::TaskSend)),
            ("GetTask", op(Operation::TaskRead)),
            ("ListTasks", op(Operation::TaskRead)),
            ("SubscribeToTask", op(Operation::TaskRead)),
            ("CancelTask", op(Operation::TaskCancel)),
            ("CreateTaskPushNotificationConfig", RouteClass::AuthOnly),
            ("GetTaskPushNotificationConfig", RouteClass::AuthOnly),
            ("ListTaskPushNotificationConfigs", RouteClass::AuthOnly),
            ("DeleteTaskPushNotificationConfig", RouteClass::AuthOnly),
            ("GetExtendedAgentCard", RouteClass::AuthOnly),
            ("cancelTask", RouteClass::AuthOnly),
            ("", RouteClass::AuthOnly),
        ] {
            assert_eq!(classify(&post, "/jsonrpc", Some(name)), class, "{name}");
            assert_eq!(classify(&post, "/jsonrpc/", Some(name)), class, "{name}");
        }
        assert_eq!(classify(&post, "/jsonrpc", None), RouteClass::AuthOnly);
        assert_eq!(
            classify(&Method::GET, "/jsonrpc", Some("CancelTask")),
            RouteClass::AuthOnly
        );
        // Not the JSON-RPC binding, whatever the body says.
        assert_eq!(
            classify(&post, "/jsonrpcx", Some("CancelTask")),
            RouteClass::AuthOnly
        );
    }

    #[test]
    fn rest_routes_are_classified_by_operation() {
        let (get, head, post, delete) = (Method::GET, Method::HEAD, Method::POST, Method::DELETE);
        let send = op(Operation::TaskSend);
        let read = op(Operation::TaskRead);
        let cancel = op(Operation::TaskCancel);
        let auth = RouteClass::AuthOnly;
        for (method, path, class) in [
            (&post, "/rest/message:send", send),
            (&post, "/rest/message/send", send),
            (&post, "/rest/message:stream", send),
            (&post, "/rest/message/stream", send),
            (&get, "/rest/message:send", auth),
            (&get, "/rest/tasks", read),
            (&head, "/rest/tasks", read),
            (&post, "/rest/tasks", auth),
            (&get, "/rest/tasks/t-1", read),
            (&head, "/rest/tasks/t-1", read),
            (&get, "/rest/tasks/t-1:subscribe", read),
            (&post, "/rest/tasks/t-1:cancel", cancel),
            (&post, "/rest/tasks/t-1%3Acancel", cancel),
            (&post, "/rest/tasks/t-1%3acancel", cancel),
            (&post, "/rest/tasks/t-1%3A%63ancel", cancel),
            (&post, "/rest/tasks/t-1:subscribe", read),
            (&post, "/rest/tasks/t-1%3Asubscribe", read),
            (&post, "/rest/tasks/t:subscribe:cancel", cancel),
            (&post, "/rest/tasks/t-1", auth),
            (&post, "/rest/tasks/t-1%2Fcancel", auth),
            (&post, "/rest/tasks/t-1%FF:cancel", auth),
            (&post, "/rest/tasks/t-1/cancel", cancel),
            (&get, "/rest/tasks/t-1/cancel", auth),
            (&get, "/rest/tasks/t-1/subscribe", read),
            (&head, "/rest/tasks/t-1/subscribe", read),
            (&post, "/rest/tasks/t-1/subscribe", read),
            (&post, "/rest/tasks/t-1/pushNotificationConfigs", auth),
            (&get, "/rest/tasks/t-1/push-configs/c-1", auth),
            (&delete, "/rest/tasks/t-1/pushNotificationConfigs/c-1", auth),
            (&get, "/rest/extendedAgentCard", auth),
            (&get, "/rest/agent-card/extended", auth),
            (&get, "/restaurant/tasks", auth),
            (&get, "/rest", auth),
            (&get, "/", auth),
            (&get, "/.well-known/agent-card.json", RouteClass::Public),
            (&head, "/.well-known/agent-card.json", RouteClass::Public),
            (&post, "/.well-known/agent-card.json", auth),
            (&get, "/.well-known/agent-card.json/", auth),
        ] {
            assert_eq!(classify(method, path, None), class, "{method} {path}");
        }
    }

    #[test]
    fn grant_errors_map_to_401_or_403() {
        for err in [
            GrantError::Expired,
            GrantError::NotYetValid,
            GrantError::Revoked,
            GrantError::IssuerNotAccepted,
            GrantError::WrongSubject,
            GrantError::WrongAudience,
            GrantError::BadSignature,
            GrantError::Malformed,
            GrantError::TooLarge,
            GrantError::UnknownSchema,
        ] {
            assert!(
                matches!(Refusal::from(err), Refusal::Unauthenticated(_)),
                "{err}"
            );
        }
        assert_eq!(
            Refusal::from(GrantError::NotYetValid),
            Refusal::Unauthenticated("not_yet_valid")
        );
        for err in [
            GrantError::OperationNotGranted,
            GrantError::ResourceNotGranted,
        ] {
            assert!(matches!(Refusal::from(err), Refusal::Forbidden(_)), "{err}");
        }
    }

    #[test]
    fn only_one_bearer_authorization_is_a_token() {
        let headers = |values: &[&str]| {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
            }
            headers
        };
        assert_eq!(bearer_token(&headers(&["Bearer abc"])), Some("abc"));
        assert_eq!(bearer_token(&headers(&["bearer abc"])), Some("abc"));
        assert_eq!(bearer_token(&headers(&[])), None);
        assert_eq!(bearer_token(&headers(&["Bearer"])), None);
        assert_eq!(bearer_token(&headers(&["Bearer "])), None);
        assert_eq!(bearer_token(&headers(&["Bearer a b"])), None);
        assert_eq!(bearer_token(&headers(&["Basic abc"])), None);
        assert_eq!(bearer_token(&headers(&["Bearer abc", "Bearer abc"])), None);
    }

    /// An app that answers with the `x-agentnet-*` headers and the body it
    /// received, behind an authenticator of `me` that issued `grant`.
    fn echo_app(me: &AgentKey, grant: &SignedGrant) -> Router {
        async fn echo(headers: HeaderMap, body: Bytes) -> Json<serde_json::Value> {
            let own: serde_json::Map<String, serde_json::Value> = headers
                .iter()
                .filter(|(name, _)| name.as_str().starts_with(OWN_HEADER_PREFIX))
                .map(|(name, value)| (name.to_string(), json!(value.to_str().unwrap())))
                .collect();
            Json(json!({"headers": own, "body": String::from_utf8_lossy(&body)}))
        }
        let store = Arc::new(Store::open_in_memory(me.id()).unwrap());
        store.grant_issued_put(grant).unwrap();
        let app = Router::new()
            .route(JSONRPC_PATH, post(echo))
            .route(WELL_KNOWN_AGENT_CARD_PATH, get(echo));
        Authenticator::new(me.id(), &[], store, 1024).guard(app)
    }

    fn grant_for(me: &AgentKey, peer: &AgentId) -> SignedGrant {
        let now = chrono::Utc::now().timestamp();
        let grant = Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: "g-unit".to_owned(),
            issuer: me.id(),
            subject: *peer,
            audience: me.id(),
            project: "demo".to_owned(),
            resources: vec![ASK_RESOURCE.to_owned()],
            operations: vec![Operation::TaskSend],
            issued_at: format_timestamp(now).unwrap(),
            expires_at: format_timestamp(now + 3600).unwrap(),
        };
        SignedGrant::issue(me, grant).unwrap()
    }

    async fn call(app: Router, request: Request) -> (StatusCode, serde_json::Value) {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn own_headers_come_from_the_grant_and_the_body_is_handed_on() {
        let me = AgentKey::generate();
        let peer = AgentKey::generate().id();
        let grant = grant_for(&me, &peer);
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{}}"#;
        let request = Request::post(JSONRPC_PATH)
            .header(PEER_HEADER, peer.to_string())
            .header(AUTHORIZATION, format!("Bearer {}", grant.to_token()))
            .header(PROJECT_HEADER, "someone-elses")
            .header(GRANT_ID_HEADER, "g-forged")
            .header("x-agentnet-anything", "forged")
            .body(Body::from(body))
            .unwrap();
        let (status, seen) = call(echo_app(&me, &grant), request).await;
        assert_eq!(status, StatusCode::OK, "{seen}");
        assert_eq!(
            seen["headers"],
            json!({
                PEER_HEADER: peer.to_string(),
                GRANT_ID_HEADER: "g-unit",
                PROJECT_HEADER: "demo",
            })
        );
        assert_eq!(seen["body"], body);
    }

    #[tokio::test]
    async fn the_public_card_gets_no_client_sent_own_headers() {
        let me = AgentKey::generate();
        let grant = grant_for(&me, &AgentKey::generate().id());
        let request = Request::get(WELL_KNOWN_AGENT_CARD_PATH)
            .header(PROJECT_HEADER, "forged")
            .header(GRANT_ID_HEADER, "g-forged")
            .body(Body::empty())
            .unwrap();
        let (status, seen) = call(echo_app(&me, &grant), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seen["headers"], json!({}));
    }

    #[tokio::test]
    async fn a_request_without_an_authenticated_peer_is_refused() {
        let me = AgentKey::generate();
        let peer = AgentKey::generate().id();
        let grant = grant_for(&me, &peer);
        let request = Request::post(JSONRPC_PATH)
            .header(AUTHORIZATION, format!("Bearer {}", grant.to_token()))
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage"}"#,
            ))
            .unwrap();
        let (status, seen) = call(echo_app(&me, &grant), request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(seen, json!({"error": "unknown_peer"}));
    }
}
