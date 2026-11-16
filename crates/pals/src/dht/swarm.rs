//! The node's swarm: `libp2p-kad` over [`IrohTransport`], driven by a task
//! of its own that a [`Dht`](super::Dht) talks to
//! (`docs/protocols/agentnet-kad-0.md`, section 7).
//!
//! Adapted from the phase 1 spike's `kad-iroh` backend
//! (`spikes/hito-3/kad-iroh/src/behaviour.rs`), without its instrumentation:
//! no sweep of the store and no counters refreshed every second; [`Task`]
//! computes the status when asked, and counts only what has not expired.
//!
//! - **The swarm.** `Swarm::new` with the agent's `PeerId` and no keypair:
//!   libp2p never gets the secret key. One dial at a time per peer
//!   (`with_dial_concurrency_factor(1)`): a router with two addresses is
//!   one connection, which the transport dials at all of them. A
//!   connection with no stream closes after `idle_timeout`, and nothing
//!   keeps one open: a client spends no traffic at rest, and its first
//!   lookup after a rest pays an Iroh handshake with each router.
//! - **The behaviours.** `connection_limits` ([`MAX_CONNECTIONS_PER_PEER`]
//!   established per peer, [`MAX_INCOMING_CONNECTIONS`] incoming in all),
//!   `libp2p-kad` on [`KAD_PROTOCOL`], `identify`, and [`NoDial`].
//! - **`identify`, and why.** `libp2p-kad` puts a peer in the routing table
//!   only with the address it dialed it at: of a peer that dialed in, it
//!   knows no address, and reports it unroutable. Without `identify`, a
//!   router with no bootstrap never had another in its table: the test
//!   `a_router_with_no_bootstrap_learns_the_others` failed with R1 having
//!   accepted the connections of R2 and R3 and 0 routing peers, and so did
//!   every test whose routers learn each other through R1
//!   (`clients_never_enter_a_routing_table` among them). With `identify`,
//!   each side tells the other the protocols it serves and the addresses
//!   it declares; a node gives Kademlia, and the address book, the
//!   addresses in the router form of a peer that serves [`KAD_PROTOCOL`],
//!   which only a router in server mode does, so a client never enters a
//!   routing table. `identify` sends only the declared addresses
//!   (`with_hide_listen_addrs`), never the endpoint's sockets, keeps no
//!   cache of what peers say (`with_cache_size(0)`), which it would add to
//!   every dial of them, and gets only the public key, so it signs no peer
//!   record.
//! - **Kademlia.** Records and provider records live `record_ttl`; the
//!   node publishes nothing of its own on `libp2p-kad`'s timers (the rounds
//!   of task 12 do), and a router replicates what it holds every
//!   `republish`. Every inbound record is filtered (`FilterBoth`), nothing
//!   is cached, and a query lasts `query_timeout` at most. The mode is
//!   always set, never automatic (H3-D3): server on a router, client on a
//!   client, which `libp2p-kad` would otherwise turn into a server once it
//!   declares an address.
//! - **What arrives.** Every `PutRecord` and `AddProvider` goes through
//!   [`validate::check_put`] or [`validate::check_provider`] at the node's
//!   clock: what passes goes to the store, and one the store has no room
//!   for counts as [`Rejection::StoreFull`]. Each refusal is counted by its
//!   name and logged at `debug` with who sent it. When a router stores a
//!   newer announce of an agent, it drops that agent's provider records of
//!   the capabilities the announce no longer lists: an agent has at most
//!   [`MAX_ANNOUNCED_CAPABILITIES`] provider records at a router at any
//!   time.
//! - **Entry.** Each `[dht] bootstrap` ticket gives its direct IP
//!   addresses, which go to Kademlia and to the [`AddressBook`], and the
//!   node bootstraps. If no router answered by the end of it, the node
//!   dials its saved contacts that have a ticket, the first
//!   [`ENTRY_CONTACTS`] by name, and tells neither Kademlia nor the book of
//!   them: most are agents that are no router. One enters the routing table
//!   only once it shows it serves [`KAD_PROTOCOL`], through `identify`, and
//!   the node bootstraps through the first that does. A node with no
//!   bootstrap starts alone.
//! - **What the node remembers of a router.** The [`AddressBook`] holds the
//!   addresses of the bootstrap tickets and of the peers in the routing
//!   table, and of nothing else. `libp2p-kad` drops a peer's addresses after
//!   a failed dial to it, but the last; the node gives it back every one the
//!   book has of that server, so that a router that stopped a moment is
//!   dialed at all of them again, and a peer that never was a router gets
//!   nothing back.
//! - **What a node declares.** `libp2p-kad` returns no provider record
//!   without addresses, so every node declares one: a router its
//!   `[dht] external_addrs` or, with none, its endpoint's direct addresses,
//!   never an unspecified IP ([`router_addrs`]), in the router form; a
//!   client only [`located_nowhere`], which is no location.
//!
//! [`MAX_ANNOUNCED_CAPABILITIES`]: pals_core::records::MAX_ANNOUNCED_CAPABILITIES

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::num::NonZeroU8;
use std::sync::Arc;
use std::task::Poll;
use std::time::Instant;

use anyhow::{Context, ensure};
use iroh::{Endpoint, Watcher};
use iroh_tickets::endpoint::EndpointTicket;
use libp2p::core::transport::PortUse;
use libp2p::core::{ConnectedPoint, Endpoint as Role, Transport};
use libp2p::futures::stream::{self, BoxStream};
use libp2p::futures::{StreamExt, future};
use libp2p::kad::store::{MemoryStore, MemoryStoreConfig, RecordStore};
use libp2p::kad::{
    self, Caching, GetProvidersOk, InboundRequest, Mode, ProviderRecord, QueryId, QueryResult,
    QueryStats, Record, RecordKey, StoreInserts,
};
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, SwarmEvent, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, connection_limits, identify, swarm};
use pals_core::clock::Clock;
use pals_core::identity::AgentId;
use pals_core::store::Store;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::keys::{Slot, agent_of, announce_key, peer_id, provider_key, public_key, slot};
use super::transport::{
    AddressBook, IrohListener, IrohTransport, Refusals, dial_target, located_nowhere, multiaddr_of,
    quic_addr,
};
use super::validate::{self, Rejection, Verdict};
use super::{DhtParts, DhtStatus, DhtTiming};
use crate::config::DhtMode;
use crate::net::agent_id_of;
use crate::node::dialable_addr;

/// The DHT's own Kademlia protocol (design §6.2), never IPFS's: frozen by
/// `kad-v0.json`.
pub const KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/agentnet/kad/1.0.0");
/// What a router stores, at most:
/// - 1024 values, two per agent (its contact and its announce);
/// - 8 KiB a value, above a contact's or an announce's 4 KiB
///   ([`MAX_RECORD_BYTES`](pals_core::records::MAX_RECORD_BYTES));
/// - `K` (20) providers per key; further ones are ignored, as
///   `libp2p-kad` does against floods;
/// - 1024 keys with providers, the node's own among them.
pub const STORE_LIMITS: MemoryStoreConfig = MemoryStoreConfig {
    max_records: 1024,
    max_value_bytes: 8 * 1024,
    max_providers_per_key: 20,
    max_provided_keys: 1024,
};
/// The connections of `agentnet-kad/0` established with one peer at a
/// time, both ways; one more is closed.
pub const MAX_CONNECTIONS_PER_PEER: u32 = 2;
/// The incoming connections of `agentnet-kad/0` established at a time; one
/// more is closed.
pub const MAX_INCOMING_CONNECTIONS: u32 = 256;
/// The saved contacts a node tries to enter through, when no bootstrap
/// router answered.
pub const ENTRY_CONTACTS: usize = 8;
/// Commands waiting for the task.
const COMMANDS_QUEUED: usize = 16;

/// Routers to enter the network through: each one's agent and direct
/// addresses.
type Routers = Vec<(AgentId, Vec<SocketAddr>)>;
/// The protocol family `identify` says the node speaks.
const IDENTIFY_VERSION: &str = "agentnet/0";

/// What a node's swarm runs: its connection limits, Kademlia, `identify`,
/// and the peers its lookups do not dial.
#[derive(swarm::NetworkBehaviour)]
pub(crate) struct Behaviour {
    limits: connection_limits::Behaviour,
    kad: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    undialed: NoDial,
}

/// The peers the node's lookups keep themselves from dialing.
///
/// `libp2p-kad` adds each provider a `get_providers` query finds to the
/// query, as one more node to ask. A provider that is a client is no DHT
/// node and declares only `/p2p/<peer_id>`, which is never dialable; the
/// node reaches a provider through its contact, never through libp2p. So,
/// for as long as the query runs, each provider it found that is not in the
/// routing table is held here ([`Task`] does it as the providers arrive),
/// and a dial to it is refused at once, before it starts: the query counts
/// it as failed and goes on. Each refusal is counted, so that a lookup's
/// requests leave it out. Unlike `allow_block_list`, it never closes a
/// connection, and it lets the peer dial in.
#[derive(Debug, Default)]
pub(crate) struct NoDial {
    /// Each peer held, with how many queries hold it.
    held: HashMap<PeerId, usize>,
    /// The dials refused to each peer held, since it was first held.
    denied: HashMap<PeerId, u64>,
}

impl NoDial {
    /// Refuses the dials to `peer` until as many [`NoDial::release`].
    fn hold(&mut self, peer: PeerId) {
        *self.held.entry(peer).or_default() += 1;
    }

    /// One query no longer holds `peer`.
    fn release(&mut self, peer: &PeerId) {
        if let Some(count) = self.held.get_mut(peer) {
            *count -= 1;
            if *count == 0 {
                self.held.remove(peer);
                self.denied.remove(peer);
            }
        }
    }

    /// The dials refused to `peer` since it was first held.
    fn denied(&self, peer: &PeerId) -> u64 {
        self.denied.get(peer).copied().unwrap_or(0)
    }
}

/// Why [`NoDial`] refused a dial.
#[derive(Debug)]
struct NotDialed;

impl std::fmt::Display for NotDialed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a lookup does not dial the providers it finds")
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
        _: Role,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(peer) = peer
            && self.held.contains_key(&peer)
        {
            *self.denied.entry(peer).or_default() += 1;
            return Err(ConnectionDenied::new(NotDialed));
        }
        Ok(Vec::new())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Role,
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

/// What a [`Dht`](super::Dht) asks its task.
pub(crate) enum Command {
    Status(oneshot::Sender<DhtStatus>),
    HeldProviders(RecordKey, oneshot::Sender<Vec<(AgentId, Vec<Multiaddr>)>>),
}

/// The Kademlia configuration of every node, with `timing`.
fn kad_config(timing: &DhtTiming) -> kad::Config {
    let mut config = kad::Config::new(KAD_PROTOCOL);
    config
        .set_record_ttl(Some(timing.record_ttl))
        .set_provider_record_ttl(Some(timing.record_ttl))
        .set_publication_interval(None)
        .set_provider_publication_interval(None)
        .set_replication_interval(Some(timing.republish))
        .set_record_filtering(StoreInserts::FilterBoth)
        .set_caching(Caching::Disabled)
        .set_query_timeout(timing.query_timeout);
    config
}

/// What a router declares: `external` when it has some, else its `direct`
/// addresses; never an unspecified IP nor port 0.
pub(crate) fn router_addrs(
    external: &[SocketAddr],
    direct: impl IntoIterator<Item = SocketAddr>,
) -> Vec<SocketAddr> {
    let dialable = |addr: &SocketAddr| !addr.ip().is_unspecified() && addr.port() != 0;
    let mut addrs: Vec<SocketAddr> = if external.is_empty() {
        direct.into_iter().filter(dialable).collect()
    } else {
        external.iter().copied().filter(dialable).collect()
    };
    addrs.dedup();
    addrs
}

/// Starts the swarm `parts` describe on a task of its own: the commands it
/// takes, the task, and the handler of [`ALPN_KAD`](super::transport::ALPN_KAD)
/// the node's router must serve.
pub(crate) fn spawn(
    parts: DhtParts,
) -> anyhow::Result<(mpsc::Sender<Command>, JoinHandle<()>, IrohListener)> {
    let (task, listener) = Task::new(parts)?;
    let (commands, queue) = mpsc::channel(COMMANDS_QUEUED);
    let task = tokio::spawn(task.run(queue));
    Ok((commands, task, listener))
}

/// How the node enters the network.
#[derive(Debug, Default)]
struct Entry {
    /// The bootstrap that enters, while it runs.
    query: Option<QueryId>,
    /// Whether a router answered it.
    answered: bool,
    /// Whether the saved contacts are still to be tried.
    contacts_left: bool,
    /// The saved contacts dialed, none of them a router yet.
    trying: HashSet<PeerId>,
}

/// The task that owns the swarm.
pub(crate) struct Task {
    swarm: Swarm<Behaviour>,
    local: PeerId,
    own: AgentId,
    mode: DhtMode,
    endpoint: Endpoint,
    /// A router's `[dht] external_addrs`.
    external: Vec<SocketAddr>,
    clock: Arc<dyn Clock>,
    store: Arc<Store>,
    book: AddressBook,
    refusals: Refusals,
    store_limits: MemoryStoreConfig,
    /// Whether the endpoint is bound to loopback: only then is a peer's
    /// loopback address one to dial.
    on_loopback: bool,
    entry: Entry,
    /// The saved contacts being read, to enter through.
    reading: Option<oneshot::Receiver<Routers>>,
    connections_accepted: u64,
    rejected: BTreeMap<Rejection, u64>,
    /// Every key the node stored others' provider records under, at most
    /// about `store_limits.max_provided_keys`.
    provider_keys: HashSet<RecordKey>,
    /// The providers each running `get_providers` query holds in
    /// [`NoDial`], with the dials refused to each when it held it.
    held_by_query: HashMap<QueryId, Vec<(PeerId, u64)>>,
    /// Each `get_providers` query that ended: its requests to the network,
    /// and the dials [`NoDial`] refused it.
    #[cfg(test)]
    lookups_ended: Vec<(QueryId, u32, u64)>,
}

impl Task {
    /// The swarm of `parts`, listening and entering the network, and the
    /// handler its node serves; fails on a bootstrap ticket that is the
    /// node's own, or none.
    fn new(parts: DhtParts) -> anyhow::Result<(Self, IrohListener)> {
        let DhtParts {
            endpoint,
            key,
            cfg,
            timing,
            quotas,
            store_limits,
            clock,
            store,
        } = parts;
        let own = key.id();
        let local = peer_id(&own);
        let bootstrap = cfg.bootstrap_peers()?;
        ensure!(
            bootstrap.iter().all(|(agent, _)| *agent != own),
            "dht.bootstrap: a ticket is this agent's own: a node enters the DHT through other \
             routers"
        );
        let book = AddressBook::default();
        let (transport, listener, refusals) = IrohTransport::new(
            endpoint.clone(),
            quotas,
            Arc::clone(&clock),
            book.clone(),
            timing.idle_timeout,
        );
        let limits = connection_limits::ConnectionLimits::default()
            .with_max_established_per_peer(Some(MAX_CONNECTIONS_PER_PEER))
            .with_max_established_incoming(Some(MAX_INCOMING_CONNECTIONS));
        let behaviour = Behaviour {
            limits: connection_limits::Behaviour::new(limits),
            kad: kad::Behaviour::with_config(
                local,
                MemoryStore::with_config(local, store_limits.clone()),
                kad_config(&timing),
            ),
            identify: identify::Behaviour::new(
                identify::Config::new(IDENTIFY_VERSION.to_owned(), public_key(&own))
                    .with_hide_listen_addrs(true)
                    .with_cache_size(0),
            ),
            undialed: NoDial::default(),
        };
        let config = swarm::Config::with_tokio_executor()
            .with_dial_concurrency_factor(NonZeroU8::MIN)
            .with_idle_connection_timeout(timing.idle_timeout);
        let mut swarm = Swarm::new(transport.boxed(), behaviour, local, config);
        let mode = match cfg.mode {
            DhtMode::Router => Mode::Server,
            DhtMode::Client => Mode::Client,
        };
        swarm.behaviour_mut().kad.set_mode(Some(mode));
        let sockets = endpoint.bound_sockets();
        let bound = sockets
            .first()
            .copied()
            .context("the endpoint is bound to no socket")?;
        let on_loopback = sockets.iter().any(|socket| socket.ip().is_loopback());
        swarm
            .listen_on(quic_addr(bound))
            .with_context(|| format!("libp2p cannot listen on the endpoint's {bound}"))?;
        let mut task = Task {
            swarm,
            local,
            own,
            mode: cfg.mode,
            endpoint,
            external: cfg.external_addrs,
            clock,
            store,
            book,
            refusals,
            store_limits,
            on_loopback,
            entry: Entry::default(),
            reading: None,
            connections_accepted: 0,
            rejected: BTreeMap::new(),
            provider_keys: HashSet::new(),
            held_by_query: HashMap::new(),
            #[cfg(test)]
            lookups_ended: Vec::new(),
        };
        task.declare();
        let routers = bootstrap.len();
        task.enter(bootstrap);
        task.entry.contacts_left = routers > 0;
        tracing::info!(
            peer_id = %local,
            mode = ?cfg.mode,
            %bound,
            bootstrap = routers,
            "the DHT started"
        );
        Ok((task, listener))
    }

    /// Runs the swarm and answers `commands` until they end or the task is
    /// aborted.
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        // A router with no external address declares its endpoint's
        // direct ones, which change.
        let mut address_changes: BoxStream<'static, ()> =
            if self.mode == DhtMode::Router && self.external.is_empty() {
                self.endpoint
                    .watch_addr()
                    .stream()
                    .map(|_| ())
                    .fuse()
                    .boxed()
            } else {
                stream::pending().boxed()
            };
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => break,
                },
                event = self.swarm.select_next_some() => self.on_swarm_event(event),
                Some(contacts) = read(&mut self.reading) => self.try_contacts(contacts),
                Some(()) = address_changes.next() => self.declare(),
            }
        }
    }

    fn kad(&mut self) -> &mut kad::Behaviour<MemoryStore> {
        &mut self.swarm.behaviour_mut().kad
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Command::HeldProviders(key, reply) => {
                let _ = reply.send(self.held_providers(&key));
            }
        }
    }

    /// Makes what the node declares its external addresses, as the module
    /// says.
    fn declare(&mut self) {
        let wanted: Vec<Multiaddr> = match self.mode {
            DhtMode::Client => vec![located_nowhere(&self.local)],
            DhtMode::Router => {
                let direct = dialable_addr(&self.endpoint, &[]);
                let addrs = router_addrs(&self.external, direct.ip_addrs().copied());
                if addrs.is_empty() {
                    tracing::warn!(
                        "a DHT router with no address to declare: set [dht] external_addrs"
                    );
                }
                addrs
                    .into_iter()
                    .map(|addr| multiaddr_of(&self.local, addr))
                    .collect()
            }
        };
        let stale: Vec<Multiaddr> = self
            .swarm
            .external_addresses()
            .filter(|addr| !wanted.contains(addr))
            .cloned()
            .collect();
        for addr in stale {
            self.swarm.remove_external_address(&addr);
        }
        for addr in wanted {
            self.swarm.add_external_address(addr);
        }
    }

    /// Gives Kademlia and the address book every address of `routers`, and
    /// bootstraps through them, if any.
    fn enter(&mut self, routers: Routers) {
        if routers.is_empty() {
            return;
        }
        for (agent, addrs) in routers {
            let peer = peer_id(&agent);
            for addr in addrs {
                self.book.learn(peer, addr);
                self.kad().add_address(&peer, multiaddr_of(&peer, addr));
            }
        }
        self.entry.query = self.kad().bootstrap().ok();
        self.entry.answered = false;
    }

    /// The bootstrap that entered ended with no router answering: reads
    /// the saved contacts, to enter through them, once.
    fn read_contacts(&mut self) {
        if !std::mem::take(&mut self.entry.contacts_left) {
            return;
        }
        tracing::info!("no DHT bootstrap router answered: trying the saved contacts");
        let (store, own) = (Arc::clone(&self.store), self.own);
        let (done, contacts) = oneshot::channel();
        self.reading = Some(contacts);
        tokio::task::spawn_blocking(move || {
            let saved = match store.contacts() {
                Ok(saved) => saved,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "cannot read the contacts");
                    Vec::new()
                }
            };
            // By name, as the store keeps them.
            let routers = saved
                .into_iter()
                .filter_map(|contact| {
                    let ticket: EndpointTicket = contact.ticket?.parse().ok()?;
                    let addr = ticket.endpoint_addr();
                    let agent = agent_id_of(&addr.id);
                    let ips: Vec<SocketAddr> = addr.ip_addrs().copied().collect();
                    (agent != own && !ips.is_empty()).then_some((agent, ips))
                })
                .take(ENTRY_CONTACTS)
                .collect();
            let _ = done.send(routers);
        });
    }

    /// Dials `contacts`, the saved contacts read when no bootstrap router
    /// answered, and tells neither Kademlia nor the address book of them:
    /// one enters the routing table only once it shows it is a router
    /// ([`Task::on_identify`], or `libp2p-kad`'s own check of the protocol
    /// on the connection it was dialed on), and the node bootstraps through
    /// the first that does ([`Task::on_routing_updated`]).
    fn try_contacts(&mut self, contacts: Routers) {
        tracing::info!(
            contacts = contacts.len(),
            "dialing the saved contacts, to find a DHT router among them"
        );
        for (agent, ips) in contacts {
            let peer = peer_id(&agent);
            let addrs = ips.into_iter().map(|ip| multiaddr_of(&peer, ip)).collect();
            match self
                .swarm
                .dial(DialOpts::peer_id(peer).addresses(addrs).build())
            {
                Ok(()) => {
                    self.entry.trying.insert(peer);
                }
                Err(error) => tracing::debug!(%peer, %error, "a saved contact not dialed"),
            }
        }
    }

    fn on_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) => self.on_kad(event),
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => self.on_identify(peer_id, info),
            SwarmEvent::ConnectionEstablished {
                endpoint: ConnectedPoint::Listener { .. },
                ..
            } => self.connections_accepted += 1,
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer),
                error,
                ..
            } => {
                tracing::debug!(%peer, %error, "an agentnet-kad/0 dial failed");
                self.give_back(peer);
            }
            SwarmEvent::IncomingConnectionError { peer_id, error, .. } => {
                tracing::debug!(?peer_id, %error, "an agentnet-kad/0 connection refused");
            }
            SwarmEvent::ListenerClosed {
                reason: Err(error), ..
            } => tracing::warn!(%error, "libp2p stopped listening"),
            SwarmEvent::ListenerError { error, .. } => {
                tracing::warn!(%error, "libp2p's listener failed");
            }
            _ => {}
        }
    }

    /// What `peer` says of itself: when it serves [`KAD_PROTOCOL`], so is a
    /// router, its addresses in the router form that name it and are
    /// routable from here go to Kademlia and to the address book.
    fn on_identify(&mut self, peer: PeerId, info: identify::Info) {
        if !info.protocols.contains(&KAD_PROTOCOL) {
            return;
        }
        for addr in info.listen_addrs {
            let Some((named, socket)) = dial_target(&addr) else {
                continue;
            };
            if named != peer || !self.routable(socket) {
                continue;
            }
            self.book.learn(peer, socket);
            self.kad().add_address(&peer, addr);
        }
    }

    /// Whether a peer's `socket` is one to dial: not unspecified nor
    /// multicast, and loopback only when this node is bound there.
    fn routable(&self, socket: SocketAddr) -> bool {
        let ip = socket.ip();
        !ip.is_unspecified() && !ip.is_multicast() && (!ip.is_loopback() || self.on_loopback)
    }

    /// `libp2p-kad` drops every address of a peer a dial to it failed at,
    /// but the last, even when the failure was for a while: a router on two
    /// networks that stops a moment would be left with only its address on
    /// the other, and stay out of reach once back. So the node gives
    /// Kademlia back every address it knows of `peer`.
    fn give_back(&mut self, peer: PeerId) {
        for addr in self.book.of(&peer) {
            self.kad().add_address(&peer, multiaddr_of(&peer, addr));
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
            } => self.on_put(source, record),
            kad::Event::InboundRequest {
                request:
                    InboundRequest::AddProvider {
                        record: Some(record),
                    },
            } => self.on_provider(record),
            kad::Event::OutboundQueryProgressed {
                id,
                result,
                stats,
                step,
            } => self.on_query(id, result, &stats, step.last),
            kad::Event::RoutingUpdated {
                peer,
                is_new_peer,
                addresses,
                ..
            } => self.on_routing_updated(peer, is_new_peer, &addresses),
            _ => {}
        }
    }

    /// `peer` is in the routing table at `addresses`: the address book
    /// learns them, and when it is the first saved contact to show it is a
    /// router, the node bootstraps through it.
    fn on_routing_updated(&mut self, peer: PeerId, is_new_peer: bool, addresses: &kad::Addresses) {
        if is_new_peer {
            tracing::debug!(%peer, "a DHT router in the routing table");
        }
        for addr in addresses.iter() {
            if let Some((named, socket)) = dial_target(addr)
                && named == peer
            {
                self.book.learn(peer, socket);
            }
        }
        if self.entry.trying.remove(&peer) {
            self.entry.trying.clear();
            tracing::info!(%peer, "a saved contact is a DHT router: entering through it");
            if let Err(error) = self.kad().bootstrap() {
                tracing::debug!(%error, "cannot bootstrap through the saved contact");
            }
        }
    }

    fn on_query(&mut self, id: QueryId, result: QueryResult, stats: &QueryStats, last: bool) {
        match result {
            QueryResult::Bootstrap(result) => {
                if let Err(error) = result {
                    tracing::debug!(%error, "a DHT bootstrap failed");
                }
                if self.entry.query == Some(id) {
                    self.entry.answered |= stats.num_successes() > 0;
                    if last {
                        self.entry.query = None;
                        if !self.entry.answered {
                            self.read_contacts();
                        }
                    }
                }
            }
            QueryResult::GetProviders(Ok(GetProvidersOk::FoundProviders { providers, .. })) => {
                self.hold_undialed(id, providers);
            }
            QueryResult::RepublishRecord(Err(error)) => {
                tracing::debug!(%error, "replicating a record");
            }
            _ => {}
        }
        if last && self.held_by_query.contains_key(&id) {
            self.end_lookup(id, stats);
        }
    }

    /// Providers a `get_providers` query found: each one not in the
    /// routing table is held in [`NoDial`] until the query ends.
    fn hold_undialed(&mut self, id: QueryId, providers: HashSet<PeerId>) {
        let local = self.local;
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
        let held = self.held_by_query.entry(id).or_default();
        let no_dial = &mut self.swarm.behaviour_mut().undialed;
        for peer in providers {
            if peer == local || routers.contains(&peer) || held.iter().any(|(p, _)| *p == peer) {
                continue;
            }
            held.push((peer, no_dial.denied(&peer)));
            no_dial.hold(peer);
        }
    }

    /// A `get_providers` query that held providers ended: releases them,
    /// and gives its requests to the network, leaving out the dials
    /// [`NoDial`] refused it: each provider it held whose refusals rose
    /// while it held it.
    fn end_lookup(&mut self, id: QueryId, stats: &QueryStats) -> u64 {
        let held = self.held_by_query.remove(&id).unwrap_or_default();
        let no_dial = &mut self.swarm.behaviour_mut().undialed;
        let denied = held
            .iter()
            .filter(|(peer, at)| no_dial.denied(peer) > *at)
            .count() as u64;
        for (peer, _) in &held {
            no_dial.release(peer);
        }
        let requests = u64::from(stats.num_requests()).saturating_sub(denied);
        tracing::debug!(query = ?id, requests, denied, "a DHT lookup ended");
        #[cfg(test)]
        self.lookups_ended.push((id, stats.num_requests(), denied));
        requests
    }

    /// A `PutRecord` to this node: stored only if [`validate::check_put`]
    /// says so.
    fn on_put(&mut self, source: PeerId, record: Record) {
        let now = self.clock.now_unix();
        let store = self.kad().store_mut();
        let held = held_value(store, &record.key);
        match validate::check_put(&record.key, &record.value, held.as_deref(), now) {
            Verdict::Store => {
                let changed = held.as_deref() != Some(record.value.as_slice());
                let (key, value) = (record.key.clone(), record.value.clone());
                if let Err(error) = store.put(record) {
                    tracing::debug!(?error, "the store is full");
                    self.reject(Rejection::StoreFull, &source, &key);
                    return;
                }
                if changed && let Some(Slot::Announce(agent)) = slot(&key) {
                    self.drop_unlisted(&agent, &value, now);
                }
            }
            Verdict::Stale => {
                tracing::debug!(%source, key = %key_text(&record.key), "an older record, kept out");
            }
            Verdict::Reject(why) => self.reject(why, &source, &record.key),
        }
    }

    /// `agent`'s announce `value` replaced the one this node held: its
    /// provider records of the capabilities it no longer lists go.
    fn drop_unlisted(&mut self, agent: &AgentId, value: &[u8], now_unix: i64) {
        let Ok(announce) = validate::announce_for(agent, value, now_unix) else {
            return;
        };
        let listed: HashSet<RecordKey> = announce
            .doc
            .capabilities
            .iter()
            .filter_map(|capability| provider_key(capability).ok())
            .collect();
        let peer = peer_id(agent);
        let store = self.swarm.behaviour_mut().kad.store_mut();
        let mut dropped = 0;
        for key in self
            .provider_keys
            .iter()
            .filter(|key| !listed.contains(*key))
        {
            if store.providers(key).iter().any(|p| p.provider == peer) {
                store.remove_provider(key, &peer);
                dropped += 1;
            }
        }
        if dropped > 0 {
            tracing::debug!(%agent, dropped, "provider records its new announce no longer lists");
        }
    }

    /// An `AddProvider` to this node: stored only if
    /// [`validate::check_provider`] says so.
    fn on_provider(&mut self, record: ProviderRecord) {
        let now = self.clock.now_unix();
        let max_keys = self.store_limits.max_provided_keys;
        let store = self.swarm.behaviour_mut().kad.store_mut();
        let held_announce =
            agent_of(&record.provider).and_then(|agent| held_value(store, &announce_key(&agent)));
        let verdict =
            validate::check_provider(&record.key, &record.provider, held_announce.as_deref(), now);
        let (key, provider) = (record.key.clone(), record.provider);
        match verdict {
            Verdict::Store => {
                if let Err(error) = store.add_provider(record) {
                    tracing::debug!(?error, "the store is full");
                    self.reject(Rejection::StoreFull, &provider, &key);
                    return;
                }
                if self.provider_keys.insert(key) && self.provider_keys.len() > max_keys {
                    self.provider_keys
                        .retain(|key| !store.providers(key).is_empty());
                }
            }
            Verdict::Stale => {}
            Verdict::Reject(why) => self.reject(why, &provider, &key),
        }
    }

    /// Counts a record refused for `why`, and logs who sent it.
    fn reject(&mut self, why: Rejection, from: &PeerId, key: &RecordKey) {
        *self.rejected.entry(why).or_default() += 1;
        tracing::debug!(
            peer = %from,
            key = %key_text(key),
            why = why.as_str(),
            "a DHT record refused"
        );
    }

    /// The DHT's state now: what has not expired, and the counts since the
    /// node started.
    fn status(&mut self) -> DhtStatus {
        let now = Instant::now();
        let local = self.local;
        let connections = self
            .swarm
            .network_info()
            .connection_counters()
            .num_established() as usize;
        let refused = self.refusals.counts();
        let rejected = Rejection::ALL
            .iter()
            .map(|why| (why.as_str(), self.rejected.get(why).copied().unwrap_or(0)))
            .collect();
        let kad = &mut self.swarm.behaviour_mut().kad;
        let mode = match kad.mode() {
            Mode::Server => DhtMode::Router,
            Mode::Client => DhtMode::Client,
        };
        let routing_peers = kad.kbuckets().map(|bucket| bucket.num_entries()).sum();
        let store = kad.store_mut();
        let mut stored_by_agent: BTreeMap<AgentId, u32> = BTreeMap::new();
        let mut records_stored = 0;
        for record in store.records() {
            if record.is_expired(now) || record.publisher == Some(local) {
                continue;
            }
            records_stored += 1;
            // A router stores a value only under its signer's key.
            let agent = match slot(&record.key) {
                Some(Slot::Contact(agent) | Slot::Announce(agent)) => agent,
                None => continue,
            };
            *stored_by_agent.entry(agent).or_default() += 1;
        }
        let mut provider_records_stored = 0;
        for key in &self.provider_keys {
            for provider in store.providers(key) {
                if provider.is_expired(now) || provider.provider == local {
                    continue;
                }
                provider_records_stored += 1;
                if let Some(agent) = agent_of(&provider.provider) {
                    *stored_by_agent.entry(agent).or_default() += 1;
                }
            }
        }
        DhtStatus {
            mode,
            routing_peers,
            connections,
            connections_accepted: self.connections_accepted,
            refused,
            records_stored,
            provider_records_stored,
            stored_by_agent,
            rejected,
        }
    }

    /// The unexpired provider records of others this node holds under
    /// `key`.
    fn held_providers(&mut self, key: &RecordKey) -> Vec<(AgentId, Vec<Multiaddr>)> {
        let now = Instant::now();
        let local = self.local;
        self.kad()
            .store_mut()
            .providers(key)
            .into_iter()
            .filter(|provider| !provider.is_expired(now) && provider.provider != local)
            .filter_map(|provider| Some((agent_of(&provider.provider)?, provider.addresses)))
            .collect()
    }
}

/// What `reading` gives, once it does, leaving `None` in its place: `None`
/// too when its sender went away. Never, when there is nothing to read.
async fn read<T>(reading: &mut Option<oneshot::Receiver<T>>) -> Option<T> {
    let Some(receiver) = reading else {
        return future::pending().await;
    };
    let read = receiver.await.ok();
    *reading = None;
    read
}

/// What `store` holds under `key`, unless `libp2p-kad` marked it expired.
fn held_value(store: &MemoryStore, key: &RecordKey) -> Option<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use iroh::endpoint::{PortmapperConfig, presets};
    use iroh::protocol::Router;
    use iroh::{EndpointAddr, RelayMode, SecretKey, TransportAddr};
    use pals_core::clock::SystemClock;
    use pals_core::identity::AgentKey;

    use super::*;
    use crate::config::DhtCfg;
    use crate::dht::transport::{ALPN_KAD, KadQuotas};

    #[test]
    fn a_router_never_declares_an_unspecified_address() {
        let port = 4433;
        let any = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
        let any6 = SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port));
        let lan = SocketAddr::from(([192, 168, 1, 10], port));
        let public = SocketAddr::from(([203, 0, 113, 10], port));
        // Its direct ones, with the unspecified sockets and port 0 left out.
        let unbound = SocketAddr::from(([192, 168, 1, 11], 0));
        assert_eq!(router_addrs(&[], [any, lan, any6, unbound]), vec![lan]);
        assert!(router_addrs(&[], [any, any6]).is_empty());
        // Its external ones, when it has some, and only them.
        assert_eq!(router_addrs(&[public], [any, lan]), vec![public]);
        assert_eq!(router_addrs(&[any, public], [lan]), vec![public]);
    }

    /// An endpoint with `key` on a free port of 127.0.0.1, as the daemon
    /// binds it without relay.
    async fn endpoint(key: &AgentKey) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    /// A task for `key` on `endpoint` in `mode`, entering through
    /// `bootstrap`.
    fn task(
        key: AgentKey,
        endpoint: Endpoint,
        mode: DhtMode,
        bootstrap: Vec<String>,
    ) -> (Task, IrohListener) {
        let key = Arc::new(key);
        let store = Arc::new(Store::open_in_memory(key.id()).unwrap());
        let cfg = DhtCfg {
            enabled: true,
            mode,
            bootstrap,
            ..DhtCfg::default()
        };
        Task::new(DhtParts {
            endpoint,
            key,
            timing: DhtTiming::from_config(&cfg),
            cfg,
            quotas: KadQuotas::default(),
            store_limits: STORE_LIMITS,
            clock: Arc::new(SystemClock),
            store,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn a_lookup_refuses_at_once_to_dial_a_client_provider() {
        const PROMPTLY: Duration = Duration::from_secs(30);
        // A router that holds a client's provider record, with only the
        // client form as its address.
        let router_key = AgentKey::generate();
        let router_endpoint = endpoint(&router_key).await;
        let router_socket = router_endpoint.bound_sockets()[0];
        let (mut router, listener) = task(
            router_key,
            router_endpoint.clone(),
            DhtMode::Router,
            Vec::new(),
        );
        let client = peer_id(&AgentKey::generate().id());
        let capability = provider_key("vision.ocr").unwrap();
        router
            .kad()
            .store_mut()
            .add_provider(ProviderRecord {
                key: capability.clone(),
                provider: client,
                expires: None,
                addresses: vec![located_nowhere(&client)],
            })
            .unwrap();
        let (_commands, queue) = mpsc::channel(1);
        let _router_task = tokio::spawn(router.run(queue));
        let _served = Router::builder(router_endpoint.clone())
            .accept(ALPN_KAD, listener)
            .spawn();

        // A client that knows only the router, and looks the capability up.
        let ticket = EndpointTicket::new(EndpointAddr::from_parts(
            router_endpoint.id(),
            [TransportAddr::Ip(router_socket)],
        ))
        .to_string();
        let searcher_key = AgentKey::generate();
        let searcher_endpoint = endpoint(&searcher_key).await;
        let (mut searcher, _listener) = task(
            searcher_key,
            searcher_endpoint,
            DhtMode::Client,
            vec![ticket],
        );
        let id = searcher.kad().get_providers(capability);
        let ended = tokio::time::timeout(PROMPTLY, async {
            loop {
                let event = searcher.swarm.select_next_some().await;
                searcher.on_swarm_event(event);
                if let Some(ended) = searcher.lookups_ended.iter().find(|(q, ..)| *q == id) {
                    return *ended;
                }
            }
        })
        .await
        .expect("the lookup ends, without waiting on the client");
        // Two requests, the router's and the client's, and the client's was
        // refused before it left: one went to the network.
        let (_, requests, denied) = ended;
        assert_eq!((requests, denied), (2, 1));
        let no_dial = &searcher.swarm.behaviour().undialed;
        assert!(no_dial.held.is_empty() && no_dial.denied.is_empty());
    }

    #[test]
    fn no_dial_holds_a_peer_until_every_query_releases_it() {
        let mut no_dial = NoDial::default();
        let (peer, other) = (PeerId::random(), PeerId::random());
        let dial = |no_dial: &mut NoDial, peer: PeerId| {
            no_dial
                .handle_pending_outbound_connection(
                    ConnectionId::new_unchecked(0),
                    Some(peer),
                    &[],
                    Role::Dialer,
                )
                .is_ok()
        };
        assert!(dial(&mut no_dial, peer));
        no_dial.hold(peer);
        no_dial.hold(peer);
        assert!(!dial(&mut no_dial, peer));
        assert!(dial(&mut no_dial, other));
        assert_eq!(no_dial.denied(&peer), 1);
        no_dial.release(&peer);
        assert!(!dial(&mut no_dial, peer));
        assert_eq!(no_dial.denied(&peer), 2);
        no_dial.release(&peer);
        assert!(dial(&mut no_dial, peer));
        assert_eq!(no_dial.denied(&peer), 0);
    }
}
