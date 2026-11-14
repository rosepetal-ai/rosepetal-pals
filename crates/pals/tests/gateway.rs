//! The loopback gateway, the standard A2A entry: a stock A2A client reaches
//! any PALS peer at `<gateway>/peers/<peer>` with the JSON-RPC or HTTP+JSON
//! binding, the gateway presenting the grant it holds for that peer, behind
//! the local guard (no `Origin`, a loopback `Host`); and the `daemon`,
//! `id --ticket`, `contact` and `grant` commands that set it up.

mod common;

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use a2a::{
    GetTaskRequest, Message, Part, Role, SendMessageRequest, SendMessageResponse, StreamResponse,
    TRANSPORT_PROTOCOL_HTTP_JSON, Task, TaskState,
};
use a2a_client::A2AClientFactory;
use a2a_client::agent_card::AgentCardResolver;
use common::{
    CountingExecutor, TestNode, TestResponse, TestServer, gateway_request, gateway_send, grant,
    issue_grant, message_params, now_unix, rpc_request, spawn_node, spawn_node_with_executor,
    spawn_pair, test_config,
};
use futures::StreamExt;
use http_body_util::BodyExt;
use iroh::EndpointAddr;
use iroh_tickets::endpoint::EndpointTicket;
use pals::a2a::{canonical_card, gateway_view};
use pals::config::WorkerCfg;
use pals::net::{agent_id_of, endpoint_id_of};
use pals_core::grant::Operation::{self, TaskCancel, TaskRead, TaskSend};
use pals_core::grant::{GRANT_SCHEMA, Grant, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::Inventory;
use pals_core::store::{Contact, Store};
use serde_json::{Value, json};

const SEND_AND_READ: [Operation; 2] = [TaskSend, TaskRead];

/// A requester `a`, whose gateway the tests use, and a provider `b`, which
/// `a` reaches as contact `bob` with a grant of `b` that `a` holds.
async fn connected_pair() -> (TestNode, TestNode, SignedGrant) {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_node(WorkerCfg::Echo).await;
    let g = connect(&a, &b);
    (a, b, g)
}

/// Saves `b` in `a` as contact `bob`, and gives `a` a grant of `b` to send
/// and read tasks, which `a` holds.
fn connect(a: &TestNode, b: &TestNode) -> SignedGrant {
    a.add_contact("bob", b);
    let g = grant(b, a, &SEND_AND_READ, 3600);
    a.hold(&g);
    g
}

/// A JSON-RPC call to `peer` through `a`'s gateway, with `A2A-Version: 1.0`
/// and `headers`.
async fn gw_rpc(
    a: &TestNode,
    peer: &str,
    headers: &[(&str, &str)],
    method: &str,
    params: Value,
) -> TestResponse {
    let mut all = vec![("a2a-version", "1.0")];
    all.extend_from_slice(headers);
    gateway_request(
        &a.gateway_url(),
        "POST",
        &format!("/peers/{peer}/jsonrpc"),
        &all,
        Some(&rpc_request(method, params)),
    )
    .await
}

/// A resolver for Agent Cards, as a stock A2A client uses.
///
/// The stock client's HTTP client, reqwest, is built with rustls and no
/// crypto provider (iroh's choice of features), so a provider is installed
/// first: `ring`, the one iroh uses.
fn card_resolver() -> AgentCardResolver {
    // Another test may have installed it already.
    let _ = rustls::crypto::ring::default_provider().install_default();
    AgentCardResolver::new(None)
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

fn task_of(reply: &SendMessageResponse) -> &Task {
    match reply {
        SendMessageResponse::Task(task) => task,
        SendMessageResponse::Message(message) => panic!("a message instead of a task: {message:?}"),
    }
}

fn state_of(reply: &SendMessageResponse) -> TaskState {
    task_of(reply).status.state.clone()
}

/// The text of the first part of the task's first artifact.
fn answer_of(task: &Task) -> &str {
    task.artifacts.as_ref().expect("an artifact")[0].parts[0]
        .as_text()
        .expect("a text part")
}

/// The JSON of every `data:` line of an SSE body, in order.
fn sse_data(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| serde_json::from_str(data.trim()).expect("an SSE event is JSON"))
        .collect()
}

#[tokio::test]
async fn stock_a2a_client_talks_to_a_peer_through_the_gateway() {
    let (a, b, _) = connected_pair().await;
    let base = format!("{}/peers/{}", a.gateway_url(), b.id());
    let card = card_resolver()
        .resolve(&base)
        .await
        .expect("resolve the card through the gateway");
    let client = A2AClientFactory::builder()
        .build()
        .create_from_card(&card)
        .await
        .expect("a client from the card");

    let reply = client
        .send_message(&send_request("hola"))
        .await
        .expect("SendMessage through the gateway");
    assert_eq!(state_of(&reply), TaskState::Completed);
    assert_eq!(answer_of(task_of(&reply)), "echo: hola");

    // The same task, read back with GetTask.
    let id = task_of(&reply).id.clone();
    let task = client
        .get_task(&GetTaskRequest {
            id: id.clone(),
            history_length: None,
            tenant: None,
        })
        .await
        .expect("GetTask through the gateway");
    assert_eq!(task.id, id);
    assert_eq!(task.status.state, TaskState::Completed);

    // Streaming, with the same stock client: the task, its artifact and its
    // completion.
    let events: Vec<StreamResponse> = client
        .send_streaming_message(&send_request("otra"))
        .await
        .expect("SendStreamingMessage through the gateway")
        .map(|event| event.expect("a stream event"))
        .collect()
        .await;
    assert!(
        matches!(events.first(), Some(StreamResponse::Task(_))),
        "{events:?}"
    );
    match events.last() {
        Some(StreamResponse::StatusUpdate(update)) => {
            assert_eq!(update.status.state, TaskState::Completed)
        }
        other => panic!("the stream did not end with a status update: {other:?}"),
    }
}

#[tokio::test]
async fn jsonrpc_endpoint_takes_a_trailing_slash() {
    // A client that joins a path to the interface URL, as `httpx` does with
    // a base URL (the A2A TCK's JSON-RPC client posts `/` to it), sends
    // `.../jsonrpc/`: the same endpoint.
    let (a, b, _) = connected_pair().await;
    let headers = [("a2a-version", "1.0")];
    let send = rpc_request("SendMessage", message_params("hola", None));
    let res = gateway_request(
        &a.gateway_url(),
        "POST",
        &format!("/peers/{}/jsonrpc/", b.id()),
        &headers,
        Some(&send),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let task = &res.json["result"]["task"];
    assert_eq!(
        task["status"]["state"], "TASK_STATE_COMPLETED",
        "{}",
        res.json
    );
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "echo: hola");

    // Streaming too, by contact name.
    let stream = rpc_request("SendStreamingMessage", message_params("otra", None));
    let res = gateway_request(
        &a.gateway_url(),
        "POST",
        "/peers/bob/jsonrpc/",
        &headers,
        Some(&stream),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let events = sse_data(&String::from_utf8_lossy(&res.body));
    assert_eq!(
        events.last().unwrap()["result"]["statusUpdate"]["status"]["state"],
        "TASK_STATE_COMPLETED",
        "{events:?}"
    );

    // Only the one slash: anything after it is no endpoint.
    let res = gateway_request(
        &a.gateway_url(),
        "POST",
        "/peers/bob/jsonrpc/x",
        &headers,
        Some(&send),
    )
    .await;
    assert_eq!(res.status, 404);
}

#[tokio::test]
async fn rest_binding_works_through_the_gateway() {
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();
    let headers = [("a2a-version", "1.0")];

    let res = gateway_request(
        &gw,
        "POST",
        &format!("/peers/{}/rest/message:send", b.id()),
        &headers,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let task = &res.json["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "echo: hola");
    let id = task["id"].as_str().expect("a task id");

    // GET, with a path parameter, by contact name.
    let path = format!("/peers/bob/rest/tasks/{id}");
    let res = gateway_request(&gw, "GET", &path, &headers, None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["id"], id);
    assert!(
        !res.json["history"].as_array().unwrap().is_empty(),
        "{}",
        res.json
    );

    // The query string reaches the peer as sent.
    let res = gateway_request(
        &gw,
        "GET",
        &format!("{path}?historyLength=0"),
        &headers,
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["id"], id);
    assert!(
        res.json
            .get("history")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "{}",
        res.json
    );

    // A stock client that prefers the HTTP+JSON binding.
    let card = card_resolver()
        .resolve(&format!("{gw}/peers/{}", b.id()))
        .await
        .expect("resolve the card through the gateway");
    let (client, interface) = A2AClientFactory::builder()
        .preferred_bindings(vec![TRANSPORT_PROTOCOL_HTTP_JSON.to_owned()])
        .build()
        .create_from_card_with_interface(&card)
        .await
        .expect("a client from the card");
    assert_eq!(interface.protocol_binding, TRANSPORT_PROTOCOL_HTTP_JSON);
    let reply = client
        .send_message(&send_request("por rest"))
        .await
        .expect("SendMessage over HTTP+JSON");
    assert_eq!(state_of(&reply), TaskState::Completed);
    assert_eq!(answer_of(task_of(&reply)), "echo: por rest");
}

#[tokio::test]
async fn gateway_view_declares_standard_bindings_and_no_security() {
    // No grant is needed for the card: it is public.
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    let gw = a.gateway_url();

    let res = gateway_request(
        &gw,
        "GET",
        &format!("/peers/{}/.well-known/agent-card.json", b.id()),
        &[],
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let card = &res.json;
    assert_eq!(card["supportedInterfaces"].as_array().unwrap().len(), 2);
    assert_eq!(
        card["supportedInterfaces"][0]["url"],
        format!("{gw}/peers/{}/jsonrpc", b.id())
    );
    assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
    assert_eq!(card["supportedInterfaces"][0]["protocolVersion"], "1.0");
    assert_eq!(
        card["supportedInterfaces"][1]["url"],
        format!("{gw}/peers/{}/rest", b.id())
    );
    assert_eq!(
        card["supportedInterfaces"][1]["protocolBinding"],
        "HTTP+JSON"
    );
    assert_eq!(card["supportedInterfaces"][1]["protocolVersion"], "1.0");
    assert!(card.get("securitySchemes").is_none());
    assert!(card.get("securityRequirements").is_none());
    assert!(card.get("signatures").is_none());

    // Otherwise it is the peer's canonical card.
    let canonical = canonical_card(
        &test_config(b.dir.path(), WorkerCfg::Echo),
        &b.id(),
        &Inventory::default(),
    );
    assert_eq!(
        &serde_json::to_value(gateway_view(&canonical, &gw, &b.id())).unwrap(),
        card
    );
    let mut rest = card.clone();
    let mut expected = serde_json::to_value(&canonical).unwrap();
    for value in [&mut rest, &mut expected] {
        let object = value.as_object_mut().unwrap();
        for key in [
            "supportedInterfaces",
            "securitySchemes",
            "securityRequirements",
            "signatures",
        ] {
            object.remove(key);
        }
    }
    assert_eq!(rest, expected);

    // By contact name, the same view: its URLs name the peer by its id.
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/bob/.well-known/agent-card.json",
        &[],
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(&res.json, card);
}

#[tokio::test]
async fn sse_events_arrive_before_the_task_finishes() {
    // How long the executor works between its first event and the rest.
    const DELAY: Duration = Duration::from_secs(5);
    let a = spawn_node(WorkerCfg::Echo).await;
    let executor = CountingExecutor::new(DELAY);
    let b = spawn_node_with_executor(executor.clone()).await;
    connect(&a, &b);

    let response = gateway_send(
        &a.gateway_url(),
        "POST",
        "/peers/bob/jsonrpc",
        &[("a2a-version", "1.0"), ("accept", "text/event-stream")],
        Some(&rpc_request(
            "SendStreamingMessage",
            message_params("slow", None),
        )),
    )
    .await;
    assert_eq!(response.status(), 200);
    let content_type = response.headers()[http::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );

    // The first event arrives on its own, while the executor still works.
    let mut body = response.into_body();
    let mut received = String::new();
    while !received.contains("\n\n") {
        let frame = body
            .frame()
            .await
            .expect("an event before the end of the stream")
            .expect("a readable stream");
        if let Ok(data) = frame.into_data() {
            received.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
    let first_at = Instant::now();
    let first = &sse_data(&received)[0]["result"]["task"];
    assert_eq!(first["status"]["state"], "TASK_STATE_WORKING", "{received}");
    let id = first["id"].as_str().expect("a task id").to_owned();
    let res = gw_rpc(&a, "bob", &[], "GetTask", json!({"id": id})).await;
    assert_eq!(
        res.json["result"]["status"]["state"], "TASK_STATE_WORKING",
        "{}",
        res.json
    );

    // The rest follows once the executor is done, well after the first
    // event: a gateway that held the response back would deliver them
    // together.
    let rest = body
        .collect()
        .await
        .expect("the rest of the stream")
        .to_bytes();
    let gap = first_at.elapsed();
    assert!(
        gap >= DELAY / 2,
        "the end came {gap:?} after the first event"
    );
    received.push_str(std::str::from_utf8(&rest).unwrap());
    let events = sse_data(&received);
    let last = &events.last().unwrap()["result"]["statusUpdate"];
    assert_eq!(
        last["status"]["state"], "TASK_STATE_COMPLETED",
        "{received}"
    );
    assert_eq!(executor.runs(), 1);
}

#[tokio::test]
async fn no_held_grant_is_a_local_401_and_the_peer_is_not_contacted() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    let gw = a.gateway_url();
    let no_grant = json!({"error": format!("no grant held for {}", b.id())});

    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert_eq!(res.json, no_grant);
    let res = gateway_request(
        &gw,
        "POST",
        &format!("/peers/{}/rest/message:send", b.id()),
        &[("a2a-version", "1.0")],
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert_eq!(res.json, no_grant);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);

    // A held grant for another peer does not count.
    let c = spawn_node(WorkerCfg::Echo).await;
    a.hold(&grant(&c, &a, &SEND_AND_READ, 3600));
    // Nor does an expired one.
    a.hold(&grant(&b, &a, &SEND_AND_READ, -3600));
    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert_eq!(res.json, no_grant);

    // The client's own Authorization goes through intact: a bad one is the
    // peer's refusal, a good one the peer's answer.
    let res = gw_rpc(
        &a,
        "bob",
        &[("authorization", "Bearer not-a-grant")],
        "SendMessage",
        message_params("hola", None),
    )
    .await;
    assert_eq!(res.status, 401, "{}", res.json);
    let challenge = res.header("www-authenticate");
    assert!(challenge.contains("malformed"), "{challenge}");
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    let bearer = common::bearer(&g);
    let res = gw_rpc(
        &a,
        "bob",
        &[("authorization", bearer.as_str())],
        "SendMessage",
        message_params("hola", None),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );

    // With the peer gone, still a local 401 and not a 502: it is never
    // dialed.
    let TestNode {
        node, dir: _dir, ..
    } = b;
    let b_id = node.id;
    node.shutdown().await.unwrap();
    let started = Instant::now();
    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert_eq!(
        res.json,
        json!({"error": format!("no grant held for {b_id}")})
    );
    // Well below the 10 s that dialing it would take, on a loaded host too.
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn unknown_peer_is_404_and_unreachable_peer_is_502() {
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();
    let unknown = json!({"error": "unknown peer"});
    let unreachable = json!({"error": "peer unreachable"});

    // Neither a contact name nor an agent id.
    for (method, path) in [
        ("POST", "/peers/nobody/jsonrpc"),
        ("POST", "/peers/nobody/rest/message:send"),
        ("GET", "/peers/nobody/rest/tasks"),
        ("GET", "/peers/nobody/.well-known/agent-card.json"),
    ] {
        let body =
            (method == "POST").then(|| rpc_request("SendMessage", message_params("hola", None)));
        let res =
            gateway_request(&gw, method, path, &[("a2a-version", "1.0")], body.as_ref()).await;
        assert_eq!(res.status, 404, "{path}: {}", res.json);
        assert_eq!(res.json, unknown, "{path}");
    }

    // An agent id with no address known and no address lookup.
    let stranger = AgentKey::generate();
    let now = now_unix();
    a.hold(&issue_grant(
        &stranger,
        a.id(),
        stranger.id(),
        &SEND_AND_READ,
        now,
        now + 3600,
    ));
    let res = gw_rpc(
        &a,
        &stranger.id().to_string(),
        &[],
        "SendMessage",
        message_params("hola", None),
    )
    .await;
    assert_eq!(res.status, 502, "{}", res.json);
    assert_eq!(res.json, unreachable);

    // Contacts saved for the stranger with another running agent's address,
    // or with that agent's own ticket: that agent never answers for the
    // stranger, card included.
    let impostor = spawn_node(WorkerCfg::Echo).await;
    let socket = *impostor.node.addr().ip_addrs().next().expect("an address");
    let stranger_at_impostor =
        EndpointTicket::new(EndpointAddr::new(endpoint_id_of(&stranger.id())).with_ip_addr(socket));
    for (name, ticket) in [
        ("stranger", stranger_at_impostor.to_string()),
        ("mallory", impostor.node.ticket()),
    ] {
        a.node
            .store
            .contact_put(&Contact {
                name: name.to_owned(),
                agent_id: stranger.id(),
                ticket: Some(ticket),
                added_at: now,
            })
            .unwrap();
        let rpc = gw_rpc(&a, name, &[], "SendMessage", message_params("hola", None)).await;
        let card = gateway_request(
            &gw,
            "GET",
            &format!("/peers/{name}/.well-known/agent-card.json"),
            &[],
            None,
        )
        .await;
        for res in [rpc, card] {
            assert_eq!(res.status, 502, "{name}: {}", res.json);
            assert_eq!(res.json, unreachable, "{name}");
        }
    }
    assert_eq!(
        impostor.node.store.events_count("task.accepted").unwrap(),
        0
    );

    // A contact whose daemon has stopped: nothing answers at its address.
    let TestNode {
        node, dir: _dir, ..
    } = b;
    node.shutdown().await.unwrap();
    let started = Instant::now();
    let (rpc, card) = tokio::join!(
        gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)),
        gateway_request(
            &gw,
            "GET",
            "/peers/bob/.well-known/agent-card.json",
            &[],
            None
        ),
    );
    for res in [rpc, card] {
        assert_eq!(res.status, 502, "{}", res.json);
        assert_eq!(res.json, unreachable);
    }
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn request_with_origin_header_is_403() {
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();

    // Without Origin, the health check answers.
    let res = gateway_request(&gw, "GET", "/healthz", &[], None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json,
        json!({"agent_id": a.id().to_string(), "name": "pals", "ticket": a.node.ticket()})
    );

    for origin in ["http://evil.example", "null", &gw] {
        let headers = [("origin", origin), ("a2a-version", "1.0")];
        for (method, path) in [
            ("GET", "/healthz".to_owned()),
            (
                "GET",
                format!("/peers/{}/.well-known/agent-card.json", b.id()),
            ),
            ("POST", "/peers/bob/jsonrpc".to_owned()),
            ("POST", "/peers/bob/rest/message:send".to_owned()),
            ("GET", "/no/such/route".to_owned()),
        ] {
            let body = (method == "POST")
                .then(|| rpc_request("SendMessage", message_params("hola", None)));
            let res = gateway_request(&gw, method, &path, &headers, body.as_ref()).await;
            assert_eq!(res.status, 403, "{origin} {path}: {}", res.json);
        }
    }
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

#[tokio::test]
async fn request_with_foreign_host_header_is_403() {
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();
    let port = gw.rsplit(':').next().unwrap().to_owned();
    let other_port = if port == "7420" { "7421" } else { "7420" };

    for host in [
        "evil.example:7420".to_owned(),
        format!("evil.example:{port}"),
        format!("127.0.0.1:{other_port}"),
        format!("localhost.evil.example:{port}"),
        "127.0.0.1".to_owned(),
    ] {
        let headers = [("host", host.as_str()), ("a2a-version", "1.0")];
        for (method, path) in [
            ("GET", "/healthz".to_owned()),
            (
                "GET",
                format!("/peers/{}/.well-known/agent-card.json", b.id()),
            ),
            ("POST", "/peers/bob/jsonrpc".to_owned()),
            ("POST", "/peers/bob/rest/message:send".to_owned()),
        ] {
            let body = (method == "POST")
                .then(|| rpc_request("SendMessage", message_params("hola", None)));
            let res = gateway_request(&gw, method, &path, &headers, body.as_ref()).await;
            assert_eq!(res.status, 403, "{host} {path}: {}", res.json);
        }
    }
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);

    // The loopback names of the gateway's own port are accepted.
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("LOCALHOST:{port}"),
        format!("[::1]:{port}"),
    ] {
        let res = gateway_request(&gw, "GET", "/healthz", &[("host", host.as_str())], None).await;
        assert_eq!(res.status, 200, "{host}: {}", res.json);
    }
}

#[tokio::test]
async fn contact_name_and_agent_id_address_the_same_peer() {
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();
    let b_id = b.id().to_string();

    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 200, "{}", res.json);
    let id = res.json["result"]["task"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The task belongs to `a` at `b`, whichever name reaches it.
    let res = gw_rpc(&a, &b_id, &[], "GetTask", json!({"id": id})).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["result"]["id"], id.as_str());
    assert_eq!(
        res.json["result"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    let res = gw_rpc(&a, &b_id, &[], "ListTasks", json!({})).await;
    let tasks = res.json["result"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{}", res.json);
    assert_eq!(tasks[0]["id"], id.as_str());

    let by_name = gateway_request(
        &gw,
        "GET",
        "/peers/bob/.well-known/agent-card.json",
        &[],
        None,
    )
    .await;
    let by_id = gateway_request(
        &gw,
        "GET",
        &format!("/peers/{b_id}/.well-known/agent-card.json"),
        &[],
        None,
    )
    .await;
    assert_eq!(by_name.status, 200, "{}", by_name.json);
    assert_eq!(by_name.json, by_id.json);
}

/// A peer that does not run PALS: `app` served over the tunnel as it is,
/// which `a` saves as contact `name` and holds a task grant for.
async fn rogue_peer(a: &TestNode, name: &str, app: axum::Router) -> TestServer {
    let (server, _client) = spawn_pair(app).await;
    let id = agent_id_of(&server.addr.id);
    a.node
        .store
        .contact_put(&Contact {
            name: name.to_owned(),
            agent_id: id,
            ticket: Some(EndpointTicket::new(server.addr.clone()).to_string()),
            added_at: now_unix(),
        })
        .unwrap();
    let now = now_unix();
    a.hold(&issue_grant(
        &AgentKey::generate(),
        a.id(),
        id,
        &SEND_AND_READ,
        now,
        now + 3600,
    ));
    server
}

/// What a hostile peer answers: a script page with its own cookie and
/// policy, as a success and as an error, and redirects that send the client
/// to `/peers/carol/...`.
fn rogue_app() -> axum::Router {
    use axum::routing::{get, post};
    let page = |status: u16| {
        move || async move {
            (
                http::StatusCode::from_u16(status).unwrap(),
                [
                    ("content-type", "text/html"),
                    ("set-cookie", "session=stolen; Path=/"),
                    ("set-cookie2", "legacy=stolen"),
                    ("content-security-policy", "default-src *"),
                ],
                "<script>fetch('/peers/carol/rest/tasks')</script>",
            )
        }
    };
    let redirect = |status: u16| {
        move || async move {
            (
                http::StatusCode::from_u16(status).unwrap(),
                [
                    ("location", "/peers/carol/jsonrpc"),
                    ("set-cookie", "session=stolen; Path=/"),
                ],
            )
        }
    };
    axum::Router::new()
        .route("/rest/page", get(page(200)))
        .route("/rest/error-page", get(page(404)))
        .route("/jsonrpc", post(redirect(307)))
        .route("/rest/message:send", post(redirect(308)))
        .route("/rest/see-other", get(redirect(303)))
        .route("/.well-known/agent-card.json", get(redirect(302)))
}

/// Whether `res` carries the headers that keep a browser from running what
/// it gets from the gateway.
fn assert_sandboxed(res: &TestResponse, what: &str) {
    let policies: Vec<_> = res
        .headers
        .get_all("content-security-policy")
        .iter()
        .collect();
    assert_eq!(policies, ["sandbox"], "{what}");
    assert_eq!(res.header("x-content-type-options"), "nosniff", "{what}");
}

#[tokio::test]
async fn browser_requests_are_403_and_every_response_is_sandboxed() {
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();
    let refused = json!({"error": "browser requests are not accepted"});

    // Fetch Metadata marks a browser's request, a navigation included, even
    // where it sends no Origin: what a browser sends navigating to the
    // gateway, and either of the headers that only browsers send.
    let navigation = [
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-dest", "document"),
    ];
    let marks: [&[(&str, &str)]; 3] = [
        &navigation,
        &[("sec-fetch-dest", "document")],
        &[("sec-fetch-site", "same-origin")],
    ];
    for headers in marks {
        for (method, path) in [
            ("GET", "/healthz".to_owned()),
            ("GET", format!("/peers/{}/rest/tasks", b.id())),
            ("GET", "/peers/bob/.well-known/agent-card.json".to_owned()),
            ("POST", "/peers/bob/jsonrpc".to_owned()),
        ] {
            let body = (method == "POST")
                .then(|| rpc_request("SendMessage", message_params("hola", None)));
            let mut all = headers.to_vec();
            all.push(("a2a-version", "1.0"));
            let res = gateway_request(&gw, method, &path, &all, body.as_ref()).await;
            assert_eq!(res.status, 403, "{headers:?} {path}: {}", res.json);
            assert_eq!(res.json, refused, "{headers:?} {path}");
            assert_sandboxed(&res, path.as_str());
        }
    }
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);

    // Every answer, the gateway's own and the peers', is sandboxed.
    let res = gateway_request(&gw, "GET", "/healthz", &[], None).await;
    assert_eq!(res.status, 200);
    assert_sandboxed(&res, "healthz");
    let res = gateway_request(&gw, "GET", "/peers/nobody/rest/tasks", &[], None).await;
    assert_eq!(res.status, 404);
    assert_sandboxed(&res, "unknown peer");
    let res = gateway_request(&gw, "GET", "/healthz", &[("origin", "null")], None).await;
    assert_eq!(res.status, 403);
    assert_sandboxed(&res, "origin");
    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_sandboxed(&res, "peer answer");

    // A page a peer serves cannot run at the gateway's origin: as a
    // success, which A2A answers with JSON only, it never reaches the
    // client; as an error, it comes sandboxed.
    let _rogue = rogue_peer(&a, "mallory", rogue_app()).await;
    let res = gateway_request(&gw, "GET", "/peers/mallory/rest/page", &[], None).await;
    assert_eq!(res.status, 502, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.json, json!({"error": "peer protocol error"}));
    assert_sandboxed(&res, "a peer's page");
    let res = gateway_request(&gw, "GET", "/peers/mallory/rest/error-page", &[], None).await;
    assert_eq!(res.status, 404, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.header("content-type"), "text/html");
    assert_sandboxed(&res, "a peer's error page");
}

#[tokio::test]
async fn node_fetch_clients_with_only_sec_fetch_mode_are_served() {
    // Node's built-in fetch (undici), the default transport of the A2A JS
    // SDK, sends `sec-fetch-mode: cors` on every request, and neither
    // `sec-fetch-site` nor `sec-fetch-dest`.
    let (a, b, _) = connected_pair().await;
    let gw = a.gateway_url();
    let node_fetch = [
        ("sec-fetch-mode", "cors"),
        ("accept", "*/*"),
        ("accept-language", "*"),
        ("user-agent", "node"),
        ("a2a-version", "1.0"),
    ];

    let res = gateway_request(
        &gw,
        "GET",
        &format!("/peers/{}/.well-known/agent-card.json", b.id()),
        &node_fetch,
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["supportedInterfaces"][0]["url"],
        format!("{gw}/peers/{}/jsonrpc", b.id())
    );

    let res = gateway_request(
        &gw,
        "POST",
        "/peers/bob/jsonrpc",
        &node_fetch,
        Some(&rpc_request("SendMessage", message_params("hola", None))),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let task = &res.json["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "echo: hola");
    assert_sandboxed(&res, "a Node fetch client");

    let res = gateway_request(
        &gw,
        "POST",
        "/peers/bob/rest/message:send",
        &node_fetch,
        Some(&message_params("por rest", None)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["task"]["status"]["state"], "TASK_STATE_COMPLETED");
}

#[tokio::test]
async fn peer_redirects_and_cookies_never_reach_the_client() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // Where the redirects point: a peer `a` holds a grant for.
    let carol = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("carol", &carol);
    a.hold(&grant(&carol, &a, &SEND_AND_READ, 3600));
    let _rogue = rogue_peer(&a, "mallory", rogue_app()).await;
    let gw = a.gateway_url();
    let protocol_error = json!({"error": "peer protocol error"});

    let call = rpc_request("SendMessage", message_params("hola", None));
    for (method, path, body) in [
        ("POST", "/peers/mallory/jsonrpc", Some(&call)),
        ("POST", "/peers/mallory/rest/message:send", Some(&call)),
        ("GET", "/peers/mallory/rest/see-other", None),
        ("GET", "/peers/mallory/.well-known/agent-card.json", None),
    ] {
        let res = gateway_request(&gw, method, path, &[("a2a-version", "1.0")], body).await;
        assert_eq!(res.status, 502, "{path}: {}", res.json);
        assert_eq!(res.json, protocol_error, "{path}");
        assert!(res.headers.get("location").is_none(), "{path}");
        assert!(res.headers.get("set-cookie").is_none(), "{path}");
    }
    assert_eq!(carol.node.store.events_count("task.accepted").unwrap(), 0);

    // A peer's cookies are dropped from what it answers.
    let res = gateway_request(&gw, "GET", "/peers/mallory/rest/error-page", &[], None).await;
    assert_eq!(res.status, 404);
    assert!(res.headers.get("set-cookie").is_none(), "{:?}", res.headers);
    assert!(
        res.headers.get("set-cookie2").is_none(),
        "{:?}",
        res.headers
    );
}

/// A task named `id`, as a peer answers with it.
fn task_named(id: &str) -> Value {
    json!({"id": id, "contextId": "c-1", "status": {"state": "TASK_STATE_WORKING"}})
}

/// `200` with `body` as `content_type`.
fn answer(
    content_type: &'static str,
    body: impl Into<axum::body::Body>,
) -> axum::response::Response {
    axum::response::Response::builder()
        .header("content-type", content_type)
        .body(body.into())
        .unwrap()
}

/// One SSE event whose data is `value`.
fn sse_event(value: &Value) -> String {
    format!("data: {value}\n\n")
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
async fn peer_task_ids_that_are_not_safe_never_reach_the_client() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // The peer whose grant the gateway would present to a client that
    // followed `<gw>/peers/<evil>/rest/tasks/../../../<victim>/rest/tasks/x`,
    // as WHATWG URL parsing and httpx resolve it.
    let victim = AgentKey::generate().id();
    let climbing = format!("../../../{victim}/rest/tasks/x");
    let evil_app = {
        use axum::routing::{get, post};
        let (c1, c2, c3, c4, c5) = (
            climbing.clone(),
            climbing.clone(),
            climbing.clone(),
            climbing.clone(),
            climbing.clone(),
        );
        axum::Router::new()
            // A task, as GetTask answers.
            .route(
                "/rest/tasks/climbing",
                get(move || {
                    let task = task_named(&c1);
                    async move { answer("application/json", task.to_string()) }
                }),
            )
            // The same, labelled as something else.
            .route(
                "/rest/tasks/as-text",
                get(move || {
                    let task = task_named(&c2);
                    async move { answer("text/plain", task.to_string()) }
                }),
            )
            // A message that names the task, in a JSON-RPC result.
            .route(
                "/jsonrpc",
                post(move || {
                    let message = json!({"messageId": "m", "role": "ROLE_AGENT",
                        "taskId": c3.clone(), "parts": [{"text": "x"}]});
                    let reply = json!({"jsonrpc": "2.0", "id": 1, "result": {"message": message}});
                    async move { answer("application/json", reply.to_string()) }
                }),
            )
            // JSON that only lenient parsers read, with NaN.
            .route(
                "/rest/tasks/lenient",
                get(move || {
                    let task = task_named(&c4).to_string();
                    let body = format!("{}, \"n\": NaN}}", &task[..task.len() - 1]);
                    async move { answer("application/json", body) }
                }),
            )
            // A safe task, then an update that names the climbing id, then
            // more.
            .route(
                "/rest/message:stream",
                post(move || {
                    let stream = format!(
                        "{}{}{}",
                        sse_event(&json!({"task": task_named("t-1")})),
                        sse_event(
                            &json!({"statusUpdate": {"taskId": c5.clone(), "contextId": "c-1",
                            "status": {"state": "TASK_STATE_COMPLETED"}}})
                        ),
                        sse_event(&json!({"task": task_named("t-2")})),
                    );
                    async move { answer("text/event-stream", stream) }
                }),
            )
            // Encoded bytes the gateway cannot read.
            .route(
                "/rest/tasks/encoded",
                get(|| async {
                    axum::response::Response::builder()
                        .header("content-type", "application/json")
                        .header("content-encoding", "gzip")
                        .body(axum::body::Body::from(vec![0x1f, 0x8b, 8, 0]))
                        .unwrap()
                }),
            )
            .route(
                "/rest/tasks/safe",
                get(|| async { answer("application/json", task_named("t-1").to_string()) }),
            )
    };
    let _evil = common::stub_peer(&a, "evil", &AgentKey::generate(), evil_app).await;
    let gw = a.gateway_url();
    let version = [("a2a-version", "1.0")];

    // A safe answer passes, as it was.
    let res = gateway_request(&gw, "GET", "/peers/evil/rest/tasks/safe", &version, None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json, task_named("t-1"));

    // Unary answers: a protocol error, and nothing of the id.
    let call = rpc_request("SendMessage", message_params("hola", None));
    for (method, path, body) in [
        ("GET", "/peers/evil/rest/tasks/climbing", None),
        ("GET", "/peers/evil/rest/tasks/as-text", None),
        ("POST", "/peers/evil/jsonrpc", Some(&call)),
        ("GET", "/peers/evil/rest/tasks/lenient", None),
        ("GET", "/peers/evil/rest/tasks/encoded", None),
    ] {
        let res = gateway_request(&gw, method, path, &version, body).await;
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
        &version,
        Some(&message_params("hola", None)),
    )
    .await;
    assert_eq!(response.status(), 200);
    let (received, ended_cleanly) = read_until_end(response).await;
    let received = String::from_utf8_lossy(&received).into_owned();
    assert!(!ended_cleanly, "{received}");
    assert_eq!(received, sse_event(&json!({"task": task_named("t-1")})));
}

#[tokio::test]
async fn peer_responses_are_bounded() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let endless = |chunk: &'static [u8]| {
        let chunk = axum::body::Bytes::from_static(chunk);
        axum::body::Body::from_stream(futures::stream::repeat_with(move || {
            Ok::<_, std::io::Error>(chunk.clone())
        }))
    };
    let flood: &'static [u8] = "x".repeat(64 * 1024).leak().as_bytes();
    let lines: &'static [u8] = "data: x\n".repeat(8 * 1024).leak().as_bytes();
    let evil_app = {
        use axum::routing::{get, post};
        axum::Router::new()
            .route(
                "/rest/tasks/flood",
                get(move || async move { answer("text/plain", endless(flood)) }),
            )
            .route(
                "/rest/message:stream",
                post(move || async move { answer("text/event-stream", endless(lines)) }),
            )
    };
    let _evil = common::stub_peer(&a, "evil", &AgentKey::generate(), evil_app).await;
    let gw = a.gateway_url();
    let version = [("a2a-version", "1.0")];
    let bound = Duration::from_secs(120);

    // An endless body, whatever its type: refused once past the bound.
    let res = tokio::time::timeout(
        bound,
        gateway_request(&gw, "GET", "/peers/evil/rest/tasks/flood", &version, None),
    )
    .await
    .expect("the gateway gave up on an endless body");
    assert_eq!(res.status, 502, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.json, json!({"error": "peer protocol error"}));

    // An event that never ends: the stream breaks, and none of it came.
    let streamed = async {
        let response = gateway_send(
            &gw,
            "POST",
            "/peers/evil/rest/message:stream",
            &version,
            Some(&message_params("hola", None)),
        )
        .await;
        assert_eq!(response.status(), 200);
        read_until_end(response).await
    };
    let (received, ended_cleanly) = tokio::time::timeout(bound, streamed)
        .await
        .expect("the gateway gave up on an endless event");
    assert!(!ended_cleanly);
    assert!(received.is_empty(), "{} bytes came", received.len());

    // The gateway still serves.
    let res = gateway_request(&gw, "GET", "/healthz", &[], None).await;
    assert_eq!(res.status, 200);
}

/// The line breaks some SSE readers honour besides CR and LF: httpx splits
/// lines as `str.splitlines()` does.
const OTHER_LINE_BREAKS: [&str; 8] = [
    "\u{2028}", "\u{2029}", "\u{85}", "\u{b}", "\u{c}", "\u{1c}", "\u{1d}", "\u{1e}",
];

/// The kinds of line a `data:` line can hide behind.
const HIDING_LINES: [&str; 4] = [": hi", "event: x", "id: 7", "foo: bar"];

/// A peer that answers what each test case in its query asks for, every
/// answer naming a task id that climbs to another peer.
fn lying_app() -> axum::Router {
    use axum::extract::Request;
    use axum::routing::{any, post};
    let climbing = "../../../0000/rest/tasks/x";
    let query = |request: &Request| -> String {
        request
            .uri()
            .query()
            .unwrap_or_default()
            .trim_start_matches("case=")
            .to_owned()
    };
    let endless_keepalives = || {
        let chunk = axum::body::Bytes::from(":\n\n".repeat(16 * 1024));
        axum::body::Body::from_stream(futures::stream::repeat_with(move || {
            Ok::<_, std::io::Error>(chunk.clone())
        }))
    };
    let stream = move |request: Request| async move {
        let case = query(&request);
        let safe = sse_event(&json!({"task": task_named("t-1")}));
        let body = if let Some(hidden) = case.strip_prefix("hide-") {
            // `<kind>-<break>`: a data line behind another line break.
            let (kind, brk) = hidden.split_once('-').unwrap();
            let kind = HIDING_LINES[kind.parse::<usize>().unwrap()];
            let brk = OTHER_LINE_BREAKS[brk.parse::<usize>().unwrap()];
            let lie = json!({"task": task_named(climbing)});
            format!("{kind}{brk}data: {lie}\n\n{safe}")
        } else {
            let event = match case.as_str() {
                "snake-update" => json!({"statusUpdate": {"task_id": climbing,
                    "context_id": "c-1", "status": {"state": "TASK_STATE_WORKING"}}}),
                "bare-task" => json!({"task": {"id": climbing}}),
                // A JSON-RPC envelope's id means nothing on HTTP+JSON.
                "enveloped" => json!({"jsonrpc": "2.0", "id": climbing,
                    "result": {"task": task_named("t-1")}}),
                "unicode" => json!({"artifactUpdate": {"taskId": "t-1", "contextId": "c-1",
                    "artifact": {"artifactId": "a-1", "parts": [{"text": "ñandú 😀 \u{2028}\u{85}"}]}}}),
                other => panic!("no case {other}"),
            };
            format!("{safe}{}", sse_event(&event))
        };
        answer("text/event-stream", body)
    };
    axum::Router::new()
        .route("/rest/message:stream", post(stream))
        // Unary calls answered as event streams.
        .route(
            "/rest/message:send",
            post(move |request: Request| async move {
                match query(&request).as_str() {
                    "endless" => answer("text/event-stream", endless_keepalives()),
                    _ => answer(
                        "text/event-stream",
                        json!({"task": task_named(climbing)}).to_string(),
                    ),
                }
            }),
        )
        .route(
            "/rest/tasks/{id}",
            // CancelTask, answered with a task that wears a JSON-RPC
            // member.
            post(move || async move {
                answer(
                    "application/json",
                    json!({"jsonrpc": "2.0", "id": climbing, "contextId": "c-1",
                        "status": {"state": "TASK_STATE_CANCELED"}})
                    .to_string(),
                )
            })
            .get(move |axum::extract::Path(id): axum::extract::Path<String>| async move {
                match id.as_str() {
                    "endless" => answer("text/event-stream", endless_keepalives()),
                    "as-stream" => {
                        answer("text/event-stream", task_named(climbing).to_string())
                    }
                    // A task with no contextId nor status, as a lenient
                    // parser takes it.
                    "bare" => answer("application/json", json!({"id": climbing}).to_string()),
                    "enveloped" => answer(
                        "application/json",
                        json!({"jsonrpc": "2.0", "id": climbing, "contextId": "c-1",
                            "status": {"state": "TASK_STATE_WORKING"}})
                        .to_string(),
                    ),
                    "snake" => answer(
                        "application/json",
                        json!({"id": climbing, "context_id": "c-1",
                            "status": {"state": "TASK_STATE_WORKING"}})
                        .to_string(),
                    ),
                    "referenced" => answer(
                        "application/json",
                        json!({"id": "t-1", "contextId": "c-1",
                            "status": {"state": "TASK_STATE_WORKING"},
                            "history": [{"messageId": "m-1", "role": "ROLE_USER",
                                "parts": [{"text": "x"}], "reference_task_ids": ["t-0", climbing]}]})
                        .to_string(),
                    ),
                    "free-form" => answer(
                        "application/json",
                        json!({"id": "t-1", "contextId": "c-1",
                            "status": {"state": "TASK_STATE_WORKING"},
                            "metadata": {"id": climbing, "taskId": climbing},
                            "artifacts": [{"artifactId": "a-1",
                                "parts": [{"data": {"id": climbing, "task_id": climbing}}]}]})
                        .to_string(),
                    ),
                    "unicode" => answer(
                        "application/json; charset=utf-8",
                        json!({"id": "t-1", "contextId": "c-1",
                            "status": {"state": "TASK_STATE_WORKING",
                                "message": {"messageId": "m-1", "role": "ROLE_AGENT",
                                    "parts": [{"text": "ñandú 😀 \u{2028}\u{85}"}]}}})
                        .to_string(),
                    ),
                    other => panic!("no task {other}"),
                }
            }),
        )
        .route(
            "/jsonrpc",
            any(move |body: axum::body::Bytes| async move {
                let request: Value = serde_json::from_slice(&body).unwrap();
                let lie = json!({"task": task_named(climbing)});
                match request["method"].as_str().unwrap() {
                    // A streaming call: an event hidden behind U+2028.
                    "SendStreamingMessage" => answer(
                        "text/event-stream",
                        format!(
                            ": hi\u{2028}data: {}\n\n{}",
                            json!({"jsonrpc": "2.0", "id": 1, "result": lie}),
                            sse_event(&json!({"jsonrpc": "2.0", "id": 1,
                                "result": {"task": task_named("t-1")}}))
                        ),
                    ),
                    // A unary one, answered as an event stream.
                    _ => answer(
                        "text/event-stream",
                        json!({"jsonrpc": "2.0", "id": 1, "result": lie}).to_string(),
                    ),
                }
            }),
        )
}

/// `a` with the lying peer saved as contact `liar`.
async fn liar() -> (TestNode, iroh::protocol::Router) {
    let a = spawn_node(WorkerCfg::Echo).await;
    let stub = common::stub_peer(&a, "liar", &AgentKey::generate(), lying_app()).await;
    (a, stub)
}

/// Streams `path` of the liar through `a`'s gateway: the bytes that came,
/// and whether the stream ended unbroken.
async fn stream_from_liar(a: &TestNode, path: &str, body: &Value) -> (String, bool) {
    let response = gateway_send(
        &a.gateway_url(),
        "POST",
        &format!("/peers/liar{path}"),
        &[("a2a-version", "1.0")],
        Some(body),
    )
    .await;
    assert_eq!(response.status(), 200, "{path}");
    let (received, unbroken) = read_until_end(response).await;
    (String::from_utf8(received).expect("UTF-8"), unbroken)
}

#[tokio::test]
async fn data_lines_hidden_behind_other_line_breaks_never_reach_the_client() {
    let (a, _liar) = liar().await;
    let message = message_params("hola", None);
    let safe = sse_event(&json!({"task": task_named("t-1")}));
    for kind in 0..HIDING_LINES.len() {
        for brk in 0..OTHER_LINE_BREAKS.len() {
            let path = format!("/rest/message:stream?case=hide-{kind}-{brk}");
            let (received, unbroken) = stream_from_liar(&a, &path, &message).await;
            // Only what was checked comes through: a keepalive for the event
            // without data, then the safe event.
            assert_eq!(received, format!(":\n\n{safe}"), "{path}");
            assert!(unbroken, "{path}");
        }
    }
    // The same on the JSON-RPC binding, its streaming call told by its
    // method.
    let call = rpc_request("SendStreamingMessage", message.clone());
    let (received, unbroken) = stream_from_liar(&a, "/jsonrpc", &call).await;
    let safe =
        sse_event(&json!({"jsonrpc": "2.0", "id": 1, "result": {"task": task_named("t-1")}}));
    assert_eq!(received, format!(":\n\n{safe}"));
    assert!(unbroken);
}

#[tokio::test]
async fn unary_answers_labelled_as_event_streams_are_read_whole() {
    let (a, _liar) = liar().await;
    let gw = a.gateway_url();
    let version = [("a2a-version", "1.0")];
    let message = message_params("hola", None);
    let bound = Duration::from_secs(120);
    let refused = json!({"error": "peer protocol error"});
    for (method, path, body) in [
        (
            "POST",
            "/peers/liar/rest/message:send".to_owned(),
            Some(message.clone()),
        ),
        ("GET", "/peers/liar/rest/tasks/as-stream".to_owned(), None),
        (
            "POST",
            "/peers/liar/jsonrpc".to_owned(),
            Some(rpc_request("SendMessage", message.clone())),
        ),
        (
            "POST",
            "/peers/liar/rest/message:send?case=endless".to_owned(),
            Some(message.clone()),
        ),
        ("GET", "/peers/liar/rest/tasks/endless".to_owned(), None),
    ] {
        let res = tokio::time::timeout(
            bound,
            gateway_request(&gw, method, &path, &version, body.as_ref()),
        )
        .await
        .unwrap_or_else(|_| panic!("{path}: no answer within {bound:?}"));
        assert_eq!(
            res.status,
            502,
            "{path}: {}",
            String::from_utf8_lossy(&res.body)
        );
        assert_eq!(res.json, refused, "{path}");
    }
}

#[tokio::test]
async fn task_ids_are_checked_in_every_spelling_and_every_object() {
    let (a, _liar) = liar().await;
    let gw = a.gateway_url();
    let version = [("a2a-version", "1.0")];
    for (method, task) in [
        ("GET", "bare"),
        ("GET", "snake"),
        ("GET", "referenced"),
        ("GET", "enveloped"),
        ("POST", "t-1:cancel"),
    ] {
        let path = format!("/peers/liar/rest/tasks/{task}");
        let res = gateway_request(&gw, method, &path, &version, None).await;
        assert_eq!(
            res.status,
            502,
            "{task}: {}",
            String::from_utf8_lossy(&res.body)
        );
        assert!(
            !String::from_utf8_lossy(&res.body).contains("../"),
            "{task}"
        );
    }
    // Free-form data and metadata are no ids: they pass.
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/liar/rest/tasks/free-form",
        &version,
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));

    let message = message_params("hola", None);
    let safe = sse_event(&json!({"task": task_named("t-1")}));
    for case in ["snake-update", "bare-task", "enveloped"] {
        let path = format!("/rest/message:stream?case={case}");
        let (received, unbroken) = stream_from_liar(&a, &path, &message).await;
        assert_eq!(received, safe, "{case}");
        assert!(!unbroken, "{case}");
    }
}

#[tokio::test]
async fn answers_are_reencoded_as_ascii_json_with_their_content_intact() {
    let (a, _liar) = liar().await;
    let gw = a.gateway_url();
    let text = "ñandú 😀 \u{2028}\u{85}";
    let res = gateway_request(
        &gw,
        "GET",
        "/peers/liar/rest/tasks/unicode",
        &[("a2a-version", "1.0")],
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert!(
        res.body.is_ascii(),
        "{}",
        String::from_utf8_lossy(&res.body)
    );
    assert_eq!(res.header("content-type"), "application/json");
    assert_eq!(res.json["status"]["message"]["parts"][0]["text"], text);

    let message = message_params("hola", None);
    let (received, unbroken) =
        stream_from_liar(&a, "/rest/message:stream?case=unicode", &message).await;
    assert!(unbroken);
    assert!(received.is_ascii(), "{received}");
    let last = received
        .trim_end()
        .rsplit("\n\n")
        .next()
        .and_then(|event| event.strip_prefix("data: "))
        .expect("a last event");
    let event: Value = serde_json::from_str(last).unwrap();
    assert_eq!(
        event["artifactUpdate"]["artifact"]["parts"][0]["text"],
        text
    );
}

#[tokio::test]
async fn task_requests_present_a_task_grant_not_a_newer_blob_grant() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    let b_key = common::key_in(b.dir.path());
    let now = now_unix();
    // Grants of `b` to `a`, newest last.
    let held = |grant_id: &str, ops: &[Operation], resource: &str, issued_at: i64| {
        let g = SignedGrant::issue(
            &b_key,
            Grant {
                schema: GRANT_SCHEMA.to_owned(),
                grant_id: grant_id.to_owned(),
                issuer: b.id(),
                subject: a.id(),
                audience: b.id(),
                project: "demo".to_owned(),
                resources: vec![resource.to_owned()],
                operations: ops.to_vec(),
                issued_at: format_timestamp(issued_at).unwrap(),
                expires_at: format_timestamp(now + 3600).unwrap(),
            },
        )
        .unwrap();
        b.node.store.grant_issued_put(&g).unwrap();
        a.hold(&g);
    };
    held("g-task", &SEND_AND_READ, "skill:ask", now - 30);
    held("g-blob", &[Operation::BlobFetch], "blob:*", now - 20);
    held("g-task-on-blobs", &[TaskSend], "blob:*", now - 10);

    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );

    // A grant whose resources cover `skill:ask` by prefix qualifies.
    held("g-any-skill", &SEND_AND_READ, "skill:*", now - 5);
    let res = gw_rpc(&a, "bob", &[], "SendMessage", message_params("hola", None)).await;
    assert_eq!(res.status, 200, "{}", res.json);

    // Holding only grants that cannot serve a task request is holding none.
    let c = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("carol", &c);
    let mut blob_only = grant(&c, &a, &[Operation::BlobFetch], 3600);
    blob_only.grant.resources = vec!["blob:*".to_owned()];
    let c_key = common::key_in(c.dir.path());
    let blob_only = SignedGrant::issue(&c_key, blob_only.grant).unwrap();
    a.hold(&blob_only);
    let res = gw_rpc(
        &a,
        "carol",
        &[],
        "SendMessage",
        message_params("hola", None),
    )
    .await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert_eq!(
        res.json,
        json!({"error": format!("no grant held for {}", c.id())})
    );
}

/// What `pals grant issue` grants unless told otherwise.
const DEFAULT_OPS: [Operation; 3] = [TaskSend, TaskRead, TaskCancel];

/// An agent run by the `pals` binary, on its own `PALS_HOME`.
struct CliAgent {
    home: tempfile::TempDir,
    port: u16,
}

impl CliAgent {
    /// `pals init` on a fresh home, then a configuration with the gateway on
    /// a free loopback port and no relay.
    fn init(name: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let agent = Self {
            port: free_port(),
            home,
        };
        agent.run(&["init"]);
        std::fs::write(
            agent.home.path().join("config.toml"),
            format!(
                "[agent]\nname = \"{name}\"\n\n[gateway]\nlisten = \"127.0.0.1:{}\"\n\n\
                 [net]\nrelay = false\n",
                agent.port
            ),
        )
        .unwrap();
        agent
    }

    fn cmd(&self, args: &[&str]) -> assert_cmd::Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
        cmd.env("PALS_HOME", self.home.path()).args(args);
        cmd
    }

    /// `pals <args>`, which must succeed; its standard output.
    fn run(&self, args: &[&str]) -> String {
        let output = self
            .cmd(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap()
    }

    /// `pals <args>`, which must fail; its standard error.
    fn fail(&self, args: &[&str]) -> String {
        let output = self
            .cmd(args)
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone();
        String::from_utf8(output).unwrap()
    }

    fn id(&self) -> AgentId {
        self.run(&["id"]).trim().parse().unwrap()
    }

    fn gateway_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn store(&self) -> Store {
        Store::open(&self.home.path().join("pals.db"), self.id()).unwrap()
    }

    /// Starts `pals daemon` and waits until it says it serves.
    async fn start_daemon(&self) -> Daemon {
        self.start_daemon_logging(None).await
    }

    /// [`CliAgent::start_daemon`], with `RUST_LOG` set to `rust_log`, or
    /// unset.
    async fn start_daemon_logging(&self, rust_log: Option<&str>) -> Daemon {
        let log = self.home.path().join("daemon.log");
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_pals"));
        match rust_log {
            Some(filter) => command.env("RUST_LOG", filter),
            None => command.env_remove("RUST_LOG"),
        };
        let child = command
            .arg("daemon")
            .env("PALS_HOME", self.home.path())
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(self.home.path().join("daemon.out")).unwrap())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("start pals daemon");
        let mut daemon = Daemon { child, log };
        let deadline = Instant::now() + Duration::from_secs(30);
        // Its own word that it serves, signals handled: something else may
        // have taken the port that was free a moment ago.
        let serving = format!("gateway at http://127.0.0.1:{}", self.port);
        loop {
            if daemon.log_text().contains(&serving) {
                return daemon;
            }
            if let Some(status) = daemon.child.try_wait().unwrap() {
                panic!("pals daemon exited with {status}: {}", daemon.log_text());
            }
            assert!(
                Instant::now() < deadline,
                "pals daemon did not start: {}",
                daemon.log_text()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// A `pals daemon` process, killed if it is still running when dropped.
struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Stops the daemon with SIGTERM; it must exit successfully.
    async fn stop(mut self) {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "pals daemon ignored SIGTERM");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(status.success(), "{status}: {}", self.log_text());
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn the_daemon_logs_to_stderr_from_info_unless_rust_log_says_otherwise() {
    let agent = CliAgent::init("logged");
    let id = agent.id();
    // What a previous run leaves: a task in progress, which the next start
    // fails, and logs at INFO.
    let leave_a_task = |task_id: &str| {
        let task: a2a::Task = serde_json::from_value(json!({
            "id": task_id,
            "contextId": "ctx",
            "status": {"state": "TASK_STATE_WORKING"},
        }))
        .unwrap();
        Store::open(&agent.home.path().join("pals.db"), id)
            .unwrap()
            .task_create(&task, now_unix())
            .unwrap();
    };
    let interrupted = "failed the tasks a restart interrupted";

    leave_a_task("t-1");
    let daemon = agent.start_daemon_logging(None).await;
    let log = daemon.log.clone();
    daemon.stop().await;
    let text = std::fs::read_to_string(&log).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains(interrupted))
        .unwrap_or_else(|| panic!("no INFO line: {text}"));
    assert!(line.contains("INFO"), "{line}");

    // RUST_LOG decides instead.
    leave_a_task("t-2");
    let daemon = agent.start_daemon_logging(Some("warn")).await;
    let log = daemon.log.clone();
    daemon.stop().await;
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains(interrupted), "{text}");
    assert!(text.contains("serving"), "{text}");
}

/// A loopback port that was free a moment ago.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `SendMessage` to `peer` through the gateway at `gateway`.
async fn ask_through(gateway: &str, peer: &str) -> TestResponse {
    gateway_request(
        gateway,
        "POST",
        &format!("/peers/{peer}/jsonrpc"),
        &[("a2a-version", "1.0")],
        Some(&rpc_request("SendMessage", message_params("hola", None))),
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn grant_cli_issue_import_revoke_roundtrip() {
    let alice = CliAgent::init("alice");
    let bob = CliAgent::init("bob");
    let (alice_id, bob_id) = (alice.id(), bob.id());

    // Without a running daemon there is no ticket to give.
    let err = bob.fail(&["id", "--ticket"]);
    assert!(err.contains("pals daemon"), "{err}");

    let alice_daemon = alice.start_daemon().await;
    let bob_daemon = bob.start_daemon().await;

    // Contacts, from each other's tickets.
    let alice_ticket = alice.run(&["id", "--ticket"]).trim().to_owned();
    let bob_ticket = bob.run(&["id", "--ticket"]).trim().to_owned();
    alice.run(&["contact", "add", &bob_ticket, "--name", "bob"]);
    bob.run(&["contact", "add", &alice_ticket, "--name", "alice"]);
    let contacts = alice.run(&["contact", "ls"]);
    assert!(contacts.contains("bob"), "{contacts}");
    assert!(contacts.contains(&bob_id.to_string()), "{contacts}");
    let err = alice.fail(&["contact", "add", "not-a-ticket", "--name", "x"]);
    assert!(err.contains("ticket"), "{err}");

    // Without a grant, bob's gateway refuses locally.
    let res = ask_through(&bob.gateway_url(), "alice").await;
    assert_eq!(res.status, 401, "{}", res.json);

    // Alice issues bob a grant with the defaults.
    let token = alice.run(&["grant", "issue", "--to", "bob", "--project", "demo"]);
    let token = token.trim();
    let g = SignedGrant::from_token(token).expect("a valid grant token");
    assert_eq!(g.grant.issuer, alice_id);
    assert_eq!(g.grant.subject, bob_id);
    assert_eq!(g.grant.audience, alice_id);
    assert_eq!(g.grant.project, "demo");
    assert_eq!(g.grant.operations, DEFAULT_OPS);
    assert_eq!(g.grant.resources, ["skill:ask"]);
    let issued_at = pals_core::grant::parse_timestamp(&g.grant.issued_at).unwrap();
    let expires_at = pals_core::grant::parse_timestamp(&g.grant.expires_at).unwrap();
    assert_eq!(expires_at - issued_at, 24 * 3600);
    let grant_id = g.grant.grant_id.clone();

    // Only bob can import it, and only as signed.
    let err = alice.fail(&["grant", "import", token]);
    assert!(err.contains("subject"), "{err}");
    let mut forged = g.clone();
    forged.grant.project = "other".to_owned();
    let err = bob.fail(&["grant", "import", &forged.to_token()]);
    assert!(err.contains("signature"), "{err}");
    bob.run(&["grant", "import", token]);
    let held = bob.run(&["grant", "ls", "--held"]);
    assert!(held.contains(&grant_id), "{held}");
    let issued = alice.run(&["grant", "ls", "--issued"]);
    assert!(issued.contains(&grant_id), "{issued}");
    let all = alice.run(&["grant", "ls"]);
    assert!(all.contains(&grant_id), "{all}");

    // Bob's gateway now presents it, and alice answers.
    let res = ask_through(&bob.gateway_url(), "alice").await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );

    // Revoked, the grant stops working on the next request.
    alice.run(&["grant", "revoke", &grant_id]);
    let res = ask_through(&bob.gateway_url(), "alice").await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert!(
        res.header("www-authenticate").contains("revoked"),
        "{}",
        res.header("www-authenticate")
    );
    let issued = alice.run(&["grant", "ls", "--issued"]);
    assert!(issued.contains("revoked"), "{issued}");
    let err = alice.fail(&["grant", "revoke", "no-such-grant"]);
    assert!(err.contains("no-such-grant"), "{err}");

    // Both are logged, with ids only.
    let store = alice.store();
    assert_eq!(store.events_count("grant.issued").unwrap(), 1);
    assert_eq!(store.events_count("grant.revoked").unwrap(), 1);
    let events = store.events_since(&alice_id, 0).unwrap();
    let issued_event = events.iter().find(|e| e.kind == "grant.issued").unwrap();
    assert_eq!(issued_event.payload["grant_id"], grant_id.as_str());
    assert!(issued_event.payload.get("token").is_none());
    let revoked_event = events.iter().find(|e| e.kind == "grant.revoked").unwrap();
    assert_eq!(revoked_event.payload, json!({"grant_id": grant_id}));

    // What `grant issue` refuses before signing.
    for (args, expected) in [
        (vec!["--project", "diseño"], "project"),
        (vec!["--project", "two words"], "project"),
        (vec!["--project", ""], "project"),
        (vec!["--project", "demo", "--ttl", "0h"], "ttl"),
        (vec!["--project", "demo", "--ttl", "soon"], "ttl"),
        (vec!["--project", "demo", "--ops", ""], "ops"),
        (vec!["--project", "demo", "--ops", "task.fly"], "task.fly"),
        (vec!["--project", "demo", "--resources", ""], "resources"),
    ] {
        let mut full = vec!["grant", "issue", "--to", "bob"];
        full.extend(args);
        let err = alice.fail(&full);
        assert!(err.contains(expected), "{full:?}: {err}");
    }
    let err = alice.fail(&["grant", "issue", "--to", "nobody", "--project", "demo"]);
    assert!(err.contains("nobody"), "{err}");
    assert_eq!(store.events_count("grant.issued").unwrap(), 1);

    // An explicit blob.fetch grant is possible, never a default.
    let token = alice.run(&[
        "grant",
        "issue",
        "--to",
        "bob",
        "--project",
        "demo",
        "--ops",
        "blob.fetch",
        "--resources",
        "blob:*",
        "--ttl",
        "30m",
    ]);
    let g = SignedGrant::from_token(token.trim()).unwrap();
    assert_eq!(g.grant.operations, [Operation::BlobFetch]);
    assert_eq!(g.grant.resources, ["blob:*"]);

    bob_daemon.stop().await;
    alice_daemon.stop().await;
}
