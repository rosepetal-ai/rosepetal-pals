//! Contacts that are plain A2A agents (H2-D3): a stock A2A server reached by
//! its own URL, with no PALS daemon behind it, which this agent, its gateway
//! and its outbox call, never the other way round. Its card is read bounded
//! and validated when it is saved and at every use, its signature reported;
//! only interfaces of the origin the operator gave are ever called; no
//! proxy, no redirect, no grant, no file; credentials only from an
//! environment variable it names; and its answers pass the gateway's checks
//! as a PALS peer's do.

mod common;

use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use a2a::{
    AgentCapabilities, Message, Part, Role, SendMessageRequest, SendMessageResponse,
    StreamResponse, TaskState,
};
use a2a_client::A2AClientFactory;
use a2a_client::agent_card::AgentCardResolver;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::rest::rest_router;
use a2a_server::{DefaultRequestHandler, InMemoryTaskStore};
use axum::body::Body;
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{
    CountingExecutor, TestNode, echo_app_at, echo_app_with_card, echo_card_at, gateway_request,
    gateway_send, grant, message_params, now_unix, rpc_request, serve_at, spawn_node,
};
use futures::StreamExt;
use http_body_util::BodyExt;
use pals::a2a::gateway_view_for;
use pals::config::WorkerCfg;
use pals::gateway::plain::PLAIN_NOT_SENT;
use pals::gateway::plain::{
    MAX_JWKS_BYTES, PlainCard, PlainError, SignatureStatus, fetch_plain_card, http_client,
};
use pals::outbox::{AskError, AskRequest, Outbox, PLAIN_NEEDS_STREAMING};
use pals_core::card_jws::{CanonicalForm, CardError, MAX_CARD_BYTES, canonical_payload, card_hash};
use pals_core::discovery::selection::{SearchRequest, Stage};
use pals_core::grant::Operation::{TaskRead, TaskSend};
use pals_core::identity::AgentKey;
use pals_core::jcs;
use pals_core::store::{A2aContact, Contact, OUTBOX_PENDING, PeerRef, Store};
use serde_json::{Value, json};

/// How long the asks of these tests wait for their answer.
const WAIT: Duration = Duration::from_secs(120);
/// The A2A version header every call carries.
const VERSION: [(&str, &str); 1] = [("a2a-version", "1.0")];

/// The plain contact `name` for the agent at `origin`, its card at the
/// well-known path, with no credentials.
fn plain_contact(name: &str, origin: &str) -> A2aContact {
    A2aContact {
        name: name.to_owned(),
        card_url: format!("{origin}/.well-known/agent-card.json"),
        origin: origin.to_owned(),
        bearer_env: None,
        added_at: now_unix(),
    }
}

/// Saves the agent at `origin` in `a` as the plain contact `name`, as `pals
/// contact add-a2a` does once its card is read.
fn add_plain(a: &TestNode, name: &str, origin: &str) {
    a.node
        .store
        .a2a_contact_put(&plain_contact(name, origin))
        .expect("save the plain contact");
}

/// The card of the plain contact `name` for the agent at `origin`, read as
/// the gateway reads it.
async fn fetch(name: &str, origin: &str) -> Result<PlainCard, PlainError> {
    fetch_plain_card(&http_client(), &plain_contact(name, origin)).await
}

/// The echo agent's card at `origin`, as JSON.
fn card_json(origin: &str) -> Value {
    serde_json::to_value(echo_card_at(origin)).unwrap()
}

/// `value` as JSON followed by spaces, `len` bytes in all: still the same
/// JSON document.
fn padded(value: &Value, len: usize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    assert!(bytes.len() <= len, "{} > {len}", bytes.len());
    bytes.resize(len, b' ');
    bytes
}

/// A JWS protected header for `alg`, `kid` and `jku`.
fn header(alg: &str, kid: &str, jku: &str) -> Value {
    json!({"alg": alg, "jku": jku, "kid": kid, "typ": "JOSE"})
}

/// `card` signed by `key` with the protected header `protected`, over its
/// JCS as is: a detached JWS in `signatures`, as A2A §8.4 says.
fn sign(card: &Value, key: &AgentKey, protected: &Value) -> Value {
    let protected = URL_SAFE_NO_PAD.encode(jcs::canonical(protected));
    let payload = canonical_payload(card, CanonicalForm::Raw).unwrap();
    let input = format!("{protected}.{}", URL_SAFE_NO_PAD.encode(&payload));
    let signature = URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()));
    let mut signed = card.clone();
    signed["signatures"] = json!([{"protected": protected, "signature": signature}]);
    signed
}

/// A JWK Set holding `key`'s public half as an OKP Ed25519 key, `kid`.
fn okp_jwks(kid: &str, key: &AgentKey) -> Value {
    json!({"keys": [{
        "kty": "OKP",
        "crv": "Ed25519",
        "kid": kid,
        "alg": "EdDSA",
        "use": "sig",
        "x": URL_SAFE_NO_PAD.encode(key.id().as_bytes()),
    }]})
}

/// The echo agent at `origin`, its card signed by `key` as `k1` with its
/// JWK Set at `<origin>/jwks.json`, which serves `jwks` as given.
fn signed_echo_app(origin: &str, key: &AgentKey, jwks: Vec<u8>) -> axum::Router {
    let jku = format!("{origin}/jwks.json");
    let card = sign(&card_json(origin), key, &header("EdDSA", "k1", &jku));
    echo_app_with_card(card).route("/jwks.json", get(move || async move { json_bytes(jwks) }))
}

/// An agent that serves `card`, exactly as given, at
/// `/.well-known/agent-card.json`, and nothing else until routes are added.
fn card_app(card: Value) -> axum::Router {
    axum::Router::new().route(
        "/.well-known/agent-card.json",
        get(move || {
            let card = card.clone();
            async move { axum::Json(card) }
        }),
    )
}

/// The bindings a [`counting_app`] serves.
#[derive(Clone, Copy)]
enum Served {
    JsonRpc,
    Both,
}

/// A stock A2A agent whose work `exec` does, serving `card` exactly as
/// given at `/.well-known/agent-card.json`, with the streaming its
/// `capabilities` declare, and the bindings `served`: JSON-RPC at
/// `/jsonrpc`, HTTP+JSON at `/rest`.
fn counting_app(card: Value, exec: CountingExecutor, served: Served) -> axum::Router {
    let capabilities = AgentCapabilities {
        streaming: card["capabilities"]["streaming"].as_bool(),
        push_notifications: Some(false),
        ..AgentCapabilities::default()
    };
    let handler = Arc::new(
        DefaultRequestHandler::new(exec, InMemoryTaskStore::new()).with_capabilities(capabilities),
    );
    let app = card_app(card).nest("/jsonrpc", jsonrpc_router(Arc::clone(&handler)));
    match served {
        Served::JsonRpc => app,
        Served::Both => app.nest("/rest", rest_router(handler)),
    }
}

/// `200` with `bytes` typed as JSON.
fn json_bytes(bytes: Vec<u8>) -> axum::response::Response {
    axum::response::Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(bytes))
        .unwrap()
}

/// `200` typed as JSON whose body never ends: the bytes of `chunk` again
/// and again, with no length declared.
fn endless(chunk: &'static [u8]) -> axum::response::Response {
    let body = futures::stream::repeat_with(move || Ok::<_, std::io::Error>(chunk));
    axum::response::Response::builder()
        .header("content-type", "application/json")
        .body(Body::from_stream(body))
        .unwrap()
}

/// A redirect to `location`, setting a cookie.
fn redirect(status: u16, location: String) -> axum::response::Response {
    axum::response::Response::builder()
        .status(status)
        .header("location", location)
        .header("set-cookie", "session=stolen; Path=/")
        .body(Body::empty())
        .unwrap()
}

/// Writes a configuration in `a`'s data directory, so that the `pals`
/// command finds its key and its store there.
fn cli_home(a: &TestNode) -> &Path {
    std::fs::write(a.dir.path().join("config.toml"), "[net]\nrelay = false\n").unwrap();
    a.dir.path()
}

/// Runs `pals --home <home> <args>` with `env` added, off the async
/// threads; its exit code, stdout and stderr.
async fn pals(home: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .arg("--home")
        .arg(home)
        .args(args);
    for (name, value) in env {
        cmd.env(name, value);
    }
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .expect("the command's thread");
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
    )
}

/// A `SendMessage` request with one text part.
fn send_request(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        message: Message::new(Role::User, vec![Part::text(text)]),
        configuration: None,
        metadata: None,
        tenant: None,
    }
}

/// The text of the first part of the task's first artifact.
fn answer_of(reply: &SendMessageResponse) -> String {
    let SendMessageResponse::Task(task) = reply else {
        panic!("a message instead of a task: {reply:?}");
    };
    assert_eq!(task.status.state, TaskState::Completed, "{task:?}");
    task.artifacts.as_ref().expect("an artifact")[0].parts[0]
        .as_text()
        .expect("a text part")
        .to_owned()
}

/// The `answer` text of a `SendMessage` result as JSON: the task's first
/// artifact's first part.
fn answer_in(result: &Value) -> &str {
    result["task"]["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no answer in {result}"))
}

/// The body of `response`, read until it ends or breaks: the bytes that
/// came, and whether it ended cleanly.
async fn read_until_end(response: axum::response::Response) -> (Vec<u8>, bool) {
    let mut body = response.into_body();
    let mut received = Vec::new();
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    received.extend_from_slice(&data);
                }
            }
            Some(Err(_)) => return (received, false),
            None => return (received, true),
        }
    }
}

#[tokio::test]
async fn stock_a2a_agent_over_loopback_http_becomes_a_contact() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let (origin, agent) = echo_app_at().await;
    let home = cli_home(&a);

    let (code, out, err) = pals(
        home,
        &["contact", "add-a2a", &origin, "--name", "echo"],
        &[],
    )
    .await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("saved contact echo"), "{out}");
    assert!(out.contains("unsigned"), "{out}");

    // Saved by its card's URL and its origin, with no credentials.
    let card_url = format!("{origin}/.well-known/agent-card.json");
    let saved = a
        .node
        .store
        .a2a_contact_by_name("echo")
        .unwrap()
        .expect("the contact is saved");
    assert_eq!(saved.card_url, card_url);
    assert_eq!(saved.origin, origin);
    assert_eq!(saved.bearer_env, None);
    // Its card, read and kept as received, with no signer.
    let served = card_json(&origin);
    let (kept, hash) = a
        .node
        .store
        .peer_card("a2a:echo", "public")
        .unwrap()
        .expect("the card is kept");
    assert_eq!(kept, served);
    assert_eq!(hash, card_hash(&served));
    // Read with A2A-Version: 1.0, and nothing else asked.
    let seen = agent.requests();
    assert!(!seen.is_empty());
    for request in &seen {
        assert_eq!(
            (request.method.as_str(), request.uri.as_str()),
            ("GET", "/.well-known/agent-card.json")
        );
        assert_eq!(request.headers["a2a-version"], "1.0");
    }

    // The card's own URL is taken as well.
    let (code, _, err) = pals(
        home,
        &["contact", "add-a2a", &card_url, "--name", "echo-card"],
        &[],
    )
    .await;
    assert_eq!(code, 0, "{err}");
    let by_card = a.node.store.a2a_contact_by_name("echo-card").unwrap();
    assert_eq!(
        by_card.map(|c| (c.card_url, c.origin)),
        Some((card_url.clone(), origin.clone()))
    );
    assert_eq!(
        a.node.store.a2a_contacts().unwrap().len(),
        2,
        "both are listed"
    );
    let (_, listed, _) = pals(home, &["contact", "ls"], &[]).await;
    assert!(listed.contains(&format!("echo\t{card_url}")), "{listed}");

    // The gateway serves its view: its own bindings, no security.
    let gw = a.gateway_url();
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/echo/.well-known/agent-card.json",
        &VERSION,
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let view = serde_json::to_value(gateway_view_for(&echo_card_at(&origin), &gw, "echo")).unwrap();
    assert_eq!(res.json, view);
    assert_eq!(
        res.json["supportedInterfaces"][0]["url"],
        format!("{gw}/peers/echo/jsonrpc")
    );
    assert!(res.json.get("securitySchemes").is_none(), "{}", res.json);
}

#[tokio::test]
async fn ask_reaches_a_plain_a2a_agent_through_the_gateway() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let (origin, agent) = echo_app_at().await;
    add_plain(&a, "echo", &origin);

    let outcome = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "echo".to_owned(),
            question: "hola".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: None,
        })
        .await
        .expect("the plain agent answers");
    assert_eq!(outcome.answer.as_deref(), Some("echo: hola"));
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED");

    // Kept under the contact's reference, and taken up again by it.
    let row = a
        .node
        .store
        .outbox_get(&outcome.message_id)
        .unwrap()
        .expect("the question is kept");
    assert_eq!(row.peer, "a2a:echo");
    assert_eq!(row.state, "TASK_STATE_COMPLETED");
    let again = a
        .node
        .outbox()
        .resume(&outcome.message_id, WAIT)
        .await
        .expect("the same outcome");
    assert_eq!(again.answer.as_deref(), Some("echo: hola"));
    assert_eq!(again.task_id, outcome.task_id);

    // Asked on the agent's own HTTP+JSON binding, activating idempotency as
    // with a peer.
    let calls: Vec<_> = agent
        .requests()
        .into_iter()
        .filter(|r| r.uri.starts_with("/rest/"))
        .collect();
    assert!(
        calls
            .iter()
            .any(|r| r.method == "POST" && r.uri == "/rest/message:stream"),
        "{calls:?}"
    );
    for call in &calls {
        let extensions = call.headers["a2a-extensions"].to_str().unwrap();
        assert!(extensions.contains("extensions/idempotency/v1"), "{call:?}");
    }
}

#[tokio::test]
async fn ask_reaches_a_json_rpc_only_plain_agent_once() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let exec = CountingExecutor::new(Duration::ZERO);
    let worker = exec.clone();
    // Its card declares its JSON-RPC interface only, and streaming.
    let (origin, agent) = serve_at(move |origin| {
        let mut card = card_json(origin);
        let jsonrpc = card["supportedInterfaces"][0].clone();
        assert_eq!(jsonrpc["protocolBinding"], "JSONRPC");
        card["supportedInterfaces"] = json!([jsonrpc]);
        counting_app(card, worker, Served::JsonRpc)
    })
    .await;
    add_plain(&a, "rpc", &origin);

    let outcome = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "rpc".to_owned(),
            question: "hola".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: None,
        })
        .await
        .expect("the JSON-RPC agent answers");
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED");
    assert_eq!(outcome.answer.as_deref(), Some("echo: hola"));
    assert_eq!(exec.runs(), 1, "asked once");
    // Taken up again: the same outcome, and no other run.
    let again = a
        .node
        .outbox()
        .resume(&outcome.message_id, WAIT)
        .await
        .expect("the same outcome");
    assert_eq!(again.answer.as_deref(), Some("echo: hola"));
    assert_eq!(again.task_id, outcome.task_id);
    assert_eq!(exec.runs(), 1);
    // On its JSON-RPC binding only, activating idempotency as with a peer.
    let calls: Vec<_> = agent
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(call.uri, "/jsonrpc", "{call:?}");
        let extensions = call.headers["a2a-extensions"].to_str().unwrap();
        assert!(extensions.contains("extensions/idempotency/v1"), "{call:?}");
    }
}

#[tokio::test]
async fn plain_agent_without_streaming_is_refused_before_anything_reaches_it() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let home = cli_home(&a);
    let exec = CountingExecutor::new(Duration::ZERO);
    let worker = exec.clone();
    let key = AgentKey::generate();
    let jwks = serde_json::to_vec(&okp_jwks("k1", &key)).unwrap();
    // A verified card, with a skill of the capability searched for, that
    // does not declare streaming.
    let (origin, agent) = serve_at(move |origin| {
        let mut card = card_json(origin);
        card["capabilities"]["streaming"] = json!(false);
        card["skills"][0]["tags"] = json!(["agentnet:capability:v1:code.rust.review"]);
        let jku = format!("{origin}/jwks.json");
        let card = sign(&card, &key, &header("EdDSA", "k1", &jku));
        counting_app(card, worker, Served::Both)
            .route("/jwks.json", get(move || async move { json_bytes(jwks) }))
    })
    .await;

    // Saved, with a note that says so.
    let (code, out, err) = pals(
        home,
        &["contact", "add-a2a", &origin, "--name", "unary"],
        &[],
    )
    .await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("verified"), "{out}");
    assert!(out.contains("does not declare streaming"), "{out}");

    // Asked: refused, with nothing kept and nothing sent.
    let asked = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "unary".to_owned(),
            question: "hola".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: None,
        })
        .await;
    assert_eq!(
        asked,
        Err(AskError::Local(PLAIN_NEEDS_STREAMING.to_owned()))
    );
    assert!(a.node.store.outbox_all().unwrap().is_empty());

    // Discarded by a search, at its card, so never chosen.
    let outcome = a
        .node
        .searcher()
        .search(SearchRequest {
            capability: "code.rust.review".to_owned(),
            ..SearchRequest::default()
        })
        .await
        .unwrap();
    assert!(outcome.ranked.is_empty(), "{outcome:#?}");
    let discarded = outcome
        .discarded
        .iter()
        .find(|d| d.peer == "unary")
        .unwrap_or_else(|| panic!("discarded: {outcome:#?}"));
    assert_eq!(
        (discarded.stage, discarded.reason.as_str()),
        (Stage::Card, "card declares no streaming")
    );
    // Only its card was ever read.
    assert!(
        agent.requests().iter().all(|r| r.method == "GET"),
        "{:?}",
        agent.requests()
    );
    assert_eq!(exec.runs(), 0);

    // A stock client still reaches it through the gateway.
    let res = gateway_request(
        &a.gateway_url(),
        "POST",
        "/peers/unary/jsonrpc",
        &VERSION,
        Some(&rpc_request("SendMessage", message_params("hola", None))),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(answer_in(&res.json["result"]), "echo: hola");
    assert_eq!(exec.runs(), 1);
}

#[tokio::test]
async fn stock_client_reaches_a_plain_contact_through_the_gateway() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let (origin, _agent) = echo_app_at().await;
    add_plain(&a, "echo", &origin);
    let gw = a.gateway_url();

    // A stock client, from the card the gateway serves.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let card = AgentCardResolver::new(None)
        .resolve(&format!("{gw}/peers/echo"))
        .await
        .expect("resolve the card through the gateway");
    assert!(
        card.supported_interfaces
            .iter()
            .all(|iface| iface.url.starts_with(&format!("{gw}/peers/echo/"))),
        "{card:?}"
    );
    let client = A2AClientFactory::builder()
        .build()
        .create_from_card(&card)
        .await
        .expect("a client from the card");
    let reply = client
        .send_message(&send_request("hola"))
        .await
        .expect("SendMessage through the gateway");
    assert_eq!(answer_of(&reply), "echo: hola");
    let events: Vec<StreamResponse> = client
        .send_streaming_message(&send_request("otra"))
        .await
        .expect("SendStreamingMessage through the gateway")
        .map(|event| event.expect("a stream event"))
        .collect()
        .await;
    match events.last() {
        Some(StreamResponse::StatusUpdate(update)) => {
            assert_eq!(update.status.state, TaskState::Completed)
        }
        other => panic!("the stream did not end with a status update: {other:?}"),
    }

    // Both bindings, as a client calls them by hand.
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/echo/jsonrpc",
        &VERSION,
        Some(&rpc_request(
            "SendMessage",
            message_params("por jsonrpc", None),
        )),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(answer_in(&res.json["result"]), "echo: por jsonrpc");
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/echo/rest/message:send",
        &VERSION,
        Some(&message_params("por rest", None)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(answer_in(&res.json), "echo: por rest");
}

#[tokio::test]
async fn interface_on_another_origin_is_never_called() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // A PALS peer `a` holds a grant for, reached through `a`'s gateway.
    let b = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    a.hold(&grant(&b, &a, &[TaskSend, TaskRead], 3600));
    let gw = a.gateway_url();
    let home = cli_home(&a);
    // Another local server, which a card may name.
    let (victim_origin, victim) = echo_app_at().await;

    // Cards whose every interface is on another origin: the victim's, and
    // the gateway's own route to `bob`.
    let at_victim = card_json(&victim_origin);
    let (liar, _liar) = serve_at(move |_| echo_app_with_card(at_victim)).await;
    let mut at_gateway = card_json(&format!("{gw}/peers/bob"));
    at_gateway["supportedInterfaces"][1]["url"] = json!(format!("{gw}/peers/bob/rest"));
    let (sneak, _sneak) = serve_at(move |_| echo_app_with_card(at_gateway)).await;

    for (name, origin) in [("liar", &liar), ("sneak", &sneak)] {
        let no_interface = format!("no A2A 1.0 interface at {origin}");
        // Refused when saved.
        let (code, _, err) = pals(home, &["contact", "add-a2a", origin, "--name", name], &[]).await;
        assert_ne!(code, 0, "{name}");
        assert!(err.contains(&no_interface), "{name}: {err}");
        assert!(a.node.store.a2a_contact_by_name(name).unwrap().is_none());

        // Saved anyway, as a card can change after it was read: refused at
        // every use, before anything is sent.
        add_plain(&a, name, origin);
        let call = rpc_request("SendMessage", message_params("hola", None));
        for (method, path, body) in [
            ("POST", format!("/peers/{name}/jsonrpc"), Some(&call)),
            (
                "POST",
                format!("/peers/{name}/rest/message:send"),
                Some(&call),
            ),
            (
                "GET",
                format!("/peers/{name}/.well-known/agent-card.json"),
                None,
            ),
        ] {
            let res = gateway_request(&gw, method, &path, &VERSION, body).await;
            assert_eq!(res.status, 502, "{path}: {}", res.json);
            assert_eq!(res.json, json!({"error": no_interface}), "{path}");
        }
    }

    // A card with an interface of its own origin and one of another: only
    // its own is declared and called.
    let (mixed, _mixed) = serve_at(|origin| {
        let mut card = card_json(origin);
        card["supportedInterfaces"][1]["url"] = json!(format!("{victim_origin}/rest"));
        echo_app_with_card(card)
    })
    .await;
    add_plain(&a, "mixed", &mixed);
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/mixed/.well-known/agent-card.json",
        &VERSION,
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["supportedInterfaces"],
        json!([{"url": format!("{gw}/peers/mixed/jsonrpc"), "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"}])
    );
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/mixed/rest/message:send",
        &VERSION,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(res.status, 404, "{}", res.json);
    assert_eq!(res.json, json!({"error": "no HTTP+JSON interface"}));
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/mixed/jsonrpc",
        &VERSION,
        Some(&rpc_request("SendMessage", message_params("hola", None))),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(answer_in(&res.json["result"]), "echo: hola");

    // Nothing ever reached the other origins.
    assert!(victim.requests().is_empty(), "{:?}", victim.requests());
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

#[tokio::test]
async fn redirect_is_not_followed() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    let home = cli_home(&a);
    let (victim_origin, victim) = echo_app_at().await;

    // A real card, and bindings that redirect to the victim.
    let to = victim_origin.clone();
    let (hop, _hop) = serve_at(move |origin| {
        let (j, r, t) = (to.clone(), to.clone(), to.clone());
        card_app(card_json(origin))
            .route(
                "/jsonrpc",
                post(move || async move { redirect(307, format!("{j}/jsonrpc")) }),
            )
            .route(
                "/rest/message:send",
                post(move || async move { redirect(308, format!("{r}/rest/message:send")) }),
            )
            .route(
                "/rest/tasks/t-1",
                get(move || async move { redirect(302, format!("{t}/rest/tasks/t-1")) }),
            )
    })
    .await;
    add_plain(&a, "hop", &hop);
    let call = rpc_request("SendMessage", message_params("hola", None));
    for (method, path, body) in [
        ("POST", "/peers/hop/jsonrpc", Some(&call)),
        ("POST", "/peers/hop/rest/message:send", Some(&call)),
        ("GET", "/peers/hop/rest/tasks/t-1", None),
    ] {
        let res = gateway_request(&gw, method, path, &VERSION, body).await;
        assert_eq!(res.status, 502, "{path}: {}", res.json);
        assert_eq!(res.json, json!({"error": "peer protocol error"}), "{path}");
        assert!(res.headers.get("location").is_none(), "{path}");
        assert!(res.headers.get("set-cookie").is_none(), "{path}");
    }

    // A card that redirects: refused when saved, and at use.
    let to = victim_origin.clone();
    let (moved, _moved) = serve_at(move |_| {
        axum::Router::new().route(
            "/.well-known/agent-card.json",
            get(move || async move { redirect(301, format!("{to}/.well-known/agent-card.json")) }),
        )
    })
    .await;
    let (code, _, err) = pals(
        home,
        &["contact", "add-a2a", &moved, "--name", "moved"],
        &[],
    )
    .await;
    assert_ne!(code, 0);
    assert!(err.contains("301"), "{err}");
    assert!(a.node.store.a2a_contact_by_name("moved").unwrap().is_none());
    add_plain(&a, "moved", &moved);
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/moved/.well-known/agent-card.json",
        &VERSION,
        None,
    )
    .await;
    assert_eq!(res.status, 502, "{}", res.json);
    assert_eq!(res.json, json!({"error": "peer protocol error"}));
    assert!(matches!(
        fetch("moved", &moved).await,
        Err(PlainError::Refused(301))
    ));

    assert!(victim.requests().is_empty(), "{:?}", victim.requests());
}

#[tokio::test]
async fn http_to_a_non_loopback_host_is_refused_before_connecting() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    let home = cli_home(&a);
    // A server on another loopback address, not one of the three names
    // `http://` is allowed to: it counts the connections it accepts.
    let accepted = Arc::new(AtomicUsize::new(0));
    let other = match tokio::net::TcpListener::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).await {
        Ok(listener) => {
            let port = listener.local_addr().unwrap().port();
            let count = Arc::clone(&accepted);
            tokio::spawn(async move {
                while listener.accept().await.is_ok() {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            });
            Some(format!("http://127.0.0.2:{port}"))
        }
        // Not every system routes 127.0.0.2.
        Err(_) => None,
    };

    let only_loopback = "http:// is only for 127.0.0.1, [::1] and localhost";
    let mut refused: Vec<String> = vec![
        "http://192.0.2.1:9/".to_owned(),
        "http://example.com/agent".to_owned(),
    ];
    refused.extend(other.clone());
    for url in &refused {
        let (code, _, err) = pals(home, &["contact", "add-a2a", url, "--name", "far"], &[]).await;
        assert_ne!(code, 0, "{url}");
        assert!(err.contains(only_loopback), "{url}: {err}");
    }
    // Nor any other scheme, or credentials in the URL.
    for url in [
        "ftp://127.0.0.1/agent",
        "https://user:secret@example.com/",
        "not a url",
    ] {
        let (code, _, err) = pals(home, &["contact", "add-a2a", url, "--name", "far"], &[]).await;
        assert_ne!(code, 0, "{url}");
        assert!(err.contains("https://"), "{url}: {err}");
    }
    assert!(a.node.store.a2a_contacts().unwrap().is_empty());

    // Saved by other means: refused at use, as when it was saved.
    for url in &refused {
        let origin = url.trim_end_matches('/').to_owned();
        let contact = A2aContact {
            name: "far".to_owned(),
            card_url: format!("{origin}/.well-known/agent-card.json"),
            origin: origin.clone(),
            bearer_env: None,
            added_at: now_unix(),
        };
        a.node.store.a2a_contact_put(&contact).unwrap();
        let fetched = fetch_plain_card(&http_client(), &contact).await;
        assert!(
            matches!(&fetched, Err(PlainError::BadContact(why)) if why.contains(only_loopback)),
            "{url}: {:?}",
            fetched.err()
        );
        let res = gateway_request(
            &gw,
            "POST",
            "/peers/far/jsonrpc",
            &VERSION,
            Some(&rpc_request("SendMessage", message_params("hola", None))),
        )
        .await;
        assert_eq!(res.status, 502, "{url}: {}", res.json);
        assert!(
            res.json["error"].as_str().unwrap().contains(only_loopback),
            "{url}: {}",
            res.json
        );
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "{other:?} was connected to"
    );
}

#[tokio::test]
async fn oversized_or_invalid_card_is_refused() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    let home = cli_home(&a);

    // One byte too many, declared or not.
    let (big, _big) = serve_at(|origin| {
        let body = padded(&card_json(origin), MAX_CARD_BYTES + 1);
        axum::Router::new().route(
            "/.well-known/agent-card.json",
            get(move || async move { json_bytes(body) }),
        )
    })
    .await;
    let (huge, _huge) = serve_at(|_| {
        axum::Router::new().route(
            "/.well-known/agent-card.json",
            get(|| async { endless(b"[[[[[[[[") }),
        )
    })
    .await;
    // Exactly the limit is a card.
    let (fits, _fits) = serve_at(|origin| {
        let body = padded(&card_json(origin), MAX_CARD_BYTES);
        axum::Router::new().route(
            "/.well-known/agent-card.json",
            get(move || async move { json_bytes(body) }),
        )
    })
    .await;
    assert!(fetch("fits", &fits).await.is_ok());
    for (name, origin) in [("big", &big), ("huge", &huge)] {
        let fetched = fetch(name, origin).await;
        assert!(
            matches!(fetched, Err(PlainError::Invalid(CardError::TooLarge))),
            "{name}: {:?}",
            fetched.err()
        );
    }

    // Not a card: not JSON, not an Agent Card, no A2A 1.0 interface, or not
    // there.
    let not_a_card = |body: Vec<u8>| {
        move |_: &str| {
            axum::Router::new().route(
                "/.well-known/agent-card.json",
                get(move || async move { json_bytes(body) }),
            )
        }
    };
    let (garbage, _g) = serve_at(not_a_card(b"<html>card</html>".to_vec())).await;
    let (shapeless, _s) = serve_at(not_a_card(br#"{"name": 1}"#.to_vec())).await;
    let (old, _o) = serve_at(|origin| {
        let mut card = card_json(origin);
        for iface in card["supportedInterfaces"].as_array_mut().unwrap() {
            iface["protocolVersion"] = json!("0.3");
        }
        echo_app_with_card(card)
    })
    .await;
    let (missing, _m) = serve_at(|_| axum::Router::new()).await;
    assert!(matches!(
        fetch("garbage", &garbage).await,
        Err(PlainError::Invalid(CardError::NotJson))
    ));
    assert!(matches!(
        fetch("shapeless", &shapeless).await,
        Err(PlainError::Invalid(CardError::Invalid(_)))
    ));
    assert!(matches!(
        fetch("old", &old).await,
        Err(PlainError::Invalid(CardError::Invalid(
            "no A2A 1.0 interface"
        )))
    ));
    assert!(matches!(
        fetch("missing", &missing).await,
        Err(PlainError::Refused(404))
    ));

    // Neither is saved, and the gateway refuses to use them.
    let (code, _, err) = pals(home, &["contact", "add-a2a", &big, "--name", "big"], &[]).await;
    assert_ne!(code, 0);
    assert!(err.contains("card larger than 32768 bytes"), "{err}");
    let (code, _, err) = pals(
        home,
        &["contact", "add-a2a", &garbage, "--name", "garbage"],
        &[],
    )
    .await;
    assert_ne!(code, 0);
    assert!(err.contains("card is not JSON"), "{err}");
    assert!(a.node.store.a2a_contacts().unwrap().is_empty());
    add_plain(&a, "huge", &huge);
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/huge/.well-known/agent-card.json",
        &VERSION,
        None,
    )
    .await;
    assert_eq!(res.status, 502, "{}", res.json);
    assert_eq!(
        res.json,
        json!({"error": "peer card invalid: card larger than 32768 bytes"})
    );
    assert!(
        a.node
            .store
            .peer_card("a2a:huge", "public")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn jwks_over_16_kib_is_refused() {
    let key = AgentKey::generate();
    let jwks = okp_jwks("k1", &key);

    // One byte over the limit; and a set that never ends.
    let over = padded(&jwks, MAX_JWKS_BYTES + 1);
    assert_eq!(over.len(), 16 * 1024 + 1);
    let k = AgentKey::from_bytes(&key.to_bytes());
    let (big, _big) = serve_at(move |origin| signed_echo_app(origin, &k, over)).await;
    let k = AgentKey::from_bytes(&key.to_bytes());
    let (endless_set, _e) = serve_at(move |origin| {
        let jku = format!("{origin}/jwks.json");
        let card = sign(&card_json(origin), &k, &header("EdDSA", "k1", &jku));
        echo_app_with_card(card).route("/jwks.json", get(|| async { endless(b"{\"keys\":[") }))
    })
    .await;
    for (name, origin) in [("big", &big), ("endless", &endless_set)] {
        let plain = fetch(name, origin).await.expect("the card itself is good");
        assert_eq!(plain.signature, SignatureStatus::Unverifiable, "{name}");
    }

    // Exactly the limit verifies.
    let k = AgentKey::from_bytes(&key.to_bytes());
    let at_limit = padded(&jwks, MAX_JWKS_BYTES);
    let (fits, _fits) = serve_at(move |origin| signed_echo_app(origin, &k, at_limit)).await;
    let plain = fetch("fits", &fits).await.unwrap();
    assert_eq!(
        plain.signature,
        SignatureStatus::Verified {
            kid: "k1".to_owned()
        }
    );
}

#[tokio::test]
async fn okp_signed_plain_card_is_verified_with_its_kid() {
    let key = AgentKey::generate();
    let jwks = serde_json::to_vec(&okp_jwks("k1", &key)).unwrap();

    // EdDSA, kid `k1`, an OKP Ed25519 key in a JWK Set of the same origin.
    let (k, set) = (AgentKey::from_bytes(&key.to_bytes()), jwks.clone());
    let (good, good_agent) = serve_at(move |origin| signed_echo_app(origin, &k, set)).await;
    let plain = fetch("good", &good).await.unwrap();
    assert_eq!(
        plain.signature,
        SignatureStatus::Verified {
            kid: "k1".to_owned()
        }
    );
    let mut expected = echo_card_at(&good);
    expected.signatures = plain.card.signatures.clone();
    assert_eq!(plain.card, expected);
    assert_eq!(plain.hash, card_hash(&plain.json));
    assert_eq!(
        plain.jsonrpc_url.as_deref(),
        Some(&*format!("{good}/jsonrpc"))
    );
    assert_eq!(plain.rest_url.as_deref(), Some(&*format!("{good}/rest")));
    let jwks_reads = good_agent
        .requests()
        .iter()
        .filter(|r| r.uri == "/jwks.json")
        .count();
    assert_eq!(jwks_reads, 1);

    // The same kid with an EC key: a key this verifier does not take.
    let ec = json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1",
        "x": URL_SAFE_NO_PAD.encode([1u8; 32]), "y": URL_SAFE_NO_PAD.encode([2u8; 32])}]});
    let k = AgentKey::from_bytes(&key.to_bytes());
    let (ec_origin, _ec) =
        serve_at(move |origin| signed_echo_app(origin, &k, serde_json::to_vec(&ec).unwrap())).await;
    // Another algorithm in the header.
    let k = AgentKey::from_bytes(&key.to_bytes());
    let set = jwks.clone();
    let (es256, _es) = serve_at(move |origin| {
        let jku = format!("{origin}/jwks.json");
        let card = sign(&card_json(origin), &k, &header("ES256", "k1", &jku));
        echo_app_with_card(card).route("/jwks.json", get(move || async move { json_bytes(set) }))
    })
    .await;
    // A JWK Set on another origin, which is never read.
    let (elsewhere, elsewhere_server) = {
        let set = jwks.clone();
        serve_at(move |_| {
            axum::Router::new().route("/jwks.json", get(move || async move { json_bytes(set) }))
        })
        .await
    };
    let k = AgentKey::from_bytes(&key.to_bytes());
    let foreign_jku = format!("{elsewhere}/jwks.json");
    let (foreign, _f) = serve_at(move |origin| {
        let card = sign(&card_json(origin), &k, &header("EdDSA", "k1", &foreign_jku));
        echo_app_with_card(card)
    })
    .await;
    // A card changed after it was signed.
    let k = AgentKey::from_bytes(&key.to_bytes());
    let set = jwks.clone();
    let (altered, _a) = serve_at(move |origin| {
        let jku = format!("{origin}/jwks.json");
        let mut card = sign(&card_json(origin), &k, &header("EdDSA", "k1", &jku));
        card["description"] = json!("Does something else");
        echo_app_with_card(card).route("/jwks.json", get(move || async move { json_bytes(set) }))
    })
    .await;
    for (name, origin) in [
        ("ec", &ec_origin),
        ("es256", &es256),
        ("foreign", &foreign),
        ("altered", &altered),
    ] {
        let plain = fetch(name, origin).await.expect("the card itself is good");
        assert_eq!(plain.signature, SignatureStatus::Unverifiable, "{name}");
    }
    assert!(elsewhere_server.requests().is_empty());

    // No signature at all.
    let (unsigned, _u) = echo_app_at().await;
    let plain = fetch("unsigned", &unsigned).await.unwrap();
    assert_eq!(plain.signature, SignatureStatus::Unsigned);
}

#[tokio::test]
async fn proxy_environment_is_ignored() {
    let home = tempfile::tempdir().unwrap();
    let (code, _, err) = pals(home.path(), &["init"], &[]).await;
    assert_eq!(code, 0, "{err}");
    let (origin, agent) = echo_app_at().await;
    // A proxy no one listens at, for every scheme.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let proxies: Vec<(&str, &str)> = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .into_iter()
    .map(|name| (name, closed.as_str()))
    .collect();

    let (code, out, err) = pals(
        home.path(),
        &["contact", "add-a2a", &origin, "--name", "echo"],
        &proxies,
    )
    .await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("saved contact echo"), "{out}");
    assert!(
        !agent.requests().is_empty(),
        "the agent was reached directly"
    );
}

/// An agent run by the `pals` binary on its own home, with its gateway on a
/// free loopback port.
struct CliAgent {
    home: tempfile::TempDir,
    port: u16,
}

impl CliAgent {
    async fn init() -> Self {
        let home = tempfile::tempdir().unwrap();
        let (code, _, err) = pals(home.path(), &["init"], &[]).await;
        assert_eq!(code, 0, "{err}");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(
            home.path().join("config.toml"),
            format!("[gateway]\nlisten = \"127.0.0.1:{port}\"\n\n[net]\nrelay = false\n"),
        )
        .unwrap();
        Self { home, port }
    }

    fn gateway_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Starts `pals daemon` with `env` added and `unset` removed, and waits
    /// until it says it serves.
    async fn start_daemon(&self, env: &[(&str, &str)], unset: &[&str]) -> Daemon {
        let log = self.home.path().join("daemon.log");
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_pals"));
        for (name, value) in env {
            command.env(name, value);
        }
        for name in unset {
            command.env_remove(name);
        }
        let child = command
            .arg("daemon")
            .env("PALS_HOME", self.home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("start pals daemon");
        let daemon = Daemon { child, log };
        let serving = format!("gateway at {}", self.gateway_url());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let text = std::fs::read_to_string(&daemon.log).unwrap_or_default();
            if text.contains(&serving) {
                return daemon;
            }
            assert!(
                Instant::now() < deadline,
                "pals daemon did not start: {text}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// A `pals daemon` process, killed when dropped.
struct Daemon {
    child: Child,
    log: std::path::PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every file under `dir`, read whole.
fn files_under(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            files.push((path, bytes));
        }
    }
    files
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[tokio::test]
async fn bearer_comes_from_the_named_env_var_and_is_never_stored() {
    const VAR: &str = "PALS_T9_PLAIN_TOKEN";
    const UNSET: &str = "PALS_T9_PLAIN_TOKEN_UNSET";
    const SECRET: &str = "s3cret-token-of-the-plain-agent";
    let agent_home = CliAgent::init().await;
    let home = agent_home.home.path();
    let (origin, agent) = echo_app_at().await;

    // Saved with the variable's name, even when it is set.
    let (code, _, err) = pals(
        home,
        &[
            "contact",
            "add-a2a",
            &origin,
            "--name",
            "echo",
            "--bearer-env",
            VAR,
        ],
        &[(VAR, SECRET)],
    )
    .await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = pals(
        home,
        &[
            "contact",
            "add-a2a",
            &origin,
            "--name",
            "unset",
            "--bearer-env",
            UNSET,
        ],
        &[],
    )
    .await;
    assert_eq!(code, 0, "{err}");
    // A name that cannot be a variable's is refused.
    let (code, _, err) = pals(
        home,
        &[
            "contact",
            "add-a2a",
            &origin,
            "--name",
            "bad",
            "--bearer-env",
            "A-B=C",
        ],
        &[],
    )
    .await;
    assert_ne!(code, 0);
    assert!(err.contains("environment variable"), "{err}");
    // The card is read with no credentials.
    assert!(
        agent
            .requests()
            .iter()
            .all(|r| r.headers.get("authorization").is_none())
    );

    let daemon = agent_home.start_daemon(&[(VAR, SECRET)], &[UNSET]).await;
    let gw = agent_home.gateway_url();
    let send = rpc_request("SendMessage", message_params("hola", None));
    let authorization_of_last_call = || {
        let seen = agent.requests();
        let call = seen
            .iter()
            .rev()
            .find(|r| r.uri == "/jsonrpc")
            .expect("a call");
        call.headers
            .get("authorization")
            .map(|value| value.to_str().unwrap().to_owned())
    };

    // Read from the variable when the call is sent.
    let res = gateway_request(&gw, "POST", "/peers/echo/jsonrpc", &VERSION, Some(&send)).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(answer_in(&res.json["result"]), "echo: hola");
    assert_eq!(
        authorization_of_last_call(),
        Some(format!("Bearer {SECRET}"))
    );
    // The client's own Authorization passes intact.
    let own = [
        ("a2a-version", "1.0"),
        ("authorization", "Bearer client-own"),
    ];
    let res = gateway_request(&gw, "POST", "/peers/echo/jsonrpc", &own, Some(&send)).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        authorization_of_last_call(),
        Some("Bearer client-own".to_owned())
    );
    // A variable that is not set: refused here, and nothing sent.
    let before = agent.requests().len();
    let res = gateway_request(&gw, "POST", "/peers/unset/jsonrpc", &VERSION, Some(&send)).await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert_eq!(res.json, json!({"error": "bearer env var not set"}));
    assert_eq!(agent.requests().len(), before);
    // Cards are read with no credentials.
    assert!(
        agent
            .requests()
            .iter()
            .filter(|r| r.uri == "/.well-known/agent-card.json")
            .all(|r| r.headers.get("authorization").is_none())
    );
    drop(daemon);

    // The token is nowhere in the agent's files; the variable's name is.
    let files = files_under(home);
    for (path, bytes) in &files {
        assert!(!contains(bytes, SECRET), "{}", path.display());
    }
    let store = Store::open(&home.join("pals.db"), {
        let (_, id, _) = pals(home, &["id"], &[]).await;
        id.trim().parse().unwrap()
    })
    .unwrap();
    let saved = store.a2a_contact_by_name("echo").unwrap().unwrap();
    assert_eq!(saved.bearer_env.as_deref(), Some(VAR));
    assert!(store.a2a_contact_by_name("bad").unwrap().is_none());
}

#[tokio::test]
async fn grants_are_never_sent_to_a_plain_contact() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // `a` holds grants, for a PALS peer.
    let b = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    let held = grant(&b, &a, &[TaskSend, TaskRead], 3600);
    a.hold(&held);
    let (origin, agent) = echo_app_at().await;
    add_plain(&a, "echo", &origin);
    let gw = a.gateway_url();

    // Through the gateway, on both bindings, with headers only PALS sets.
    let own = [("a2a-version", "1.0"), ("x-agentnet-peer", "forged")];
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/echo/jsonrpc",
        &own,
        Some(&rpc_request("SendMessage", message_params("hola", None))),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/echo/rest/message:send",
        &own,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    // And through the outbox.
    let outcome = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "echo".to_owned(),
            question: "hola".to_owned(),
            attachments: Vec::new(),
            wait: WAIT,
            offer: None,
        })
        .await
        .expect("answered");
    assert_eq!(outcome.answer.as_deref(), Some("echo: hola"));

    let seen = agent.requests();
    assert!(seen.len() >= 3, "{seen:?}");
    let token = held.to_token();
    for request in &seen {
        assert!(
            request.headers.get("authorization").is_none(),
            "{request:?}"
        );
        for (name, value) in &request.headers {
            assert!(!name.as_str().starts_with("x-agentnet-"), "{request:?}");
            assert!(!contains(value.as_bytes(), &token), "{request:?}");
        }
    }
}

#[tokio::test]
async fn attachments_to_a_plain_contact_are_refused_locally() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let (origin, agent) = echo_app_at().await;
    add_plain(&a, "echo", &origin);
    let gw = a.gateway_url();
    let files = tempfile::tempdir().unwrap();
    let file = files.path().join("notes.txt");
    std::fs::write(&file, "private notes").unwrap();

    let asked = a
        .node
        .outbox()
        .ask(AskRequest {
            peer: "echo".to_owned(),
            question: "hola".to_owned(),
            attachments: vec![file],
            wait: WAIT,
            offer: None,
        })
        .await;
    assert_eq!(
        asked,
        Err(AskError::Local("attachments need a PALS peer".to_owned()))
    );
    assert!(a.node.store.outbox_all().unwrap().is_empty());

    // Through the gateway: a part naming a blob of this gateway, or one
    // between peers.
    let upload = common::gateway_send_raw(
        &gw,
        "POST",
        "/blobs",
        &[],
        Some(("application/octet-stream", b"private notes".to_vec())),
    )
    .await;
    let created: Value =
        serde_json::from_slice(&upload.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let blob_url = created["url"].as_str().unwrap().to_owned();
    let wire_url = format!(
        "iroh-blob://{}/{}",
        a.id(),
        created["hash"].as_str().unwrap()
    );
    for url in [blob_url, wire_url] {
        let mut params = message_params("hola", None);
        params["message"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"url": url, "filename": "notes.txt", "mediaType": "text/plain"}));
        for (path, body) in [
            (
                "/peers/echo/jsonrpc",
                rpc_request("SendMessage", params.clone()),
            ),
            ("/peers/echo/rest/message:send", params),
        ] {
            let res = gateway_request(&gw, "POST", path, &VERSION, Some(&body)).await;
            assert_eq!(res.status, 400, "{path}: {}", res.json);
            assert_eq!(
                res.json,
                json!({"error": "files go only to PALS peers"}),
                "{path}"
            );
        }
    }
    assert!(agent.requests().is_empty(), "{:?}", agent.requests());
}

#[tokio::test]
async fn contact_names_are_one_namespace() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_node(WorkerCfg::Echo).await;
    let home = cli_home(&a);
    let (origin, _agent) = echo_app_at().await;
    let store = &a.node.store;
    a.add_contact("bob", &b);

    // A PALS contact's name is not a plain contact's, nor the reverse.
    let err = store
        .a2a_contact_put(&plain_contact("bob", &origin))
        .unwrap_err();
    assert!(format!("{err:#}").contains("bob"), "{err:#}");
    let (code, _, err) = pals(home, &["contact", "add-a2a", &origin, "--name", "bob"], &[]).await;
    assert_ne!(code, 0);
    assert!(err.contains("bob"), "{err}");
    add_plain(&a, "echo", &origin);
    let pals_echo = Contact {
        name: "echo".to_owned(),
        agent_id: b.id(),
        ticket: Some(b.node.ticket()),
        added_at: now_unix(),
    };
    assert!(store.contact_put(&pals_echo).is_err());
    let ticket = b.node.ticket();
    let (code, _, err) = pals(home, &["contact", "add", &ticket, "--name", "echo"], &[]).await;
    assert_ne!(code, 0);
    assert!(err.contains("echo"), "{err}");
    assert_eq!(store.contact_by_name("echo").unwrap(), None);

    // An agent id is no plain contact's name, nor is anything that is not a
    // plain path segment.
    let id = b.id().to_string();
    for name in [id.as_str(), "a/b", "..", "", "with space", "a2a:echo"] {
        assert!(
            store
                .a2a_contact_put(&plain_contact(name, &origin))
                .is_err(),
            "{name:?}"
        );
    }
    let (code, _, err) = pals(home, &["contact", "add-a2a", &origin, "--name", &id], &[]).await;
    assert_ne!(code, 0);
    assert!(err.contains("agent id"), "{err}");

    // `{peer}` is an agent id first, then a PALS contact, then a plain one.
    assert_eq!(
        store.resolve_peer(&id).unwrap(),
        Some(PeerRef::Pals(b.id()))
    );
    assert_eq!(
        store.resolve_peer("bob").unwrap(),
        Some(PeerRef::Pals(b.id()))
    );
    let plain = store
        .resolve_peer("echo")
        .unwrap()
        .expect("the plain contact");
    assert_eq!(plain.key(), "a2a:echo");
    assert_eq!(PeerRef::Pals(b.id()).key(), id);
    assert_eq!(store.resolve_peer("nobody").unwrap(), None);
}

#[tokio::test]
async fn unsafe_task_id_from_a_plain_agent_is_refused_like_a_peers() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    let victim = AgentKey::generate().id();
    let climbing = format!("../../../{victim}/rest/tasks/x");
    let task_named =
        |id: &str| json!({"id": id, "contextId": "c-1", "status": {"state": "TASK_STATE_WORKING"}});
    let (c1, c2, c3) = (climbing.clone(), climbing.clone(), climbing.clone());
    let (evil, _evil) = serve_at(move |origin| {
        card_app(card_json(origin))
            .route(
                "/rest/tasks/climbing",
                get(move || {
                    let task = json!({"id": c1, "contextId": "c-1",
                        "status": {"state": "TASK_STATE_WORKING"}});
                    async move { json_bytes(task.to_string().into_bytes()) }
                }),
            )
            .route(
                "/jsonrpc",
                post(move || {
                    let message = json!({"messageId": "m", "role": "ROLE_AGENT",
                        "taskId": c2, "parts": [{"text": "x"}]});
                    let reply = json!({"jsonrpc": "2.0", "id": 1, "result": {"message": message}});
                    async move { json_bytes(reply.to_string().into_bytes()) }
                }),
            )
            .route(
                "/rest/message:stream",
                post(move || {
                    let event = |value: Value| format!("data: {value}\n\n");
                    let safe = json!({"task": {"id": "t-1", "contextId": "c-1",
                        "status": {"state": "TASK_STATE_WORKING"}}});
                    let stream = format!(
                        "{}{}{}",
                        event(safe.clone()),
                        event(json!({"statusUpdate": {"taskId": c3, "contextId": "c-1",
                            "status": {"state": "TASK_STATE_COMPLETED"}}})),
                        event(safe),
                    );
                    async move {
                        axum::response::Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(Body::from(stream))
                            .unwrap()
                    }
                }),
            )
    })
    .await;
    add_plain(&a, "evil", &evil);

    let call = rpc_request("SendMessage", message_params("hola", None));
    for (method, path, body) in [
        ("GET", "/peers/evil/rest/tasks/climbing", None),
        ("POST", "/peers/evil/jsonrpc", Some(&call)),
    ] {
        let res = gateway_request(&gw, method, path, &VERSION, body).await;
        assert_eq!(
            res.status,
            502,
            "{path}: {}",
            String::from_utf8_lossy(&res.body)
        );
        assert_eq!(res.json, json!({"error": "peer protocol error"}), "{path}");
        assert!(
            !String::from_utf8_lossy(&res.body).contains("../"),
            "{path}"
        );
    }

    // A stream: the safe event comes, then it breaks before the unsafe one.
    let response = gateway_send(
        &gw,
        "POST",
        "/peers/evil/rest/message:stream",
        &VERSION,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(response.status(), 200);
    let (received, ended_cleanly) = read_until_end(response).await;
    let received = String::from_utf8_lossy(&received).into_owned();
    assert!(!ended_cleanly, "{received}");
    assert_eq!(
        received,
        format!("data: {}\n\n", json!({"task": task_named("t-1")}))
    );
}

/// The card of an agent at `origin` that streams, serving the bindings
/// `served`, declaring the idempotency extension when `idempotent`.
fn streaming_card(origin: &str, served: Served, idempotent: bool) -> Value {
    let mut card = card_json(origin);
    if let Served::JsonRpc = served {
        let jsonrpc = card["supportedInterfaces"][0].clone();
        card["supportedInterfaces"] = json!([jsonrpc]);
    }
    if idempotent {
        card["capabilities"]["extensions"] = json!([{"uri": common::IDEMPOTENCY_EXTENSION}]);
    }
    card
}

/// `200` with `events` as an event stream, one `data:` line each.
fn event_stream(events: &[Value]) -> axum::response::Response {
    let body: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

/// An agent whose card is `card` and whose every question, at
/// `/rest/message:stream` and at `/jsonrpc`, it counts in `asked` and
/// answers with `answer`.
fn counted_answer_app(
    card: Value,
    asked: Arc<AtomicUsize>,
    answer: fn() -> axum::response::Response,
) -> axum::Router {
    let (rest, rpc) = (Arc::clone(&asked), asked);
    card_app(card)
        .route(
            "/rest/message:stream",
            post(move || {
                rest.fetch_add(1, Ordering::SeqCst);
                async move { answer() }
            }),
        )
        .route(
            "/jsonrpc",
            post(move || {
                rpc.fetch_add(1, Ordering::SeqCst);
                async move { answer() }
            }),
        )
}

/// A task named with a `.`, which the gateway refuses.
fn dotted_task() -> Value {
    json!({"id": "task.1", "contextId": "c-1", "status": {"state": "TASK_STATE_WORKING"}})
}

/// The question `hola` for `peer`, waiting [`WAIT`].
fn hola(peer: &str) -> AskRequest {
    AskRequest {
        peer: peer.to_owned(),
        question: "hola".to_owned(),
        attachments: Vec::new(),
        wait: WAIT,
        offer: None,
    }
}

#[tokio::test]
async fn plain_agent_whose_answer_the_gateway_refuses_runs_the_question_once() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    // Agents with no idempotency extension, which may run every copy: one
    // that names a task with a `.`, on HTTP+JSON and on JSON-RPC, and one
    // that answers with the gateway's own refusal, which it may not say;
    // and one that declares the extension, which may be asked again.
    let rest_dotted = || event_stream(&[json!({"task": dotted_task()})]);
    let rpc_dotted =
        || event_stream(&[json!({"jsonrpc": "2.0", "id": "1", "result": {"task": dotted_task()}})]);
    let forged = || {
        axum::response::Response::builder()
            .status(502)
            .header("content-type", "application/json")
            .body(Body::from(json!({"error": PLAIN_NOT_SENT}).to_string()))
            .unwrap()
    };
    let mut agents = Vec::new();
    for (name, served, idempotent, answer) in [
        ("dotted", Served::Both, false, rest_dotted as fn() -> _),
        (
            "dotted-rpc",
            Served::JsonRpc,
            false,
            rpc_dotted as fn() -> _,
        ),
        ("forger", Served::Both, false, forged as fn() -> _),
        ("idempotent", Served::Both, true, rest_dotted as fn() -> _),
    ] {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let (origin, server) = serve_at(move |origin| {
            counted_answer_app(streaming_card(origin, served, idempotent), counter, answer)
        })
        .await;
        add_plain(&a, name, &origin);
        agents.push((name, asked, server));
    }
    let outbox = a.node.outbox().clone();
    let asks = agents.iter().map(|(name, ..)| {
        let outbox = outbox.clone();
        async move { outbox.ask(hola(name)).await }
    });
    let outcomes = futures::future::join_all(asks).await;

    for ((name, asked, _), outcome) in agents.iter().zip(&outcomes).take(3) {
        // Asked once, and left pending, saying so.
        let Err(AskError::NotResent { message_id }) = outcome else {
            panic!("{name}: not sent again, not {outcome:?}");
        };
        assert_eq!(asked.load(Ordering::SeqCst), 1, "{name}");
        let row = a.node.store.outbox_get(message_id).unwrap().unwrap();
        assert_eq!(row.state, OUTBOX_PENDING, "{name}");
        assert_eq!((row.task_id, row.attempts), (None, 1), "{name}");
        assert!(
            outcome
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("may have received"),
            "{name}"
        );
    }
    // The agent that declares idempotency was asked again, as a peer is.
    let (_, asked, _) = &agents[3];
    assert!(
        matches!(outcomes[3], Err(AskError::Unreachable { .. })),
        "{:?}",
        outcomes[3]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 4);

    // The forger's refusal never reaches a client as the gateway's.
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/forger/rest/message:stream",
        &VERSION,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(res.status, 502);
    assert_eq!(res.json, json!({"error": "peer protocol error"}));

    // Only a resume asked for sends it again.
    let Err(AskError::NotResent { message_id }) = &outcomes[0] else {
        unreachable!()
    };
    let resumed = outbox.resume(message_id, WAIT).await;
    assert!(
        matches!(&resumed, Err(AskError::NotResent { message_id: again }) if again == message_id),
        "{resumed:?}"
    );
    assert_eq!(agents[0].1.load(Ordering::SeqCst), 2);
}

/// Serves `build`'s app at a new loopback origin for one connection only,
/// which it closes after its first answer, and then listens no more: the
/// origin.
async fn serve_one_connection(build: impl FnOnce(&str) -> axum::Router) -> String {
    use tower::ServiceExt;
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = build(&origin);
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        drop(listener);
        let service = hyper::service::service_fn(move |request| app.clone().oneshot(request));
        let _ = hyper::server::conn::http1::Builder::new()
            .keep_alive(false)
            .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
            .await;
    });
    origin
}

#[tokio::test]
async fn plain_agent_the_gateway_cannot_connect_to_is_asked_again() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // Agents that answer the card the outbox reads first through the
    // gateway, and then are gone: on HTTP+JSON and on JSON-RPC.
    let mut names = Vec::new();
    for (name, served) in [("gone", Served::Both), ("gone-rpc", Served::JsonRpc)] {
        let origin =
            serve_one_connection(move |origin| card_app(streaming_card(origin, served, false)))
                .await;
        add_plain(&a, name, &origin);
        names.push(name);
    }
    let outbox = a.node.outbox().clone();
    let asks = names.iter().map(|name| {
        let outbox = outbox.clone();
        async move { outbox.ask(hola(name)).await }
    });
    for (name, outcome) in names.iter().zip(futures::future::join_all(asks).await) {
        // The gateway sent nothing, each time: asked again, then kept.
        let Err(AskError::Unreachable { message_id }) = &outcome else {
            panic!("{name}: unreachable, not {outcome:?}");
        };
        let row = a.node.store.outbox_get(message_id).unwrap().unwrap();
        assert_eq!(row.state, OUTBOX_PENDING, "{name}");
        assert_eq!((row.task_id, row.attempts), (None, 4), "{name}");
    }
}

/// A stand-in for a gateway at a new loopback origin: it answers the first
/// connection's request with `card`, as a gateway serves a plain
/// contact's card, and closes it; then, `cut`, it reads the head of the
/// next connection's request and closes it with no answer, or else it
/// listens no more. Its origin.
async fn fake_gateway(card: Value, cut: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn read_head(stream: &mut tokio::net::TcpStream) {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).await.is_ok() {
            head.push(byte[0]);
        }
    }
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        read_head(&mut stream).await;
        let body = card.to_string();
        let answer = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(answer.as_bytes()).await;
        drop(stream);
        if !cut {
            return;
        }
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        read_head(&mut stream).await;
        drop(stream);
        std::future::pending::<()>().await;
    });
    origin
}

#[tokio::test]
async fn a_question_resent_only_when_the_gateway_was_never_reached() {
    let store = Arc::new(Store::open_in_memory(AgentKey::generate().id()).unwrap());
    store
        .a2a_contact_put(&plain_contact("x", "http://127.0.0.1:9"))
        .unwrap();
    // The card a gateway serves of a plain agent that streams, with no
    // idempotency extension; where its interfaces say does not matter.
    let plain = streaming_card("http://127.0.0.1:9", Served::Both, false);
    let view = gateway_view_for(
        &serde_json::from_value(plain).unwrap(),
        "http://127.0.0.1:1",
        "x",
    );
    for (cut, attempts) in [(false, 4), (true, 1)] {
        let gateway = fake_gateway(serde_json::to_value(&view).unwrap(), cut).await;
        let asked = Outbox::new(Arc::clone(&store), gateway)
            .ask(hola("x"))
            .await;
        let message_id = match (&asked, cut) {
            // No connection to the gateway: sent again, then kept.
            (Err(AskError::Unreachable { message_id }), false) => message_id,
            // It may have left: not sent again.
            (Err(AskError::NotResent { message_id }), true) => message_id,
            other => panic!("{other:?}"),
        };
        let row = store.outbox_get(message_id).unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.attempts),
            (OUTBOX_PENDING, attempts)
        );
    }
}

/// The CRC-32 of `data`, as gzip has it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `data` as a gzip member, in stored deflate blocks.
fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    if data.is_empty() {
        out.extend([1, 0, 0, 0xff, 0xff]);
    }
    let mut blocks = data.chunks(usize::from(u16::MAX)).peekable();
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = u16::try_from(block.len()).unwrap();
        out.extend(len.to_le_bytes());
        out.extend((!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend(crc32(data).to_le_bytes());
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out
}

/// `app`, answering in gzip whenever the request's `Accept-Encoding`
/// allows it, as HTTPS front ends do for JSON.
fn compressing(app: axum::Router) -> axum::Router {
    app.layer(axum::middleware::from_fn(
        |request: axum::extract::Request, next: axum::middleware::Next| async move {
            let gzip_allowed = request
                .headers()
                .get_all("accept-encoding")
                .iter()
                .any(|value| value.to_str().is_ok_and(|value| value.contains("gzip")));
            let response = next.run(request).await;
            if !gzip_allowed {
                return response;
            }
            let (mut parts, body) = response.into_parts();
            let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            parts.headers.remove("content-length");
            parts.headers.insert(
                "content-encoding",
                axum::http::HeaderValue::from_static("gzip"),
            );
            axum::response::Response::from_parts(parts, Body::from(gzip(&bytes)))
        },
    ))
}

#[tokio::test]
async fn plain_agent_that_compresses_is_asked_for_identity() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    let (origin, agent) =
        serve_at(|origin| compressing(echo_app_with_card(card_json(origin)))).await;
    // It compresses for a client that allows it.
    let direct = gateway_send(
        &origin,
        "GET",
        "/.well-known/agent-card.json",
        &[("accept-encoding", "gzip, deflate")],
        None,
    )
    .await;
    assert_eq!(direct.headers()["content-encoding"], "gzip");
    add_plain(&a, "zip", &origin);
    let before = agent.requests().len();

    // A stock client that allows gzip and deflate, as httpx does.
    let headers = [("a2a-version", "1.0"), ("accept-encoding", "gzip, deflate")];
    let calls = [
        (
            "/peers/zip/rest/message:send",
            message_params("por rest", None),
        ),
        (
            "/peers/zip/jsonrpc",
            rpc_request("SendMessage", message_params("por jsonrpc", None)),
        ),
    ];
    for (path, body) in &calls {
        let res = gateway_request(&gw, "POST", path, &headers, Some(body)).await;
        assert_eq!(
            res.status,
            200,
            "{path}: {}",
            String::from_utf8_lossy(&res.body)
        );
        assert!(res.headers.get("content-encoding").is_none(), "{path}");
    }
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/zip/.well-known/agent-card.json",
        &headers,
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    // Every request of the gateway's, cards included, asked for identity.
    let seen = agent.requests();
    assert!(seen.len() > before);
    for request in &seen[before..] {
        assert_eq!(
            request.headers["accept-encoding"], "identity",
            "{request:?}"
        );
    }
}

#[tokio::test]
async fn a_plain_agents_blob_urls_pass_unchanged() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let gw = a.gateway_url();
    // A blob URL naming this very agent as its holder, which the gateway
    // would serve from its store, or fetch, if it were rewritten.
    let blob = format!("iroh-blob://{}/{}", a.id(), "0".repeat(64));
    let task = json!({"id": "t-1", "contextId": "c-1",
        "status": {"state": "TASK_STATE_COMPLETED"},
        "artifacts": [{"artifactId": "a-1", "parts": [{"url": blob, "filename": "x.txt"}]}]});
    let (unary, streamed) = (task.clone(), task.clone());
    let (origin, _agent) = serve_at(move |origin| {
        card_app(card_json(origin))
            .route(
                "/rest/message:send",
                post(move || {
                    let reply = json!({"task": unary});
                    async move { json_bytes(reply.to_string().into_bytes()) }
                }),
            )
            .route(
                "/rest/message:stream",
                post(move || {
                    let event = json!({"task": streamed});
                    async move { event_stream(&[event]) }
                }),
            )
    })
    .await;
    add_plain(&a, "blobs", &origin);

    let res = gateway_request(
        &gw,
        "POST",
        "/peers/blobs/rest/message:send",
        &VERSION,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["task"]["artifacts"][0]["parts"][0]["url"], blob);
    let response = gateway_send(
        &gw,
        "POST",
        "/peers/blobs/rest/message:stream",
        &VERSION,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(response.status(), 200);
    let (received, ended_cleanly) = read_until_end(response).await;
    assert!(ended_cleanly);
    let received = String::from_utf8(received).unwrap();
    assert_eq!(received, format!("data: {}\n\n", json!({"task": task})));
    assert!(!received.contains("/blobs/"), "{received}");
}

#[tokio::test]
async fn the_gateways_own_origin_is_no_plain_contact() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // A PALS peer that a holds a grant of, which an alias through a's own
    // gateway would reach with that grant.
    let b = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    a.hold(&grant(&b, &a, &[TaskSend, TaskRead], 3600));
    let gw = a.gateway_url();
    let port = gw.rsplit(':').next().unwrap().to_owned();
    let home = cli_home(&a);
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"127.0.0.1:{port}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    let own = "a plain contact cannot be this agent's own gateway";

    for (name, origin) in [
        ("alias", format!("http://127.0.0.1:{port}")),
        ("alias-name", format!("http://localhost:{port}")),
    ] {
        // Refused when saved.
        let url = format!("{origin}/peers/bob");
        let (code, _, err) = pals(home, &["contact", "add-a2a", &url, "--name", name], &[]).await;
        assert_ne!(code, 0, "{name}");
        assert!(err.contains(own), "{name}: {err}");
        assert!(a.node.store.a2a_contact_by_name(name).unwrap().is_none());

        // Saved by other means: refused at every use.
        a.node
            .store
            .a2a_contact_put(&A2aContact {
                name: name.to_owned(),
                card_url: format!("{url}/.well-known/agent-card.json"),
                origin: origin.clone(),
                bearer_env: None,
                added_at: now_unix(),
            })
            .unwrap();
        let call = rpc_request("SendMessage", message_params("hola", None));
        for (method, path, body) in [
            ("POST", format!("/peers/{name}/jsonrpc"), Some(&call)),
            (
                "GET",
                format!("/peers/{name}/.well-known/agent-card.json"),
                None,
            ),
        ] {
            let res = gateway_request(&gw, method, &path, &VERSION, body).await;
            assert_eq!(res.status, 502, "{path}: {}", res.json);
            assert!(
                res.json["error"].as_str().unwrap().contains(own),
                "{path}: {}",
                res.json
            );
        }
        let asked = a.node.outbox().ask(hola(name)).await;
        assert!(
            matches!(&asked, Err(AskError::Local(why)) if why.contains(own)),
            "{asked:?}"
        );
    }
    // A search discards them at their card.
    let outcome = a
        .node
        .searcher()
        .search(SearchRequest {
            capability: "code.rust.review".to_owned(),
            peers: Some(vec!["alias".to_owned(), "alias-name".to_owned()]),
            ..SearchRequest::default()
        })
        .await
        .unwrap();
    for name in ["alias", "alias-name"] {
        let discarded = outcome
            .discarded
            .iter()
            .find(|d| d.peer == name)
            .unwrap_or_else(|| panic!("{name}: {outcome:#?}"));
        assert_eq!(
            discarded.reason, "card: the contact cannot be a plain contact",
            "{name}"
        );
    }
    // Nothing reached b through the alias.
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

#[tokio::test]
async fn contact_add_a2a_shows_the_card_s_words_as_printable_ascii() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let home = cli_home(&a);
    let key = AgentKey::generate();
    let kid = "k\u{1b}[2J1";
    let jwks = serde_json::to_vec(&okp_jwks(kid, &key)).unwrap();
    let (origin, _agent) = serve_at(move |origin| {
        let mut card = card_json(origin);
        card["name"] = json!("plain\u{1b}]0;owned\u{7}\necho");
        let jku = format!("{origin}/jwks.json");
        let card = sign(&card, &key, &header("EdDSA", kid, &jku));
        card_app(card).route("/jwks.json", get(move || async move { json_bytes(jwks) }))
    })
    .await;
    let (code, out, err) = pals(home, &["contact", "add-a2a", &origin, "--name", "esc"], &[]).await;
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("(plain]0;ownedecho), card signature verified k[2J1"),
        "{out:?}"
    );
    assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
    assert_eq!(out.lines().count(), 1, "{out:?}");
}
