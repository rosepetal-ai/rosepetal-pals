//! The DHT of milestone 3: a private Kademlia network (`libp2p-kad`) whose
//! connections are the daemon's own Iroh connections, on the ALPN
//! `agentnet-kad/0`, and whose `PeerId` is the agent's own key
//! (`docs/protocols/agentnet-kad-0.md`). An agent finds there the providers
//! of a capability it has no contact with; what it gets is untrusted data,
//! checked before it is used.
//!
//! [`keys`] fixes what the network is addressed by, frozen by the vector
//! `crates/pals-core/tests/vectors/kad-v0.json`: the agent's `PeerId`, the
//! keys of its signed contact and announce, and the key of a capability's
//! provider records. [`transport`] and [`muxer`] are libp2p's transport
//! over the node's Iroh endpoint, with the address forms and the limits of
//! `agentnet-kad/0`'s connections and streams; their tests,
//! `crates/pals/tests/dht_transport.rs`, are the gate of any change of the
//! `libp2p` or `iroh` versions. [`validate`] is what a router stores under
//! those keys, and the reason for each refusal. [`swarm`] runs them: the
//! node's swarm, its mode, how it enters the network and what it declares,
//! on a task of its own that a [`Dht`] talks to. [`publish`] is what that
//! task publishes and how it finds others: the rounds that put the node's
//! [`PublishedRecords`] in the DHT, and the lookups of [`Dht::providers`].
//!
//! The DHT is off unless `[dht] enabled`: then the node serves
//! `agentnet-kad/0` and runs its swarm, as a client unless `[dht] mode =
//! "router"` (H3-D3). It publishes only with `[dht] announce`, and then only
//! the capabilities of the public services (H3-D4).

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iroh::Endpoint;
use libp2p::Multiaddr;
use libp2p::kad::store::MemoryStoreConfig;
use pals_core::clock::Clock;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use pals_core::store::Store;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::{DhtCfg, DhtMode};

pub mod keys;
pub mod muxer;
pub mod publish;
pub mod swarm;
pub mod transport;
pub mod validate;

pub use self::publish::{PublicationStatus, PublishedRecords};
use self::swarm::Command;
use self::transport::{IrohListener, KadQuotas, RefusedCounts};

/// How long one Kademlia query may take (`libp2p-kad`'s is 60 s).
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a connection, or a stream the other side opened, may go idle:
/// `libp2p-swarm`'s idle connection timeout.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// The pause before a round of publication that reached no router is tried
/// again.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// The DHT's times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhtTiming {
    /// How long the routers keep a value or a provider record.
    pub record_ttl: Duration,
    /// How often the node publishes its own again, and a router replicates
    /// what it holds.
    pub republish: Duration,
    /// How long one Kademlia query may take.
    pub query_timeout: Duration,
    /// How long a connection with no stream stays open, and an incoming
    /// stream with no read or write.
    pub idle_timeout: Duration,
    /// The pause before a round of publication that reached no router is
    /// tried again.
    pub retry_after: Duration,
}

impl DhtTiming {
    /// `[dht] record_ttl_s` and `republish_s`, with [`QUERY_TIMEOUT`],
    /// [`IDLE_TIMEOUT`] and [`RETRY_AFTER`].
    pub fn from_config(cfg: &DhtCfg) -> Self {
        Self {
            record_ttl: Duration::from_secs(cfg.record_ttl_s),
            republish: Duration::from_secs(cfg.republish_s),
            query_timeout: QUERY_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            retry_after: RETRY_AFTER,
        }
    }
}

/// What a [`Dht`] runs on.
pub struct DhtParts {
    /// The node's endpoint, bound, with the agent's key.
    pub endpoint: Endpoint,
    pub key: Arc<AgentKey>,
    pub cfg: DhtCfg,
    pub timing: DhtTiming,
    pub quotas: KadQuotas,
    /// The limits of what a router stores ([`swarm::STORE_LIMITS`] in the
    /// daemon).
    pub store_limits: MemoryStoreConfig,
    /// The clock the signed records and the stream quotas follow.
    pub clock: Arc<dyn Clock>,
    /// The node's store: its saved contacts, entered through when no
    /// bootstrap router answers.
    pub store: Arc<Store>,
    /// What the node publishes: `Some` with `[dht] announce`, and then the
    /// node's own records; none publishes nothing, not even the contact.
    pub published: Option<Arc<dyn PublishedRecords>>,
}

/// The node's DHT: its swarm, running on a task of its own. Cloning is
/// cheap; every clone talks to the same task.
#[derive(Clone)]
pub struct Dht {
    inner: Arc<Inner>,
}

struct Inner {
    commands: mpsc::Sender<Command>,
    /// A router's `[dht] external_addrs`, which its ticket carries.
    external_addrs: Vec<SocketAddr>,
    /// The swarm's task, until [`Dht::shutdown`].
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let task = self.task.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(task) = task.take() {
            task.abort();
        }
    }
}

/// The DHT's state, as `GET /dht` and `pals dht status` show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DhtStatus {
    /// `libp2p-kad`'s mode: `router` is its server mode.
    pub mode: DhtMode,
    /// The routers in the routing table.
    pub routing_peers: usize,
    /// The connections of `agentnet-kad/0` open now.
    pub connections: usize,
    /// The incoming connections established since the node started.
    pub connections_accepted: u64,
    /// What the transport refused since the node started.
    pub refused: RefusedCounts,
    /// Others' values stored, unexpired.
    pub records_stored: usize,
    /// Others' provider records stored, unexpired.
    pub provider_records_stored: usize,
    /// Of those values and provider records, how many each agent has.
    pub stored_by_agent: BTreeMap<AgentId, u32>,
    /// The records refused since the node started, by
    /// [`validate::Rejection::as_str`]; every reason, zero included.
    pub rejected: BTreeMap<&'static str, u64>,
    /// What the node publishes, with `[dht] announce`.
    pub publication: Option<PublicationStatus>,
}

/// What a lookup of a capability's providers found ([`Dht::providers`]).
#[derive(Debug, Clone, Default)]
pub struct DhtLookup {
    /// The providers with a valid contact and a valid announce that lists
    /// the capability, in the order they arrived.
    pub found: Vec<DhtCandidate>,
    /// The providers that were not candidates by the end, each with why:
    /// `capability_not_announced` when its valid announce does not list the
    /// capability; else the record it has no valid copy of: the name of
    /// the last refusal of a copy of it ([`validate::Rejection::as_str`]),
    /// or, with none refused, `no valid contact` or `no valid announce`.
    /// When it has neither, the announce's refusal, else the contact's,
    /// else `no valid contact`.
    pub discarded: Vec<(AgentId, String)>,
    /// The requests its queries sent to the network.
    pub requests: u64,
    /// Whether its deadline ended it, before its queries did.
    pub cut: bool,
}

/// A provider found in the DHT: its newest valid contact and announce, as
/// they were verified and as they arrived. Untrusted all the same: a search
/// checks them again before it uses them.
#[derive(Debug, Clone)]
pub struct DhtCandidate {
    pub agent_id: AgentId,
    pub contact: Signed<ContactRecord>,
    pub contact_json: Vec<u8>,
    pub announce: Signed<AnnounceRecord>,
    pub announce_json: Vec<u8>,
}

/// Why the DHT did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DhtError {
    /// No router answered.
    #[error("no DHT router answered")]
    NoRouters,
    /// The DHT's task is not running: the node is stopping.
    #[error("the DHT is stopped")]
    Stopped,
}

impl Dht {
    /// Starts the node's swarm over `parts.endpoint`, as [`swarm`] says,
    /// and returns the handler of [`transport::ALPN_KAD`] that the node's
    /// router must serve. Fails when a bootstrap ticket is not one, has no
    /// direct IP address, or is the node's own.
    pub fn spawn(parts: DhtParts) -> anyhow::Result<(Dht, IrohListener)> {
        let external_addrs = match parts.cfg.mode {
            DhtMode::Router => parts.cfg.external_addrs.clone(),
            DhtMode::Client => Vec::new(),
        };
        let (commands, task, listener) = swarm::spawn(parts)?;
        let dht = Dht {
            inner: Arc::new(Inner {
                commands,
                external_addrs,
                task: Mutex::new(Some(task)),
            }),
        };
        Ok((dht, listener))
    }

    /// The DHT's state now: what has not expired, and the counts since the
    /// node started.
    pub async fn status(&self) -> Result<DhtStatus, DhtError> {
        self.ask(Command::Status).await
    }

    /// What the node holds as a router of `capability`'s providers: each
    /// provider's agent and the addresses its record carries, unexpired.
    /// None for a string that is not a capability.
    pub async fn held_providers(
        &self,
        capability: &str,
    ) -> Result<Vec<(AgentId, Vec<Multiaddr>)>, DhtError> {
        let Ok(key) = keys::provider_key(capability) else {
            return Ok(Vec::new());
        };
        self.ask(|reply| Command::HeldProviders(key, reply)).await
    }

    /// What the node holds as a router of `agent`'s contact and announce,
    /// each as the signed JSON it stored, unexpired.
    pub async fn held_records(
        &self,
        agent: &AgentId,
    ) -> Result<(Option<Vec<u8>>, Option<Vec<u8>>), DhtError> {
        let agent = *agent;
        self.ask(|reply| Command::HeldRecords(agent, reply)).await
    }

    /// Looks for `capability`'s providers, up to `limit` of them, as
    /// [`publish`] says: what it has at `deadline`, with
    /// [`DhtLookup::cut`], if its queries did not end before.
    /// [`DhtError::NoRouters`] when its queries ended with no router
    /// having answered. Nothing is found for a string that is not a
    /// capability.
    pub async fn providers(
        &self,
        capability: &str,
        limit: usize,
        deadline: tokio::time::Instant,
    ) -> Result<DhtLookup, DhtError> {
        self.providers_except(capability, HashSet::new(), limit, deadline)
            .await
    }

    /// [`Dht::providers`], but for the agents of `known`: none of them is
    /// taken, counted in `limit` or looked up further, so the `limit` goes
    /// to the others. A search leaves out its contacts this way, which it
    /// evaluates as contacts.
    pub async fn providers_except(
        &self,
        capability: &str,
        known: HashSet<AgentId>,
        limit: usize,
        deadline: tokio::time::Instant,
    ) -> Result<DhtLookup, DhtError> {
        let Ok(key) = keys::provider_key(capability) else {
            return Ok(DhtLookup::default());
        };
        self.ask(|reply| Command::Providers {
            key,
            known,
            limit,
            deadline,
            reply,
        })
        .await?
    }

    /// A router's `[dht] external_addrs`, which its ticket and its contact
    /// carry; none for a client.
    pub fn external_addrs(&self) -> &[SocketAddr] {
        &self.inner.external_addrs
    }

    /// Stops the swarm's task and waits until it is over: every clone
    /// answers [`DhtError::Stopped`] from then on.
    pub async fn shutdown(&self) {
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.abort();
            // Cancelled, or over before; either way it is done.
            let _ = task.await;
        }
    }

    async fn ask<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, DhtError> {
        let (reply, answer) = oneshot::channel();
        self.inner
            .commands
            .send(command(reply))
            .await
            .map_err(|_| DhtError::Stopped)?;
        answer.await.map_err(|_| DhtError::Stopped)
    }
}
