//! A complete node: the public Agent Card, grant authentication on every other
//! request (401 / 403), the ownership of tasks, the task event log and the
//! single-instance lock.

mod common;

use std::sync::Arc;

use a2a::{Message, Part, Role, Task, TaskState, TaskStatus};
use common::{
    TestNode, grant, issue_grant, key_in, message_params, now_unix, spawn_node, spawn_node_in,
    spawn_node_with, test_config,
};
use pals::a2a::{SECURITY_SCHEME, canonical_card};
use pals::config::{AlreadyRunning, WorkerCfg};
use pals::node::Node;
use pals_core::grant::Operation::{self, TaskCancel, TaskRead, TaskSend};
use pals_core::store::{INTERRUPTED_MESSAGE, Store};
use serde_json::{Value, json};

const SEND_AND_READ: [Operation; 2] = [TaskSend, TaskRead];
const ALL_TASK_OPS: [Operation; 3] = [TaskSend, TaskRead, TaskCancel];

/// A serving node and a requesting node.
async fn pair() -> (TestNode, TestNode) {
    (
        spawn_node(WorkerCfg::Echo).await,
        spawn_node(WorkerCfg::Echo).await,
    )
}

/// The JSON-RPC error of a response, which must be one.
fn rpc_error(res: &common::TestResponse) -> &Value {
    assert_eq!(res.status, 200, "{}", res.json);
    let err = &res.json["error"];
    assert!(err.is_object(), "expected a JSON-RPC error: {}", res.json);
    err
}

fn task_id(task: &Value) -> &str {
    task["id"].as_str().expect("a task id")
}

#[tokio::test]
async fn card_is_public_and_declares_only_what_is_served() {
    let (b, a) = pair().await;
    let res = a.get(&b, "/.well-known/agent-card.json", None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    let card = &res.json;

    assert_eq!(card["name"], "pals");
    assert_eq!(card["description"], "A PALS agent");
    assert_eq!(card["version"], env!("CARGO_PKG_VERSION"));
    // Both bindings it serves to peers, JSON-RPC first, each over Iroh.
    assert_eq!(
        card["supportedInterfaces"],
        json!([
            {
                "url": format!("iroh://{}/jsonrpc", b.id()),
                "protocolBinding":
                    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/jsonrpc",
                "protocolVersion": "1.0",
            },
            {
                "url": format!("iroh://{}/rest", b.id()),
                "protocolBinding":
                    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/http-json",
                "protocolVersion": "1.0",
            },
        ]),
        "{card}"
    );
    assert_eq!(card["capabilities"]["streaming"], true);
    assert_eq!(card["capabilities"]["pushNotifications"], false);
    assert_eq!(card["capabilities"]["extendedAgentCard"], false);
    // Text, and files of any media type.
    assert_eq!(card["defaultInputModes"], json!(["text/plain", "*/*"]));
    assert_eq!(
        card["defaultOutputModes"],
        json!(["text/plain", "application/octet-stream"])
    );
    assert_eq!(
        card["skills"],
        json!([{
            "id": "ask",
            "name": "Ask",
            "description": "Answers a question with the context of one authorized project.",
            "tags": ["ask"],
        }])
    );
    let schemes = card["securitySchemes"].as_object().unwrap();
    assert_eq!(schemes.len(), 1, "{card}");
    assert_eq!(
        card["securitySchemes"][SECURITY_SCHEME],
        json!({"httpAuthSecurityScheme": {"scheme": "Bearer", "bearerFormat": "agentnet.grant/1"}})
    );
    let requirements = card["securityRequirements"].as_array().unwrap();
    assert_eq!(requirements.len(), 1, "{card}");
    assert!(
        requirements[0]["schemes"][SECURITY_SCHEME].is_object(),
        "{card}"
    );
    assert!(card.get("signatures").is_none());

    // The served card is the canonical one.
    let cfg = test_config(b.dir.path(), WorkerCfg::Echo);
    assert_eq!(
        &serde_json::to_value(canonical_card(&cfg, &b.id())).unwrap(),
        card
    );
}

#[tokio::test]
async fn request_without_grant_is_401_with_challenge() {
    let (b, a) = pair().await;
    let res = a
        .rpc(&b, None, "SendMessage", message_params("hola", None))
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    let challenge = res.header("www-authenticate");
    assert!(challenge.starts_with("Bearer"), "{challenge}");
    assert!(challenge.contains(r#"realm="agentnet""#), "{challenge}");
    assert!(challenge.contains("missing_token"), "{challenge}");

    // A token that is not a grant, and a scheme other than Bearer.
    for authorization in ["Bearer not-a-grant", "Basic dXNlcjpwYXNz"] {
        let res = a
            .request(
                &b,
                "POST",
                "/jsonrpc",
                &[("a2a-version", "1.0"), ("authorization", authorization)],
                Some(&common::rpc_request(
                    "SendMessage",
                    message_params("hola", None),
                )),
            )
            .await;
        assert_eq!(res.status, 401, "{authorization}");
        assert!(res.header("www-authenticate").starts_with("Bearer"));
    }

    // Every route but the card needs a grant: the REST binding, methods and
    // paths the SDK does not know, and a body that is not JSON-RPC.
    for (method, path) in [
        ("GET", "/rest/tasks"),
        ("POST", "/rest/message:send"),
        ("GET", "/rest/extendedAgentCard"),
        ("GET", "/no/such/path"),
    ] {
        let res = a.request(&b, method, path, &[], None).await;
        assert_eq!(res.status, 401, "{method} {path}");
        assert!(res.header("www-authenticate").starts_with("Bearer"));
    }
    let res = a.rpc(&b, None, "NoSuchMethod", json!({})).await;
    assert_eq!(res.status, 401);
    let res = a
        .request(
            &b,
            "POST",
            "/jsonrpc",
            &[("a2a-version", "1.0")],
            Some(&json!("not a request")),
        )
        .await;
    assert_eq!(res.status, 401);

    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

#[tokio::test]
async fn valid_grant_completes_a_task() {
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);

    let res = a
        .rpc(&b, Some(&g), "SendMessage", message_params("hola", None))
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let task = &res.json["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "echo: hola");

    // Its owner reads it back, over both bindings.
    let res = a
        .rpc(&b, Some(&g), "GetTask", json!({"id": task_id(task)}))
        .await;
    assert_eq!(res.json["result"]["id"], task["id"], "{}", res.json);
    let res = a
        .get(&b, &format!("/rest/tasks/{}", task_id(task)), Some(&g))
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["id"], task["id"]);

    // The HTTP+JSON binding takes the same grant.
    let res = a
        .request(
            &b,
            "POST",
            "/rest/message:send",
            &[
                ("a2a-version", "1.0"),
                ("authorization", &common::bearer(&g)),
            ],
            Some(&message_params("hola rest", None)),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(
        res.json["task"]["artifacts"][0]["parts"][0]["text"],
        "echo: hola rest"
    );
}

#[tokio::test]
async fn expired_grant_is_401_and_says_expired() {
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, -3600);
    let res = a
        .rpc(&b, Some(&g), "SendMessage", message_params("hola", None))
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    let challenge = res.header("www-authenticate");
    assert!(challenge.starts_with("Bearer"), "{challenge}");
    assert!(
        challenge.contains(r#"error="invalid_token""#),
        "{challenge}"
    );
    assert!(challenge.contains("expired"), "{challenge}");

    // Within the 60 s clock skew an expired grant still works.
    let g = grant(&b, &a, &SEND_AND_READ, -30);
    a.ask(&b, &g, "hola").await;
}

#[tokio::test]
async fn grant_from_the_future_says_not_yet_valid() {
    let (b, a) = pair().await;
    let now = now_unix();
    let issue = |issued_at: i64| {
        let g = issue_grant(
            &key_in(b.dir.path()),
            a.id(),
            b.id(),
            &SEND_AND_READ,
            issued_at,
            issued_at + 3600,
        );
        b.node.store.grant_issued_put(&g).unwrap();
        g
    };

    let g = issue(now + 3600);
    let res = a
        .rpc(&b, Some(&g), "SendMessage", message_params("hola", None))
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    let challenge = res.header("www-authenticate");
    assert!(
        challenge.contains(r#"error_description="not_yet_valid""#),
        "{challenge}"
    );

    // Within the 60 s clock skew a grant from the future already works.
    let g = issue(now + 30);
    a.ask(&b, &g, "hola").await;
}

#[tokio::test]
async fn revoked_grant_stops_working_on_next_request() {
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    a.ask(&b, &g, "before").await;

    assert!(b.node.store.revoke(&g.grant.grant_id, now_unix()).unwrap());
    let res = a
        .rpc(&b, Some(&g), "SendMessage", message_params("after", None))
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert!(
        res.header("www-authenticate").contains("revoked"),
        "{}",
        res.header("www-authenticate")
    );
}

#[tokio::test]
async fn grant_issued_to_another_peer_is_401() {
    let (b, a) = pair().await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let for_c = grant(&b, &c, &SEND_AND_READ, 3600);
    let res = a
        .rpc(
            &b,
            Some(&for_c),
            "SendMessage",
            message_params("hola", None),
        )
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert!(
        res.header("www-authenticate").contains("wrong_subject"),
        "{}",
        res.header("www-authenticate")
    );
    // The peer it was issued to may use it.
    c.ask(&b, &for_c, "hola").await;
}

#[tokio::test]
async fn untrusted_issuer_is_401() {
    let (b, a) = pair().await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let now = now_unix();
    // C grants A access to B, which does not trust C.
    let from_c = issue_grant(
        &key_in(c.dir.path()),
        a.id(),
        b.id(),
        &SEND_AND_READ,
        now,
        now + 3600,
    );
    let res = a
        .rpc(
            &b,
            Some(&from_c),
            "SendMessage",
            message_params("hola", None),
        )
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert!(
        res.header("www-authenticate")
            .contains("issuer_not_accepted"),
        "{}",
        res.header("www-authenticate")
    );

    // A node that lists C in trusted_issuers accepts C's grants for it.
    let c_id = c.id();
    let trusting = spawn_node_with(WorkerCfg::Echo, |cfg| cfg.trusted_issuers = vec![c_id]).await;
    let from_c = issue_grant(
        &key_in(c.dir.path()),
        a.id(),
        trusting.id(),
        &SEND_AND_READ,
        now,
        now + 3600,
    );
    a.ask(&trusting, &from_c, "hola").await;
}

#[tokio::test]
async fn cancel_without_task_cancel_is_403() {
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    let task = a.ask(&b, &g, "hola").await;
    let id = task_id(&task);

    let res = a.rpc(&b, Some(&g), "CancelTask", json!({"id": id})).await;
    assert_eq!(res.status, 403, "{}", res.json);
    assert!(res.headers.get("www-authenticate").is_none());

    // The HTTP+JSON routes to the same operation, however the path spells it.
    let bearer = common::bearer(&g);
    for path in [
        format!("/rest/tasks/{id}:cancel"),
        format!("/rest/tasks/{id}%3Acancel"),
        format!("/rest/tasks/{id}%3acancel"),
        format!("/rest/tasks/{id}/cancel"),
    ] {
        let res = a
            .request(&b, "POST", &path, &[("authorization", &bearer)], None)
            .await;
        assert_eq!(res.status, 403, "{path}: {}", res.json);
    }
    // Spellings the router does not route to the cancel pass as any request
    // with a valid grant, and then nothing serves them.
    for path in [
        format!("/rest/tasks/{id}/c%61ncel"),
        format!("/rest/t%61sks/{id}:cancel"),
        format!("/rest//tasks/{id}:cancel"),
        format!("/rest/x/../tasks/{id}:cancel"),
        format!("/rest/tasks/{id}:cancel/"),
    ] {
        let res = a
            .request(&b, "POST", &path, &[("authorization", &bearer)], None)
            .await;
        assert_eq!(res.status, 404, "{path}: {}", res.json);
    }

    // A grant without task.send cannot send either.
    let read_only = grant(&b, &a, &[TaskRead], 3600);
    let res = a
        .rpc(
            &b,
            Some(&read_only),
            "SendMessage",
            message_params("hola", None),
        )
        .await;
    assert_eq!(res.status, 403, "{}", res.json);

    // With task.cancel the request reaches the SDK, which finds the task
    // already completed.
    let full = grant(&b, &a, &ALL_TASK_OPS, 3600);
    let res = a
        .rpc(&b, Some(&full), "CancelTask", json!({"id": id}))
        .await;
    assert_eq!(rpc_error(&res)["code"], -32002);
}

#[tokio::test]
async fn peer_cannot_read_cancel_or_subscribe_to_anothers_task() {
    let (b, a) = pair().await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let for_a = grant(&b, &a, &ALL_TASK_OPS, 3600);
    let for_c = grant(&b, &c, &ALL_TASK_OPS, 3600);
    let task = a.ask(&b, &for_a, "secret").await;
    let id = task_id(&task);

    for (method, params) in [
        ("GetTask", json!({"id": id})),
        ("CancelTask", json!({"id": id})),
        ("SubscribeToTask", json!({"id": id})),
        ("SendMessage", message_params("mine now", Some(id))),
        ("SendStreamingMessage", message_params("mine now", Some(id))),
    ] {
        let res = c.rpc(&b, Some(&for_c), method, params).await;
        let err = rpc_error(&res);
        assert_eq!(err["code"], -32001, "{method}: {err}");
        assert_eq!(err["message"], format!("task not found: {id}"), "{method}");
    }
    // Exactly what a task that does not exist gets.
    let res = c
        .rpc(&b, Some(&for_c), "GetTask", json!({"id": "no-such-task"}))
        .await;
    assert_eq!(rpc_error(&res)["code"], -32001);

    // The HTTP+JSON binding answers 404.
    let res = c.get(&b, &format!("/rest/tasks/{id}"), Some(&for_c)).await;
    assert_eq!(res.status, 404, "{}", res.json);
    assert!(!res.json.to_string().contains("secret"), "{}", res.json);

    // The owner still has its task, untouched.
    let res = a.rpc(&b, Some(&for_a), "GetTask", json!({"id": id})).await;
    assert_eq!(
        res.json["result"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    assert_eq!(
        res.json["result"]["artifacts"][0]["parts"][0]["text"],
        "echo: secret"
    );
}

#[tokio::test]
async fn list_tasks_returns_only_own_tasks() {
    let (b, a) = pair().await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let for_a = grant(&b, &a, &SEND_AND_READ, 3600);
    let for_c = grant(&b, &c, &SEND_AND_READ, 3600);
    let mut mine = vec![
        task_id(&a.ask(&b, &for_a, "a1").await).to_owned(),
        task_id(&a.ask(&b, &for_a, "a2").await).to_owned(),
    ];
    // Listed most recently updated first (A2A §3.1.4).
    mine.reverse();
    let theirs = task_id(&c.ask(&b, &for_c, "c1").await).to_owned();

    // One page at a time, every page holds only A's tasks.
    let mut seen = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut params = json!({"pageSize": 1});
        if let Some(token) = &page_token {
            params["pageToken"] = json!(token);
        }
        let res = a.rpc(&b, Some(&for_a), "ListTasks", params).await;
        let result = &res.json["result"];
        assert_eq!(result["totalSize"], 2, "{}", res.json);
        for task in result["tasks"].as_array().unwrap() {
            seen.push(task_id(task).to_owned());
        }
        match result["nextPageToken"].as_str() {
            Some(token) if !token.is_empty() => page_token = Some(token.to_owned()),
            _ => break,
        }
    }
    assert_eq!(seen, mine);

    // Over HTTP+JSON too.
    let res = a.get(&b, "/rest/tasks", Some(&for_a)).await;
    assert_eq!(res.status, 200, "{}", res.json);
    let listed: Vec<&str> = res.json["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(task_id)
        .collect();
    assert_eq!(listed, mine);

    let res = c.rpc(&b, Some(&for_c), "ListTasks", json!({})).await;
    let listed: Vec<&str> = res.json["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(task_id)
        .collect();
    assert_eq!(listed, [theirs.as_str()]);
    assert_eq!(res.json["result"]["totalSize"], 1);
}

#[tokio::test]
async fn streamed_task_is_owned_by_its_creator() {
    let (b, a) = pair().await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let for_a = grant(&b, &a, &SEND_AND_READ, 3600);
    let for_c = grant(&b, &c, &SEND_AND_READ, 3600);

    let res = a
        .rpc(
            &b,
            Some(&for_a),
            "SendStreamingMessage",
            message_params("streamed", None),
        )
        .await;
    let events = common::sse_results(&res);
    let id = events[0]["task"]["id"].as_str().expect("a task first");

    let res = a.rpc(&b, Some(&for_a), "GetTask", json!({"id": id})).await;
    assert_eq!(res.json["result"]["id"], id, "{}", res.json);
    assert_eq!(
        res.json["result"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );

    let res = c.rpc(&b, Some(&for_c), "GetTask", json!({"id": id})).await;
    assert_eq!(rpc_error(&res)["code"], -32001);

    let res = a.rpc(&b, Some(&for_a), "ListTasks", json!({})).await;
    let listed: Vec<&str> = res.json["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(task_id)
        .collect();
    assert_eq!(listed, [id]);
}

#[tokio::test]
async fn push_config_call_reports_not_supported() {
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    let task = a.ask(&b, &g, "hola").await;
    let params = json!({"taskId": task_id(&task), "url": "https://hooks.invalid/a2a"});

    // A push call needs a grant like any other...
    let res = a
        .rpc(&b, None, "CreateTaskPushNotificationConfig", params.clone())
        .await;
    assert_eq!(res.status, 401);

    // ...and then the SDK refuses it.
    let res = a
        .rpc(&b, Some(&g), "CreateTaskPushNotificationConfig", params)
        .await;
    let err = rpc_error(&res);
    assert_eq!(err["code"], -32003, "{err}");

    // The extended card is not offered either.
    let res = a.rpc(&b, Some(&g), "GetExtendedAgentCard", json!({})).await;
    assert_eq!(rpc_error(&res)["code"], -32004);
}

#[tokio::test]
async fn parts_of_any_media_type_are_taken() {
    // As the card's `*/*` says: a part's media type never refuses it.
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    for part in [
        json!({"raw": "dGNr", "mediaType": "application/x-unsupported-tck-type"}),
        json!({"text": "hola", "mediaType": "text/x-anything"}),
        json!({"text": "hola"}),
    ] {
        let mut params = message_params("hola", None);
        params["message"]["parts"] = json!([part]);
        let res = a.rpc(&b, Some(&g), "SendMessage", params).await;
        assert_eq!(res.status, 200, "{}", res.json);
        assert_eq!(
            res.json["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
            "{part}: {}",
            res.json
        );
    }
}

#[tokio::test]
async fn accepted_and_terminal_events_are_logged_once_per_task() {
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    let store = Arc::clone(&b.node.store);

    let task = a.ask(&b, &g, "hola").await;
    assert_eq!(store.events_count("task.accepted").unwrap(), 1);
    assert_eq!(store.events_count("task.terminal").unwrap(), 1);

    // Reading the task logs nothing.
    a.rpc(&b, Some(&g), "GetTask", json!({"id": task_id(&task)}))
        .await;
    a.rpc(&b, Some(&g), "ListTasks", json!({})).await;
    assert_eq!(store.events_count("task.accepted").unwrap(), 1);
    assert_eq!(store.events_count("task.terminal").unwrap(), 1);

    // A streamed task logs its own pair, once.
    let res = a
        .rpc(
            &b,
            Some(&g),
            "SendStreamingMessage",
            message_params("hola", None),
        )
        .await;
    assert_eq!(res.status, 200);
    assert!(
        String::from_utf8_lossy(&res.body).contains("TASK_STATE_COMPLETED"),
        "{}",
        String::from_utf8_lossy(&res.body)
    );
    assert_eq!(store.events_count("task.accepted").unwrap(), 2);
    assert_eq!(store.events_count("task.terminal").unwrap(), 2);

    // Signed by the node, chained, and holding only ids and states.
    assert_eq!(store.events_verify(&b.id()).unwrap(), 4);
    let events = store.events_since(&b.id(), 0).unwrap();
    assert_eq!(events[0].kind, "task.accepted");
    assert_eq!(
        events[0].payload,
        json!({
            "task_id": task["id"],
            "context_id": task["contextId"],
            "state": "TASK_STATE_SUBMITTED",
        })
    );
    assert_eq!(events[1].kind, "task.terminal");
    assert_eq!(
        events[1].payload,
        json!({
            "task_id": task["id"],
            "context_id": task["contextId"],
            "state": "TASK_STATE_COMPLETED",
        })
    );
}

#[tokio::test]
async fn second_daemon_on_same_data_dir_refuses_to_start() {
    let b = spawn_node(WorkerCfg::Echo).await;
    let err = match Node::spawn(test_config(b.dir.path(), WorkerCfg::Echo)).await {
        Ok(_) => panic!("a second node started on the same data directory"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err.downcast_ref::<AlreadyRunning>(),
            Some(AlreadyRunning::Held { .. })
        ),
        "{err:#}"
    );

    // Once the first one shuts down, the directory is free again.
    let TestNode { node, dir, .. } = b;
    let id = node.id;
    common::shut_down(node).await;
    let again = spawn_node_in(dir, WorkerCfg::Echo, |_| {}).await;
    assert_eq!(again.id(), id);
}

#[tokio::test]
async fn shutdown_lets_go_of_the_store_before_it_returns() {
    // A node that served a request, and a peer that knows it.
    let (b, a) = pair().await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    a.ask(&b, &g, "hola").await;
    let TestNode {
        node, dir: _dir, ..
    } = b;
    let store = Arc::downgrade(&node.store);
    node.shutdown().await.unwrap();
    // Its background tasks are over and every handle on the store it held
    // is gone, as when its process exits: the lock was released last.
    common::wait_for_store_to_close(&store).await;

    // A node with nothing in flight lets go of it before shutdown returns.
    let c = spawn_node(WorkerCfg::Echo).await;
    let TestNode {
        node, dir: _dir, ..
    } = c;
    let store = Arc::downgrade(&node.store);
    node.shutdown().await.unwrap();
    assert_eq!(store.strong_count(), 0, "the store outlives its node");
}

#[tokio::test]
async fn tasks_interrupted_by_a_restart_are_failed_before_serving() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let dir = tempfile::tempdir().unwrap();
    let b_id = key_in(dir.path()).id();
    {
        // What a previous run left behind: a task of A still working.
        let store = Store::open(&dir.path().join("pals.db"), b_id).unwrap();
        let mut message = Message::new(Role::User, vec![Part::text("hola")]);
        message.task_id = Some("t-left".to_owned());
        let task = Task {
            id: "t-left".to_owned(),
            context_id: "ctx".to_owned(),
            status: TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: Some(vec![message]),
            metadata: None,
        };
        store.task_create(&task, now_unix()).unwrap();
        store.task_owner_put("t-left", &a.id()).unwrap();
    }
    let b = spawn_node_in(dir, WorkerCfg::Echo, |_| {}).await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    let res = a
        .rpc(&b, Some(&g), "GetTask", json!({"id": "t-left"}))
        .await;
    let status = &res.json["result"]["status"];
    assert_eq!(status["state"], "TASK_STATE_FAILED", "{}", res.json);
    assert_eq!(status["message"]["parts"][0]["text"], INTERRUPTED_MESSAGE);
}

#[tokio::test]
async fn ticket_names_the_node() {
    let (b, a) = pair().await;
    let ticket: iroh_tickets::endpoint::EndpointTicket = b.node.ticket().parse().unwrap();
    let addr = ticket.endpoint_addr().clone();
    assert_eq!(addr, b.node.addr());
    assert_eq!(addr.id.as_bytes(), b.id().as_bytes());
    let request = axum::http::Request::get("/.well-known/agent-card.json")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = a.node.peer_http.send(addr, request).await.unwrap();
    assert_eq!(res.status(), 200);
}
