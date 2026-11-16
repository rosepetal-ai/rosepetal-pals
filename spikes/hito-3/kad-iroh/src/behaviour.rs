//! The node's libp2p side: a swarm of `libp2p-kad` and `identify` over
//! the Iroh transport ([`crate::transport`]), driven by a task of its own
//! that the backend talks to through a [`Handle`].
//!
//! Copied from kad-libp2p's `behaviour.rs`; the Kademlia configuration,
//! the validation of what arrives, the search, the sweep, the counters and
//! the workarounds of its decision record (`NoDial`, the external
//! addresses, `identify` without loopback or cache, the server addresses
//! given back) are the same. What changed:
//! - [`spawn`] binds the node's one Iroh endpoint at `--listen` with the
//!   agent's key, serves [`ALPN_KAD`] on it through an Iroh router, and
//!   builds the swarm over [`IrohTransport`] with `Swarm::new` and the
//!   `PeerId` of the agent's public key: libp2p never gets the secret key.
//!   `identify` gets only the public key, so it sends no signed peer
//!   record;
//! - the contact goes under `/agentnet/contact/<peer_id>` in place of the
//!   identity link, and a search looks up the contact and the announce;
//! - `stored_by_agent` attributes each record to the agent its key's
//!   `PeerId` is, with no link to remember;
//! - the dial that `NoDial` saves a search from would wait the transport's
//!   [`crate::transport::DIAL_TIMEOUT`] instead of `libp2p-quic`'s
//!   handshake timeout, which is as long (5 s);
//! - an announce answers at its deadline even with a put still in flight
//!   (`Task::end_announce`), so that the control API gets its answer within
//!   its 30 s.
//!
//! The addresses keep libp2p's QUIC form,
//! `/ip4/<ip>/udp/<port>/quic-v1/p2p/<peer_id>`, which the transport turns
//! into an Iroh endpoint and a direct address.
//!
//! Kademlia (`libp2p-kad` 0.49) runs on protocol [`KAD_PROTOCOL`], in
//! server mode on routers and in client mode on everyone else, over a
//! `MemoryStore` with [`STORE_LIMITS`]. The library's own mechanisms keep
//! the records alive and make them expire: records and provider records
//! last the DHT's TTL (`set_record_ttl`, `set_provider_record_ttl`), the
//! publisher republishes its own every `republish`
//! (`set_publication_interval`, `set_provider_publication_interval`), and
//! the records' job runs at that same interval (`set_replication_interval`:
//! the job that republishes is the replication job, which runs hourly by
//! default and republishes only on its runs). Replication never extends a
//! record's life. `libp2p-kad` removes an expired record lazily (when it is
//! read, or on the job's run); the task sweeps the store every
//! `SWEEP_EVERY` of what the library itself marked expired, so that what
//! a router counts is what it would still serve.
//!
//! Every `PutRecord` and `AddProvider` a router receives comes to the task
//! (`StoreInserts::FilterBoth`), which stores it only if
//! [`crate::validate`] says so; whatever it refuses adds to
//! `records_rejected`.
//!
//! The node bootstraps only from `--bootstrap`: no IPFS nodes, no mDNS, no
//! relay, AutoNAT, UPnP or DCUtR, and Iroh's address lookup and relays are
//! off. Kademlia learns a peer's addresses from `--bootstrap`, from
//! `identify` (only for peers that speak [`KAD_PROTOCOL`] as servers, and
//! only the addresses they declare external) and from other servers'
//! answers, so a node with no bootstrap knows no one. After a failed dial
//! to a server, the task gives Kademlia back the addresses it knew of it,
//! which `libp2p-kad` would drop.
//!
//! A search ([`Handle::search`]) asks for the capability's providers and,
//! as each `PeerId` arrives, up to `limit`, for its contact and its
//! announce. It verifies each copy, keeps the newest valid contact, and
//! passes it on with the newest valid announce of the same agent, for
//! `spike_common::verify` to check again. It never dials the providers it
//! finds (`Task::on_providers` says why). A candidate's `found_ms` is when
//! the search first had a valid contact and a valid announce of the same
//! agent; `requests` sums `QueryStats::num_requests` of its queries, minus
//! the dials `NoDial` denied.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU8;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail, ensure};
use iroh::protocol::Router;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Transport};
use libp2p::futures::StreamExt;
use libp2p::kad::store::{MemoryStore, MemoryStoreConfig, RecordStore};
use libp2p::kad::{
    self, Caching, GetProvidersOk, GetRecordOk, InboundRequest, Mode, ProviderRecord,
    PutRecordError, QueryId, QueryResult, QueryStats, Quorum, Record, RecordKey, StoreInserts,
};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, SwarmEvent, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, identify, swarm};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use spike_common::{BackendStats, RawCandidate, RawSearch, Role, StoredRecords, local_addrs};
use tokio::sync::{mpsc, oneshot};

use crate::transport::{ALPN_KAD, IrohTransport, quic_addr};
use crate::validate::{self, Slot, Verdict};
use crate::{bind_endpoint, now_unix};

/// The DHT's own Kademlia protocol (spec §6.2), never IPFS's.
pub const KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/agentnet/kad/1.0.0");
/// What `identify` says the node speaks.
const IDENTIFY_VERSION: &str = "agentnet/0";
/// The store's limits, per node:
/// - at most 1024 values, two per agent (contact and announce);
/// - 8 KiB a value, kad-libp2p's, above both a contact's and an
///   announce's limit (`pals_core::records::MAX_RECORD_BYTES`, 4 KiB);
/// - `K` (20) providers per key, the replication factor: further ones are
///   ignored, as `libp2p-kad` does against floods;
/// - 1024 provider keys: the store counts every key it holds providers
///   for, not only the node's own.
pub const STORE_LIMITS: MemoryStoreConfig = MemoryStoreConfig {
    max_records: 1024,
    max_value_bytes: 8 * 1024,
    max_providers_per_key: 20,
    max_provided_keys: 1024,
};
/// How long one Kademlia query may take (60 s by default).
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a search may take, all its queries together; it then returns
/// what it has.
const SEARCH_DEADLINE: Duration = Duration::from_secs(30);
/// How long an announce keeps trying to reach a router, inside the control
/// API's 30 s: after a network change (scenario S7) the first puts go over
/// connections that are dead but not yet closed.
const ANNOUNCE_DEADLINE: Duration = Duration::from_secs(25);
/// The pause before a put that reached no one is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(1);
/// How long the start waits for the first bootstrap to finish.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the task sweeps expired records and refreshes its stats.
const SWEEP_EVERY: Duration = Duration::from_secs(1);
/// The variable that sets the swarm's dial concurrency factor, for the
/// repeated measurements of task 7 of phase 2 of the hito 3 plan: the idle
/// traffic of kad-iroh dialing each peer once (H3-D1, condition 1).
const DIAL_CONCURRENCY_VAR: &str = "SPIKE_DIAL_CONCURRENCY";

/// Kademlia and `identify`, and the peers a search does not dial.
#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub kad: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub undialed: NoDial,
}

/// The peers a search keeps itself from dialing (`Task::on_providers`):
/// a dial to one of them is denied, and counted, before it starts. Unlike
/// `allow_block_list`, it never closes a connection, and it lets a peer
/// dial in.
#[derive(Debug, Default)]
pub struct NoDial {
    peers: HashSet<PeerId>,
    denied: u64,
}

impl NoDial {
    /// Denies the dials to `peer`; whether it was not denied already.
    fn deny(&mut self, peer: PeerId) -> bool {
        self.peers.insert(peer)
    }

    fn allow(&mut self, peer: &PeerId) {
        self.peers.remove(peer);
    }

    /// The dials denied since the last call.
    fn take_denied(&mut self) -> u64 {
        std::mem::take(&mut self.denied)
    }
}

/// Why [`NoDial`] denied a dial.
#[derive(Debug)]
struct NotDialed;

impl std::fmt::Display for NotDialed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a search does not dial the providers it finds")
    }
}

impl std::error::Error for NotDialed {}

impl NetworkBehaviour for NoDial {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: Option<PeerId>,
        _: &[Multiaddr],
        _: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(peer) = peer
            && self.peers.contains(&peer)
        {
            self.denied += 1;
            return Err(ConnectionDenied::new(NotDialed));
        }
        Ok(Vec::new())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, _: FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

/// What the libp2p side starts with.
#[derive(Debug, Clone)]
pub struct Settings {
    pub role: Role,
    /// Where the node's Iroh endpoint binds, and so libp2p listens.
    pub listen: SocketAddr,
    /// `--bootstrap`: each one peer's multiaddrs, comma-separated, each
    /// ending in `/p2p/<PeerId>`.
    pub bootstrap: Vec<String>,
    pub ttl: Duration,
    pub republish: Duration,
}

/// The Kademlia configuration of every node: [`KAD_PROTOCOL`], the DHT's
/// TTL and republication, every inbound record filtered, no caching.
pub fn kad_config(ttl: Duration, republish: Duration) -> kad::Config {
    let mut config = kad::Config::new(KAD_PROTOCOL);
    config
        .set_record_ttl(Some(ttl))
        .set_provider_record_ttl(Some(ttl))
        .set_publication_interval(Some(republish))
        .set_provider_publication_interval(Some(republish))
        .set_replication_interval(Some(republish))
        .set_record_filtering(StoreInserts::FilterBoth)
        .set_caching(Caching::Disabled)
        .set_query_timeout(QUERY_TIMEOUT);
    config
}

/// The backend's side of the task.
pub struct Handle {
    commands: mpsc::Sender<Command>,
    stats: Arc<Mutex<BackendStats>>,
    /// Serves [`ALPN_KAD`] on the endpoint; held, since it stops when
    /// dropped.
    _router: Router,
}

enum Command {
    Announce {
        contact: Vec<u8>,
        announce: Vec<u8>,
        capabilities: Vec<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Unannounce {
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Search {
        capability: String,
        limit: usize,
        reply: oneshot::Sender<anyhow::Result<RawSearch>>,
    },
}

impl Handle {
    /// Publishes `contact` and `announce` under the node's `PeerId`, then
    /// provides each capability, replacing an earlier announcement; returns
    /// once each record reached at least one router.
    pub async fn announce(
        &self,
        contact: Vec<u8>,
        announce: Vec<u8>,
        capabilities: Vec<String>,
    ) -> anyhow::Result<()> {
        self.ask(|reply| Command::Announce {
            contact,
            announce,
            capabilities,
            reply,
        })
        .await?
    }

    /// Stops republishing and providing; the routers' copies expire.
    pub async fn unannounce(&self) -> anyhow::Result<()> {
        self.ask(|reply| Command::Unannounce { reply }).await?
    }

    /// Looks for `capability`'s providers, up to `limit`, as the module
    /// says.
    pub async fn search(&self, capability: String, limit: usize) -> anyhow::Result<RawSearch> {
        self.ask(|reply| Command::Search {
            capability,
            limit,
            reply,
        })
        .await?
    }

    /// The counters as the task last left them, at most `SWEEP_EVERY`
    /// ago.
    pub fn stats(&self) -> BackendStats {
        self.stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    async fn ask<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> anyhow::Result<T> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| anyhow!("the libp2p task stopped"))?;
        answer.await.map_err(|_| anyhow!("the libp2p task stopped"))
    }
}

/// Starts the node: its Iroh endpoint at `settings.listen` with `key`,
/// serving [`ALPN_KAD`], and over it Kademlia in the role's mode, joined
/// through `settings.bootstrap`. Returns once it listens and its first
/// bootstrap finished, or `JOIN_TIMEOUT` passed.
pub async fn spawn(settings: Settings, key: &AgentKey) -> anyhow::Result<Handle> {
    let bootstrap = settings
        .bootstrap
        .iter()
        .map(|text| parse_bootstrap(text))
        .collect::<anyhow::Result<Vec<_>>>()?
        .concat();
    let endpoint = bind_endpoint(settings.listen, key).await?;
    let (transport, listener) = IrohTransport::new(endpoint.clone());
    let router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
    let local = transport.local_peer_id();
    let public_key = transport.local_public_key();
    let config = kad_config(settings.ttl, settings.republish);
    let behaviour = Behaviour {
        kad: kad::Behaviour::with_config(
            local,
            MemoryStore::with_config(local, STORE_LIMITS),
            config,
        ),
        // A node tells others only its external addresses, its local ones
        // but loopback ([`Task::refresh_external_addrs`]): every node
        // listens at the same port, so a peer's `127.0.0.1` is the dialer
        // itself. And `identify` keeps no cache of what peers say, which it
        // would add to every dial of them: in kad-libp2p, a dial whose
        // loopback address answered first failed whole. Kept as it was, not
        // tried without over Iroh. Kademlia gets what `identify` learns
        // through [`Task::on_swarm_event`] only. With the public key alone,
        // `identify` sends no signed peer record.
        identify: identify::Behaviour::new(
            identify::Config::new(IDENTIFY_VERSION.to_owned(), public_key)
                .with_hide_listen_addrs(true)
                .with_cache_size(0),
        ),
        undialed: NoDial::default(),
    };
    // `SwarmBuilder` wants the secret key; the swarm itself needs only the
    // `PeerId`, and its default configuration is the builder's. Without
    // SPIKE_DIAL_CONCURRENCY it stays so, as in phase 1: libp2p's default
    // dial concurrency factor, 8, which dials both addresses of a router at
    // once. With it, at most that many at once: 1 dials them one after
    // another, so a router gets one dial of each peer at a time.
    let raw_dial_concurrency = match std::env::var(DIAL_CONCURRENCY_VAR) {
        Ok(text) => Some(text),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => bail!("{DIAL_CONCURRENCY_VAR}: {error}"),
    };
    let dial_concurrency = dial_concurrency(raw_dial_concurrency.as_deref())?;
    let mut swarm_config = swarm::Config::with_tokio_executor();
    if let Some(factor) = dial_concurrency {
        swarm_config = swarm_config.with_dial_concurrency_factor(factor);
    }
    let mut swarm = Swarm::new(Transport::boxed(transport), behaviour, local, swarm_config);
    let mode = match settings.role {
        Role::Router => Mode::Server,
        Role::Provider | Role::Client => Mode::Client,
    };
    swarm.behaviour_mut().kad.set_mode(Some(mode));
    swarm
        .listen_on(quic_addr(settings.listen))
        .with_context(|| format!("cannot listen on {}", settings.listen))?;
    let mut known_addrs: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    for (peer, addr) in &bootstrap {
        swarm.behaviour_mut().kad.add_address(peer, addr.clone());
        known_addrs.entry(*peer).or_default().push(addr.clone());
    }
    let joining = if bootstrap.is_empty() {
        None
    } else {
        swarm.behaviour_mut().kad.bootstrap().ok()
    };
    tracing::info!(peer_id = %local, ?mode, listen = %settings.listen, bootstrap = bootstrap.len(), ?dial_concurrency, "libp2p over Iroh started");

    let stats = Arc::new(Mutex::new(BackendStats::default()));
    let (commands, queue) = mpsc::channel(4);
    let (ready, started) = oneshot::channel();
    let task = Task {
        swarm,
        role: settings.role,
        listen: settings.listen,
        local,
        stats: Arc::clone(&stats),
        bootstrap_addr: None,
        port: None,
        joining,
        rejected: 0,
        provider_keys: HashSet::new(),
        provided: Vec::new(),
        known_addrs,
        announcing: None,
        searching: None,
    };
    tokio::spawn(task.run(queue, ready));
    started
        .await
        .map_err(|_| anyhow!("the libp2p task stopped"))??;
    Ok(Handle {
        commands,
        stats,
        _router: router,
    })
}

/// A `--bootstrap` of this backend: one peer's multiaddrs, comma-separated,
/// each ending in `/p2p/<PeerId>`.
fn parse_bootstrap(text: &str) -> anyhow::Result<Vec<(PeerId, Multiaddr)>> {
    let addrs = text
        .split(',')
        .map(|part| {
            let addr: Multiaddr = part
                .trim()
                .parse()
                .with_context(|| format!("not a multiaddr: {part:?}"))?;
            match addr.iter().last() {
                Some(Protocol::P2p(peer)) => Ok((peer, addr)),
                _ => bail!("a bootstrap multiaddr must end in /p2p/<PeerId>: {part:?}"),
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(!addrs.is_empty(), "an empty --bootstrap");
    Ok(addrs)
}

/// The task that owns the swarm.
struct Task {
    swarm: Swarm<Behaviour>,
    role: Role,
    listen: SocketAddr,
    local: PeerId,
    stats: Arc<Mutex<BackendStats>>,
    /// A router's addresses, for `--bootstrap`, once it listens.
    bootstrap_addr: Option<String>,
    /// The port the listener got, once it listens.
    port: Option<u16>,
    /// The first bootstrap, while it runs.
    joining: Option<QueryId>,
    rejected: u64,
    /// The keys the node stores others' provider records under.
    provider_keys: HashSet<RecordKey>,
    /// The keys the node itself provides.
    provided: Vec<RecordKey>,
    /// Every address `--bootstrap` and `identify` gave of each DHT server,
    /// handed back to Kademlia after a dial to it fails
    /// ([`Task::on_swarm_event`]).
    known_addrs: HashMap<PeerId, Vec<Multiaddr>>,
    announcing: Option<Announcing>,
    searching: Option<Searching>,
}

/// An announce in progress: the puts of the contact and the announce, then
/// the capabilities provided.
struct Announcing {
    reply: oneshot::Sender<anyhow::Result<()>>,
    deadline: Instant,
    records: Vec<Record>,
    /// Which record each put in flight is of.
    puts: HashMap<QueryId, usize>,
    /// Whether each record reached a router.
    reached: Vec<bool>,
    retry_at: Option<Instant>,
    capabilities: Vec<RecordKey>,
    providing: HashSet<QueryId>,
}

/// A search in progress.
struct Searching {
    reply: oneshot::Sender<anyhow::Result<RawSearch>>,
    started: Instant,
    limit: usize,
    providers: Option<QueryId>,
    found: Vec<Found>,
    /// Which candidate and which of its records each lookup in flight is
    /// for.
    lookups: HashMap<QueryId, (usize, Slot)>,
    /// Every query's requests so far.
    requests: HashMap<QueryId, u32>,
    /// The providers it found that it keeps itself from dialing, until it
    /// ends.
    undialed: Vec<PeerId>,
}

/// One provider a search found, and what it got for it.
struct Found {
    peer: PeerId,
    contacts: Vec<Signed<ContactRecord>>,
    announces: Vec<Signed<AnnounceRecord>>,
    /// The first copy of the announce, when none verified: so that
    /// `spike_common::verify` says why.
    raw_announce: Option<Vec<u8>>,
    found_ms: Option<u64>,
}

impl Task {
    async fn run(
        mut self,
        mut queue: mpsc::Receiver<Command>,
        ready: oneshot::Sender<anyhow::Result<()>>,
    ) {
        let mut ready = Some(ready);
        let join_by = Instant::now() + JOIN_TIMEOUT;
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            if self.port.is_some()
                && (self.joining.is_none() || Instant::now() >= join_by)
                && let Some(ready) = ready.take()
            {
                self.publish_stats();
                let _ = ready.send(Ok(()));
            }
            tokio::select! {
                command = queue.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => break,
                },
                event = self.swarm.select_next_some() => {
                    if let Err(error) = self.on_swarm_event(event)
                        && let Some(ready) = ready.take()
                    {
                        let _ = ready.send(Err(error));
                        return;
                    }
                }
                _ = tick.tick() => self.on_tick(),
            }
        }
    }

    fn kad(&mut self) -> &mut kad::Behaviour<MemoryStore> {
        &mut self.swarm.behaviour_mut().kad
    }

    fn on_command(&mut self, command: Command) {
        if self.announcing.is_some() || self.searching.is_some() {
            // `run_node` waits for each call before the next.
            let busy = || anyhow!("another call is in progress");
            match command {
                Command::Announce { reply, .. } | Command::Unannounce { reply } => {
                    let _ = reply.send(Err(busy()));
                }
                Command::Search { reply, .. } => {
                    let _ = reply.send(Err(busy()));
                }
            }
            return;
        }
        match command {
            Command::Announce {
                contact,
                announce,
                capabilities,
                reply,
            } => {
                // Where the node is now goes in its provider records
                // (scenario S7).
                self.refresh_external_addrs();
                self.announce(contact, announce, capabilities, reply)
            }
            Command::Unannounce { reply } => {
                let local = self.local;
                self.kad().remove_record(&validate::contact_key(&local));
                self.kad().remove_record(&validate::announce_key(&local));
                for key in std::mem::take(&mut self.provided) {
                    self.kad().stop_providing(&key);
                }
                let _ = reply.send(Ok(()));
            }
            Command::Search {
                capability,
                limit,
                reply,
            } => {
                self.swarm.behaviour_mut().undialed.take_denied();
                let providers = self
                    .kad()
                    .get_providers(validate::provider_key(&capability));
                self.searching = Some(Searching {
                    reply,
                    started: Instant::now(),
                    limit,
                    providers: Some(providers),
                    found: Vec::new(),
                    lookups: HashMap::new(),
                    requests: HashMap::new(),
                    undialed: Vec::new(),
                });
            }
        }
    }

    fn announce(
        &mut self,
        contact: Vec<u8>,
        announce: Vec<u8>,
        capabilities: Vec<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    ) {
        let local = self.local;
        let capabilities: Vec<RecordKey> = capabilities
            .iter()
            .map(|c| validate::provider_key(c))
            .collect();
        let records = vec![
            Record::new(validate::contact_key(&local), contact),
            Record::new(validate::announce_key(&local), announce),
        ];
        if let Err(error) = self.check_room(&records, &capabilities) {
            let _ = reply.send(Err(error));
            return;
        }
        let dropped: Vec<RecordKey> = self
            .provided
            .iter()
            .filter(|key| !capabilities.contains(key))
            .cloned()
            .collect();
        for key in &dropped {
            self.kad().stop_providing(key);
        }
        self.provided.retain(|key| !dropped.contains(key));
        let mut announcing = Announcing {
            reply,
            deadline: Instant::now() + ANNOUNCE_DEADLINE,
            reached: vec![false; records.len()],
            records,
            puts: HashMap::new(),
            retry_at: None,
            capabilities,
            providing: HashSet::new(),
        };
        for i in 0..announcing.records.len() {
            let record = announcing.records[i].clone();
            // Fails only when the store has no room, which `check_room`
            // ruled out with nothing run since.
            match self.kad().put_record(record, Quorum::All) {
                Ok(id) => {
                    announcing.puts.insert(id, i);
                }
                Err(error) => {
                    let _ = announcing
                        .reply
                        .send(Err(anyhow!("cannot store the own record: {error:?}")));
                    return;
                }
            }
        }
        self.announcing = Some(announcing);
    }

    /// Whether the store has room for `records` and for providing `keys`:
    /// `put_record` and `start_providing` fail only when it has none, so an
    /// announce checks first and fails, if it must, before it changes
    /// anything. It counts as `MemoryStore` does: a value as large as
    /// `max_value_bytes` does not fit, and every distinct provider key the
    /// store holds, the node's own and others', counts towards
    /// `max_provided_keys`.
    fn check_room(&mut self, records: &[Record], keys: &[RecordKey]) -> anyhow::Result<()> {
        let held: HashSet<&RecordKey> = self.provider_keys.iter().chain(&self.provided).collect();
        let new_keys = keys.iter().filter(|key| !held.contains(key)).count();
        ensure!(
            held.len() + new_keys <= STORE_LIMITS.max_provided_keys,
            "the store has no room to provide {new_keys} more capabilities"
        );
        let store = self.swarm.behaviour_mut().kad.store_mut();
        ensure!(
            records
                .iter()
                .all(|r| r.value.len() < STORE_LIMITS.max_value_bytes),
            "an own record is {} bytes or more",
            STORE_LIMITS.max_value_bytes
        );
        let new_records = records
            .iter()
            .filter(|r| store.get(&r.key).is_none())
            .count();
        ensure!(
            store.records().count() + new_records <= STORE_LIMITS.max_records,
            "the store has no room for the own records"
        );
        Ok(())
    }

    /// Provides each of `keys`. When one cannot be (only if, after
    /// `check_room`, others' provider records filled the store, which only
    /// happens to a node that is also a router), stops providing the keys
    /// this call started and fails: the node then provides what it did
    /// before, minus what the new announcement dropped, while the DHT
    /// already has its new contact and announce.
    fn provide(&mut self, keys: &[RecordKey]) -> anyhow::Result<HashSet<QueryId>> {
        let mut queries = HashSet::new();
        let mut started = Vec::new();
        for key in keys {
            match self.kad().start_providing(key.clone()) {
                Ok(id) => {
                    queries.insert(id);
                    if !self.provided.contains(key) {
                        self.provided.push(key.clone());
                        started.push(key.clone());
                    }
                }
                Err(error) => {
                    for key in &started {
                        self.kad().stop_providing(key);
                    }
                    self.provided.retain(|key| !started.contains(key));
                    return Err(anyhow!("cannot provide a capability: {error:?}"));
                }
            }
        }
        Ok(queries)
    }

    /// A put of the announce in flight finished: once both did, provides
    /// the capabilities, or tries again the ones that reached no router.
    /// Past the deadline, it provides them all the same and fails: the node
    /// holds its new records and the library republishes them, so its
    /// provider records must match them.
    fn on_put(&mut self, id: QueryId, result: Result<kad::PutRecordOk, PutRecordError>) {
        let Some(announcing) = self.announcing.as_mut() else {
            return;
        };
        let Some(i) = announcing.puts.remove(&id) else {
            return;
        };
        let reached = match &result {
            Ok(_) => true,
            Err(
                PutRecordError::QuorumFailed { success, .. }
                | PutRecordError::Timeout { success, .. },
            ) => !success.is_empty(),
        };
        if let Err(error) = &result {
            tracing::debug!(reached, "put of the own record: {error}");
        }
        announcing.reached[i] |= reached;
        if !announcing.puts.is_empty() {
            return;
        }
        let all_reached = announcing.reached.iter().all(|r| *r);
        if !all_reached && Instant::now() < announcing.deadline {
            announcing.retry_at = Some(Instant::now() + RETRY_AFTER);
            return;
        }
        let keys = announcing.capabilities.clone();
        match self.provide(&keys) {
            Ok(providing) if all_reached => {
                let announcing = self.announcing.as_mut().expect("in progress");
                announcing.providing = providing;
                self.finish_announce_if_done();
            }
            Ok(_) => {
                let announcing = self.announcing.take().expect("in progress");
                let _ = announcing.reply.send(Err(anyhow!(
                    "the records reached no router in {} s; the node republishes them",
                    ANNOUNCE_DEADLINE.as_secs()
                )));
            }
            Err(error) => {
                let announcing = self.announcing.take().expect("in progress");
                let _ = announcing.reply.send(Err(error));
            }
        }
    }

    fn finish_announce_if_done(&mut self) {
        if let Some(announcing) = &self.announcing
            && announcing.puts.is_empty()
            && announcing.retry_at.is_none()
            && announcing.providing.is_empty()
            && announcing.reached.iter().all(|r| *r)
        {
            let announcing = self.announcing.take().expect("in progress");
            let _ = announcing.reply.send(Ok(()));
        }
    }

    /// Puts again, at their time, the records that reached no router.
    fn retry_puts(&mut self) {
        let Some(announcing) = self.announcing.as_mut() else {
            return;
        };
        if announcing.retry_at.is_none_or(|at| Instant::now() < at) {
            return;
        }
        announcing.retry_at = None;
        let pending: Vec<(usize, Record)> = announcing
            .records
            .iter()
            .enumerate()
            .filter(|(i, _)| !announcing.reached[*i])
            .map(|(i, r)| (i, r.clone()))
            .collect();
        for (i, record) in pending {
            match self.kad().put_record(record, Quorum::All) {
                Ok(id) => {
                    let announcing = self.announcing.as_mut().expect("in progress");
                    announcing.puts.insert(id, i);
                }
                Err(error) => {
                    let announcing = self.announcing.take().expect("in progress");
                    let _ = announcing
                        .reply
                        .send(Err(anyhow!("cannot store the own record: {error:?}")));
                    return;
                }
            }
        }
    }

    fn on_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) -> anyhow::Result<()> {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) => self.on_kad(event),
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                if info.protocols.contains(&KAD_PROTOCOL) {
                    for addr in info.listen_addrs {
                        if self.routable(&addr) {
                            self.kad().add_address(&peer_id, addr.clone());
                            let known = self.known_addrs.entry(peer_id).or_default();
                            if !known.contains(&addr) {
                                known.push(addr);
                            }
                        }
                    }
                }
            }
            // `libp2p-kad` drops every address of a peer a dial to it failed
            // at, but the last, even when the failure was for a while: a
            // router on two networks that stops a moment would be left, on
            // the nodes of one of them, with only its address on the other,
            // and stay out of their reach once back. So the node gives
            // Kademlia back what it knows of a DHT server after each failed
            // dial: a server that is down is dialed at all its addresses
            // again, and one that is back answers at the right one.
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer),
                ..
            } => {
                if let Some(addrs) = self.known_addrs.get(&peer).cloned() {
                    for addr in addrs {
                        self.kad().add_address(&peer, addr);
                    }
                }
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                if self.port.is_none() {
                    self.port = address.iter().find_map(|p| match p {
                        Protocol::Udp(port) => Some(port),
                        _ => None,
                    });
                    self.refresh_external_addrs();
                    if self.role == Role::Router {
                        let addrs: Vec<String> = self
                            .addrs_now()
                            .into_iter()
                            .map(|addr| addr.with(Protocol::P2p(self.local)).to_string())
                            .collect();
                        self.bootstrap_addr = Some(addrs.join(","));
                    }
                    tracing::info!(%address, bootstrap_addr = ?self.bootstrap_addr, "libp2p listening");
                }
            }
            SwarmEvent::ListenerClosed {
                reason: Err(error), ..
            } if self.port.is_none() => {
                return Err(anyhow!("libp2p cannot listen: {error}"));
            }
            SwarmEvent::ListenerError { error, .. } if self.port.is_none() => {
                return Err(anyhow!("libp2p cannot listen: {error}"));
            }
            _ => {}
        }
        Ok(())
    }

    /// Whether an address a peer listens at is one to dial: not
    /// unspecified, and loopback only when this node listens there.
    fn routable(&self, addr: &Multiaddr) -> bool {
        let ip = match addr.iter().next() {
            Some(Protocol::Ip4(ip)) => IpAddr::V4(ip),
            Some(Protocol::Ip6(ip)) => IpAddr::V6(ip),
            _ => return false,
        };
        !ip.is_unspecified() && (!ip.is_loopback() || self.listen.ip().is_loopback())
    }

    /// Every local address the endpoint is at now, `/ip4/<ip>/udp/<port>/quic-v1`
    /// (`spike_common::local_addrs`, read again on every call); none before
    /// it listens.
    fn addrs_now(&self) -> Vec<Multiaddr> {
        let Some(port) = self.port else {
            return Vec::new();
        };
        local_addrs(SocketAddr::new(self.listen.ip(), port))
            .into_iter()
            .map(quic_addr)
            .collect()
    }

    /// Makes the node's addresses now its external ones. On a private
    /// network every local address is reachable, and there is no AutoNAT
    /// to confirm one; `libp2p-kad` needs them, because a provider record
    /// carries its provider's external addresses, and a router leaves out
    /// of its answers one with none whose peer is not in its routing table,
    /// as no client-mode node is. A searcher never dials them: it reaches
    /// a provider through its contact.
    fn refresh_external_addrs(&mut self) {
        let now = self.addrs_now();
        let stale: Vec<Multiaddr> = self
            .swarm
            .external_addresses()
            .filter(|addr| !now.contains(addr))
            .cloned()
            .collect();
        for addr in stale {
            self.swarm.remove_external_address(&addr);
        }
        for addr in now {
            self.swarm.add_external_address(addr);
        }
    }

    fn on_kad(&mut self, event: kad::Event) {
        match event {
            kad::Event::InboundRequest {
                request:
                    InboundRequest::PutRecord {
                        source,
                        record: Some(record),
                        ..
                    },
            } => self.on_inbound_put(source, record),
            kad::Event::InboundRequest {
                request:
                    InboundRequest::AddProvider {
                        record: Some(record),
                    },
            } => self.on_inbound_provider(record),
            kad::Event::OutboundQueryProgressed {
                id,
                result,
                stats,
                step,
            } => self.on_query(id, result, stats, step.last),
            _ => {}
        }
    }

    /// A `PutRecord` to this router: stored only if valid.
    fn on_inbound_put(&mut self, source: PeerId, record: Record) {
        let now = now_unix();
        let store = self.swarm.behaviour_mut().kad.store_mut();
        let verdict = validate::check_put(&record.key, &record.value, |key| held(store, key), now);
        match verdict {
            Verdict::Store => {
                let key = record.key.clone();
                if let Err(error) = store.put(record) {
                    tracing::warn!(key = %key_text(&key), "record not stored: {error:?}");
                }
            }
            Verdict::Stale => {
                tracing::debug!(%source, key = %key_text(&record.key), "an older record, not stored");
            }
            Verdict::Reject(why) => {
                self.rejected += 1;
                tracing::info!(%source, key = %key_text(&record.key), why, "record rejected");
            }
        }
    }

    /// An `AddProvider` to this router: stored only if valid.
    fn on_inbound_provider(&mut self, record: ProviderRecord) {
        let now = now_unix();
        let store = self.swarm.behaviour_mut().kad.store_mut();
        let verdict =
            validate::check_provider(&record.key, &record.provider, |key| held(store, key), now);
        match verdict {
            Verdict::Store => {
                let (key, provider) = (record.key.clone(), record.provider);
                if let Err(error) = store.add_provider(record) {
                    tracing::warn!(%provider, key = %key_text(&key), "provider record not stored: {error:?}");
                    return;
                }
                self.provider_keys.insert(key);
            }
            Verdict::Stale => {}
            Verdict::Reject(why) => {
                self.rejected += 1;
                tracing::info!(provider = %record.provider, key = %key_text(&record.key), why, "provider record rejected");
            }
        }
    }

    fn on_query(&mut self, id: QueryId, result: QueryResult, stats: QueryStats, last: bool) {
        if let Some(searching) = self.searching.as_mut()
            && (searching.providers == Some(id) || searching.lookups.contains_key(&id))
        {
            searching.requests.insert(id, stats.num_requests());
        }
        match result {
            QueryResult::Bootstrap(result) => {
                if let Err(error) = result {
                    tracing::debug!("bootstrap: {error}");
                }
                if last && self.joining == Some(id) {
                    self.joining = None;
                }
            }
            QueryResult::PutRecord(result) => self.on_put(id, result),
            QueryResult::StartProviding(result) => {
                if let Err(error) = &result {
                    tracing::debug!("providing: {error}");
                }
                if let Some(announcing) = self.announcing.as_mut()
                    && last
                    && announcing.providing.remove(&id)
                {
                    self.finish_announce_if_done();
                }
            }
            QueryResult::GetProviders(result) => {
                match result {
                    Ok(GetProvidersOk::FoundProviders { providers, .. }) => {
                        self.on_providers(id, providers);
                    }
                    Ok(GetProvidersOk::FinishedWithNoAdditionalRecord { .. }) => {}
                    Err(error) => tracing::debug!(query = ?id, "providers: {error}"),
                }
                if last
                    && let Some(searching) = self.searching.as_mut()
                    && searching.providers == Some(id)
                {
                    searching.providers = None;
                    self.finish_search_if_done();
                }
            }
            QueryResult::GetRecord(result) => {
                match result {
                    Ok(GetRecordOk::FoundRecord(found)) => self.on_record(id, found.record),
                    Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. }) => {}
                    Err(error) => tracing::debug!(query = ?id, "record: {error}"),
                }
                if last
                    && let Some(searching) = self.searching.as_mut()
                    && searching.lookups.remove(&id).is_some()
                {
                    self.finish_search_if_done();
                }
            }
            QueryResult::RepublishRecord(Err(error)) => {
                tracing::debug!("republishing a record: {error}");
            }
            QueryResult::RepublishProvider(Err(error)) => {
                tracing::debug!("republishing a provider record: {error}");
            }
            _ => {}
        }
    }

    /// Providers a search found: each new one, up to its limit, gets the
    /// lookups of its contact and its announce.
    ///
    /// `libp2p-kad` also adds every provider to the providers query, as if
    /// it were one more DHT node to ask. No provider that is not a router
    /// can answer (it is a DHT client), and the search reaches it through
    /// its contact, never through libp2p; one on another network would hold
    /// the query until the transport's dial gives up
    /// ([`crate::transport::DIAL_TIMEOUT`],
    /// 5 s), because a datagram with no route is only a lost packet to
    /// QUIC. So the search keeps itself from dialing, until it ends, every
    /// provider not in its routing table ([`NoDial`]): the dial fails at
    /// once, and the query goes on without it.
    fn on_providers(&mut self, id: QueryId, providers: HashSet<PeerId>) {
        let local = self.local;
        let Some(searching) = self.searching.as_ref() else {
            return;
        };
        if searching.providers != Some(id) {
            return;
        }
        let routers: HashSet<PeerId> = self
            .kad()
            .kbuckets()
            .flat_map(|bucket| {
                bucket
                    .iter()
                    .map(|entry| *entry.node.key.preimage())
                    .collect::<Vec<_>>()
            })
            .collect();
        for peer in &providers {
            if *peer != local
                && !routers.contains(peer)
                && self.swarm.behaviour_mut().undialed.deny(*peer)
            {
                let searching = self.searching.as_mut().expect("in progress");
                searching.undialed.push(*peer);
            }
        }
        let searching = self.searching.as_mut().expect("in progress");
        let mut new = Vec::new();
        for peer in providers {
            if peer == local
                || searching.found.iter().any(|f| f.peer == peer)
                || searching.found.len() + new.len() >= searching.limit
            {
                continue;
            }
            new.push(peer);
        }
        let full = searching.found.len() + new.len() >= searching.limit;
        for peer in new {
            let contact = self.kad().get_record(validate::contact_key(&peer));
            let announce = self.kad().get_record(validate::announce_key(&peer));
            let searching = self.searching.as_mut().expect("in progress");
            let i = searching.found.len();
            searching.found.push(Found {
                peer,
                contacts: Vec::new(),
                announces: Vec::new(),
                raw_announce: None,
                found_ms: None,
            });
            searching.lookups.insert(contact, (i, Slot::Contact(peer)));
            searching
                .lookups
                .insert(announce, (i, Slot::Announce(peer)));
        }
        if full && let Some(mut query) = self.kad().query_mut(&id) {
            query.finish();
        }
    }

    /// A copy of a contact or an announce a search looked up.
    fn on_record(&mut self, id: QueryId, record: Record) {
        let now = now_unix();
        let Some(searching) = self.searching.as_mut() else {
            return;
        };
        let Some(&(i, slot)) = searching.lookups.get(&id) else {
            return;
        };
        let elapsed = millis(searching.started);
        let found = &mut searching.found[i];
        match slot {
            Slot::Contact(peer) => match validate::contact_for(&peer, &record.value, now) {
                Ok(contact) => found.contacts.push(contact),
                Err(why) => tracing::debug!(%peer, why, "a contact that does not verify"),
            },
            Slot::Announce(peer) => match validate::announce_for(&peer, &record.value, now) {
                Ok(announce) => found.announces.push(announce),
                Err(why) => {
                    tracing::debug!(%peer, why, "an announce that does not verify");
                    found.raw_announce.get_or_insert(record.value);
                }
            },
        }
        if found.found_ms.is_none()
            && found.contacts.iter().any(|c| {
                found
                    .announces
                    .iter()
                    .any(|a| a.doc.agent_id == c.doc.agent_id)
            })
        {
            found.found_ms = Some(elapsed);
        }
    }

    fn finish_search_if_done(&mut self) {
        if let Some(searching) = &self.searching
            && searching.providers.is_none()
            && searching.lookups.is_empty()
        {
            self.finish_search();
        }
    }

    /// Answers the search with what it has, and stops its queries.
    fn finish_search(&mut self) {
        let Some(searching) = self.searching.take() else {
            return;
        };
        let queries = searching
            .providers
            .into_iter()
            .chain(searching.lookups.keys().copied());
        for id in queries.collect::<Vec<_>>() {
            if let Some(mut query) = self.kad().query_mut(&id) {
                query.finish();
            }
        }
        let undialed = &mut self.swarm.behaviour_mut().undialed;
        for peer in &searching.undialed {
            undialed.allow(peer);
        }
        // `QueryStats` counts a request when the query picks a peer, before
        // any dial: the dials to the providers that `NoDial` denied never
        // reached the network.
        let denied = undialed.take_denied();
        let requests = searching
            .requests
            .values()
            .map(|n| u64::from(*n))
            .sum::<u64>()
            .saturating_sub(denied);
        let candidates = searching.found.into_iter().map(Found::into_raw).collect();
        let _ = searching.reply.send(Ok(RawSearch {
            candidates,
            requests: Some(requests),
        }));
    }

    fn on_tick(&mut self) {
        self.sweep();
        if self
            .searching
            .as_ref()
            .is_some_and(|s| s.started.elapsed() >= SEARCH_DEADLINE)
        {
            tracing::warn!("a search reached its deadline: answering with what it has");
            self.finish_search();
        }
        if self
            .announcing
            .as_ref()
            .is_some_and(|a| Instant::now() >= a.deadline)
        {
            self.end_announce();
        }
        self.retry_puts();
        self.publish_stats();
    }

    /// Answers the announce at its deadline, whatever still runs, so that
    /// the control API, which waits 30 s, gets its answer. If every record
    /// reached a router, only the providing can be running, and the provider
    /// records are the node's own and republished anyway: done. If one did
    /// not, the node provides the capabilities all the same, as
    /// [`Task::on_put`] does past the deadline, and fails; a put still in
    /// flight goes on and may yet land. (kad-libp2p answered here only once
    /// no put was in flight, which a put over a dead path can hold past the
    /// control API's time: scenario S7 over Iroh.)
    fn end_announce(&mut self) {
        let Some(announcing) = self.announcing.take() else {
            return;
        };
        let result = if announcing.reached.iter().all(|r| *r) {
            tracing::info!("the announce's providing is still running at its deadline: done");
            Ok(())
        } else {
            self.provide(&announcing.capabilities).and_then(|_| {
                Err(anyhow!(
                    "the records reached no router in {} s; the node republishes them",
                    ANNOUNCE_DEADLINE.as_secs()
                ))
            })
        };
        let _ = announcing.reply.send(result);
    }

    /// Removes from the store what `libp2p-kad` marked expired.
    fn sweep(&mut self) {
        let now = Instant::now();
        let store = self.swarm.behaviour_mut().kad.store_mut();
        let expired: Vec<RecordKey> = store
            .records()
            .filter(|r| r.is_expired(now))
            .map(|r| r.key.clone())
            .collect();
        for key in &expired {
            store.remove(key);
        }
        self.provider_keys.retain(|key| {
            for provider in store.providers(key) {
                if provider.is_expired(now) {
                    store.remove_provider(key, &provider.provider);
                }
            }
            !store.providers(key).is_empty()
        });
    }

    /// Computes the counters and leaves them for [`Handle::stats`].
    fn publish_stats(&mut self) {
        let local = self.local;
        let connections = u64::from(
            self.swarm
                .network_info()
                .connection_counters()
                .num_established(),
        );
        let kad = &mut self.swarm.behaviour_mut().kad;
        let routing_peers = kad.kbuckets().map(|b| b.num_entries() as u64).sum();
        let store = kad.store_mut();
        let mut by_agent: BTreeMap<AgentId, StoredRecords> = BTreeMap::new();
        let mut records_stored = 0;
        for record in store.records() {
            if record.publisher == Some(local) {
                continue;
            }
            records_stored += 1;
            // The router stored it only under its signer's `PeerId`, which
            // is the agent ([`validate::check_put`]).
            let slot = validate::slot(&record.key);
            if let Some(agent) = slot.and_then(|slot| validate::agent_of(&slot.peer())) {
                let entry = by_agent.entry(agent).or_default();
                match slot {
                    Some(Slot::Contact(_)) => entry.contact_records += 1,
                    Some(Slot::Announce(_)) => entry.announce_records += 1,
                    None => entry.other_records += 1,
                }
            }
        }
        let mut provider_records_stored = 0;
        for key in &self.provider_keys {
            for provider in store.providers(key) {
                if provider.provider == local {
                    continue;
                }
                provider_records_stored += 1;
                if let Some(agent) = validate::agent_of(&provider.provider) {
                    by_agent.entry(agent).or_default().provider_records += 1;
                }
            }
        }
        let stats = BackendStats {
            connections,
            routing_peers,
            records_stored,
            provider_records_stored,
            records_rejected: self.rejected,
            bootstrap_addr: self.bootstrap_addr.clone(),
            stored_by_agent: by_agent,
        };
        *self.stats.lock().unwrap_or_else(|e| e.into_inner()) = stats;
    }
}

impl Found {
    /// The newest valid contact, and the newest valid announce of its agent
    /// (of any agent, or the first copy, when there is none).
    fn into_raw(self) -> RawCandidate {
        let contact = self.contacts.into_iter().max_by_key(|c| c.doc.seq);
        let agent = contact.as_ref().map(|c| c.doc.agent_id);
        let (same, others): (Vec<_>, Vec<_>) = self
            .announces
            .into_iter()
            .partition(|a| Some(a.doc.agent_id) == agent);
        let announce = if same.is_empty() { others } else { same }
            .into_iter()
            .max_by_key(|a| a.doc.seq)
            .map(|a| a.to_json())
            .or(self.raw_announce);
        RawCandidate {
            contact: contact.map(|c| c.to_json()),
            announce,
            found_ms: self.found_ms,
        }
    }
}

/// What `store` holds under `key`, unless `libp2p-kad` marked it expired.
fn held(store: &MemoryStore, key: &RecordKey) -> Option<Vec<u8>> {
    store
        .get(key)
        .filter(|record| !record.is_expired(Instant::now()))
        .map(|record| record.value.clone())
}

/// A key for the logs: the text of a value's key, the hex of a provider
/// key.
fn key_text(key: &RecordKey) -> String {
    match std::str::from_utf8(key.as_ref()) {
        Ok(text) if text.starts_with('/') => text.to_owned(),
        _ => key.as_ref().iter().map(|b| format!("{b:02x}")).collect(),
    }
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The dial concurrency factor that `SPIKE_DIAL_CONCURRENCY`'s value asks
/// for, 1 to 255; `None` when the variable is not set, which keeps
/// libp2p's default.
fn dial_concurrency(value: Option<&str>) -> anyhow::Result<Option<NonZeroU8>> {
    value
        .map(|text| {
            text.parse::<NonZeroU8>()
                .with_context(|| format!("{DIAL_CONCURRENCY_VAR} must be 1 to 255, not {text:?}"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dial_concurrency_is_libp2ps_unless_set() {
        assert_eq!(dial_concurrency(None).unwrap(), None);
        assert_eq!(dial_concurrency(Some("1")).unwrap(), NonZeroU8::new(1));
        assert!(dial_concurrency(Some("0")).is_err());
        assert!(dial_concurrency(Some("eight")).is_err());
    }
}
