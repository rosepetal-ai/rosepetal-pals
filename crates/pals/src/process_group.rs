//! A child process that leads a process group of its own ([`GroupLeader`]):
//! how the `exec` worker adapter runs its commands and the inventory's MCP
//! adapter its `stdio` servers, so that a kill reaches whatever the child
//! started too.
//!
//! The child is spawned with `process_group(0)`, so its pid is the group's
//! id. That id names this group only while the leader is unreaped (until
//! then its pid cannot be reused), so the group is signalled only then
//! ([`GroupLeader::kill_group`]). A leader that exited can be waited for
//! without being reaped ([`GroupLeader::exited_unreaped`]), so that the
//! group, with whatever the child left running, is killed before it is.
//! Dropped before its leader is reaped, a [`GroupLeader`] kills the group.
//!
//! Known limits: a descendant that leaves the group, with `setsid()` or
//! `setpgid()`, escapes every one of these kills; and if the daemon itself
//! is killed with SIGKILL, nothing kills the group (no `PR_SET_PDEATHSIG` is
//! set). Containing those would take a subreaper or a cgroup.

use std::io;
use std::time::Duration;

use tokio::process::Child;
use tracing::warn;

/// How long a killed child has to be reaped before it is left to the
/// runtime.
const REAP_WAIT: Duration = Duration::from_secs(10);

/// A spawned child, leader of its own process group. Dropped before it is
/// reaped, it kills the whole group.
pub(crate) struct GroupLeader {
    child: Child,
    pgid: libc::pid_t,
    /// Whether the leader is unreaped: then its pid cannot have been reused,
    /// and the group id still names this group only.
    unreaped: bool,
    /// What the child is, for the log: `worker command`, `MCP server`.
    what: &'static str,
}

impl GroupLeader {
    /// `child`, spawned with `process_group(0)`, named `what` in the log.
    pub(crate) fn new(child: Child, what: &'static str) -> Self {
        let pid = child.id().expect("a child just spawned has a pid");
        Self {
            child,
            pgid: libc::pid_t::try_from(pid).expect("a pid fits in pid_t"),
            unreaped: true,
            what,
        }
    }

    /// The child, for its pipes and its wait. A caller that reaps it with
    /// [`Child::wait`] says so with [`GroupLeader::reaped`].
    pub(crate) fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Records that the leader has been reaped: the group is no longer
    /// this one to kill.
    pub(crate) fn reaped(&mut self) {
        self.unreaped = false;
    }

    /// Sends SIGKILL to every process in the group, while the leader is
    /// unreaped.
    pub(crate) fn kill_group(&self) {
        if self.unreaped {
            // SAFETY: killpg(2) only sends a signal. The group's leader is an
            // unreaped child of ours, so its id, the group's id, is not reused.
            unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
        }
    }

    /// Kills the group and reaps its leader, waiting at most [`REAP_WAIT`].
    pub(crate) async fn kill(&mut self) {
        self.kill_group();
        match tokio::time::timeout(REAP_WAIT, self.child.wait()).await {
            Ok(Ok(_)) => self.reaped(),
            Ok(Err(err)) => warn!(error = %err, "cannot reap a killed {}", self.what),
            Err(_) => warn!("a killed {} is not gone yet", self.what),
        }
    }

    /// Waits, on a blocking thread, until the leader has exited, and leaves
    /// it unreaped (`WNOWAIT`): until it is reaped, its pid, and so the id
    /// of the group it leads, cannot be reused. POSIX `waitid`, as on Linux
    /// and macOS.
    pub(crate) async fn exited_unreaped(&self) -> io::Result<()> {
        let id = libc::id_t::try_from(self.pgid).map_err(io::Error::other)?;
        tokio::task::spawn_blocking(move || {
            loop {
                // SAFETY: an all-zero `siginfo_t` is a valid value of this
                // plain C struct, which `waitid` only writes to.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                // SAFETY: `info` is valid for writes for the whole call.
                let done = unsafe {
                    libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT)
                };
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
}

impl Drop for GroupLeader {
    fn drop(&mut self) {
        // Before `child` drops, whose own kill reaches the leader only.
        self.kill_group();
    }
}
