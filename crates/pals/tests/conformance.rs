//! What PALS adds in front of the pinned SDK so that the agent conforms to
//! A2A v1.0 where the SDK does not: required message fields, the protocol
//! version and the error format of the HTTP+JSON binding, `SubscribeToTask`
//! on a task with no live execution, and cacheable Agent Cards.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, header};
use common::{TestNode, TestResponse, bearer, gateway_request, grant, rpc_request, spawn_node};
use http_body_util::BodyExt;
use pals::config::WorkerCfg;
use pals_core::grant::Operation::{TaskCancel, TaskRead, TaskSend};
use pals_core::grant::SignedGrant;
use serde_json::{Value, json};

/// How long a test waits for an event that should come at once: generous,
/// because the host may be heavily loaded.
const PATIENCE: Duration = Duration::from_secs(30);

/// A provider B with the `tck` adapter, a requester A, and the grant that
/// lets A send to, read and cancel B's tasks.
async fn setup() -> (TestNode, TestNode, SignedGrant) {
    let b = spawn_node(WorkerCfg::Tck).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant(&b, &a, &[TaskSend, TaskRead, TaskCancel], 3600);
    (b, a, g)
}

/// The params of a message with `message_id`, continuing `task_id` when
/// given.
fn message(message_id: &str, task_id: Option<&str>) -> Value {
    let mut message = json!({
        "messageId": message_id,
        "role": "ROLE_USER",
        "parts": [{"text": "TCK"}],
    });
    if let Some(task_id) = task_id {
        message["taskId"] = json!(task_id);
    }
    json!({ "message": message })
}

/// `method path` to B's HTTP+JSON binding with the grant and `headers`.
async fn rest(
    a: &TestNode,
    b: &TestNode,
    g: &SignedGrant,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> TestResponse {
    let bearer = bearer(g);
    let mut all = vec![("authorization", bearer.as_str())];
    all.extend_from_slice(headers);
    a.request(b, method, path, &all, body).await
}

/// Asserts that `res` is an HTTP+JSON error of A2A §11.6: `status`, JSON,
/// a `google.rpc.Status` whose `ErrorInfo` detail has `reason`.
fn assert_rest_error(res: &TestResponse, status: u16, reason: &str) {
    assert_eq!(res.status, status, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(
        res.header("content-type"),
        "application/json",
        "{}",
        res.json
    );
    let error = &res.json["error"];
    assert_eq!(error["code"], status, "{}", res.json);
    assert!(error["message"].is_string(), "{}", res.json);
    let info = &error["details"][0];
    assert_eq!(
        info["@type"], "type.googleapis.com/google.rpc.ErrorInfo",
        "{}",
        res.json
    );
    assert_eq!(info["reason"], reason, "{}", res.json);
    assert_eq!(info["domain"], "a2a-protocol.org", "{}", res.json);
}

#[tokio::test]
async fn a_message_request_without_a_message_is_invalid_params() {
    let (b, a, g) = setup().await;
    for method in ["SendMessage", "SendStreamingMessage"] {
        let res = a.rpc(&b, Some(&g), method, json!({})).await;
        assert_eq!(res.status, 200, "{method}: {}", res.json);
        assert_eq!(res.json["error"]["code"], -32602, "{method}: {}", res.json);
    }
    // A message with no parts is as invalid: `parts` is REQUIRED.
    let mut empty = message("tck-complete-task-1", None);
    empty["message"]["parts"] = json!([]);
    let res = a.rpc(&b, Some(&g), "SendMessage", empty.clone()).await;
    assert_eq!(res.json["error"]["code"], -32602, "{}", res.json);

    let version = [("a2a-version", "1.0")];
    for path in ["/rest/message:send", "/rest/message:stream"] {
        let res = rest(&a, &b, &g, "POST", path, &version, Some(&json!({}))).await;
        assert_rest_error(&res, 400, "INVALID_PARAMS");
    }
    let res = rest(
        &a,
        &b,
        &g,
        "POST",
        "/rest/message:send",
        &version,
        Some(&empty),
    )
    .await;
    assert_rest_error(&res, 400, "INVALID_PARAMS");

    // Nothing ran: no task (ProtoJSON omits the empty list and the zero).
    let res = a.rpc(&b, Some(&g), "ListTasks", json!({})).await;
    let result = &res.json["result"];
    assert!(result.is_object(), "{}", res.json);
    assert!(
        result
            .get("tasks")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "{}",
        res.json
    );
}

#[tokio::test]
async fn the_rest_binding_checks_the_protocol_version_as_json_rpc_does() {
    let (b, a, g) = setup().await;
    // An absent header means 0.3 (A2A §3.6.2), which is not served.
    for headers in [
        &[][..],
        &[("a2a-version", "0.3")],
        &[("a2a-version", "99.0")],
    ] {
        let res = rest(&a, &b, &g, "GET", "/rest/tasks/none", headers, None).await;
        assert_rest_error(&res, 400, "VERSION_NOT_SUPPORTED");
        let res = rest(
            &a,
            &b,
            &g,
            "POST",
            "/rest/message:send",
            headers,
            Some(&message(&format!("tck-version-{}", headers.len()), None)),
        )
        .await;
        assert_rest_error(&res, 400, "VERSION_NOT_SUPPORTED");
    }
    // JSON-RPC answers the same versions the same way.
    let body = rpc_request("GetTask", json!({"id": "none"}));
    let res = a
        .request(
            &b,
            "POST",
            "/jsonrpc",
            &[("authorization", &bearer(&g))],
            Some(&body),
        )
        .await;
    assert_eq!(res.json["error"]["code"], -32009, "{}", res.json);

    // 1.0 is served.
    let version = [("a2a-version", "1.0")];
    let res = rest(&a, &b, &g, "GET", "/rest/tasks/none", &version, None).await;
    assert_rest_error(&res, 404, "TASK_NOT_FOUND");
    let res = rest(
        &a,
        &b,
        &g,
        "POST",
        "/rest/message:send",
        &version,
        Some(&message("tck-complete-task-1", None)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["task"]["status"]["state"], "TASK_STATE_COMPLETED");
}

#[tokio::test]
async fn the_protocol_version_may_come_as_a_query_parameter() {
    // A2A §3.6.1: a client MAY send `A2A-Version` as a request parameter
    // instead of a header; the key is case-insensitive (§3.2.6).
    let (b, a, g) = setup().await;
    let res = a
        .rpc(
            &b,
            Some(&g),
            "SendMessage",
            message("tck-complete-task-1", None),
        )
        .await;
    let id = res.json["result"]["task"]["id"]
        .as_str()
        .expect("a task")
        .to_owned();

    for query in ["A2A-Version=1.0", "a2a-version=1.0", "A2A-VERSION=1&x=y"] {
        let path = format!("/rest/tasks/{id}?{query}");
        let res = rest(&a, &b, &g, "GET", &path, &[], None).await;
        assert_eq!(res.status, 200, "{query}: {}", res.json);
        assert_eq!(res.json["id"], id.as_str(), "{query}");
    }
    let body = rpc_request("GetTask", json!({ "id": id }));
    let res = a
        .request(
            &b,
            "POST",
            "/jsonrpc?a2a-version=1.0",
            &[("authorization", &bearer(&g))],
            Some(&body),
        )
        .await;
    assert_eq!(res.json["result"]["id"], id.as_str(), "{}", res.json);

    // An unsupported version in the query is refused like one in a header,
    // and a header wins over the query.
    let path = format!("/rest/tasks/{id}?A2A-Version=0.3");
    let res = rest(&a, &b, &g, "GET", &path, &[], None).await;
    assert_rest_error(&res, 400, "VERSION_NOT_SUPPORTED");
    let path = format!("/rest/tasks/{id}?A2A-Version=1.0");
    let res = rest(&a, &b, &g, "GET", &path, &[("a2a-version", "0.3")], None).await;
    assert_rest_error(&res, 400, "VERSION_NOT_SUPPORTED");
    let res = a
        .request(
            &b,
            "POST",
            "/jsonrpc?a2a-version=0.3",
            &[("authorization", &bearer(&g))],
            Some(&body),
        )
        .await;
    assert_eq!(res.json["error"]["code"], -32009, "{}", res.json);

    // Through the gateway, which passes the query on as sent.
    a.add_contact("bob", &b);
    a.hold(&g);
    let path = format!("/peers/bob/rest/tasks/{id}?A2A-Version=1.0");
    let res = gateway_request(&a.gateway_url(), "GET", &path, &[], None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    let res = gateway_request(
        &a.gateway_url(),
        "POST",
        "/peers/bob/jsonrpc/?A2A-Version=1.0",
        &[],
        Some(&body),
    )
    .await;
    assert_eq!(res.json["result"]["id"], id.as_str(), "{}", res.json);
}

/// `SubscribeToTask` of `task_id` over JSON-RPC, with its body still
/// streaming.
async fn subscribe(a: &TestNode, b: &TestNode, g: &SignedGrant, task_id: &str) -> Body {
    let body = rpc_request("SubscribeToTask", json!({ "id": task_id }));
    let request = Request::post("/jsonrpc")
        .header(header::CONTENT_TYPE, "application/json")
        .header("a2a-version", "1.0")
        .header(header::AUTHORIZATION, bearer(g))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = a.send(b, request).await;
    assert_eq!(response.status(), 200);
    let content_type = response.headers()[header::CONTENT_TYPE].to_str().unwrap();
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );
    response.into_body()
}

/// The next SSE event's JSON-RPC `result` from `body`, `None` at its end;
/// `pending` holds what was read past the event.
async fn next_event(body: &mut Body, pending: &mut String) -> Option<Value> {
    loop {
        if let Some(end) = pending.find("\n\n") {
            let event: String = pending.drain(..end + 2).collect();
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .collect::<String>();
            if data.trim().is_empty() {
                // A keep-alive comment.
                continue;
            }
            let response: Value = serde_json::from_str(data.trim()).expect("an event is JSON");
            assert!(response.get("error").is_none(), "{response}");
            return Some(response["result"].clone());
        }
        let frame = tokio::time::timeout(PATIENCE, body.frame())
            .await
            .expect("an event in time")?
            .expect("a readable stream");
        if let Ok(data) = frame.into_data() {
            pending.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
}

fn state_of(event: &Value) -> &str {
    let status = if let Some(task) = event.get("task") {
        &task["status"]
    } else {
        &event["statusUpdate"]["status"]
    };
    status["state"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn subscribing_to_a_task_waiting_for_input_follows_it_to_its_end() {
    let (b, a, g) = setup().await;
    let res = a
        .rpc(
            &b,
            Some(&g),
            "SendMessage",
            message("tck-input-required-1", None),
        )
        .await;
    let task = res.json["result"]["task"].clone();
    let id = task["id"].as_str().expect("a task").to_owned();
    assert_eq!(
        state_of(&json!({ "task": task })),
        "TASK_STATE_INPUT_REQUIRED"
    );

    // Two subscribers: each gets the task as it is now, first.
    let mut first = subscribe(&a, &b, &g, &id).await;
    let mut second = subscribe(&a, &b, &g, &id).await;
    let (mut pending_1, mut pending_2) = (String::new(), String::new());
    let event_1 = next_event(&mut first, &mut pending_1).await.unwrap();
    let event_2 = next_event(&mut second, &mut pending_2).await.unwrap();
    assert_eq!(event_1["task"]["id"], id.as_str(), "{event_1}");
    assert_eq!(state_of(&event_1), "TASK_STATE_INPUT_REQUIRED");
    assert_eq!(event_1, event_2);

    // Closing one leaves the other.
    drop(first);

    // The next turn completes the task: the subscriber sees it, last, and
    // the stream ends.
    let res = a
        .rpc(
            &b,
            Some(&g),
            "SendMessage",
            message("tck-complete-task-1", Some(&id)),
        )
        .await;
    assert_eq!(
        res.json["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
        "{}",
        res.json
    );
    let mut states = Vec::new();
    while let Some(event) = next_event(&mut second, &mut pending_2).await {
        states.push(state_of(&event).to_owned());
    }
    assert_eq!(
        states.last().map(String::as_str),
        Some("TASK_STATE_COMPLETED"),
        "{states:?}"
    );
}

#[tokio::test]
async fn subscribing_to_a_terminal_task_is_still_unsupported() {
    let (b, a, g) = setup().await;
    let res = a
        .rpc(
            &b,
            Some(&g),
            "SendMessage",
            message("tck-complete-task-1", None),
        )
        .await;
    let id = res.json["result"]["task"]["id"].clone();
    let res = a
        .rpc(&b, Some(&g), "SubscribeToTask", json!({ "id": id }))
        .await;
    assert_eq!(res.json["error"]["code"], -32004, "{}", res.json);
    let res = a
        .rpc(&b, Some(&g), "SubscribeToTask", json!({ "id": "none" }))
        .await;
    assert_eq!(res.json["error"]["code"], -32001, "{}", res.json);
}

#[tokio::test]
async fn agent_cards_can_be_cached() {
    let (b, a, g) = setup().await;
    // The node's own card, as the SDK serves it.
    let res = a.get(&b, "/.well-known/agent-card.json", None).await;
    assert_eq!(res.status, 200);
    assert!(
        res.header("cache-control").contains("max-age"),
        "{:?}",
        res.headers
    );
    assert!(!res.header("etag").is_empty(), "{:?}", res.headers);

    // The gateway's view of it.
    a.add_contact("bob", &b);
    a.hold(&g);
    let gw = a.gateway_url();
    let path = "/peers/bob/.well-known/agent-card.json";
    let res = gateway_request(&gw, "GET", path, &[], None).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.header("cache-control"), "max-age=300");
    assert_eq!(res.header("content-type"), "application/json");
    let etag = res.header("etag").to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
    let card = res.json.clone();
    assert_eq!(card["name"], "pals");

    // The same view again, conditionally: not modified, no body.
    let res = gateway_request(&gw, "GET", path, &[("if-none-match", &etag)], None).await;
    assert_eq!(res.status, 304);
    assert!(res.body.is_empty());
    assert_eq!(res.header("etag"), etag);
    // Another tag gets the card.
    let res = gateway_request(&gw, "GET", path, &[("if-none-match", "\"other\"")], None).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json, card);
}
