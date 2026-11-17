//! The node contract of the hito 3 backend spikes, which is not product code.
//!
//! Every candidate is a binary whose `main` is [`run_node`] over its own
//! [`Backend`]: `run_node` reads the command line ([`args`]), derives the
//! agent's key from `--key-seed`, starts the backend, issues and announces
//! the records of `--announce`, and then serves the control API
//! ([`control`]) the harness drives it with, verifying ([`verify`]) and
//! deduplicating ([`dedupe`]) whatever the backend's search returns. So the
//! records, their checks, the API and the measures are the same for every
//! candidate, and only the DHT changes.
//!
//! The records are `pals-core`'s `agentnet.contact/1` and
//! `agentnet.announce/1`, unchanged ([`records`]).

mod args;
pub mod control;
pub mod records;
pub mod stats;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use pals_core::identity::AgentKey;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use serde::Serialize;

pub use control::CONTROL_ADDR;
pub use records::{
    Candidate, RawCandidate, RawSearch, dedupe, issue_announce, issue_contact, local_addrs,
    tamper_signature, verify,
};
pub use stats::{BackendStats, StoredRecords, rss_kib};

/// The schemas of the records every candidate carries, from `pals-core`.
pub const RECORD_SCHEMAS: [&str; 2] = [
    pals_core::records::CONTACT_SCHEMA,
    pals_core::records::ANNOUNCE_SCHEMA,
];

/// A node's part in the topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Serves the DHT: stores and answers for others.
    Router,
    /// Announces capabilities, as a DHT client.
    Provider,
    /// Searches capabilities, as a DHT client.
    Client,
}

/// What a backend starts with, from the command line.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub name: String,
    pub role: Role,
    /// Where the backend listens.
    pub listen: SocketAddr,
    /// The nodes to join through, in the backend's own form: what their
    /// [`BackendStats::bootstrap_addr`] says. Empty for the first router, and
    /// for the isolated node of scenario S0.
    pub bootstrap: Vec<String>,
    /// How long the DHT keeps an announcement.
    pub ttl: Duration,
    /// How often the backend renews its announcements.
    pub republish: Duration,
}

/// A common scenario a backend cannot run as written, declared by the
/// backend itself so that the harness never has to know which one it
/// drives: it records the scenario with this status and these notes, and
/// does not run it, except S1 when `ByDesign`, which runs and must find
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Declared {
    /// `S0` to `S7`.
    pub scenario: &'static str,
    pub status: DeclaredStatus,
    /// Why, in Spanish, as the decision record will show it.
    pub notes: &'static str,
}

/// The status a [`Declared`] scenario gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeclaredStatus {
    /// What the scenario measures is absent on purpose (the baseline finds
    /// no one without prior contact).
    ByDesign,
    /// The scenario cannot run on this backend.
    NotApplicable,
}

/// A DHT backend: what each candidate implements.
///
/// The backend runs its network on tasks of its own: `run_node` calls these
/// methods one at a time, each after the one before it returned, from a
/// single task, so their futures need not be `Send`.
#[allow(async_fn_in_trait)]
pub trait Backend: Sized + Send + 'static {
    /// The scenarios this backend cannot run as written; none for a real
    /// candidate.
    const DECLARED: &'static [Declared] = &[];

    /// Starts the node with the agent's key, listening on `cfg.listen` and
    /// joining through `cfg.bootstrap`, and with no other bootstrap node,
    /// relay or public service.
    async fn start(cfg: &NodeConfig, key: &AgentKey) -> anyhow::Result<Self>;

    /// Publishes in the DHT the announcement of each capability and the
    /// contact, and renews them every `cfg.republish`. A later call
    /// replaces the earlier announcement: records with a higher `seq`,
    /// perhaps new addresses or other capabilities.
    async fn announce(
        &mut self,
        capabilities: &[String],
        contact: &Signed<ContactRecord>,
        announce: &Signed<AnnounceRecord>,
    ) -> anyhow::Result<()>;

    /// Stops renewing the announcement, and withdraws it where the DHT
    /// allows.
    async fn unannounce(&mut self) -> anyhow::Result<()>;

    /// Looks for providers of `capability`, up to `limit`, and returns what
    /// it found unverified: `run_node` checks it.
    async fn search(&mut self, capability: &str, limit: usize) -> anyhow::Result<RawSearch>;

    /// The counters as they are now.
    fn stats(&self) -> BackendStats;
}

/// The `main` of each candidate's binary: reads the command line, starts
/// `B` and serves the control API until the process ends.
pub async fn run_node<B: Backend>() -> anyhow::Result<()> {
    init_logging();
    let node = args::Cli::parse().into_node()?;
    tracing::info!(
        name = %node.cfg.name,
        role = ?node.cfg.role,
        agent_id = %node.key.id(),
        listen = %node.cfg.listen,
        bootstrap = ?node.cfg.bootstrap,
        rogue = node.rogue,
        "starting"
    );
    let backend = B::start(&node.cfg, &node.key)
        .await
        .context("the backend did not start")?;
    control::Node {
        backend,
        cfg: node.cfg,
        key: node.key,
        rogue: node.rogue,
        seq: 0,
    }
    .serve(node.announce)
    .await
}

/// Logs to stderr from `INFO` up; `RUST_LOG` overrides.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}
