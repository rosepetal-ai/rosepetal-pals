//! The daemon's configuration file and the single-instance lock.
//!
//! The PALS home directory is `--home DIR`, else `$PALS_HOME`, else `~/.pals`.
//! It holds [`CONFIG_FILE`]; the data directory, the home directory unless the
//! configuration says otherwise, holds [`KEY_FILE`] and [`LOCK_FILE`].

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use pals_core::identity::AgentId;
use serde::Deserialize;

/// The configuration file, in the PALS home directory.
pub const CONFIG_FILE: &str = "config.toml";
/// The agent's secret key, in the data directory.
pub const KEY_FILE: &str = "agent.key";
/// The single-instance lock, in the data directory.
pub const LOCK_FILE: &str = "daemon.lock";
/// The environment variable that names the PALS home directory.
pub const HOME_ENV: &str = "PALS_HOME";

/// What `pals init` writes: every setting at its default, with comments.
pub const INIT_TEMPLATE: &str = r#"# PALS agent configuration, written by `pals init`.

# Directory for agent.key, daemon.lock and the local store. Defaults to the
# directory that holds this file; a relative path is resolved against it.
# data_dir = "/path/to/data"

# Agents whose grants this agent accepts, besides its own.
trusted_issuers = []

[agent]
name = "pals"
description = "A PALS agent"

[gateway]
# The local A2A gateway. Only loopback addresses are accepted.
listen = "127.0.0.1:7420"

[worker]
# Who answers the questions peers ask. "echo" replies with the text it
# receives. "exec" runs `command` with `args` in the project's directory for
# each question, with the question on stdin, and answers with its stdout.
# "tck" plays the scenarios of the A2A TCK, for conformance runs only.
adapter = "echo"
# To answer with Claude Code instead:
# adapter = "exec"
# command = "claude"
# args = ["-p", "--output-format", "text"]
# timeout_s = 600                # the command's process group is killed after it
# max_concurrent = 2             # commands running at the same time
# max_queue = 8                  # questions waiting; more are rejected as busy
# max_output_bytes = 1048576     # 1 MiB; a longer answer is cut

[net]
# true: Iroh's public relays (the N0 preset); false: direct addresses only.
relay = true

[limits]
max_body_bytes = 1048576      # 1 MiB
max_blob_bytes = 2147483648   # 2 GiB
blob_fetch_timeout_s = 120

# Projects this agent answers about, by name.
# [projects.demo]
# path = "/path/to/repo"
"#;

/// The contents of `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Holds [`KEY_FILE`], [`LOCK_FILE`] and the store. After [`Config::load`]
    /// it is absolute: the configuration file's directory when the file does
    /// not set it, and resolved against that directory when it is relative.
    #[serde(default)]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub agent: AgentCfg,
    #[serde(default)]
    pub gateway: GatewayCfg,
    #[serde(default)]
    pub worker: WorkerCfg,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectCfg>,
    #[serde(default)]
    pub limits: LimitsCfg,
    #[serde(default)]
    pub net: NetCfg,
    /// Issuers whose grants this agent accepts, besides itself.
    #[serde(default)]
    pub trusted_issuers: Vec<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentCfg {
    pub name: String,
    pub description: String,
}

impl Default for AgentCfg {
    fn default() -> Self {
        Self {
            name: "pals".to_owned(),
            description: "A PALS agent".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayCfg {
    /// Must be a loopback address.
    pub listen: SocketAddr,
}

impl Default for GatewayCfg {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 7420)),
        }
    }
}

/// The executor behind the A2A server, selected by `adapter`. A key the
/// selected adapter does not take is an error.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(tag = "adapter", rename_all = "lowercase")]
pub enum WorkerCfg {
    /// Replies with the text it receives.
    #[default]
    #[serde(deserialize_with = "no_settings")]
    Echo,
    /// Runs a command for each task, see [`ExecCfg`].
    Exec(ExecCfg),
    /// Plays the scenarios of the A2A TCK, by the prefix of each message's
    /// `messageId`, see [`TckExecutor`](crate::worker::tck::TckExecutor).
    /// Only for conformance runs.
    #[serde(deserialize_with = "no_settings")]
    Tck,
}

/// The `exec` adapter: each task runs `command` with `args` in its project's
/// directory, with the question on stdin; its stdout is the answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCfg {
    /// The program: a path, or a name looked up on `PATH`.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Seconds a run may take before its process group is killed.
    #[serde(default = "ExecCfg::default_timeout_s")]
    pub timeout_s: u64,
    /// Runs at the same time.
    #[serde(default = "ExecCfg::default_max_concurrent")]
    pub max_concurrent: usize,
    /// Tasks waiting for a run; a task beyond them is rejected as busy.
    #[serde(default = "ExecCfg::default_max_queue")]
    pub max_queue: usize,
    /// Longest answer, in bytes; a longer stdout is cut.
    #[serde(default = "ExecCfg::default_max_output_bytes")]
    pub max_output_bytes: usize,
}

/// Reads an adapter that takes no settings, refusing any key besides
/// `adapter`: serde ignores the keys under a unit variant of an internally
/// tagged enum.
fn no_settings<'de, D: serde::Deserializer<'de>>(settings: D) -> Result<(), D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NoSettings {}
    NoSettings::deserialize(settings).map(|NoSettings {}| ())
}

impl ExecCfg {
    /// Running `command` with `args`, with the default limits: 600 s, 2 runs
    /// at a time, 8 waiting, 1 MiB of answer.
    pub fn new(command: &str, args: &[&str]) -> Self {
        Self {
            command: command.to_owned(),
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            timeout_s: Self::default_timeout_s(),
            max_concurrent: Self::default_max_concurrent(),
            max_queue: Self::default_max_queue(),
            max_output_bytes: Self::default_max_output_bytes(),
        }
    }

    fn default_timeout_s() -> u64 {
        600
    }

    fn default_max_concurrent() -> usize {
        2
    }

    fn default_max_queue() -> usize {
        8
    }

    fn default_max_output_bytes() -> usize {
        1024 * 1024
    }

    /// Why these settings cannot run a task, if they cannot.
    fn problem(&self) -> Option<&'static str> {
        if self.command.is_empty() {
            Some("worker.command must not be empty")
        } else if self.timeout_s == 0 {
            Some("worker.timeout_s must be at least 1")
        } else if self.max_concurrent == 0 {
            Some("worker.max_concurrent must be at least 1")
        } else if self.max_output_bytes == 0 {
            Some("worker.max_output_bytes must be at least 1")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCfg {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetCfg {
    /// `true`: Iroh's N0 preset with its public relays; `false`: no relay,
    /// direct addresses only (tests).
    pub relay: bool,
}

impl Default for NetCfg {
    fn default() -> Self {
        Self { relay: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsCfg {
    /// Largest A2A request body accepted from a peer.
    pub max_body_bytes: usize,
    /// Largest blob this agent fetches.
    pub max_blob_bytes: u64,
    /// Time allowed to fetch one blob.
    pub blob_fetch_timeout_s: u64,
}

impl Default for LimitsCfg {
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
            max_blob_bytes: 2 * 1024 * 1024 * 1024,
            blob_fetch_timeout_s: 120,
        }
    }
}

impl Config {
    /// Reads and validates the configuration file at `path`.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let path = std::path::absolute(path)
            .with_context(|| format!("cannot resolve {}", path.display()))?;
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut cfg: Self =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let dir = path.parent().expect("an absolute file path has a parent");
        cfg.data_dir = if cfg.data_dir.as_os_str().is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(&cfg.data_dir)
        };
        if !cfg.gateway.listen.ip().is_loopback() {
            bail!(
                "invalid {}: gateway.listen must be a loopback address, not {}",
                path.display(),
                cfg.gateway.listen
            );
        }
        if let Some(name) = cfg.projects.keys().find(|name| !is_header_safe(name)) {
            bail!(
                "invalid {}: the project name {name:?} must be printable ASCII without spaces, \
                 because it travels in an HTTP header",
                path.display()
            );
        }
        if let WorkerCfg::Exec(exec) = &cfg.worker
            && let Some(problem) = exec.problem()
        {
            bail!("invalid {}: {problem}", path.display());
        }
        Ok(cfg)
    }

    /// `config.toml` in the default home directory, see [`default_home`].
    pub fn default_path() -> PathBuf {
        default_home().join(CONFIG_FILE)
    }

    /// Where the agent's key lives: [`KEY_FILE`] in the data directory.
    pub fn key_path(&self) -> PathBuf {
        self.data_dir.join(KEY_FILE)
    }
}

/// Whether `text` can travel unchanged as an HTTP header value, as project
/// names and grant ids do between peers: not empty, printable ASCII and no
/// spaces.
pub fn is_header_safe(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic())
}

/// `$PALS_HOME` when it is set and not empty, else `~/.pals` (`.pals` in the
/// current directory when there is no home directory).
pub fn default_home() -> PathBuf {
    match std::env::var_os(HOME_ENV) {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => std::env::home_dir().unwrap_or_default().join(".pals"),
    }
}

/// An exclusive operating system lock on [`LOCK_FILE`] in a data directory,
/// held until this value is dropped.
///
/// Only one daemon runs per data directory. The lock dies with its process,
/// so a crash leaves nothing to clean up.
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
}

/// [`DataDirLock::acquire`] could not take the lock.
#[derive(Debug, thiserror::Error)]
pub enum AlreadyRunning {
    /// Another process holds the lock: a daemon already runs on this data
    /// directory.
    #[error("a pals daemon is already running on this data directory: {} is locked", lock_path.display())]
    Held { lock_path: PathBuf },
    /// The lock file could not be opened or locked, so a second daemon cannot
    /// be ruled out.
    #[error("cannot lock {}", lock_path.display())]
    Io {
        lock_path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl DataDirLock {
    /// Takes the lock on `dir`, which must exist, without waiting.
    pub fn acquire(dir: &Path) -> Result<Self, AlreadyRunning> {
        let lock_path = dir.join(LOCK_FILE);
        let file = match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(source) => return Err(AlreadyRunning::Io { lock_path, source }),
        };
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(AlreadyRunning::Held { lock_path }),
            Err(TryLockError::Error(source)) => Err(AlreadyRunning::Io { lock_path, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_config(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join(CONFIG_FILE);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn second_lock_on_same_dir_fails() {
        let dir = tempfile::tempdir().unwrap();
        let first = DataDirLock::acquire(dir.path()).unwrap();
        let second = DataDirLock::acquire(dir.path());
        assert!(
            matches!(second, Err(AlreadyRunning::Held { .. })),
            "{second:?}"
        );
        assert!(dir.path().join(LOCK_FILE).exists());
        drop(first);
        DataDirLock::acquire(dir.path()).expect("the lock is free once dropped");
    }

    #[test]
    fn config_rejects_non_loopback_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "[gateway]\nlisten = \"0.0.0.0:7420\"\n");
        let err = Config::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains("loopback"), "{err:#}");
    }

    #[test]
    fn empty_config_takes_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.data_dir, dir.path());
        assert_eq!(cfg.gateway.listen, "127.0.0.1:7420".parse().unwrap());
        assert_eq!(cfg.worker, WorkerCfg::Echo);
        assert!(cfg.net.relay);
        assert_eq!(cfg.limits.max_body_bytes, 1024 * 1024);
        assert_eq!(cfg.limits.max_blob_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(cfg.limits.blob_fetch_timeout_s, 120);
        assert!(cfg.projects.is_empty());
        assert!(cfg.trusted_issuers.is_empty());
        assert!(!cfg.agent.name.is_empty());
    }

    #[test]
    fn init_template_loads_with_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), INIT_TEMPLATE);
        let from_template = Config::load(&path).unwrap();
        let empty = Config::load(&write_config(dir.path(), "")).unwrap();
        assert_eq!(from_template, empty);
    }

    #[test]
    fn full_config_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let issuer = pals_core::identity::AgentKey::generate().id();
        let text = format!(
            r#"
data_dir = "state"
trusted_issuers = ["{issuer}"]

[agent]
name = "alice"
description = "Answers about the demo repo"

[gateway]
listen = "[::1]:7500"

[worker]
adapter = "echo"

[net]
relay = false

[limits]
max_body_bytes = 2048
max_blob_bytes = 4096
blob_fetch_timeout_s = 5

[projects.demo]
path = "/srv/demo"
"#
        );
        let cfg = Config::load(&write_config(dir.path(), &text)).unwrap();
        assert_eq!(cfg.data_dir, dir.path().join("state"));
        assert_eq!(cfg.trusted_issuers, vec![issuer]);
        assert_eq!(cfg.agent.name, "alice");
        assert_eq!(cfg.gateway.listen, "[::1]:7500".parse().unwrap());
        assert!(!cfg.net.relay);
        assert_eq!(cfg.limits.max_body_bytes, 2048);
        assert_eq!(cfg.projects["demo"].path, PathBuf::from("/srv/demo"));
    }

    #[test]
    fn project_names_must_be_header_safe() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["\"diseño\"", "\"two words\"", "\"\""] {
            let text = format!("[projects.{name}]\npath = \"/srv/p\"\n");
            let err = Config::load(&write_config(dir.path(), &text)).unwrap_err();
            assert!(format!("{err:#}").contains("project"), "{name}: {err:#}");
        }
        let text = "[projects.my-repo_2]\npath = \"/srv/p\"\n";
        assert!(Config::load(&write_config(dir.path(), text)).is_ok());

        assert!(is_header_safe("a.b/c:d"));
        for bad in ["", "diseño", "two words", "tab\there", "\u{7f}"] {
            assert!(!is_header_safe(bad), "{bad:?}");
        }
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "[gatway]\nlisten = \"127.0.0.1:7420\"\n");
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn exec_worker_takes_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let text = "[worker]\nadapter = \"exec\"\ncommand = \"claude\"\n";
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert_eq!(
            cfg.worker,
            WorkerCfg::Exec(ExecCfg {
                command: "claude".to_owned(),
                args: Vec::new(),
                timeout_s: 600,
                max_concurrent: 2,
                max_queue: 8,
                max_output_bytes: 1024 * 1024,
            })
        );
        assert_eq!(WorkerCfg::Exec(ExecCfg::new("claude", &[])), cfg.worker);
    }

    #[test]
    fn exec_worker_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"
[worker]
adapter = "exec"
command = "sh"
args = ["-c", "cat"]
timeout_s = 5
max_concurrent = 1
max_queue = 0
max_output_bytes = 10
"#;
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert_eq!(
            cfg.worker,
            WorkerCfg::Exec(ExecCfg {
                command: "sh".to_owned(),
                args: vec!["-c".to_owned(), "cat".to_owned()],
                timeout_s: 5,
                max_concurrent: 1,
                max_queue: 0,
                max_output_bytes: 10,
            })
        );
    }

    #[test]
    fn unknown_worker_keys_are_rejected_for_every_adapter() {
        let dir = tempfile::tempdir().unwrap();
        for (text, word) in [
            // A key only `exec` knows, under `echo`.
            (
                "[worker]\nadapter = \"echo\"\ncommand = \"claude\"\n",
                "unknown field `command`",
            ),
            (
                "[worker]\nadapter = \"echo\"\ntypo = 1\n",
                "unknown field `typo`",
            ),
            (
                "[worker]\nadapter = \"tck\"\ncommand = \"claude\"\n",
                "unknown field `command`",
            ),
            (
                "[worker]\nadapter = \"exec\"\ncommand = \"claude\"\ntimeout = 5\n",
                "unknown field `timeout`",
            ),
            ("[worker]\nadapter = \"exec\"\n", "missing field `command`"),
            ("[worker]\nadapter = \"nope\"\n", "unknown variant `nope`"),
        ] {
            let err = Config::load(&write_config(dir.path(), text)).unwrap_err();
            assert!(format!("{err:#}").contains(word), "{text}: {err:#}");
        }
    }

    #[test]
    fn tck_worker_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let text = "[worker]\nadapter = \"tck\"\n";
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert_eq!(cfg.worker, WorkerCfg::Tck);
    }

    #[test]
    fn exec_worker_limits_must_be_usable() {
        let dir = tempfile::tempdir().unwrap();
        for (setting, word) in [
            ("command = \"\"", "command"),
            ("command = \"sh\"\nmax_concurrent = 0", "max_concurrent"),
            ("command = \"sh\"\ntimeout_s = 0", "timeout_s"),
            ("command = \"sh\"\nmax_output_bytes = 0", "max_output_bytes"),
        ] {
            let text = format!("[worker]\nadapter = \"exec\"\n{setting}\n");
            let err = Config::load(&write_config(dir.path(), &text)).unwrap_err();
            assert!(format!("{err:#}").contains(word), "{setting}: {err:#}");
        }
    }

    #[test]
    fn init_template_documents_the_exec_adapter() {
        // Uncommenting the example turns the worker into `claude -p`.
        let mut text = String::new();
        let mut in_worker = false;
        for line in INIT_TEMPLATE.lines() {
            if line.starts_with('[') {
                in_worker = line == "[worker]";
            }
            let line = match line.strip_prefix("# ") {
                Some(setting) if in_worker && setting.contains(" = ") => setting,
                _ if in_worker && line == "adapter = \"echo\"" => continue,
                _ => line,
            };
            text.push_str(line);
            text.push('\n');
        }
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&write_config(dir.path(), &text)).unwrap();
        assert_eq!(
            cfg.worker,
            WorkerCfg::Exec(ExecCfg::new("claude", &["-p", "--output-format", "text"]))
        );
    }
}
