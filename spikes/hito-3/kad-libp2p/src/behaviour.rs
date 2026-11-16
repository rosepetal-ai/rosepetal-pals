//! The node's libp2p side: a swarm of `libp2p-kad` and `identify` over
//! libp2p's QUIC, driven by a task of its own that the backend talks to
//! through a [`Handle`].
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
//! [`SWEEP_EVERY`] of what the library itself marked expired, so that what
//! a router counts is what it would still serve.
//!
//! Every `PutRecord` and `AddProvider` a router receives comes to the task
//! (`StoreInserts::FilterBoth`), which stores it only if
//! [`crate::validate`] says so; whatever it refuses adds to
//! `records_rejected`.
//!
//! The node bootstraps only from `--bootstrap`: no IPFS nodes, no mDNS, no
//! relay, AutoNAT, UPnP or DCUtR. Kademlia learns a peer's addresses from
//! `--bootstrap`, from `identify` (only for peers that speak
//! [`KAD_PROTOCOL`] as servers, and only the addresses they declare
//! external) and from other servers' answers, so a node with no bootstrap
//! knows no one. After a failed dial to a server, the task gives Kademlia
//! back the addresses it knew of it, which `libp2p-kad` would drop.
//!
//! A search ([`Handle::search`]) asks for the capability's providers and,
//! as each `PeerId` arrives, up to `limit`, for its link and its announce.
//! It verifies each copy of a link and keeps the newest valid one, and
//! passes on the contact it carries and the announce of the same agent,
//! for `spike_common::verify` to check again. It never dials the providers
//! it finds ([`Task::on_providers`] says why). A candidate's `found_ms` is
//! when the search first had a valid link and a valid announce of the same
//! agent; `requests` sums `QueryStats::num_requests` of every query of the
//! search.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail, ensure};
use libp2p::futures::StreamExt;
use libp2p::identity::Keypair;
use libp2p::kad::store::{MemoryStore, MemoryStoreConfig, RecordStore};
use libp2p::kad::{
    self, Caching, GetProvidersOk, GetRecordOk, InboundRequest, Mode, ProviderRecord,
    PutRecordError, QueryId, QueryResult, QueryStats, Quorum, Record, RecordKey, StoreInserts,
};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, allow_block_list, identify};
use pals_core::identity::AgentId;
use pals_core::records::AnnounceRecord;
use pals_core::signed::Signed;
use spike_common::{BackendStats, RawCandidate, RawSearch, Role, StoredRecords, local_addrs};
use tokio::sync::{mpsc, oneshot};

use crate::link::Link;
use crate::validate::{self, Slot, Verdict};

/// The DHT's own Kademlia protocol (spec §6.2), never IPFS's.
pub const KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/agentnet/kad/1.0.0");
/// What `identify` says the node speaks.
const IDENTIFY_VERSION: &str = "agentnet/0";
/// The store's limits, per node:
/// - at most 1024 values, two per agent (link and announce);
/// - 8 KiB a value, above both a link's limit
///   ([`crate::link::MAX_LINK_BYTES`], 6 KiB) and an announce's (4 KiB);
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

/// Kademlia and `identify`, and the peers a search does not dial.
#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub kad: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub undialed: allow_block_list::Behaviour<allow_block_list::BlockedPeers>,
}

/// What the libp2p side starts with.
#[derive(Debug, Clone)]
pub struct Settings {
    pub role: Role,
    /// Where libp2p's QUIC listens.
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
}

enum Command {
    Announce {
        link: Vec<u8>,
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
    /// Publishes `link` and `announce` under the node's `PeerId`, then
    /// provides each capability, replacing an earlier announcement; returns
    /// once each record reached at least one router.
    pub async fn announce(
        &self,
        link: Vec<u8>,
        announce: Vec<u8>,
        capabilities: Vec<String>,
    ) -> anyhow::Result<()> {
        self.ask(|reply| Command::Announce {
            link,
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

    /// The counters as the task last left them, at most [`SWEEP_EVERY`]
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

/// Starts the libp2p side: QUIC at `settings.listen` with `keypair`,
/// Kademlia in the role's mode, joined through `settings.bootstrap`.
/// Returns once it listens and its first bootstrap finished, or
/// [`JOIN_TIMEOUT`] passed.
pub async fn spawn(settings: Settings, keypair: Keypair) -> anyhow::Result<Handle> {
    let bootstrap = settings
        .bootstrap
        .iter()
        .map(|text| parse_bootstrap(text))
        .collect::<anyhow::Result<Vec<_>>>()?
        .concat();
    let local = keypair.public().to_peer_id();
    let config = kad_config(settings.ttl, settings.republish);
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic()
        .with_behaviour(|key| Behaviour {
            kad: kad::Behaviour::with_config(
                local,
                MemoryStore::with_config(local, STORE_LIMITS),
                config,
            ),
            // A node tells others only its external addresses, its local
            // ones but loopback ([`Task::refresh_external_addrs`]): every
            // node listens at the same port, so a peer's `127.0.0.1` is the
            // dialer itself. And `identify` keeps no cache of what peers
            // say, which it would add to every dial of them: a dial whose
            // loopback address answers first fails whole (`WrongPeerId`).
            // Kademlia gets what `identify` learns through
            // [`Task::on_swarm_event`] only.
            identify: identify::Behaviour::new(
                identify::Config::new(IDENTIFY_VERSION.to_owned(), key.public())
                    .with_hide_listen_addrs(true)
                    .with_cache_size(0),
            ),
            undialed: allow_block_list::Behaviour::default(),
        })?
        .build();
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
    tracing::info!(peer_id = %local, ?mode, listen = %settings.listen, bootstrap = bootstrap.len(), "libp2p started");

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
        stored_agent: HashMap::new(),
        bindings: HashMap::new(),
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
    Ok(Handle { commands, stats })
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

/// `/ip4/<ip>/udp/<port>/quic-v1`, or `/ip6/…`.
fn quic_addr(addr: SocketAddr) -> Multiaddr {
    let ip = match addr.ip() {
        IpAddr::V4(ip) => Protocol::Ip4(ip),
        IpAddr::V6(ip) => Protocol::Ip6(ip),
    };
    Multiaddr::empty()
        .with(ip)
        .with(Protocol::Udp(addr.port()))
        .with(Protocol::QuicV1)
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
    /// The agent each value stored for others is about, by key.
    stored_agent: HashMap<RecordKey, AgentId>,
    /// The agent the link last stored for a `PeerId` binds it to, kept
    /// while the node stores anything of that `PeerId`, so that its
    /// provider records stay attributed after its link expires.
    bindings: HashMap<PeerId, AgentId>,
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

/// An announce in progress: the puts of the link and the announce, then
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
    links: Vec<Link>,
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
                link,
                announce,
                capabilities,
                reply,
            } => {
                // Where the node is now goes in its provider records
                // (scenario S7).
                self.refresh_external_addrs();
                self.announce(link, announce, capabilities, reply)
            }
            Command::Unannounce { reply } => {
                let local = self.local;
                self.kad().remove_record(&validate::link_key(&local));
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
        link: Vec<u8>,
        announce: Vec<u8>,
        capabilities: Vec<String>,
        reply: oneshot::Sender<anyhow::Result<()>>,
    ) {
        let local = self.local;
        let capabilities: Vec<RecordKey> = capabilities
            .iter()
            .map(|c| validate::provider_key(c))
            .collect();
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
        let records = vec![
            Record::new(validate::link_key(&local), link),
            Record::new(validate::announce_key(&local), announce),
        ];
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

    /// A put of the announce in flight finished: once both did, provides
    /// the capabilities, or tries again the ones that reached no router.
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
        if announcing.reached.iter().all(|r| *r) {
            let keys = announcing.capabilities.clone();
            let mut providing = HashSet::new();
            for key in keys {
                match self.kad().start_providing(key.clone()) {
                    Ok(id) => {
                        providing.insert(id);
                        if !self.provided.contains(&key) {
                            self.provided.push(key);
                        }
                    }
                    Err(error) => {
                        let announcing = self.announcing.take().expect("in progress");
                        let _ = announcing
                            .reply
                            .send(Err(anyhow!("cannot provide a capability: {error:?}")));
                        return;
                    }
                }
            }
            let announcing = self.announcing.as_mut().expect("in progress");
            announcing.providing = providing;
            self.finish_announce_if_done();
        } else if Instant::now() < announcing.deadline {
            announcing.retry_at = Some(Instant::now() + RETRY_AFTER);
        } else {
            let announcing = self.announcing.take().expect("in progress");
            let _ = announcing.reply.send(Err(anyhow!(
                "the records reached no router in {} s",
                ANNOUNCE_DEADLINE.as_secs()
            )));
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

    /// Every local address libp2p listens at now, `/ip4/<ip>/udp/<port>/quic-v1`
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
    /// a provider through the contact of its link.
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
            Verdict::Store(agent) => {
                let key = record.key.clone();
                if let Err(error) = store.put(record) {
                    tracing::warn!(key = %key_text(&key), "record not stored: {error:?}");
                    return;
                }
                if let Some(agent) = agent {
                    self.stored_agent.insert(key.clone(), agent);
                    if let Some(Slot::Link(peer)) = validate::slot(&key) {
                        self.bindings.insert(peer, agent);
                    }
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
            Verdict::Store(_) => {
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
            QueryResult::Bootstrap(_) => {
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
                if let Ok(GetProvidersOk::FoundProviders { providers, .. }) = result {
                    self.on_providers(id, providers);
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
                if let Ok(GetRecordOk::FoundRecord(found)) = result {
                    self.on_record(id, found.record);
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
    /// lookups of its link and its announce.
    ///
    /// `libp2p-kad` also adds every provider to the providers query, as if
    /// it were one more DHT node to ask. No provider that is not a router
    /// can answer (it is a DHT client), and the search reaches it through
    /// the contact of its link, never through libp2p; one on another
    /// network would hold the query until libp2p's QUIC handshake gives up
    /// (5 s), because a datagram with no route is only a lost packet to
    /// QUIC. So the search keeps itself from dialing, until it ends, every
    /// provider not in its routing table: the dial fails at once, and the
    /// query goes on without it.
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
                && self.swarm.behaviour_mut().undialed.block_peer(*peer)
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
            let link = self.kad().get_record(validate::link_key(&peer));
            let announce = self.kad().get_record(validate::announce_key(&peer));
            let searching = self.searching.as_mut().expect("in progress");
            let i = searching.found.len();
            searching.found.push(Found {
                peer,
                links: Vec::new(),
                announces: Vec::new(),
                raw_announce: None,
                found_ms: None,
            });
            searching.lookups.insert(link, (i, Slot::Link(peer)));
            searching
                .lookups
                .insert(announce, (i, Slot::Announce(peer)));
        }
        if full && let Some(mut query) = self.kad().query_mut(&id) {
            query.finish();
        }
    }

    /// A copy of a link or an announce a search looked up.
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
            Slot::Link(peer) => match validate::link_for(&peer, &record.value, now) {
                Ok(link) => found.links.push(link),
                Err(why) => tracing::debug!(%peer, why, "a link that does not verify"),
            },
            Slot::Announce(peer) => match validate::announce_of(&record.value, now) {
                Ok(announce) => found.announces.push(announce),
                Err(why) => {
                    tracing::debug!(%peer, why, "an announce that does not verify");
                    found.raw_announce.get_or_insert(record.value);
                }
            },
        }
        if found.found_ms.is_none()
            && found
                .links
                .iter()
                .any(|l| found.announces.iter().any(|a| a.doc.agent_id == l.agent_id))
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
        for peer in &searching.undialed {
            self.swarm.behaviour_mut().undialed.unblock_peer(*peer);
        }
        let requests = searching.requests.values().map(|n| u64::from(*n)).sum();
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
        self.retry_puts();
        if self.announcing.as_ref().is_some_and(|a| {
            a.puts.is_empty() && a.reached.iter().all(|r| *r) && Instant::now() >= a.deadline
        }) {
            // The records are at the routers and the providing goes on: its
            // provider records are the node's own and republished anyway.
            tracing::info!("the announce's providing is still running at its deadline: done");
            let announcing = self.announcing.take().expect("in progress");
            let _ = announcing.reply.send(Ok(()));
        }
        self.publish_stats();
    }

    /// Removes from the store what `libp2p-kad` marked expired, and forgets
    /// what it knew about records no longer there.
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
        self.stored_agent.retain(|key, _| store.get(key).is_some());
        let mut peers: HashSet<PeerId> = store
            .records()
            .filter_map(|r| match validate::slot(&r.key) {
                Some(Slot::Link(peer) | Slot::Announce(peer)) => Some(peer),
                None => None,
            })
            .collect();
        for key in &self.provider_keys {
            peers.extend(store.providers(key).into_iter().map(|p| p.provider));
        }
        self.bindings.retain(|peer, _| peers.contains(peer));
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
            if let Some(agent) = self.stored_agent.get(&record.key) {
                let entry = by_agent.entry(*agent).or_default();
                match validate::slot(&record.key) {
                    // The link carries the contact.
                    Some(Slot::Link(_)) => entry.contact_records += 1,
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
                if let Some(agent) = self.bindings.get(&provider.provider) {
                    by_agent.entry(*agent).or_default().provider_records += 1;
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
    /// The newest valid link's contact, and the newest valid announce of
    /// its agent (of any agent, or the first copy, when there is none).
    fn into_raw(self) -> RawCandidate {
        let link = self.links.into_iter().max_by_key(|l| l.contact.doc.seq);
        let agent = link.as_ref().map(|l| l.agent_id);
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
            contact: link.map(|l| l.contact.to_json()),
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
