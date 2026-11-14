//! Idempotency of A2A messages: the same `(peer, messageId)` runs once, a
//! retry attaches to the task of the first attempt, and a `messageId` reused
//! with different content is refused.

mod common;

use std::time::Duration;

use common::{
    CountingExecutor, TestNode, TestResponse, grant, key_in, message_params, now_unix, spawn_node,
    spawn_node_in_with_executor, spawn_node_with_executor, sse_results,
};
use pals::config::WorkerCfg;
use pals::node::STORE_FILE;
use pals_core::grant::Operation::{TaskRead, TaskSend};
use pals_core::grant::{Operation, SignedGrant};
use pals_core::identity::AgentKey;
use pals_core::store::{DEDUP_RETENTION_S, DedupBegin, DedupRow, Store};
use serde_json::{Value, json};

const SEND_AND_READ: [Operation; 2] = [TaskSend, TaskRead];

/// A serving node B whose executor counts its runs and takes `delay`, a
/// requesting node A, and the grant that lets A use B.
async fn setup(delay: Duration) -> (TestNode, TestNode, SignedGrant, CountingExecutor) {
    let exec = CountingExecutor::new(delay);
    let b = spawn_node_with_executor(exec.clone()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant(&b, &a, &SEND_AND_READ, 3600);
    (b, a, g, exec)
}

/// The task a unary `SendMessage` answered with.
fn task_of(res: &TestResponse) -> &Value {
    assert_eq!(res.status, 200, "{}", res.json);
    let task = &res.json["result"]["task"];
    assert!(task["id"].is_string(), "{}", res.json);
    task
}

/// The task id an SSE event names.
fn event_task_id(event: &Value) -> &Value {
    if event.get("task").is_some() {
        &event["task"]["id"]
    } else if event.get("statusUpdate").is_some() {
        &event["statusUpdate"]["taskId"]
    } else {
        &event["artifactUpdate"]["taskId"]
    }
}

#[tokio::test]
async fn same_message_id_sent_twice_runs_once() {
    let (b, a, g, exec) = setup(Duration::ZERO).await;
    let params = message_params("hola", None);

    let first = a.rpc(&b, Some(&g), "SendMessage", params.clone()).await;
    let t1 = task_of(&first);
    assert_eq!(t1["status"]["state"], "TASK_STATE_COMPLETED", "{t1}");
    let second = a.rpc(&b, Some(&g), "SendMessage", params.clone()).await;
    let t2 = task_of(&second);
    assert_eq!(exec.runs(), 1);
    assert_eq!(t1["id"], t2["id"]);
    assert_eq!(t2["artifacts"][0]["parts"][0]["text"], "echo: hola");

    // The same request over the HTTP+JSON binding is the same message too.
    let res = a
        .request(
            &b,
            "POST",
            "/rest/message:send",
            &[
                ("a2a-version", "1.0"),
                ("authorization", &common::bearer(&g)),
            ],
            Some(&params),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.json["task"]["id"], t1["id"]);
    assert_eq!(exec.runs(), 1);

    // A retry creates nothing: one task, one task.accepted event.
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 1);
    let res = a.rpc(&b, Some(&g), "ListTasks", json!({})).await;
    assert_eq!(res.json["result"]["totalSize"], 1, "{}", res.json);
}

#[tokio::test]
async fn retry_while_first_execution_is_running_attaches_to_it() {
    let (b, a, g, exec) = setup(Duration::from_secs(2)).await;
    let params = message_params("hola", None);
    let send = |delay_ms: u64| {
        let params = params.clone();
        let (a, b, g) = (&a, &b, &g);
        async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            a.rpc(b, Some(g), "SendMessage", params).await
        }
    };

    // Two identical requests at once, and a retry 200 ms later.
    let (first, twin, retry) = tokio::join!(send(0), send(0), send(200));
    assert_eq!(exec.runs(), 1);
    let id = &task_of(&first)["id"];
    assert_eq!(&task_of(&twin)["id"], id);
    assert_eq!(&task_of(&retry)["id"], id);
    assert_eq!(
        task_of(&retry)["status"]["state"],
        "TASK_STATE_COMPLETED",
        "{}",
        retry.json
    );
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 1);
}

#[tokio::test]
async fn streaming_retry_resubscribes_instead_of_reexecuting() {
    let (b, a, g, exec) = setup(Duration::from_secs(2)).await;
    let params = message_params("hola", None);

    let (first, retry) = tokio::join!(
        a.rpc(&b, Some(&g), "SendStreamingMessage", params.clone()),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            a.rpc(&b, Some(&g), "SendStreamingMessage", params.clone())
                .await
        }
    );
    assert_eq!(exec.runs(), 1);
    let first = sse_results(&first);
    let retry = sse_results(&retry);
    let id = event_task_id(&first[0]).clone();
    assert!(id.is_string(), "{first:?}");

    // The retry follows the running task: its state then, the events after.
    assert_eq!(retry[0]["task"]["id"], id, "{retry:?}");
    assert_eq!(
        retry[0]["task"]["status"]["state"], "TASK_STATE_WORKING",
        "{retry:?}"
    );
    assert!(retry.iter().all(|event| event_task_id(event) == &id));
    for events in [&first, &retry] {
        let last = events.last().unwrap();
        assert_eq!(
            last["statusUpdate"]["status"]["state"], "TASK_STATE_COMPLETED",
            "{events:?}"
        );
    }

    // Once the task is terminal, a retry is one event with the task.
    let res = a
        .rpc(&b, Some(&g), "SendStreamingMessage", params.clone())
        .await;
    let events = sse_results(&res);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["task"]["id"], id);
    assert_eq!(events[0]["task"]["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(
        events[0]["task"]["artifacts"][0]["parts"][0]["text"],
        "echo: hola"
    );

    // A unary retry of the streamed message gets the same task.
    let res = a.rpc(&b, Some(&g), "SendMessage", params).await;
    assert_eq!(task_of(&res)["id"], id);
    assert_eq!(exec.runs(), 1);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 1);
}

#[tokio::test]
async fn same_id_with_different_content_is_invalid_params() {
    let (b, a, g, exec) = setup(Duration::ZERO).await;
    let params = message_params("hola", None);
    let task = task_of(&a.rpc(&b, Some(&g), "SendMessage", params.clone()).await).clone();

    let mut changed = params.clone();
    changed["message"]["parts"][0]["text"] = json!("adiós");
    for method in ["SendMessage", "SendStreamingMessage"] {
        let res = a.rpc(&b, Some(&g), method, changed.clone()).await;
        let err = &res.json["error"];
        assert_eq!(err["code"], -32602, "{method}: {}", res.json);
        assert_eq!(
            err["message"], "messageId reused with different content",
            "{method}"
        );
    }
    // A change outside the message counts as different content too.
    let mut configured = params.clone();
    configured["configuration"] = json!({"historyLength": 0});
    let res = a.rpc(&b, Some(&g), "SendMessage", configured).await;
    assert_eq!(res.json["error"]["code"], -32602, "{}", res.json);

    assert_eq!(exec.runs(), 1);
    // The original stays as it was.
    let res = a.rpc(&b, Some(&g), "SendMessage", params).await;
    assert_eq!(task_of(&res)["id"], task["id"]);
}

#[tokio::test]
async fn dedup_survives_daemon_restart() {
    let (b, a, g, exec) = setup(Duration::ZERO).await;
    let params = message_params("hola", None);
    let t1 = task_of(&a.rpc(&b, Some(&g), "SendMessage", params.clone()).await).clone();

    let TestNode { node, dir } = b;
    node.shutdown().await.unwrap();
    let b = spawn_node_in_with_executor(dir, exec.clone()).await;

    let res = a.rpc(&b, Some(&g), "SendMessage", params.clone()).await;
    assert_eq!(task_of(&res)["id"], t1["id"]);
    assert_eq!(task_of(&res)["status"]["state"], "TASK_STATE_COMPLETED");
    let res = a.rpc(&b, Some(&g), "SendStreamingMessage", params).await;
    let events = sse_results(&res);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["task"]["id"], t1["id"]);
    assert_eq!(exec.runs(), 1);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 1);
}

#[tokio::test]
async fn two_peers_may_use_the_same_message_id() {
    let (b, a, for_a, exec) = setup(Duration::ZERO).await;
    let c = spawn_node(WorkerCfg::Echo).await;
    let for_c = grant(&b, &c, &SEND_AND_READ, 3600);
    let params = message_params("hola", None);

    let from_a = a.rpc(&b, Some(&for_a), "SendMessage", params.clone()).await;
    let from_c = c.rpc(&b, Some(&for_c), "SendMessage", params.clone()).await;
    assert_eq!(exec.runs(), 2);
    let (ta, tc) = (task_of(&from_a), task_of(&from_c));
    assert_ne!(ta["id"], tc["id"]);

    // Each peer's retry gets its own task.
    let res = c.rpc(&b, Some(&for_c), "SendMessage", params.clone()).await;
    assert_eq!(task_of(&res)["id"], tc["id"]);
    let res = a.rpc(&b, Some(&for_a), "SendMessage", params).await;
    assert_eq!(task_of(&res)["id"], ta["id"]);
    assert_eq!(exec.runs(), 2);
}

#[tokio::test]
async fn purge_removes_only_old_rows() {
    let peer = AgentKey::generate().id();
    let other = AgentKey::generate().id();
    let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
    for (sender, message_id, at) in [
        (&peer, "m-old", 100),
        (&other, "m-old", 150),
        (&peer, "m-edge", 200),
        (&peer, "m-new", 300),
    ] {
        assert_eq!(
            store.dedup_begin(sender, message_id, "h", at).unwrap(),
            DedupBegin::New
        );
    }
    store
        .dedup_bind(&peer, "m-old", Some("t-old"), None)
        .unwrap();

    // Rows strictly older than the cutoff go, bound or not.
    assert_eq!(store.dedup_purge(200).unwrap(), 2);
    assert_eq!(store.dedup_purge(200).unwrap(), 0);
    let existing = DedupBegin::Existing(DedupRow {
        request_hash: "h".to_owned(),
        task_id: None,
        response_json: None,
    });
    assert_eq!(
        store.dedup_begin(&peer, "m-edge", "h", 400).unwrap(),
        existing
    );
    assert_eq!(
        store.dedup_begin(&peer, "m-new", "h", 400).unwrap(),
        existing
    );
    // A purged key is new again.
    assert_eq!(
        store.dedup_begin(&peer, "m-old", "h", 400).unwrap(),
        DedupBegin::New
    );
    assert_eq!(
        store.dedup_begin(&other, "m-old", "h", 400).unwrap(),
        DedupBegin::New
    );

    // A node purges what is older than the retention when it starts.
    let dir = tempfile::tempdir().unwrap();
    let owner = key_in(dir.path()).id();
    let now = now_unix();
    {
        let store = Store::open(&dir.path().join(STORE_FILE), owner).unwrap();
        store
            .dedup_begin(&peer, "m-expired", "h", now - DEDUP_RETENTION_S - 60)
            .unwrap();
        store
            .dedup_begin(&peer, "m-kept", "h", now - DEDUP_RETENTION_S + 3600)
            .unwrap();
    }
    let node = spawn_node_in_with_executor(dir, CountingExecutor::new(Duration::ZERO)).await;
    let store = &node.node.store;
    assert_eq!(
        store.dedup_begin(&peer, "m-expired", "h", now).unwrap(),
        DedupBegin::New
    );
    assert_eq!(
        store.dedup_begin(&peer, "m-kept", "h", now).unwrap(),
        existing
    );
}
