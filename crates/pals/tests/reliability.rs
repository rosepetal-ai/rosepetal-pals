//! The requester side: an ask keeps its `messageId` from before its first
//! attempt, so that a cut connection, a provider that is down or restarts
//! and a requester that restarts never get the question run twice; and the
//! `pals ask` and `pals task` commands.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use a2a::{ListTasksRequest, PartContent, TaskState};
use assert_cmd::cargo::cargo_bin_cmd;
use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use common::{
    CountingExecutor, TestNode, grant, issue_grant, key_in, now_unix, spawn_node, spawn_node_in,
    spawn_node_in_with_executor, spawn_node_with_executor, stub_peer,
};
use pals::config::WorkerCfg;
use pals::outbox::{ASK_MAX_ATTEMPTS, AskError, AskOutcome, AskRequest, Outbox};
use pals_core::grant::Operation::{self, TaskCancel, TaskRead, TaskSend};
use pals_core::identity::AgentKey;
use pals_core::store::{Contact, INTERRUPTED_MESSAGE, OutboxRow, Store};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const OPS: [Operation; 3] = [TaskSend, TaskRead, TaskCancel];
/// How long the asks of these tests wait for their answer.
const WAIT: Duration = Duration::from_secs(120);

/// Saves `b` in `a` as contact `bob`, and gives `a` a grant of `b` to send,
/// read and cancel tasks, which `a` holds.
fn connect(a: &TestNode, b: &TestNode) {
    connect_as(a, "bob", b);
}

/// [`connect`], with `b` saved as `name`.
fn connect_as(a: &TestNode, name: &str, b: &TestNode) {
    a.add_contact(name, b);
    a.hold(&grant(b, a, &OPS, 3600));
}

/// A provider that is not running: its data directory, with its key, and a
/// grant of it that `a` holds. `a` knows it as `bob` by its id alone, with
/// no address, so that reaching it fails at once.
fn absent_provider(a: &TestNode) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a data directory");
    let key = key_in(dir.path());
    absent_contact(a, "bob", &key);
    dir
}

/// Gives `a` a grant of `key`'s agent and saves that agent as contact
/// `name` with no address.
fn absent_contact(a: &TestNode, name: &str, key: &AgentKey) {
    let now = now_unix();
    a.hold(&issue_grant(key, a.id(), key.id(), &OPS, now, now + 3600));
    a.node
        .store
        .contact_put(&Contact {
            name: name.to_owned(),
            agent_id: key.id(),
            ticket: None,
            added_at: now,
        })
        .expect("save the contact");
}

fn request(question: &str) -> AskRequest {
    AskRequest {
        peer: "bob".to_owned(),
        question: question.to_owned(),
        attachments: Vec::new(),
        wait: WAIT,
        offer: None,
    }
}

/// `a` asks `bob` `question`, on its own task.
fn ask(a: &TestNode, question: &str) -> JoinHandle<Result<AskOutcome, AskError>> {
    let outbox = a.node.outbox().clone();
    let req = request(question);
    tokio::spawn(async move { outbox.ask(req).await })
}

/// What `ask` ended with; it must end within 90 s.
async fn outcome_of(ask: JoinHandle<Result<AskOutcome, AskError>>) -> Result<AskOutcome, AskError> {
    tokio::time::timeout(Duration::from_secs(90), ask)
        .await
        .expect("the ask hung")
        .expect("the ask panicked")
}

/// The rows of `a`'s outbox still waiting for an outcome.
fn pending(a: &TestNode) -> Vec<OutboxRow> {
    a.node.outbox().pending().expect("read the outbox")
}

/// Waits up to 30 s for `done`.
async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits for `a`'s only pending ask to know its task, and returns its row.
async fn wait_for_task(a: &TestNode) -> OutboxRow {
    wait_until("the task id", || {
        pending(a).first().is_some_and(|row| row.task_id.is_some())
    })
    .await;
    pending(a).remove(0)
}

#[tokio::test]
async fn cut_connection_mid_task_still_yields_one_execution() {
    let exec = CountingExecutor::new(Duration::from_secs(2));
    let b = spawn_node_with_executor(exec.clone()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    connect(&a, &b);

    let asking = ask(&a, "hola");
    // The task is WORKING, and the requester has seen it: cut the network.
    wait_until("the run", || exec.runs() == 1).await;
    wait_for_task(&a).await;
    a.node.close_peer_connections().await;

    let outcome = outcome_of(asking).await.expect("the ask completes");
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED", "{outcome:?}");
    assert_eq!(outcome.answer.as_deref(), Some("echo: hola"));
    assert!(outcome.attempts >= 2, "{outcome:?}");
    assert_eq!(exec.runs(), 1);
    assert!(pending(&a).is_empty());
}

#[tokio::test]
async fn provider_down_then_up_completes_once() {
    let exec = CountingExecutor::new(Duration::ZERO);
    let a = spawn_node(WorkerCfg::Echo).await;
    let b_dir = absent_provider(&a);

    let asking = ask(&a, "hola");
    // A second attempt has started: the first found no one.
    wait_until("a retry", || {
        pending(&a).first().is_some_and(|row| row.attempts >= 2)
    })
    .await;
    let b = spawn_node_in_with_executor(b_dir, exec.clone()).await;
    a.add_contact("bob", &b);

    let outcome = outcome_of(asking).await.expect("the ask completes");
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED", "{outcome:?}");
    assert_eq!(outcome.answer.as_deref(), Some("echo: hola"));
    assert!(outcome.attempts >= 2, "{outcome:?}");
    assert_eq!(exec.runs(), 1);
    assert!(pending(&a).is_empty());
}

#[tokio::test]
async fn provider_never_up_leaves_a_pending_row_and_resume_finishes_it() {
    let exec = CountingExecutor::new(Duration::ZERO);
    let a = spawn_node(WorkerCfg::Echo).await;
    let b_dir = absent_provider(&a);

    let result = a.node.outbox().ask(request("hola")).await;
    let Err(AskError::Unreachable { message_id }) = result else {
        panic!("not unreachable: {result:?}");
    };
    let rows = pending(&a);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].message_id, message_id);
    assert_eq!(rows[0].attempts, ASK_MAX_ATTEMPTS + 1, "{rows:?}");
    assert_eq!(rows[0].task_id, None);

    let b = spawn_node_in_with_executor(b_dir, exec.clone()).await;
    a.add_contact("bob", &b);
    let outcome = a
        .node
        .outbox()
        .resume(&message_id, WAIT)
        .await
        .expect("the resumed ask completes");
    assert_eq!(outcome.message_id, message_id);
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED", "{outcome:?}");
    assert_eq!(outcome.answer.as_deref(), Some("echo: hola"));
    assert_eq!(exec.runs(), 1);
    assert!(pending(&a).is_empty());
}

#[tokio::test]
async fn provider_restart_mid_task_surfaces_failed_not_a_hang() {
    let exec = CountingExecutor::new(Duration::from_secs(60));
    let b = spawn_node_with_executor(exec.clone()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    connect(&a, &b);

    let asking = ask(&a, "hola");
    wait_until("the run", || exec.runs() == 1).await;
    wait_for_task(&a).await;
    // The provider restarts on the same data directory, at a new address.
    // Its run goes on, holding the old node's store, as in a process that
    // crashed later: the new one opens the database beside it, which the
    // store's busy timeout makes safe.
    let TestNode { node, dir, .. } = b;
    node.shutdown().await.expect("shut the provider down");
    let b = spawn_node_in_with_executor(dir, exec.clone()).await;
    a.add_contact("bob", &b);

    let outcome = outcome_of(asking).await.expect("the ask ends");
    assert_eq!(outcome.state, "TASK_STATE_FAILED", "{outcome:?}");
    let status_text = outcome.answer.clone().unwrap_or_default();
    assert!(status_text.contains("interrupted"), "{outcome:?}");
    assert_eq!(status_text, INTERRUPTED_MESSAGE);
    assert_eq!(exec.runs(), 1);
    assert!(pending(&a).is_empty());
}

#[tokio::test]
async fn requester_restart_does_not_resend_pending_asks() {
    let exec = CountingExecutor::new(Duration::ZERO);
    let a = spawn_node(WorkerCfg::Echo).await;
    let b_dir = absent_provider(&a);
    let result = a.node.outbox().ask(request("hola")).await;
    let Err(AskError::Unreachable { message_id }) = result else {
        panic!("not unreachable: {result:?}");
    };

    // The requester restarts, and the provider is up now.
    let TestNode { node, dir, .. } = a;
    common::shut_down(node).await;
    let a = spawn_node_in(dir, WorkerCfg::Echo, |_| {}).await;
    let b = spawn_node_in_with_executor(b_dir, exec.clone()).await;
    a.add_contact("bob", &b);
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Nothing was sent: the question waits, as it was, to be resumed.
    assert_eq!(exec.runs(), 0);
    let rows = pending(&a);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].message_id, message_id);
    assert_eq!(rows[0].attempts, ASK_MAX_ATTEMPTS + 1, "{rows:?}");
    let outcome = a
        .node
        .outbox()
        .resume(&message_id, WAIT)
        .await
        .expect("the resumed ask completes");
    assert_eq!(outcome.state, "TASK_STATE_COMPLETED", "{outcome:?}");
    assert_eq!(exec.runs(), 1);
}

#[tokio::test]
async fn cancel_reaches_the_provider() {
    let exec = CountingExecutor::new(Duration::from_secs(60));
    let b = spawn_node_with_executor(exec.clone()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    connect(&a, &b);

    let asking = ask(&a, "hola");
    wait_until("the run", || exec.runs() == 1).await;
    let row = wait_for_task(&a).await;
    let outcome = a
        .node
        .outbox()
        .cancel(&row.message_id)
        .await
        .expect("the cancel is answered");
    assert_eq!(outcome.state, "TASK_STATE_CANCELED", "{outcome:?}");
    assert_eq!(outcome.message_id, row.message_id);
    assert_eq!(outcome.task_id, row.task_id);

    // The provider's task is canceled, and the ask that waited for it ends.
    let task_id = row.task_id.expect("a task id");
    let task = b.node.store.task_get(&task_id).unwrap().expect("the task");
    assert_eq!(task.status.state, TaskState::Canceled);
    let asked = outcome_of(asking).await.expect("the ask ends");
    assert_eq!(asked.state, "TASK_STATE_CANCELED", "{asked:?}");
    assert_eq!(exec.runs(), 1);
    assert!(pending(&a).is_empty());
}

/// Runs `pals --home <home> <args>`, off the async threads, and returns its
/// exit code, stdout and stderr.
async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .arg("--home")
        .arg(home)
        .args(args);
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .expect("the command's thread");
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
    )
}

#[tokio::test]
async fn ask_cli_exit_codes() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_node(WorkerCfg::Echo).await;
    connect(&a, &b);
    let f = spawn_node_with_executor(CountingExecutor::failing()).await;
    connect_as(&a, "fail", &f);
    absent_contact(&a, "gone", &AgentKey::generate());
    // The CLI finds the running daemon by its configuration.
    let home = a.dir.path().to_owned();
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    // Outside the data directory: nothing in it is ever sent.
    let files = tempfile::tempdir().unwrap();
    let notes = files.path().join("notes.txt");
    std::fs::write(&notes, "some notes").unwrap();
    let key = home.join("agent.key");
    let (code, _, stderr) = pals(
        &home,
        &["ask", "bob", "hola", "--attach", key.to_str().unwrap()],
    )
    .await;
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("data directory"), "{stderr}");

    // 0: completed, with the answer on stdout; the attachment travels as a
    // file part.
    let attach = notes.to_str().unwrap();
    let (code, stdout, stderr) = pals(&home, &["ask", "bob", "hola", "--attach", attach]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("echo: hola"), "{stdout}");
    let all = ListTasksRequest {
        context_id: None,
        status: None,
        page_size: None,
        page_token: None,
        history_length: None,
        status_timestamp_after: None,
        include_artifacts: None,
        tenant: None,
    };
    let tasks = b.node.store.tasks_list(&all).unwrap().tasks;
    assert_eq!(tasks.len(), 1);
    let history = tasks[0].history.as_ref().expect("a history");
    let file = &history[0].parts[1];
    match &file.content {
        PartContent::Url(url) => assert!(url.starts_with("iroh-blob://"), "{url}"),
        other => panic!("not a file part: {other:?}"),
    }
    assert_eq!(file.filename.as_deref(), Some("notes.txt"));
    assert_eq!(file.media_type.as_deref(), Some("text/plain"));

    // 2: failed.
    let (code, stdout, stderr) = pals(&home, &["ask", "fail", "hola"]).await;
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stderr.contains("TASK_STATE_FAILED"), "{stderr}");

    // 3: unreachable, with the command that resumes it.
    let (code, stdout, stderr) = pals(&home, &["ask", "gone", "hola", "--wait", "60s"]).await;
    assert_eq!(code, 3, "{stdout}{stderr}");
    let message_id = stderr
        .split("pals task resume ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| c.is_whitespace() || c == '`').next())
        .unwrap_or_else(|| panic!("no resume command in {stderr}"))
        .to_owned();
    assert_eq!(pending(&a)[0].message_id, message_id);
    let (code, stdout, stderr) = pals(&home, &["task", "ls"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let line = stdout
        .lines()
        .find(|line| line.contains(&message_id))
        .unwrap_or_else(|| panic!("{message_id} not listed: {stdout}"));
    assert!(line.contains("pending"), "{line}");
}

/// The requests a stub peer got: `<method> <path>`.
type Seen = Arc<Mutex<Vec<String>>>;

/// An app that records every request in `seen` and answers what `answer`
/// gives for its path, or `404`.
fn recording_app(
    seen: Seen,
    answer: impl Fn(&str) -> Option<Response> + Clone + Send + Sync + 'static,
) -> axum::Router {
    axum::Router::new().fallback(move |request: Request| {
        let (seen, answer) = (seen.clone(), answer.clone());
        async move {
            let path = request.uri().path().to_owned();
            seen.lock()
                .unwrap()
                .push(format!("{} {path}", request.method()));
            answer(&path)
                .unwrap_or_else(|| Response::builder().status(404).body(Body::empty()).unwrap())
        }
    })
}

#[tokio::test]
async fn peer_task_id_that_is_not_safe_is_refused() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // Another peer, whose grant the requester's gateway would present.
    let victim_key = AgentKey::generate();
    let victim_seen = Seen::default();
    let _victim = stub_peer(
        &a,
        "victim",
        &victim_key,
        recording_app(victim_seen.clone(), |_| None),
    )
    .await;
    // A peer that names its task with a path that climbs to the victim:
    // `<gw>/peers/<evil>/rest/tasks/<id>` would become
    // `<gw>/peers/<victim>/rest/tasks/x`.
    let climbing = format!("../../../{}/rest/tasks/x", victim_key.id());
    let evil_stream = move |path: &str| {
        (path == "/rest/message:stream").then(|| {
            let event = serde_json::json!({"task": {
                "id": climbing,
                "contextId": "c-1",
                "status": {"state": "TASK_STATE_WORKING"}
            }});
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(format!("data: {event}\n\n")))
                .unwrap()
        })
    };
    let evil_seen = Seen::default();
    let evil_app = recording_app(evil_seen.clone(), evil_stream);
    let _evil = stub_peer(&a, "evil", &AgentKey::generate(), evil_app).await;

    let mut req = request("hola");
    req.peer = "evil".to_owned();
    let result = a.node.outbox().ask(req).await;
    // The gateway cuts every stream that names it, so the ask never learns
    // a task: each attempt breaks, as on a cut connection, until the
    // retries run out.
    let Err(AskError::Unreachable { message_id }) = result else {
        panic!("not cut: {result:?}");
    };

    // Nothing but the question left `/peers/<evil>/rest`, and nothing was
    // kept of the id.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let attempts = ASK_MAX_ATTEMPTS as usize + 1;
    assert_eq!(
        *evil_seen.lock().unwrap(),
        vec!["POST /rest/message:stream"; attempts]
    );
    assert!(victim_seen.lock().unwrap().is_empty());
    let rows = a.node.store.outbox_all().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message_id, message_id);
    assert_eq!(rows[0].task_id, None);
    assert_eq!(rows[0].state, "pending");
}

/// A stand-in for the gateway whose `POST /blobs` holds every upload until
/// `release` is notified, after notifying `uploading`; it records every
/// request it gets in `seen`.
async fn slow_gateway(seen: Seen, uploading: Arc<Notify>, release: Arc<Notify>) -> String {
    let app = recording_app(seen, |_| None).route(
        "/blobs",
        axum::routing::post(move || {
            let (uploading, release) = (Arc::clone(&uploading), Arc::clone(&release));
            async move {
                uploading.notify_one();
                release.notified().await;
                (
                    axum::http::StatusCode::CREATED,
                    axum::Json(serde_json::json!({"url": "http://gw.invalid/blobs/h/x"})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// A question for an agent id, with one attachment, from an outbox that
/// asks through the gateway at `gateway_url`.
fn outbox_with_attachment(gateway_url: String, wait: Duration) -> (Outbox, AskRequest, Arc<Store>) {
    let store = Arc::new(Store::open_in_memory(AgentKey::generate().id()).unwrap());
    let outbox = Outbox::new(Arc::clone(&store), gateway_url);
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "some notes").unwrap();
    let (_, path) = file.keep().unwrap();
    let req = AskRequest {
        peer: AgentKey::generate().id().to_string(),
        question: "hola".to_owned(),
        attachments: vec![path],
        wait,
        offer: None,
    };
    (outbox, req, store)
}

#[tokio::test]
async fn an_ask_canceled_while_its_files_upload_keeps_and_sends_nothing() {
    let (seen, uploading, release) = (Seen::default(), Arc::default(), Arc::new(Notify::new()));
    let gateway = slow_gateway(seen.clone(), Arc::clone(&uploading), Arc::clone(&release)).await;
    let (outbox, req, store) = outbox_with_attachment(gateway, WAIT);
    let cancel = CancellationToken::new();
    let kept = Arc::new(Mutex::new(None));

    let asked = outbox.ask_kept(req, &cancel, {
        let kept = Arc::clone(&kept);
        move |message_id: &str| *kept.lock().unwrap() = Some(message_id.to_owned())
    });
    let canceling = async {
        uploading.notified().await;
        cancel.cancel();
        release.notify_one();
    };
    let (asked, ()) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(asked, canceling)
    })
    .await
    .expect("the ask ended");
    let Err(AskError::Local(reason)) = asked else {
        panic!("not stopped: {asked:?}");
    };
    assert!(reason.contains("nothing was asked"), "{reason}");
    assert_eq!(*kept.lock().unwrap(), None);
    assert!(store.outbox_all().unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|request| request.contains("/blobs")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn the_wait_of_an_ask_counts_its_uploads() {
    let (seen, uploading, release) = (Seen::default(), Arc::default(), Arc::new(Notify::new()));
    let gateway = slow_gateway(seen.clone(), uploading, Arc::clone(&release)).await;
    let (outbox, req, store) = outbox_with_attachment(gateway, Duration::from_secs(2));

    let started = Instant::now();
    let asked = tokio::time::timeout(Duration::from_secs(60), outbox.ask(req))
        .await
        .expect("the ask ended");
    release.notify_one();
    let Err(AskError::Local(reason)) = asked else {
        panic!("not stopped: {asked:?}");
    };
    assert!(reason.contains("nothing was asked"), "{reason}");
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(store.outbox_all().unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|request| request.contains("/blobs")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn ask_cli_prints_the_message_id_as_soon_as_it_is_kept() {
    use std::io::BufRead;

    // A provider that takes a minute to answer.
    let exec = CountingExecutor::new(Duration::from_secs(60));
    let b = spawn_node_with_executor(exec.clone()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    connect(&a, &b);
    let home = a.dir.path().to_owned();
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();

    /// A child process, killed when this value drops.
    struct Killed(std::process::Child);
    impl Drop for Killed {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Killed(
        std::process::Command::new(assert_cmd::cargo::cargo_bin!("pals"))
            .env_remove("PALS_HOME")
            .args([
                "--home",
                home.to_str().unwrap(),
                "ask",
                "bob",
                "hola",
                "--wait",
                "600s",
            ])
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("run pals ask"),
    );
    let stderr = child.0.stderr.take().unwrap();
    // The first line comes while the question is still unanswered.
    let first = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        std::io::BufReader::new(stderr)
            .read_line(&mut line)
            .map(|_| line)
    });
    let first = tokio::time::timeout(Duration::from_secs(60), first)
        .await
        .expect("a line before the answer")
        .unwrap()
        .unwrap();
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "the ask ended: {first}"
    );
    drop(child);

    // The id of the question the outbox kept, and how to take it up again.
    let rows = a.node.store.outbox_all().unwrap();
    assert_eq!(rows.len(), 1);
    let id = &rows[0].message_id;
    assert_eq!(
        first.trim_end(),
        format!("pals: message {id} kept; resume with: pals task resume {id}")
    );
}
