//! The worker: an `ask` runs the configured adapter. With `exec`, that is a
//! command run in the project's directory, with the question on stdin and the
//! answer on stdout, in a clean environment, bounded in time, output and load,
//! and killed with its whole process group on timeout and on `CancelTask`.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{TestNode, grant, message_params, spawn_node, spawn_node_with};
use pals::config::{ExecCfg, ProjectCfg, WorkerCfg};
use pals_core::grant::Operation::{TaskCancel, TaskRead, TaskSend};
use pals_core::grant::SignedGrant;
use serde_json::{Value, json};

/// How long a test waits for something that should happen at once: generous,
/// because the host may be heavily loaded.
const PATIENCE: Duration = Duration::from_secs(30);

/// A provider B running `exec`, a requester A, the grant that lets A ask,
/// read and cancel on B's project `demo`, and the project's directory.
struct Setup {
    b: TestNode,
    a: TestNode,
    g: SignedGrant,
    project: tempfile::TempDir,
}

/// [`Setup`] with `exec` as B's worker and `demo` configured.
async fn setup(exec: ExecCfg) -> Setup {
    let project = tempfile::tempdir().expect("create a project directory");
    let path = project.path().to_path_buf();
    let b = spawn_node_with(WorkerCfg::Exec(exec), |cfg| {
        cfg.projects.insert("demo".to_owned(), ProjectCfg { path });
    })
    .await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant(&b, &a, &[TaskSend, TaskRead, TaskCancel], 3600);
    Setup { b, a, g, project }
}

/// `sh -c <script>` with the default limits.
fn sh(script: &str) -> ExecCfg {
    ExecCfg::new("sh", &["-c", script])
}

impl Setup {
    /// Asks B `text` and waits for the task to end.
    async fn ask(&self, text: &str) -> Value {
        self.a.ask(&self.b, &self.g, text).await
    }

    /// Asks B `text`, returning the task as soon as it is working.
    async fn start(&self, text: &str) -> Value {
        let mut params = message_params(text, None);
        params["configuration"] = json!({"returnImmediately": true});
        let res = self
            .a
            .rpc(&self.b, Some(&self.g), "SendMessage", params)
            .await;
        assert_eq!(res.status, 200, "{}", res.json);
        let task = res.json["result"]["task"].clone();
        assert_eq!(state(&task), "TASK_STATE_WORKING", "{task}");
        task
    }

    /// `method` on the task `id`; its `result`.
    async fn on_task(&self, method: &str, id: &str) -> Value {
        let res = self
            .a
            .rpc(&self.b, Some(&self.g), method, json!({ "id": id }))
            .await;
        assert_eq!(res.status, 200, "{}", res.json);
        assert!(res.json.get("error").is_none(), "{}", res.json);
        res.json["result"].clone()
    }

    /// The directory B keeps the working files of task `id` in.
    fn task_dir(&self, id: &str) -> PathBuf {
        self.b.dir.path().join("tasks").join(id)
    }

    /// The pid the command wrote to `name` in the project directory, once it
    /// is there.
    async fn pid_in(&self, name: &str) -> i32 {
        let path = self.project.path().join(name);
        let mut pid = None;
        wait_until(|| {
            pid = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.trim().parse().ok());
            pid.is_some()
        })
        .await;
        pid.unwrap()
    }
}

fn state(task: &Value) -> &str {
    task["status"]["state"].as_str().unwrap_or_default()
}

fn id(task: &Value) -> &str {
    task["id"].as_str().expect("a task id")
}

/// The text of a task's status message.
fn status_text(task: &Value) -> &str {
    task["status"]["message"]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

/// The text of the task's `answer` artifact, its only one.
fn answer(task: &Value) -> &str {
    let artifacts = task["artifacts"].as_array().expect("artifacts");
    assert_eq!(artifacts.len(), 1, "{task}");
    assert_eq!(artifacts[0]["name"], "answer", "{task}");
    artifacts[0]["parts"][0]["text"]
        .as_str()
        .expect("a text part")
}

/// Waits up to [`PATIENCE`] for `done`.
async fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether process `pid` still runs. A zombie has stopped running.
fn alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the command name, in parentheses.
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z')),
        Err(_) if Path::new("/proc/self/stat").exists() => false,
        // No procfs: signal 0 only checks that the process exists.
        Err(_) => unsafe { libc::kill(pid, 0) == 0 },
    }
}

/// Waits for every process in `pids` to be gone.
async fn wait_gone(pids: &[i32]) {
    wait_until(|| !pids.iter().any(|&pid| alive(pid))).await;
}

/// A shell script that records its own pid in `leader.pid` and the pid of a
/// child of its own in `sleeper.pid`, and waits for that child, which sleeps
/// far longer than any test waits.
const LEADER_AND_SLEEPER: &str = "echo $$ > leader.pid; sleep 600 & echo $! > sleeper.pid; wait";

#[tokio::test]
async fn stdin_reaches_the_command_and_stdout_is_the_answer() {
    let s = setup(sh("cat")).await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task), "hola");
    // The task's working directories are gone with it.
    assert!(!s.task_dir(id(&task)).exists());

    // Every text part of the message, one per line.
    let mut params = message_params("", None);
    params["message"]["parts"] = json!([{"text": "hola"}, {"text": "adiós"}]);
    let res = s.a.rpc(&s.b, Some(&s.g), "SendMessage", params).await;
    let task = &res.json["result"]["task"];
    assert_eq!(state(task), "TASK_STATE_COMPLETED", "{}", res.json);
    assert_eq!(answer(task), "hola\nadiós");
}

#[tokio::test]
async fn runs_in_the_project_directory() {
    let s = setup(sh("pwd")).await;
    let task = s.ask("where").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    let project = s.project.path().canonicalize().unwrap();
    assert_eq!(answer(&task).trim_end(), project.to_str().unwrap());
}

#[tokio::test]
async fn unknown_project_is_rejected() {
    // B has no project `demo`, the project of A's grant.
    let b = spawn_node(WorkerCfg::Exec(sh("cat"))).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant(&b, &a, &[TaskSend, TaskRead], 3600);
    let task = a.ask(&b, &g, "hola").await;
    let state = state(&task);
    assert_eq!(state, "TASK_STATE_REJECTED", "{task}");
    assert_eq!(status_text(&task), "unknown project");
    assert!(task.get("artifacts").is_none(), "{task}");
}

#[tokio::test]
async fn nonzero_exit_fails_with_stderr_tail() {
    // 10 KB of `a` on stderr before the last line: only the tail is kept.
    let s = setup(sh(
        "head -c 10000 /dev/zero | tr '\\0' a >&2; echo boom >&2; exit 3",
    ))
    .await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    let text = status_text(&task);
    assert!(text.contains("boom"), "{text}");
    assert!(text.contains('3'), "the exit code: {text}");
    assert!(text.contains("aaaa"), "{text}");
    assert!(text.len() < 4096 + 100, "{} bytes", text.len());
    assert!(!s.task_dir(id(&task)).exists());
}

#[tokio::test]
async fn hung_command_times_out_and_is_killed() {
    let s = setup(ExecCfg {
        timeout_s: 1,
        ..sh(LEADER_AND_SLEEPER)
    })
    .await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "timeout after 1s");
    // The whole process group is gone, the child of the command as well.
    let pids = [s.pid_in("leader.pid").await, s.pid_in("sleeper.pid").await];
    wait_gone(&pids).await;
    assert!(!s.task_dir(id(&task)).exists());
}

#[tokio::test]
async fn cancel_kills_the_process_group() {
    let s = setup(sh(LEADER_AND_SLEEPER)).await;
    let task = s.start("hola").await;
    let id = id(&task);
    let pids = [s.pid_in("leader.pid").await, s.pid_in("sleeper.pid").await];
    assert!(pids.iter().all(|&pid| alive(pid)));

    let canceled = s.on_task("CancelTask", id).await;
    assert_eq!(state(&canceled), "TASK_STATE_CANCELED", "{canceled}");
    wait_gone(&pids).await;
    wait_until(|| !s.task_dir(id).exists()).await;
    // The run that was killed does not overwrite the cancellation.
    let task = s.on_task("GetTask", id).await;
    assert_eq!(state(&task), "TASK_STATE_CANCELED", "{task}");
    assert!(task.get("artifacts").is_none(), "{task}");
}

#[tokio::test]
async fn background_children_die_with_the_command() {
    // The child lets go of every pipe, so the command's output ends and the
    // command exits normally while the child would sleep on.
    let s = setup(sh(
        "sleep 600 >/dev/null 2>&1 & echo $! > sleeper.pid; echo hi",
    ))
    .await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task), "hi\n");
    wait_gone(&[s.pid_in("sleeper.pid").await]).await;
}

#[tokio::test]
async fn missing_command_fails_and_daemon_keeps_serving() {
    // The command is a path where nothing is yet.
    let tools = tempfile::tempdir().unwrap();
    let command = tools.path().join("sh");
    let s = setup(ExecCfg::new(command.to_str().unwrap(), &["-c", "cat"])).await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "worker command not found");

    // Once the command exists, the same daemon runs it.
    std::os::unix::fs::symlink("/bin/sh", &command).unwrap();
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task), "hola");
}

#[tokio::test]
async fn huge_output_is_truncated_with_marker() {
    const MIB: usize = 1024 * 1024;
    let s = setup(sh("head -c 2097152 /dev/zero | tr '\\0' x")).await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{}", state(&task));
    let text = answer(&task);
    let marker = "\n[truncated]";
    assert!(
        text.ends_with(marker),
        "ends with {:?}",
        &text[text.len() - 20..]
    );
    assert!(text.len() <= MIB + marker.len(), "{} bytes", text.len());
    let kept = &text[..text.len() - marker.len()];
    assert!(kept.len() > MIB / 2, "{} bytes", kept.len());
    assert!(kept.bytes().all(|b| b == b'x'));
}

#[tokio::test]
async fn non_utf8_output_does_not_fail() {
    let s = setup(sh("printf 'ok \\377\\376'")).await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task), "ok \u{FFFD}\u{FFFD}");
}

/// The variable this test puts in the daemon's environment.
const SECRET: &str = "PALS_TEST_SECRET";

#[tokio::test]
async fn daemon_environment_does_not_leak() {
    // The daemon runs in this test process; the secret has to be in its
    // environment before any thread starts, so this test runs again in a
    // process of its own that has it.
    if std::env::var_os(SECRET).is_none() {
        let this = std::env::current_exe().unwrap();
        let out = std::process::Command::new(this)
            .args([
                "--exact",
                "daemon_environment_does_not_leak",
                "--test-threads=1",
            ])
            .env(SECRET, "do-not-leak")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed"), "{stdout}\n{stderr}");
        return;
    }

    let s = setup(ExecCfg::new("env", &[])).await;
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    let env: Vec<(&str, &str)> = answer(&task)
        .lines()
        .map(|line| line.split_once('=').expect("NAME=value"))
        .collect();
    let value = |name: &str| env.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);

    assert_eq!(value(SECRET), None, "{env:?}");
    let allowed = [
        "PATH",
        "HOME",
        "LANG",
        "PALS_TASK_ID",
        "PALS_PEER",
        "PALS_PROJECT",
        "PALS_IN_DIR",
        "PALS_OUT_DIR",
    ];
    for (name, _) in &env {
        assert!(allowed.contains(name), "{name} leaked: {env:?}");
    }
    let id = id(&task);
    let dir = s.task_dir(id);
    assert_eq!(value("PATH"), std::env::var("PATH").ok().as_deref());
    assert_eq!(value("PALS_TASK_ID"), Some(id));
    assert_eq!(value("PALS_PEER"), Some(s.a.id().to_string().as_str()));
    assert_eq!(value("PALS_PROJECT"), Some("demo"));
    assert_eq!(value("PALS_IN_DIR"), dir.join("in").to_str());
    assert_eq!(value("PALS_OUT_DIR"), dir.join("out").to_str());
}

#[tokio::test]
async fn third_task_is_rejected_as_busy() {
    // `wait` keeps a run busy; anything else is answered at once.
    let s = setup(ExecCfg {
        max_concurrent: 1,
        max_queue: 1,
        ..sh("read -r x; if [ \"$x\" = wait ]; then sleep 600; fi; echo \"$x\"")
    })
    .await;
    let running = s.start("wait").await;
    let queued = s.start("wait").await;

    // Rejected at once, not queued behind them.
    let task = tokio::time::timeout(PATIENCE, s.ask("hola"))
        .await
        .expect("an answer without waiting for a place");
    assert_eq!(state(&task), "TASK_STATE_REJECTED", "{task}");
    assert_eq!(status_text(&task), "busy");

    // Canceling frees their places.
    for task in [&queued, &running] {
        let canceled = s.on_task("CancelTask", id(task)).await;
        assert_eq!(state(&canceled), "TASK_STATE_CANCELED", "{canceled}");
    }
    let task = s.ask("hola").await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task), "hola\n");
}

#[tokio::test]
async fn task_directories_left_by_a_crash_are_removed_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("tasks").join("t-crashed").join("out");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::write(left.join("partial.txt"), "half").unwrap();

    let node = common::spawn_node_in(dir, WorkerCfg::Exec(sh("cat")), |_| {}).await;
    assert!(!node.dir.path().join("tasks").join("t-crashed").exists());
}
