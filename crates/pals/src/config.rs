//! The daemon's configuration file and the single-instance lock.
//!
//! The PALS home directory is `--home DIR`, else `$PALS_HOME`, else `~/.pals`.
//! It holds [`CONFIG_FILE`] and, unless the configuration says otherwise,
//! [`INVENTORY_FILE`]; the data directory, the home directory unless the
//! configuration says otherwise, holds [`KEY_FILE`] and [`LOCK_FILE`].

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use iroh_tickets::endpoint::EndpointTicket;
use pals_core::discovery::MAX_OFFER_VALID_FOR_MS;
use pals_core::identity::AgentId;
use pals_core::inventory::capability_tag;
use serde::{Deserialize, Serialize};

use crate::net::agent_id_of;

use crate::path_policy::lexically_normal;

/// The configuration file, in the PALS home directory.
pub const CONFIG_FILE: &str = "config.toml";
/// The inventory file, in the directory of [`CONFIG_FILE`] unless
/// `[inventory] file` says otherwise.
pub const INVENTORY_FILE: &str = "inventory.toml";
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
# each question, with the question on stdin, and sends its stdout to the
# peer that asked as the answer; when the command fails, that peer gets the
# last 4 KiB of its stderr as the reason. Keep secrets out of both.
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
# true: Iroh's public relays (the N0 preset), so that peers on other
# machines and networks can reach this agent. false: no relay, and unless
# `listen` says otherwise the agent listens on 127.0.0.1 only: no other
# machine can reach it (local runs and tests).
relay = true
# The one UDP address the agent's Iroh endpoint binds to, with the relay or
# without it: a fixed port, which a DHT router needs so that other agents
# can dial it. Without it: free ports on every interface with the relay, a
# free port of 127.0.0.1 without it.
# listen = "0.0.0.0:4433"

[mcp]
# The only directory pals_fetch writes the files of peers' answers to,
# created on first use with mode 0700; it may not be inside the data
# directory. Defaults to ~/Downloads/pals; a relative path is resolved
# against the directory that holds this file.
# download_dir = "/path/to/downloads"

[inventory]
# What this agent offers and with what: its services, which become skills
# of its Agent Card, and the skills, plugins, MCP connections, runtime,
# models, information sources, hardware and network behind them. `pals init`
# writes it with a commented example of each block. A relative path is
# resolved against the directory that holds this file; when the file does
# not exist the inventory is empty. It is read when the daemon starts, and
# only then: restart the daemon after changing it.
file = "inventory.toml"
# Seconds between two checks of the MCP connections the inventory configures.
refresh_interval_s = 900

[discovery]
# Peers ask this agent what it offers over agentnet-discovery/0, its own
# Iroh ALPN: they get the view of the inventory their grant gives, signed.
# true: a peer without a grant sees the public view; false: an empty one.
public = true
# How long the offers this agent makes in answer to service queries last,
# in milliseconds, rounded up to whole seconds: at most 300000. An offer
# reserves nothing and authorizes nothing.
offer_valid_for_ms = 15000
# What a search of this agent's (`pals discover`) may take: at most this
# many candidates per search (up to 32), this many queries in flight for
# the whole daemon (up to 8), this long for each candidate, its card, its
# offer and its benchmark reports (up to 10000 ms; a query waits for what
# is left of it, and tells the peer at most 3000 ms) and this long in all
# (up to 60000 ms). Past them, a search answers with what it has and says
# it is partial.
max_candidates = 8
max_concurrent_queries = 3
per_candidate_timeout_ms = 3000
total_budget_ms = 15000
# Other names a search may give a capability: with this one, `pals
# discover --capability ocr` searches vision.ocr. One step: what an alias
# names is searched as it is, even when it is another alias. Each names a
# capability; at most 256.
# [discovery.aliases]
# ocr = "vision.ocr"

# The DHT (agentnet-kad/0): a private Kademlia network over this agent's
# own Iroh endpoint, where it finds the providers of a capability it has no
# contact with. Off by default, and with no bootstrap points: nothing is
# dialed or served for it until `enabled = true`. Every lookup in the DHT
# tells the routers it asks which agent looks for which capability: the
# connection is authenticated with this agent's own key. A search (`pals
# discover`) looks there too, behind the contacts, unless it names its
# peers or says --no-dht.
# [dht]
# enabled = true
# "client" (the default) only dials out, to the routers. "router" also
# stores and answers for the others: only on a stable host that other
# agents reach directly, at a fixed `[net] listen` port. With `relay =
# true`, a dial to a router may also look it up in n0's address lookup, as
# every dial of this daemon does, but a router must have direct addresses.
# mode = "client"
# Routers to enter the network through: the tickets `pals id --ticket`
# prints on each of them, with at least one direct IP address. If none of
# them answers, the agent dials up to 8 of its saved contacts, by name, and
# enters through those that turn out to be routers.
# bootstrap = []
# A router's addresses as others dial them, when they are not the ones it
# is bound to (behind a port forward, or `listen = "0.0.0.0:<port>"`); its
# ticket carries them too. Only for a router.
# external_addrs = ["203.0.113.10:4433"]
# false: nothing of this agent goes to the DHT. true: its public
# capabilities only (visibility = "public" in the inventory), never a
# private or a project one.
# announce = false
# How long the routers keep a record, in seconds (60 to 86400), and how
# often this agent publishes its own again (at least 20, and at most half
# of record_ttl_s).
# record_ttl_s = 3600
# republish_s = 1200

[limits]
max_body_bytes = 1048576      # 1 MiB
max_blob_bytes = 2147483648   # 2 GiB
blob_fetch_timeout_s = 120

# Projects this agent answers about, by name.
# [projects.demo]
# path = "/path/to/repo"
"#;

/// What `pals init` writes to [`INVENTORY_FILE`]: comments only, so an empty
/// inventory, with an example of each block to uncomment.
pub const INVENTORY_TEMPLATE: &str = r#"# What this agent offers and with what (agentnet.inventory/1), written by
# `pals init`. The daemon reads this file when it starts, and only then:
# restart it after changing anything here.
#
# Every block is optional, and so is every field of an entry but id,
# description and provenance. Leave out what you do not know: a field left
# out is unknown, never zero, and an empty string is refused.
#
# Entry ids are <namespace>:<name>, such as example:repo-reader; the
# agentnet namespace is reserved, and one id names one entry. Keys of your
# own go in an entry's extensions table, under a namespace too.
#
# Who sees an entry or a service is its visibility, private by default:
#   visibility = "private"                # nobody
#   visibility = "public"                 # every requester
#   visibility = { project = ["demo"] }   # grants for a task operation on skill:ask in demo
#   visibility = { task = ["demo"] }      # grants for task.send on skill:ask in demo
#
# Nothing here can say that something works: only the inventory adapters
# observe that, and a declaration is never a check.
#
# To use an example, uncomment it and edit it.

# Services: each one becomes a skill of the Agent Card when its visibility
# shows it, so it needs a name, a description and a capability or a tag. Its
# id is a skill id, such as ocr-metal-v1. A capability without a namespace,
# such as vision.ocr, is from the common vocabulary. Components are entries
# of this file, required, optional or alternative (with a group).
# [[services]]
# id = "ocr-metal-v1"
# name = "Industrial OCR"
# description = "Reads serial numbers stamped on metal parts"
# capabilities = ["vision.ocr"]
# tags = ["ocr", "metal"]
# examples = ["Read the serial number in this photo"]
# input_modes = ["image/png", "image/jpeg"]
# output_modes = ["application/json"]
# languages = ["en"]
# processing_region = "EU"
# external_cloud_used = false
# estimated_duration_ms = 4000
# limitations = ["Images up to 20 MiB"]
# components = [{ id = "example:ocr-model", role = "required" }, { id = "example:workstation", role = "optional" }]
# visibility = "public"

# Skill packages installed in the runtime.
# [[skills]]
# id = "example:rust-review"
# description = "Procedure for reviewing Rust code"
# provenance = "operator"
# version = "1.2.0"
# package_id = "example/rust-review"
# name = "Rust review"
# offered_for = ["code review"]
# visibility = { project = ["demo"] }

# Plugins.
# [[plugins]]
# id = "example:vision-pack"
# description = "Vision tools for industrial inspection"
# provenance = "operator"
# version = "0.4.1"
# runtime = "claude-code"
# components = ["ocr", "deskew"]

# MCP connections. Peers may see only the offered tools, never the rest.
# The adapter table says how this agent checks the connection and is never
# shown to a peer: transport "stdio", with command, args and the names (never
# the values) of the environment variables passed on, or "http", with url
# (https://, or http:// only to 127.0.0.1, [::1] or localhost). The daemon
# asks the server for its tools (tools/list, never a tool call) when it
# starts, every refresh_interval_s of config.toml and on `pals inventory
# refresh`; at most 16 connections have an adapter.
# [[mcp]]
# id = "example:repo-reader"
# description = "Reads the project repository"
# provenance = "operator"
# server = "@modelcontextprotocol/server-filesystem"
# tools = [{ name = "read_file", description = "Reads a file" }]
# offered_tools = ["read_file"]
# visibility = { project = ["demo"] }
# [mcp.adapter]
# transport = "stdio"
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/srv/repo"]
# env_passthrough = []

# The runtime that does the work.
# [runtime]
# id = "example:claude-code"
# description = "Claude Code in print mode"
# provenance = "operator"
# version = "2.0.0"
# kind = "claude-code"
# modes = ["print"]
# restrictions = ["no network access outside the project"]

# Models. The role is primary, auxiliary or offered; execution is local, or
# external with the provider that runs it.
# [[models]]
# id = "example:ocr-model"
# description = "OCR model for stamped metal"
# provenance = "operator"
# provider = "Example"
# model_id = "example-ocr-metal"
# revision = "2026-10-01"
# role = "offered"
# execution = { kind = "local" }
# quantization = "int8"
# modalities = ["image", "text"]
# visibility = "public"

# Information sources. The operations are among answer_with_sources, search,
# query_records, aggregate, process_in_place and deliver_subset; timestamps
# are strings in UTC.
# [[information]]
# id = "project:repo-controller"
# description = "The controller repository"
# provenance = "operator"
# domains = ["industrial control"]
# languages = ["en"]
# updated_at = "2026-11-14T00:00:00Z"
# live = true
# operations = ["answer_with_sources", "search"]
# visibility = { task = ["demo"] }

# Hardware: what is installed, and in offered what a task may use, which is
# what requirements are compared with. The figures are declared, never
# measured.
# [hardware]
# id = "example:workstation"
# description = "Inspection workstation"
# provenance = "operator"
# cpu = { arch = "x86_64", cores = 16 }
# ram_mib = 65536
# accelerators = [{ kind = "gpu", model = "NVIDIA RTX 4090", count = 1, memory_mib_each = 24576 }]
# storage = { kind = "nvme", capacity_gib = 2000 }
# offered = { cpu_cores = 4, ram_mib = 8192, vram_mib_each = 8192, concurrency = 2, accelerators = ["gpu"] }

# Network.
# [network]
# id = "example:uplink"
# description = "Office uplink"
# provenance = "operator"
# transports = ["iroh-quic"]
# region = "EU"
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
    #[serde(default)]
    pub mcp: McpCfg,
    #[serde(default)]
    pub inventory: InventoryCfg,
    #[serde(default)]
    pub discovery: DiscoveryCfg,
    #[serde(default)]
    pub dht: DhtCfg,
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
    /// and, unless [`NetCfg::listen`] says otherwise, the endpoint bound to
    /// 127.0.0.1 only, so that no other machine reaches the node (local runs
    /// and tests).
    pub relay: bool,
    /// The one UDP socket address the endpoint binds to, with the relay or
    /// without it: the fixed port a DHT router is dialed at. `None`: with
    /// the relay, the N0 preset's free ports on every interface; without
    /// it, a free port of 127.0.0.1.
    pub listen: Option<SocketAddr>,
}

impl Default for NetCfg {
    fn default() -> Self {
        Self {
            relay: true,
            listen: None,
        }
    }
}

/// The local MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpCfg {
    /// The only directory `pals_fetch` writes to, created on first use with
    /// mode `0700`; never inside the data directory. After [`Config::load`]
    /// a configured one is absolute, resolved against the configuration
    /// file's directory when it is relative; `None` means
    /// [`default_download_dir`], resolved only when `pals_fetch` needs it,
    /// so that no other command depends on a home directory.
    pub download_dir: Option<PathBuf>,
}

impl McpCfg {
    /// The download directory: the configured one, else the default.
    pub fn resolved_download_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.download_dir {
            Some(dir) => Ok(dir.clone()),
            None => default_download_dir(),
        }
    }
}

/// The inventory, see [`pals_core::inventory`] and
/// [`InventoryState`](crate::inventory::InventoryState).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InventoryCfg {
    /// The inventory file, read when the node starts and only then; when it
    /// does not exist the inventory is empty. After [`Config::load`] it is
    /// absolute, resolved against the configuration file's directory when it
    /// is relative.
    pub file: PathBuf,
    /// Seconds between two runs of the inventory adapters, which check the
    /// MCP connections the inventory configures. At least 1.
    pub refresh_interval_s: u64,
}

impl Default for InventoryCfg {
    fn default() -> Self {
        Self {
            file: PathBuf::from(INVENTORY_FILE),
            refresh_interval_s: 900,
        }
    }
}

/// What the discovery protocol, `agentnet-discovery/0`, shows, see
/// [`pals_core::discovery`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryCfg {
    /// Whether a peer without a grant sees the public view of the
    /// inventory; without it, it sees an empty one. A grant's view is shown
    /// either way.
    pub public: bool,
    /// How long a service offer lasts, from 1 to
    /// [`MAX_OFFER_VALID_FOR_MS`]; rounded up to whole seconds.
    pub offer_valid_for_ms: u32,
    /// The candidates a search contacts, from 1 to
    /// [`MAX_SEARCH_CANDIDATES`].
    pub max_candidates: usize,
    /// The service queries in flight at once for the whole daemon, from 1
    /// to [`MAX_CONCURRENT_QUERIES`].
    pub max_concurrent_queries: usize,
    /// How long a search gives each candidate, from 1 to
    /// [`MAX_PER_CANDIDATE_TIMEOUT_MS`].
    pub per_candidate_timeout_ms: u64,
    /// How long a search takes at most, from 1 to
    /// [`MAX_TOTAL_BUDGET_MS`].
    pub total_budget_ms: u64,
    /// `[discovery.aliases]`: another name a search may give a capability,
    /// and the capability it searches, one step. At most [`MAX_ALIASES`],
    /// each naming a capability (`capability_tag`).
    pub aliases: BTreeMap<String, String>,
}

/// The most candidates a search may be configured to contact.
pub const MAX_SEARCH_CANDIDATES: usize = 32;
/// The most service queries a daemon may be configured to have in flight.
pub const MAX_CONCURRENT_QUERIES: usize = 8;
/// The longest a search may be configured to give a candidate.
pub const MAX_PER_CANDIDATE_TIMEOUT_MS: u64 = 10_000;
/// The longest a search may be configured to take.
pub const MAX_TOTAL_BUDGET_MS: u64 = 60_000;
/// The most aliases of capabilities that may be configured.
pub const MAX_ALIASES: usize = 256;

impl Default for DiscoveryCfg {
    /// The public view, offers valid 15 s, as the example of design §7.2,
    /// and the limits of a search of design §7.3: 8 candidates, 3 queries
    /// at once, 3 s per candidate and 15 s in all.
    fn default() -> Self {
        Self {
            public: true,
            offer_valid_for_ms: 15_000,
            max_candidates: 8,
            max_concurrent_queries: 3,
            per_candidate_timeout_ms: 3000,
            total_budget_ms: 15_000,
            aliases: BTreeMap::new(),
        }
    }
}

/// What a node is in the DHT ([`crate::dht`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DhtMode {
    /// Dials the routers, and stores and answers for no one: in no routing
    /// table.
    #[default]
    Client,
    /// Stores and answers for the others, who dial it directly.
    Router,
}

/// The DHT, `agentnet-kad/0` ([`crate::dht`]): off unless `enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DhtCfg {
    /// Whether the node serves `agentnet-kad/0` and runs its swarm.
    pub enabled: bool,
    pub mode: DhtMode,
    /// Iroh endpoint tickets of routers (`pals id --ticket`), each of
    /// another endpoint and with a direct IP address: where the node enters
    /// the network.
    pub bootstrap: Vec<String>,
    /// A router's addresses as others dial it, which it declares besides
    /// any it is bound to; none of them unspecified or with port 0. Only
    /// for a router.
    pub external_addrs: Vec<SocketAddr>,
    /// Whether the node publishes its public capabilities in the DHT.
    pub announce: bool,
    /// How long the routers keep a record, from [`MIN_RECORD_TTL_S`] to
    /// [`MAX_RECORD_TTL_S`].
    pub record_ttl_s: u64,
    /// How often the node publishes its own records again: at least
    /// [`MIN_REPUBLISH_S`], at most half of `record_ttl_s`.
    pub republish_s: u64,
}

/// The shortest life of a record in the DHT that may be configured.
pub const MIN_RECORD_TTL_S: u64 = 60;
/// The longest life of a record in the DHT that may be configured: the
/// life of a signed record.
pub const MAX_RECORD_TTL_S: u64 = 86_400;
/// The shortest interval of republication that may be configured.
pub const MIN_REPUBLISH_S: u64 = 20;

impl Default for DhtCfg {
    /// Off, a client, with no bootstrap and announcing nothing; records
    /// last an hour, republished every 20 minutes.
    fn default() -> Self {
        Self {
            enabled: false,
            mode: DhtMode::Client,
            bootstrap: Vec::new(),
            external_addrs: Vec::new(),
            announce: false,
            record_ttl_s: 3600,
            republish_s: 1200,
        }
    }
}

impl DhtCfg {
    /// Each bootstrap ticket's agent and direct IP addresses, in order;
    /// fails, naming `dht.bootstrap`, on one that is not a ticket or has no
    /// direct IP address.
    pub fn bootstrap_peers(&self) -> anyhow::Result<Vec<(AgentId, Vec<SocketAddr>)>> {
        self.bootstrap
            .iter()
            .map(|text| {
                let ticket: EndpointTicket = text.trim().parse().map_err(|err| {
                    anyhow::anyhow!(
                        "dht.bootstrap: {text:?} is not a ticket ({err}): `pals id --ticket` \
                         prints one on the router"
                    )
                })?;
                let addr = ticket.endpoint_addr();
                let ips: Vec<SocketAddr> = addr.ip_addrs().copied().collect();
                if ips.is_empty() {
                    bail!(
                        "dht.bootstrap: the ticket of {} has no direct IP address, which a \
                         router needs",
                        agent_id_of(&addr.id)
                    );
                }
                Ok((agent_id_of(&addr.id), ips))
            })
            .collect()
    }

    /// Why this section cannot run, if it cannot: what [`Config::load`]
    /// checks.
    fn problem(&self) -> Option<String> {
        if let Err(err) = self.bootstrap_peers() {
            return Some(format!("{err:#}"));
        }
        if !(MIN_RECORD_TTL_S..=MAX_RECORD_TTL_S).contains(&self.record_ttl_s) {
            return Some(format!(
                "dht.record_ttl_s must be from {MIN_RECORD_TTL_S} to {MAX_RECORD_TTL_S}"
            ));
        }
        if self.republish_s < MIN_REPUBLISH_S || self.republish_s > self.record_ttl_s / 2 {
            return Some(format!(
                "dht.republish_s must be at least {MIN_REPUBLISH_S} and at most half of \
                 dht.record_ttl_s, so that a record is republished twice before it expires"
            ));
        }
        if !self.external_addrs.is_empty() && self.mode != DhtMode::Router {
            return Some(
                "dht.external_addrs is only for a router: a client declares no address".to_owned(),
            );
        }
        if let Some(addr) = self
            .external_addrs
            .iter()
            .find(|addr| addr.ip().is_unspecified() || addr.port() == 0)
        {
            return Some(format!(
                "dht.external_addrs: {addr} is no address to dial: neither an unspecified IP \
                 nor port 0"
            ));
        }
        None
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
        if let Some(downloads) = cfg.mcp.download_dir.take() {
            let downloads = dir.join(downloads);
            if lexically_normal(&downloads).starts_with(lexically_normal(&cfg.data_dir)) {
                bail!(
                    "invalid {}: mcp.download_dir {} is inside the data directory {}, which \
                     holds the agent's key and grants: pals_fetch must never write there",
                    path.display(),
                    downloads.display(),
                    cfg.data_dir.display()
                );
            }
            cfg.mcp.download_dir = Some(downloads);
        }
        cfg.inventory.file = dir.join(&cfg.inventory.file);
        if cfg.inventory.refresh_interval_s == 0 {
            bail!(
                "invalid {}: inventory.refresh_interval_s must be at least 1",
                path.display()
            );
        }
        if !(1..=MAX_OFFER_VALID_FOR_MS).contains(&cfg.discovery.offer_valid_for_ms) {
            bail!(
                "invalid {}: discovery.offer_valid_for_ms must be from 1 to \
                 {MAX_OFFER_VALID_FOR_MS}",
                path.display()
            );
        }
        let discovery = &cfg.discovery;
        let limits = [
            (
                "max_candidates",
                u64::try_from(discovery.max_candidates).unwrap_or(u64::MAX),
                MAX_SEARCH_CANDIDATES as u64,
            ),
            (
                "max_concurrent_queries",
                u64::try_from(discovery.max_concurrent_queries).unwrap_or(u64::MAX),
                MAX_CONCURRENT_QUERIES as u64,
            ),
            (
                "per_candidate_timeout_ms",
                discovery.per_candidate_timeout_ms,
                MAX_PER_CANDIDATE_TIMEOUT_MS,
            ),
            (
                "total_budget_ms",
                discovery.total_budget_ms,
                MAX_TOTAL_BUDGET_MS,
            ),
        ];
        if let Some((name, _, max)) = limits
            .iter()
            .find(|(_, value, max)| !(1..=*max).contains(value))
        {
            bail!(
                "invalid {}: discovery.{name} must be from 1 to {max}",
                path.display()
            );
        }
        if discovery.aliases.len() > MAX_ALIASES {
            bail!(
                "invalid {}: discovery.aliases has {} aliases, at most {MAX_ALIASES}",
                path.display(),
                discovery.aliases.len()
            );
        }
        if let Some((alias, capability)) = discovery
            .aliases
            .iter()
            .find(|(_, capability)| capability_tag(capability).is_err())
        {
            bail!(
                "invalid {}: discovery.aliases: {alias:?} names {capability:?}, which is no \
                 capability: a common-vocabulary id, such as vision.ocr, or <namespace>:<name>",
                path.display()
            );
        }
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
        if let Some(problem) = cfg.dht.problem() {
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

/// Where `pals_fetch` writes when `mcp.download_dir` is not set:
/// `~/Downloads/pals`.
pub fn default_download_dir() -> anyhow::Result<PathBuf> {
    default_download_dir_in(std::env::home_dir())
}

/// [`default_download_dir`] for the home directory `home`, if any.
fn default_download_dir_in(home: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let home = home
        .filter(|home| home.is_absolute())
        .context("there is no home directory for the default mcp.download_dir: set it")?;
    Ok(home.join("Downloads").join("pals"))
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
    fn download_dir_defaults_to_downloads_and_stays_out_of_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Unset, it is resolved only when used: loading needs no home.
        let cfg = Config::load(&write_config(dir.path(), "")).unwrap();
        assert_eq!(cfg.mcp.download_dir, None);
        let home = std::env::home_dir().unwrap();
        assert_eq!(
            cfg.mcp.resolved_download_dir().unwrap(),
            home.join("Downloads").join("pals")
        );
        assert_eq!(
            default_download_dir_in(Some(PathBuf::from("/h"))).unwrap(),
            PathBuf::from("/h/Downloads/pals")
        );
        for no_home in [None, Some(PathBuf::from("relative"))] {
            let err = default_download_dir_in(no_home).unwrap_err();
            assert!(format!("{err:#}").contains("mcp.download_dir"), "{err:#}");
        }

        // Relative to the configuration file's directory, as data_dir.
        let text = "data_dir = \"state\"\n[mcp]\ndownload_dir = \"incoming\"\n";
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert_eq!(cfg.mcp.download_dir, Some(dir.path().join("incoming")));

        // Never inside the data directory, however it is spelled.
        for download_dir in [
            "state",
            "state/downloads",
            "./state/x/../downloads",
            "elsewhere/../state",
        ] {
            let text = format!("data_dir = \"state\"\n[mcp]\ndownload_dir = \"{download_dir}\"\n");
            let err = Config::load(&write_config(dir.path(), &text)).unwrap_err();
            assert!(
                format!("{err:#}").contains("inside the data directory"),
                "{download_dir}: {err:#}"
            );
        }
        // The data directory defaults to the configuration's.
        let text = "[mcp]\ndownload_dir = \"downloads\"\n";
        let err = Config::load(&write_config(dir.path(), text)).unwrap_err();
        assert!(format!("{err:#}").contains("download_dir"), "{err:#}");
        let text = format!(
            "[mcp]\ndownload_dir = \"{}\"\n",
            dir.path().join("..").join("downloads").display()
        );
        assert!(Config::load(&write_config(dir.path(), &text)).is_ok());
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
    fn discovery_shows_the_public_view_unless_told_not_to() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&write_config(dir.path(), "")).unwrap();
        assert!(cfg.discovery.public);
        let text = "[discovery]\npublic = false\n";
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert!(!cfg.discovery.public);
        assert_eq!(cfg.discovery.offer_valid_for_ms, 15_000);
        let err =
            Config::load(&write_config(dir.path(), "[discovery]\nforward = true\n")).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown field `forward`"),
            "{err:#}"
        );
    }

    #[test]
    fn offers_last_from_a_millisecond_to_five_minutes() {
        let dir = tempfile::tempdir().unwrap();
        for (ms, valid) in [(0, false), (1, true), (300_000, true), (300_001, false)] {
            let text = format!("[discovery]\noffer_valid_for_ms = {ms}\n");
            match Config::load(&write_config(dir.path(), &text)) {
                Ok(cfg) => {
                    assert!(valid, "{ms}");
                    assert_eq!(cfg.discovery.offer_valid_for_ms, ms);
                }
                Err(err) => {
                    assert!(!valid, "{ms}: {err:#}");
                    assert!(format!("{err:#}").contains("offer_valid_for_ms"), "{err:#}");
                }
            }
        }
    }

    #[test]
    fn search_limits_default_to_the_design_and_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&write_config(dir.path(), "")).unwrap();
        let discovery = &cfg.discovery;
        assert_eq!(
            (
                discovery.max_candidates,
                discovery.max_concurrent_queries,
                discovery.per_candidate_timeout_ms,
                discovery.total_budget_ms
            ),
            (8, 3, 3000, 15_000)
        );
        for (name, max) in [
            ("max_candidates", 32),
            ("max_concurrent_queries", 8),
            ("per_candidate_timeout_ms", 10_000),
            ("total_budget_ms", 60_000),
        ] {
            for (value, valid) in [(0, false), (1, true), (max, true), (max + 1, false)] {
                let text = format!("[discovery]\n{name} = {value}\n");
                match Config::load(&write_config(dir.path(), &text)) {
                    Ok(_) => assert!(valid, "{name} = {value}"),
                    Err(err) => {
                        assert!(!valid, "{name} = {value}: {err:#}");
                        let err = format!("{err:#}");
                        assert!(err.contains(&format!("discovery.{name}")), "{err}");
                    }
                }
            }
        }
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
    fn inventory_file_is_beside_the_config_and_refreshes_every_900_s() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&write_config(dir.path(), "")).unwrap();
        assert_eq!(cfg.inventory.file, dir.path().join(INVENTORY_FILE));
        assert_eq!(cfg.inventory.refresh_interval_s, 900);

        let text = "data_dir = \"state\"\n[inventory]\nfile = \"inv/mine.toml\"\nrefresh_interval_s = 60\n";
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert_eq!(cfg.inventory.file, dir.path().join("inv").join("mine.toml"));
        assert_eq!(cfg.inventory.refresh_interval_s, 60);
        let text = "[inventory]\nfile = \"/srv/pals/inventory.toml\"\n";
        let cfg = Config::load(&write_config(dir.path(), text)).unwrap();
        assert_eq!(
            cfg.inventory.file,
            PathBuf::from("/srv/pals/inventory.toml")
        );

        for (text, word) in [
            (
                "[inventory]\nrefresh_interval_s = 0\n",
                "refresh_interval_s",
            ),
            ("[inventory]\nrefresh = 60\n", "unknown field `refresh`"),
        ] {
            let err = Config::load(&write_config(dir.path(), text)).unwrap_err();
            assert!(format!("{err:#}").contains(word), "{text}: {err:#}");
        }
    }

    #[test]
    fn inventory_template_is_empty_and_its_examples_fill_every_block() {
        use pals_core::inventory::Inventory;

        assert_eq!(
            Inventory::from_toml(INVENTORY_TEMPLATE).unwrap(),
            Inventory::default()
        );
        // Uncommenting every example line, and no prose, gives an inventory
        // with every block: the examples stay valid.
        let is_example = |line: &str| {
            line.starts_with('[')
                || line.split_once(" = ").is_some_and(|(key, _)| {
                    !key.is_empty()
                        && key
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-.\"".contains(&b))
                })
        };
        let uncommented: String = INVENTORY_TEMPLATE
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(example) if is_example(example) => example,
                _ => line,
            })
            .flat_map(|line| [line, "\n"])
            .collect();
        let inventory =
            Inventory::from_toml(&uncommented).unwrap_or_else(|err| panic!("{err}\n{uncommented}"));
        crate::inventory::mcp_adapter::check_adapters(&inventory).unwrap();
        assert!(!inventory.services.is_empty());
        assert!(!inventory.skills.is_empty());
        assert!(!inventory.plugins.is_empty());
        assert!(inventory.mcp.iter().any(|mcp| mcp.body.adapter.is_some()));
        assert!(inventory.runtime.is_some());
        assert!(!inventory.models.is_empty());
        assert!(!inventory.information.is_empty());
        assert!(inventory.hardware.is_some());
        assert!(inventory.network.is_some());
    }

    #[test]
    fn init_template_documents_the_dht() {
        // Uncommenting the section's settings turns the DHT on, as a client
        // with the defaults but for the example's external address, which
        // only a router may have.
        let is_setting = |line: &str| {
            line == "[dht]"
                || line.split_once(" = ").is_some_and(|(key, _)| {
                    !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                })
        };
        let mut text = String::new();
        let mut in_dht = false;
        for line in INIT_TEMPLATE.lines() {
            if line.starts_with('[') || line == "# [dht]" {
                in_dht = line == "# [dht]";
            }
            let line = match line.strip_prefix("# ") {
                Some(setting) if in_dht && is_setting(setting) => setting,
                _ => line,
            };
            text.push_str(line);
            text.push('\n');
        }
        let dir = tempfile::tempdir().unwrap();
        let err = Config::load(&write_config(dir.path(), &text)).unwrap_err();
        assert!(format!("{err:#}").contains("dht.external_addrs"), "{err:#}");
        let text = text.replace("mode = \"client\"", "mode = \"router\"");
        let cfg = Config::load(&write_config(dir.path(), &text)).unwrap();
        assert_eq!(
            cfg.dht,
            DhtCfg {
                enabled: true,
                mode: DhtMode::Router,
                external_addrs: vec!["203.0.113.10:4433".parse().unwrap()],
                ..DhtCfg::default()
            }
        );
        // What the template says of every lookup and of the relay.
        let flat = INIT_TEMPLATE.replace("\n# ", " ");
        assert!(
            flat.contains("tells the routers it asks which agent looks for which capability"),
            "{flat}"
        );
        assert!(flat.contains("n0's address lookup"), "{flat}");
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
