//! The `tck` worker adapter: by the prefix of the `messageId`, the agent
//! does exactly what the scenarios of the A2A TCK (`a2aproject/a2a-tck` at
//! `104a7b4ae0d8`, `scenarios/core_operations.feature` and
//! `scenarios/streaming.feature`) say it does, so that the TCK can be run
//! against a PALS daemon through its gateway.

mod common;

use std::time::{Duration, Instant};

use common::{TestNode, grant, spawn_node, spawn_node_with_executor, sse_results};
use pals::config::WorkerCfg;
use pals::worker::tck::{TckExecutor, streaming_timeout};
use pals_core::grant::Operation::{TaskCancel, TaskRead, TaskSend};
use pals_core::grant::SignedGrant;
use serde_json::{Value, json};

/// A provider B, a requester A, and the grant that lets A send to, read and
/// cancel B's tasks.
struct Setup {
    b: TestNode,
    a: TestNode,
    g: SignedGrant,
}

/// [`Setup`] with B running the `tck` adapter, as `worker.adapter = "tck"`
/// configures it.
async fn setup() -> Setup {
    let b = spawn_node(WorkerCfg::Tck).await;
    with_provider(b).await
}

/// [`Setup`] with `b` as the provider.
async fn with_provider(b: TestNode) -> Setup {
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant(&b, &a, &[TaskSend, TaskRead, TaskCancel], 3600);
    Setup { b, a, g }
}

/// The params of a message with `message_id`, continuing `task_id` when
/// given.
fn params(message_id: &str, task_id: Option<&str>) -> Value {
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

impl Setup {
    /// `method` with `params` from A to B; the JSON-RPC `result`.
    async fn call(&self, method: &str, params: Value) -> Value {
        let res = self.a.rpc(&self.b, Some(&self.g), method, params).await;
        assert_eq!(res.status, 200, "{}", res.json);
        assert!(res.json.get("error").is_none(), "{}", res.json);
        res.json["result"].clone()
    }

    /// `SendMessage` with `message_id`: its `result`, `{"task": …}` or
    /// `{"message": …}`.
    async fn send(&self, message_id: &str, task_id: Option<&str>) -> Value {
        self.call("SendMessage", params(message_id, task_id)).await
    }

    /// `SendMessage` with `message_id`; the task it answers with.
    async fn send_task(&self, message_id: &str, task_id: Option<&str>) -> Value {
        let result = self.send(message_id, task_id).await;
        let task = result["task"].clone();
        assert!(task["id"].is_string(), "not a task: {result}");
        task
    }

    /// `method` (a streaming one) with `params`; every event's `result`.
    async fn stream(&self, method: &str, params: Value) -> Vec<Value> {
        let res = self.a.rpc(&self.b, Some(&self.g), method, params).await;
        sse_results(&res)
    }

    /// `GetTask` of `id`.
    async fn get_task(&self, id: &str) -> Value {
        self.call("GetTask", json!({ "id": id })).await
    }
}

fn state(task: &Value) -> &str {
    task["status"]["state"].as_str().unwrap_or("<none>")
}

/// The text of the status message of `task`.
fn status_text(task: &Value) -> &str {
    task["status"]["message"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("<none>")
}

/// The parts of the only artifact of `task`.
fn only_artifact_parts(task: &Value) -> Value {
    let artifacts = task["artifacts"]
        .as_array()
        .expect("the task has artifacts");
    assert_eq!(artifacts.len(), 1, "{task}");
    assert!(
        artifacts[0]["artifactId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{task}"
    );
    artifacts[0]["parts"].clone()
}

/// `value` with every number as a double. A data part is a protobuf
/// `Struct`, whose numbers are doubles: the SDK's ProtoJSON writes `42` as
/// `42.0`, the same JSON number.
fn as_doubles(value: Value) -> Value {
    match value {
        Value::Number(n) => json!(n.as_f64().expect("a finite number")),
        Value::Array(items) => Value::Array(items.into_iter().map(as_doubles).collect()),
        Value::Object(members) => Value::Object(
            members
                .into_iter()
                .map(|(name, member)| (name, as_doubles(member)))
                .collect(),
        ),
        other => other,
    }
}

/// One line per event of a stream: `task <state>`, `status <state>`,
/// `artifact <parts> append=<append> last=<lastChunk>` or `message <parts>`.
fn describe(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            if let Some(task) = event.get("task") {
                format!("task {}", state(task))
            } else if let Some(update) = event.get("statusUpdate") {
                format!("status {}", state(update))
            } else if let Some(update) = event.get("artifactUpdate") {
                format!(
                    "artifact {} append={} last={}",
                    update["artifact"]["parts"], update["append"], update["lastChunk"]
                )
            } else if let Some(message) = event.get("message") {
                format!("message {}", message["parts"])
            } else {
                format!("unknown {event}")
            }
        })
        .collect()
}

const COMPLETED: &str = "TASK_STATE_COMPLETED";

#[tokio::test]
async fn complete_task_prefix() {
    let s = setup().await;
    // `tck-complete-task`, and any messageId no scenario names.
    for message_id in ["tck-complete-task-1", "tck-unknown-1", "plain-message-1"] {
        let task = s.send_task(message_id, None).await;
        assert_eq!(state(&task), COMPLETED, "{message_id}: {task}");
        assert_eq!(status_text(&task), "Hello from TCK", "{message_id}: {task}");
        assert_eq!(task["status"]["message"]["role"], "ROLE_AGENT", "{task}");
        assert_eq!(task["artifacts"], Value::Null, "{message_id}: {task}");

        let stored = s.get_task(task["id"].as_str().unwrap()).await;
        assert_eq!(state(&stored), COMPLETED, "{stored}");
        assert_eq!(status_text(&stored), "Hello from TCK", "{stored}");
    }
}

#[tokio::test]
async fn artifact_text_file_url_data() {
    let s = setup().await;
    for (message_id, part) in [
        (
            "tck-artifact-text-1",
            json!({"text": "Generated text content"}),
        ),
        (
            // "tck" in base64.
            "tck-artifact-file-1",
            json!({"raw": "dGNr", "filename": "output.txt", "mediaType": "text/plain"}),
        ),
        (
            "tck-artifact-file-url-1",
            json!({
                "url": "https://example.com/output.txt",
                "filename": "output.txt",
                "mediaType": "text/plain",
            }),
        ),
        (
            "tck-artifact-data-1",
            json!({"data": {"key": "value", "count": 42}}),
        ),
    ] {
        let task = s.send_task(message_id, None).await;
        assert_eq!(state(&task), COMPLETED, "{message_id}: {task}");
        let expected = as_doubles(json!([part]));
        assert_eq!(
            as_doubles(only_artifact_parts(&task)),
            expected,
            "{message_id}"
        );
        // The artifact is kept with the task.
        let stored = s.get_task(task["id"].as_str().unwrap()).await;
        assert_eq!(
            as_doubles(only_artifact_parts(&stored)),
            expected,
            "{message_id}"
        );
    }
}

#[tokio::test]
async fn longest_prefix_wins() {
    let s = setup().await;
    // Both `tck-artifact-file` and `tck-artifact-file-url` are prefixes.
    let task = s.send_task("tck-artifact-file-url-9", None).await;
    assert_eq!(
        only_artifact_parts(&task)[0]["url"],
        "https://example.com/output.txt",
        "{task}"
    );
    let task = s.send_task("tck-artifact-file-9", None).await;
    assert_eq!(only_artifact_parts(&task)[0]["raw"], "dGNr", "{task}");
    // `tck-stream-artifact-file` is not `tck-artifact-file`.
    let events = s
        .stream(
            "SendStreamingMessage",
            params("tck-stream-artifact-file-9", None),
        )
        .await;
    assert_eq!(describe(&events)[1], "status TASK_STATE_WORKING");
}

#[tokio::test]
async fn message_response_prefix() {
    let s = setup().await;
    let result = s.send("tck-message-response-1", None).await;
    assert!(result.get("task").is_none(), "{result}");
    let message = &result["message"];
    assert_eq!(message["role"], "ROLE_AGENT", "{result}");
    assert_eq!(
        message["parts"],
        json!([{"text": "Direct message response"}]),
        "{result}"
    );
    assert!(message.get("taskId").is_none(), "{result}");
    assert!(
        message["messageId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{result}"
    );

    // Streamed: exactly that message, and the stream ends.
    let events = s
        .stream(
            "SendStreamingMessage",
            params("tck-message-response-2", None),
        )
        .await;
    assert_eq!(
        describe(&events),
        [r#"message [{"text":"Direct message response"}]"#]
    );
}

#[tokio::test]
async fn input_required_and_reject() {
    let s = setup().await;
    let task = s.send_task("tck-input-required-1", None).await;
    assert_eq!(state(&task), "TASK_STATE_INPUT_REQUIRED", "{task}");
    let id = task["id"].as_str().unwrap().to_owned();
    assert_eq!(state(&s.get_task(&id).await), "TASK_STATE_INPUT_REQUIRED");

    // The task takes more input: another turn that needs input, then one
    // that completes it, as the TCK's history tests send.
    let task = s.send_task("tck-input-required-1-1", Some(&id)).await;
    assert_eq!(task["id"], id.as_str());
    assert_eq!(state(&task), "TASK_STATE_INPUT_REQUIRED", "{task}");
    let task = s.send_task("tck-complete-task-1", Some(&id)).await;
    assert_eq!(state(&task), COMPLETED, "{task}");
    assert_eq!(status_text(&task), "Hello from TCK", "{task}");
    let history: Vec<&str> = task["history"]
        .as_array()
        .expect("the task has a history")
        .iter()
        .filter(|message| message["role"] == "ROLE_USER")
        .map(|message| message["messageId"].as_str().unwrap())
        .collect();
    assert_eq!(
        history,
        [
            "tck-input-required-1",
            "tck-input-required-1-1",
            "tck-complete-task-1"
        ],
        "{task}"
    );

    // A task waiting for input can be canceled.
    let waiting = s.send_task("tck-input-required-2", None).await;
    let canceled = s.call("CancelTask", json!({ "id": waiting["id"] })).await;
    assert_eq!(state(&canceled), "TASK_STATE_CANCELED", "{canceled}");

    // `tck-reject-task`: the executor rejects the task, with the reason
    // `rejected`.
    let task = s.send_task("tck-reject-task-1", None).await;
    assert_eq!(state(&task), "TASK_STATE_REJECTED", "{task}");
    assert_eq!(status_text(&task), "rejected", "{task}");
}

#[tokio::test]
async fn streaming_sequences_match_the_feature_file() {
    let s = setup().await;
    let text = |text: &str| json!([{ "text": text }]).to_string();
    let file =
        json!([{"raw": "dGNr", "filename": "output.txt", "mediaType": "text/plain"}]).to_string();
    let working = "status TASK_STATE_WORKING".to_owned();
    let completed = "status TASK_STATE_COMPLETED".to_owned();
    let submitted = "task TASK_STATE_SUBMITTED".to_owned();
    let artifact = |parts: &str| format!("artifact {parts} append=null last=null");
    for (prefix, expected) in [
        (
            "tck-stream-001",
            vec![
                submitted.clone(),
                working.clone(),
                artifact(&text("Stream hello from TCK")),
                completed.clone(),
            ],
        ),
        ("tck-stream-002", vec![submitted.clone(), completed.clone()]),
        (
            "tck-stream-003",
            vec![
                submitted.clone(),
                working.clone(),
                artifact(&text("Stream task lifecycle")),
                completed.clone(),
            ],
        ),
        (
            "tck-stream-ordering-001",
            vec![
                submitted.clone(),
                working.clone(),
                artifact(&text("Ordered output")),
                completed.clone(),
            ],
        ),
        (
            "tck-stream-artifact-text",
            vec![
                submitted.clone(),
                working.clone(),
                artifact(&text("Streamed text content")),
                completed.clone(),
            ],
        ),
        (
            "tck-stream-artifact-file",
            vec![
                submitted.clone(),
                working.clone(),
                artifact(&file),
                completed.clone(),
            ],
        ),
        (
            "tck-stream-artifact-chunked",
            vec![
                submitted.clone(),
                working.clone(),
                // `lastChunk: false`, the default, which ProtoJSON omits.
                format!("artifact {} append=null last=null", text("chunk-1 ")),
                format!("artifact {} append=true last=true", text("chunk-2")),
                completed.clone(),
            ],
        ),
    ] {
        let message_id = format!("{prefix}-1");
        let events = s
            .stream("SendStreamingMessage", params(&message_id, None))
            .await;
        assert_eq!(describe(&events), expected, "{prefix}: {events:?}");

        // Every event is about the one task the stream began with.
        let id = events[0]["task"]["id"].as_str().unwrap().to_owned();
        for event in &events[1..] {
            let update = event
                .get("statusUpdate")
                .or_else(|| event.get("artifactUpdate"))
                .unwrap();
            assert_eq!(update["taskId"], id.as_str(), "{prefix}: {event}");
        }

        // The stored task ends as the stream did.
        let stored = s.get_task(&id).await;
        assert_eq!(state(&stored), COMPLETED, "{prefix}: {stored}");
    }

    // The two chunks are one artifact: same id, its parts in order.
    let events = s
        .stream(
            "SendStreamingMessage",
            params("tck-stream-artifact-chunked-2", None),
        )
        .await;
    let chunk_ids: Vec<&Value> = events
        .iter()
        .filter_map(|event| event.get("artifactUpdate"))
        .map(|update| &update["artifact"]["artifactId"])
        .collect();
    assert_eq!(chunk_ids.len(), 2);
    assert_eq!(chunk_ids[0], chunk_ids[1], "{events:?}");
    let stored = s.get_task(events[0]["task"]["id"].as_str().unwrap()).await;
    assert_eq!(
        only_artifact_parts(&stored),
        json!([{"text": "chunk-1 "}, {"text": "chunk-2"}]),
        "{stored}"
    );
}

#[tokio::test]
async fn resubscribe_task_stays_active_for_twice_the_timeout() {
    // As if the daemon ran with TCK_STREAMING_TIMEOUT=2: long enough for the
    // subscription to arrive while the task runs, on a loaded host too.
    let timeout = streaming_timeout(Some("2"));
    assert_eq!(timeout, Duration::from_secs(2));
    let b = spawn_node_with_executor(TckExecutor::new(timeout)).await;
    let s = with_provider(b).await;

    // As the TCK does: start the task, then subscribe to it while it runs.
    let started = Instant::now();
    let mut start = params("test-resubscribe-message-id-1", None);
    start["configuration"] = json!({"returnImmediately": true});
    let task = s.call("SendMessage", start).await["task"].clone();
    let id = task["id"].as_str().expect("a task").to_owned();
    assert!(
        !matches!(
            state(&task),
            COMPLETED | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED"
        ),
        "{task}"
    );

    let events = s.stream("SubscribeToTask", json!({ "id": id })).await;
    let elapsed = started.elapsed();
    let described = describe(&events);
    assert!(described[0].starts_with("task "), "{described:?}");
    assert_eq!(
        described.last().map(String::as_str),
        Some("status TASK_STATE_COMPLETED"),
        "{described:?}"
    );
    assert!(
        elapsed >= 2 * timeout,
        "the task completed after {elapsed:?}"
    );

    // The same scenario, streamed from the start.
    let started = Instant::now();
    let events = s
        .stream(
            "SendStreamingMessage",
            params("test-resubscribe-message-id-2", None),
        )
        .await;
    let elapsed = started.elapsed();
    assert_eq!(
        describe(&events),
        [
            "task TASK_STATE_SUBMITTED",
            "status TASK_STATE_WORKING",
            "status TASK_STATE_COMPLETED"
        ]
    );
    assert!(
        elapsed >= 2 * timeout,
        "the task completed after {elapsed:?}"
    );
}
