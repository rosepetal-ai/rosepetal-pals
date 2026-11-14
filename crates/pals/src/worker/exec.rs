//! The `exec` adapter: each task runs a configured command, such as
//! `claude -p`, in its project's directory, with the question on stdin and
//! the answer on stdout.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{Adapter, AdapterError, AdapterOutput, TaskInput};
use crate::config::{ExecCfg, ProjectCfg};

/// The variables a command takes from the daemon's environment. It gets no
/// other but the `PALS_*` ones that describe its task.
pub const PASSED_ENV: [&str; 3] = ["PATH", "HOME", "LANG"];

/// What follows an answer cut at `max_output_bytes`.
pub const TRUNCATED: &str = "\n[truncated]";

/// How much of the end of stderr a failure reports.
pub const STDERR_TAIL_BYTES: usize = 4096;

/// How long a killed command has to be reaped before it is left to the
/// runtime.
const REAP_WAIT: Duration = Duration::from_secs(10);

/// Runs [`ExecCfg::command`] for each task.
///
/// | Aspect | Rule |
/// |-|-|
/// | Working directory | `projects[<project>].path`; a project not configured is `Rejected("unknown project")` |
/// | Input | The text on stdin; `PALS_TASK_ID`, `PALS_PEER`, `PALS_PROJECT`, `PALS_IN_DIR`, `PALS_OUT_DIR` |
/// | Environment | Nothing else but [`PASSED_ENV`] |
/// | Output | stdout as UTF-8, invalid bytes replaced; beyond `max_output_bytes`, cut and followed by [`TRUNCATED`] |
/// | Files | The regular files the command left directly in `PALS_OUT_DIR`, by name; not symbolic links, nor what is in subdirectories |
/// | Exit code other than 0 | `Failed` with the last [`STDERR_TAIL_BYTES`] of stderr |
/// | Time | Beyond `timeout_s`, the process group is killed: `Failed("timeout after <n>s")` |
/// | Cancellation | The process group is killed |
/// | Normal exit | Whatever the command left running in its process group is killed |
/// | Command not found | `Failed("worker command not found")` |
/// | Load | `max_concurrent` running and `max_queue` waiting, kept by the [`WorkerExecutor`](super::WorkerExecutor); beyond them, `Rejected("busy")` |
///
/// The command leads a process group of its own, so that a kill reaches
/// whatever it started too. A run dropped before the command ends, as when
/// the daemon stops, kills the group as well.
///
/// Known limit: a descendant that leaves the group, with `setsid()` or
/// `setpgid()`, escapes every one of these kills. Containing those would
/// take a subreaper or a cgroup.
pub struct ExecAdapter {
    cfg: ExecCfg,
    projects: BTreeMap<String, PathBuf>,
}

impl ExecAdapter {
    pub fn new(cfg: ExecCfg, projects: &BTreeMap<String, ProjectCfg>) -> Self {
        let projects = projects
            .iter()
            .map(|(name, project)| (name.clone(), project.path.clone()))
            .collect();
        Self { cfg, projects }
    }

    /// The command for `input`, run in `dir`.
    fn command(&self, input: &TaskInput, dir: &Path) -> tokio::process::Command {
        let mut command = std::process::Command::new(&self.cfg.command);
        command.args(&self.cfg.args).current_dir(dir).env_clear();
        for name in PASSED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("PALS_TASK_ID", &input.task_id)
            .env("PALS_PEER", input.peer.to_string())
            .env("PALS_PROJECT", &input.project)
            .env("PALS_IN_DIR", &input.in_dir)
            .env("PALS_OUT_DIR", &input.out_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        command
    }
}

#[async_trait]
impl Adapter for ExecAdapter {
    async fn run(
        &self,
        input: TaskInput,
        cancel: CancellationToken,
    ) -> Result<AdapterOutput, AdapterError> {
        let Some(dir) = self.projects.get(&input.project) else {
            return Err(AdapterError::Rejected("unknown project".to_owned()));
        };
        let child = self
            .command(&input, dir)
            .spawn()
            .map_err(|err| spawn_failure(&err, dir))?;
        let mut running = Running::new(child);
        let stdin = running.child.stdin.take();
        let stdout = running.child.stdout.take();
        let stderr = running.child.stderr.take();

        let max_output_bytes = self.cfg.max_output_bytes;
        let out_dir = input.out_dir;
        let text = input.text;
        let exited = async {
            let write = async {
                if let Some(mut stdin) = stdin {
                    // A command that does not read its input closes the pipe
                    // early; that is its business. Dropping stdin closes it.
                    let _ = stdin.write_all(text.as_bytes()).await;
                }
            };
            // Both pipes to their end before the wait, so that the command
            // is still unreaped, its group id still its own, should the
            // timeout come first.
            let (_, stdout, stderr) = tokio::join!(
                write,
                read_head(stdout, max_output_bytes),
                read_tail(stderr, STDERR_TAIL_BYTES),
            );
            // Then its exit, without reaping it: while its zombie keeps the
            // group id its own, the group is killed, and with it anything the
            // command left running in the background with its pipes closed.
            // Only then is it reaped.
            match exited_unreaped(running.pgid).await {
                Ok(()) => running.kill_group(),
                Err(err) => warn!(error = %err, "cannot wait for the worker command to exit"),
            }
            let status = running.child.wait().await;
            (stdout, stderr, status)
        };
        let timeout = Duration::from_secs(self.cfg.timeout_s);
        let ended = tokio::select! {
            ended = exited => Ended::Exited(ended),
            () = tokio::time::sleep(timeout) => Ended::TimedOut,
            () = cancel.cancelled() => Ended::Canceled,
        };

        match ended {
            Ended::Exited((stdout, stderr, status)) => {
                let status = match status {
                    Ok(status) => {
                        running.reaped();
                        status
                    }
                    Err(err) => {
                        warn!(error = %err, "cannot wait for the worker command");
                        running.kill().await;
                        return Err(AdapterError::Failed(
                            "cannot wait for the worker command".to_owned(),
                        ));
                    }
                };
                if !status.success() {
                    return Err(AdapterError::Failed(failure(status, &stderr)));
                }
                let (head, cut) = stdout;
                let files = output_files(&out_dir).await.map_err(|err| {
                    warn!(dir = %out_dir.display(), error = %err, "cannot list the output files");
                    AdapterError::Failed("cannot read the output directory".to_owned())
                })?;
                Ok(AdapterOutput {
                    text: answer_text(&head, cut, max_output_bytes),
                    files,
                })
            }
            Ended::TimedOut => {
                running.kill().await;
                Err(AdapterError::Failed(format!(
                    "timeout after {}s",
                    self.cfg.timeout_s
                )))
            }
            Ended::Canceled => {
                running.kill().await;
                Err(AdapterError::Failed("canceled".to_owned()))
            }
        }
    }
}

/// How a command's run ended.
enum Ended {
    /// It closed its output and exited: its head of stdout and whether it
    /// was cut, the tail of its stderr, and its status.
    Exited(((Vec<u8>, bool), Vec<u8>, io::Result<ExitStatus>)),
    TimedOut,
    Canceled,
}

/// A spawned command, leader of its own process group. Dropped before it is
/// reaped, it kills the whole group.
struct Running {
    child: Child,
    pgid: libc::pid_t,
    /// Whether the leader is unreaped: then its pid cannot have been reused,
    /// and the group id still names this group only.
    unreaped: bool,
}

impl Running {
    fn new(child: Child) -> Self {
        let pid = child.id().expect("a child just spawned has a pid");
        Self {
            child,
            pgid: libc::pid_t::try_from(pid).expect("a pid fits in pid_t"),
            unreaped: true,
        }
    }

    /// Records that the leader has been reaped: the group is no longer
    /// this one to kill.
    fn reaped(&mut self) {
        self.unreaped = false;
    }

    /// Sends SIGKILL to every process in the group, while the leader is
    /// unreaped.
    fn kill_group(&self) {
        if self.unreaped {
            // SAFETY: killpg(2) only sends a signal. The group's leader is an
            // unreaped child of ours, so its id, the group's id, is not reused.
            unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
        }
    }

    /// Kills the group and reaps its leader.
    async fn kill(&mut self) {
        self.kill_group();
        match tokio::time::timeout(REAP_WAIT, self.child.wait()).await {
            Ok(Ok(_)) => self.reaped(),
            Ok(Err(err)) => warn!(error = %err, "cannot reap a killed worker command"),
            Err(_) => warn!("a killed worker command is not gone yet"),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Before `child` drops, whose own kill reaches the leader only.
        self.kill_group();
    }
}

/// Waits, on a blocking thread, until the child `pid` has exited, and leaves
/// it unreaped (`WNOWAIT`): until it is reaped, its pid, and so the id of
/// the group it leads, cannot be reused. POSIX `waitid`, as on Linux and
/// macOS.
async fn exited_unreaped(pid: libc::pid_t) -> io::Result<()> {
    let id = libc::id_t::try_from(pid).map_err(io::Error::other)?;
    tokio::task::spawn_blocking(move || {
        loop {
            // SAFETY: an all-zero `siginfo_t` is a valid value of this plain
            // C struct, which `waitid` only writes to.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is valid for writes for the whole call.
            let done =
                unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) };
            if done == 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// The regular files directly in `dir`, sorted by name. Symbolic links are
/// left out: a file to send is one the command wrote there.
async fn output_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        // `file_type` does not follow symbolic links.
        if entry.file_type().await?.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// The reason a command could not be started.
fn spawn_failure(err: &io::Error, dir: &Path) -> AdapterError {
    // A missing working directory fails the spawn with the same error as a
    // missing program.
    let reason = if err.kind() != io::ErrorKind::NotFound {
        warn!(error = %err, "cannot start the worker command");
        format!("cannot start the worker command: {err}")
    } else if !dir.is_dir() {
        warn!(dir = %dir.display(), "the project directory does not exist");
        "project directory not found".to_owned()
    } else {
        "worker command not found".to_owned()
    };
    AdapterError::Failed(reason)
}

/// The reason for a command that did not succeed: how it ended and the end
/// of its stderr.
fn failure(status: ExitStatus, stderr: &[u8]) -> String {
    let mut reason = match (status.code(), status.signal()) {
        (Some(code), _) => format!("worker command exited with code {code}"),
        (None, Some(signal)) => format!("worker command killed by signal {signal}"),
        (None, None) => format!("worker command failed: {status}"),
    };
    let tail = String::from_utf8_lossy(stderr);
    let tail = tail.trim_end();
    if !tail.is_empty() {
        reason.push('\n');
        reason.push_str(tail);
    }
    reason
}

/// The answer from the first bytes of stdout, `head`, and whether there was
/// more: UTF-8 with invalid bytes replaced, at most `max` bytes, followed by
/// [`TRUNCATED`] when anything was left out.
fn answer_text(head: &[u8], mut cut: bool, max: usize) -> String {
    let mut text = String::from_utf8_lossy(head).into_owned();
    // Replacement characters take more room than the bytes they replace.
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        cut = true;
    }
    if cut {
        text.push_str(TRUNCATED);
    }
    text
}

/// Reads `from` to its end, keeping the first `max` bytes, and whether
/// there were more.
async fn read_head(from: Option<impl AsyncRead + Unpin>, max: usize) -> (Vec<u8>, bool) {
    let mut head = Vec::new();
    let mut cut = false;
    if let Some(mut from) = from {
        let mut chunk = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = from.read(&mut chunk).await {
            let room = max - head.len();
            cut |= n > room;
            head.extend_from_slice(&chunk[..n.min(room)]);
        }
    }
    (head, cut)
}

/// Reads `from` to its end, keeping the last `keep` bytes.
async fn read_tail(from: Option<impl AsyncRead + Unpin>, keep: usize) -> Vec<u8> {
    let mut tail = Vec::new();
    if let Some(mut from) = from {
        let mut chunk = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = from.read(&mut chunk).await {
            tail.extend_from_slice(&chunk[..n]);
            if tail.len() > 2 * keep {
                tail.drain(..tail.len() - keep);
            }
        }
    }
    if tail.len() > keep {
        tail.drain(..tail.len() - keep);
    }
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_within_the_limit_is_kept() {
        assert_eq!(answer_text(b"hola", false, 4), "hola");
        assert_eq!(answer_text(b"", false, 4), "");
    }

    #[test]
    fn answer_beyond_the_limit_is_cut_with_the_marker() {
        assert_eq!(answer_text(b"hola", true, 4), format!("hola{TRUNCATED}"));
    }

    #[test]
    fn a_character_cut_in_half_is_left_out() {
        // "añ" is 61 c3 b1: four bytes keep "a" and half of "ñ".
        let text = answer_text("añb".as_bytes(), true, 2);
        assert_eq!(text, format!("a{TRUNCATED}"));
    }

    #[test]
    fn replaced_bytes_stay_within_the_limit() {
        // Each invalid byte becomes U+FFFD, three bytes.
        let text = answer_text(&[0xff, 0xfe, 0xfd], false, 3);
        assert_eq!(text, format!("\u{FFFD}{TRUNCATED}"));
        assert_eq!(answer_text(&[0xff], false, 3), "\u{FFFD}");
    }

    #[tokio::test]
    async fn head_and_tail_keep_their_ends() {
        let bytes: Vec<u8> = (0..=255).cycle().take(200_000).collect();
        let (head, cut) = read_head(Some(bytes.as_slice()), 1000).await;
        assert!(cut);
        assert_eq!(head, bytes[..1000]);
        let (head, cut) = read_head(Some(bytes.as_slice()), bytes.len()).await;
        assert!(!cut);
        assert_eq!(head, bytes);

        let tail = read_tail(Some(bytes.as_slice()), 4096).await;
        assert_eq!(tail, bytes[bytes.len() - 4096..]);
        let tail = read_tail(Some(&bytes[..10]), 4096).await;
        assert_eq!(tail, bytes[..10]);
    }

    #[test]
    fn failure_names_the_exit_and_the_stderr_tail() {
        let reason = failure(ExitStatus::from_raw(3 << 8), b"boom\n");
        assert_eq!(reason, "worker command exited with code 3\nboom");
        let reason = failure(ExitStatus::from_raw(libc::SIGKILL), b"");
        assert_eq!(reason, "worker command killed by signal 9");
    }
}
