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
//!   `libp2p-kad` on [`KAD_PROTOCOL`] ([`Kademlia`], which counts the dials
//!   [`NoDial`] refuses each query), `identify`, and [`NoDial`].
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
//! - **The routing table.** A peer enters it only through `add_address`
//!   (`BucketInserts::Manual`): a router `identify` confirmed
//!   ([`Task::on_identify`]), a bootstrap ticket's router ([`Task::enter`]),
//!   and a router of the [`AddressBook`] given its addresses back
//!   ([`Task::give_back`]). `libp2p-kad`'s default would also put in any
//!   peer that opens a Kademlia stream on a connection this node dialed,
//!   at the address it dialed: a client does that when this node dialed it
//!   as a saved contact, and its next lookup goes over that connection.
//!   What `libp2p-kad` still does by itself is add to a peer already in
//!   the table the address this node dialed it at, once the peer confirms
//!   the protocol on that connection; the book learns it only when it is
//!   [routable](Task::routable).
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
//!   for counts as [`Rejection::StoreFull`]. Room is looked for first: what
//!   expired holds its place until something reads it, so a full store
//!   drops what `libp2p-kad` marked expired and takes the record again,
//!   once ([`Task::with_room`]). Each refusal is counted by its
//!   name and logged at `debug` with who sent it. When a router stores a
//!   newer announce of an agent, it drops that agent's provider records of
//!   the capabilities the announce no longer lists: an agent has at most
//!   [`MAX_ANNOUNCED_CAPABILITIES`] provider records at a router at any
//!   time, each with at most [`validate::MAX_PROVIDER_ADDRS`] addresses
//!   that name it.
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
//!   addresses of the bootstrap tickets and the routable addresses of the
//!   peers in the routing table, and nothing else. `libp2p-kad` drops a peer's addresses after
//!   a failed dial to it, but the last; the node gives it back every one the
//!   book has of that server, so that a router that stopped a moment is
//!   dialed at all of them again, and a peer that never was a router gets
//!   nothing back.
//! - **What a node declares.** `libp2p-kad` returns no provider record
//!   without addresses, so every node declares one: a router its
//!   `[dht] external_addrs` or, with none, its endpoint's direct addresses,
//!   never an unspecified IP ([`router_addrs`]), in the router form; a
//!   client only [`located_nowhere`], which is no location. Its own
//!   provider records carry those addresses and nothing else
//!   ([`Task::own_provider_record`]): `libp2p-kad` keeps them with none and
//!   answers for them with its listen addresses, `0.0.0.0` among them on a
//!   router bound there.
//! - **Publication and lookups.** [`super::publish`] runs on this task: the
//!   rounds that publish the node's records, and the lookups of a
//!   capability's providers. Every query belongs to one round or one
//!   lookup by its `QueryId`, so any number of them run at once.
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
use libp2p::kad::store::{self as kad_store, MemoryStore, MemoryStoreConfig, RecordStore};
use libp2p::kad::{
    self, BucketInserts, Caching, GetProvidersOk, InboundRequest, Mode, ProviderRecord, QueryId,
    QueryResult, QueryStats, Record, RecordKey, StoreInserts,
};
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, DialError, DialFailure, FromSwarm, NetworkBehaviour,
    SwarmEvent, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, connection_limits, identify, swarm};
use pals_core::clock::Clock;
use pals_core::identity::AgentId;
use pals_core::store::Store;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::keys::{
    Slot, agent_of, announce_key, contact_key, peer_id, provider_key, public_key, slot,
};
use super::publish::{Lookups, Publication};
use super::transport::{
    AddressBook, IrohListener, IrohTransport, Refusals, dial_target, located_nowhere, multiaddr_of,
    quic_addr,
};
use super::validate::{self, Rejection, Verdict};
use super::{DhtError, DhtLookup, DhtParts, DhtStatus, DhtTiming};
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
/// - 1024 keys with providers, the node's own among them;
/// - and, outside this configuration, [`validate::MAX_PROVIDER_ADDRS`]
///   addresses per provider record ([`validate::provider_addrs`]).
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
/// An agent's contact and announce a router holds, as it stored them.
type HeldRecords = (Option<Vec<u8>>, Option<Vec<u8>>);
/// The protocol family `identify` says the node speaks.
const IDENTIFY_VERSION: &str = "agentnet/0";

/// What a node's swarm runs: its connection limits, Kademlia, `identify`,
/// and the peers its lookups do not dial.
#[derive(swarm::NetworkBehaviour)]
pub(crate) struct Behaviour {
    limits: connection_limits::Behaviour,
    kad: Kademlia,
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
/// it as failed and goes on. [`Kademlia`] counts each refusal against the
/// queries it failed, so that their requests leave it out. Unlike
/// `allow_block_list`, it never closes a connection, and it lets the peer
/// dial in.
#[derive(Debug, Default)]
pub(crate) struct NoDial {
    /// Each peer held, with how many queries hold it.
    held: HashMap<PeerId, usize>,
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
            }
        }
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

/// `libp2p-kad`, with the dials [`NoDial`] refused each of its queries.
///
/// A query that picks a peer counts a request and waits on it, and the
/// dial, when NoDial refuses it, never reaches the network. A refusal
/// fails the peer, at once, in every query still waiting on it: each one
/// that picked it, since a query picks a peer only once, and none that did
/// not. So, around each refusal, the queries whose failures rose are the
/// ones it counts against, each by its `QueryId`; a second refusal of the
/// same peer fails none the first one did. Two lookups that hold the same
/// provider never count each other's refusals.
pub(crate) struct Kademlia {
    pub(crate) inner: kad::Behaviour<MemoryStore>,
    /// The dials NoDial refused each query, until
    /// [`Kademlia::take_undialed`].
    undialed: HashMap<QueryId, u32>,
}

impl Kademlia {
    fn new(inner: kad::Behaviour<MemoryStore>) -> Self {
        Self {
            inner,
            undialed: HashMap::new(),
        }
    }

    /// The dials NoDial refused query `id`, forgotten from then on.
    pub(crate) fn take_undialed(&mut self, id: &QueryId) -> u32 {
        self.undialed.remove(id).unwrap_or(0)
    }
}

impl NetworkBehaviour for Kademlia {
    type ConnectionHandler = THandler<kad::Behaviour<MemoryStore>>;
    type ToSwarm = kad::Event;

    fn handle_pending_inbound_connection(
        &mut self,
        id: ConnectionId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(id, local, remote)
    }

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_inbound_connection(id, peer, local, remote)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Role,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner
            .handle_pending_outbound_connection(id, peer, addresses, role)
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Role,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_outbound_connection(id, peer, addr, role, port_use)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        let refused = matches!(
            event,
            FromSwarm::DialFailure(DialFailure {
                error: DialError::Denied { cause },
                ..
            }) if cause.downcast_ref::<NotDialed>().is_some()
        );
        if !refused {
            self.inner.on_swarm_event(event);
            return;
        }
        let failures: Vec<(QueryId, u32)> = self
            .inner
            .iter_queries()
            .map(|query| (query.id(), query.stats().num_failures()))
            .collect();
        self.inner.on_swarm_event(event);
        for (id, before) in failures {
            if self
                .inner
                .query(&id)
                .is_some_and(|query| query.stats().num_failures() > before)
            {
                *self.undialed.entry(id).or_default() += 1;
            }
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer, id, event);
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

/// What a [`Dht`](super::Dht) asks its task.
pub(crate) enum Command {
    Status(oneshot::Sender<DhtStatus>),
    HeldProviders(RecordKey, oneshot::Sender<Vec<(AgentId, Vec<Multiaddr>)>>),
    /// The contact and the announce of an agent the node holds.
    HeldRecords(AgentId, oneshot::Sender<HeldRecords>),
    /// A lookup of the providers of the capability whose key it is.
    Providers {
        key: RecordKey,
        limit: usize,
        deadline: tokio::time::Instant,
        reply: oneshot::Sender<Result<DhtLookup, DhtError>>,
    },
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
        .set_query_timeout(timing.query_timeout)
        // A peer enters the routing table only through `add_address`: never
        // because it opened a Kademlia stream on a connection this node
        // dialed, which a client does too.
        .set_kbucket_inserts(BucketInserts::Manual);
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
    pub(super) swarm: Swarm<Behaviour>,
    pub(super) local: PeerId,
    pub(super) own: AgentId,
    mode: DhtMode,
    endpoint: Endpoint,
    /// A router's `[dht] external_addrs`.
    external: Vec<SocketAddr>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) timing: DhtTiming,
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
    /// [`NoDial`].
    held_by_query: HashMap<QueryId, Vec<PeerId>>,
    /// What the node publishes, with `[dht] announce`.
    pub(super) publication: Option<Publication>,
    /// The lookups running.
    pub(super) lookups: Lookups,
    /// Each query that ended: its requests, and the dials [`NoDial`]
    /// refused it.
    #[cfg(test)]
    queries_ended: Vec<(QueryId, u32, u32)>,
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
            published,
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
            kad: Kademlia::new(kad::Behaviour::with_config(
                local,
                MemoryStore::with_config(local, store_limits.clone()),
                kad_config(&timing),
            )),
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
        swarm.behaviour_mut().kad.inner.set_mode(Some(mode));
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
            timing,
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
            publication: published.map(Publication::new),
            lookups: Lookups::default(),
            #[cfg(test)]
            queries_ended: Vec::new(),
        };
        task.declare();
        let routers = bootstrap.len();
        task.enter(bootstrap);
        task.entry.contacts_left = routers > 0;
        // With nothing to enter through, the node is in already.
        if task.entry.query.is_none() {
            task.entered();
        }
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
            let wake = self.next_wake();
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => break,
                },
                event = self.swarm.select_next_some() => self.on_swarm_event(event),
                Some(contacts) = read(&mut self.reading) => self.try_contacts(contacts),
                Some(()) = address_changes.next() => self.declare(),
                changed = next_change(&mut self.publication) => self.on_change(changed),
                () = sleep_until(wake) => self.on_timers(),
            }
        }
    }

    pub(super) fn kad(&mut self) -> &mut kad::Behaviour<MemoryStore> {
        &mut self.swarm.behaviour_mut().kad.inner
    }

    /// Finishes query `id`, if it runs: its requests so far, the dials
    /// [`NoDial`] refused it left out.
    pub(super) fn finish_query(&mut self, id: &QueryId) -> u64 {
        let kad = &mut self.swarm.behaviour_mut().kad;
        let undialed = kad.take_undialed(id);
        let Some(mut query) = kad.inner.query_mut(id) else {
            return 0;
        };
        let requests = query.stats().num_requests().saturating_sub(undialed);
        query.finish();
        u64::from(requests)
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Command::HeldProviders(key, reply) => {
                let _ = reply.send(self.held_providers(&key));
            }
            Command::HeldRecords(agent, reply) => {
                let store = self.kad().store_mut();
                let held = (
                    held_value(store, &contact_key(&agent)),
                    held_value(store, &announce_key(&agent)),
                );
                let _ = reply.send(held);
            }
            Command::Providers {
                key,
                limit,
                deadline,
                reply,
            } => self.start_lookup(key, limit, deadline, reply),
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
        // What the node provides says where it is now.
        let provided: Vec<RecordKey> = self
            .kad()
            .store_mut()
            .provided()
            .map(|record| record.key.clone())
            .collect();
        for key in provided {
            // In place: there is room for what is held already.
            let _ = self.own_provider_record(&key);
        }
    }

    /// The addresses the node declares, as its own provider records carry
    /// them: [`located_nowhere`] when it has none, never what `libp2p-kad`
    /// would put in their place, its listen addresses.
    fn declared(&self) -> Vec<Multiaddr> {
        let declared: Vec<Multiaddr> = self.swarm.external_addresses().cloned().collect();
        if declared.is_empty() {
            vec![located_nowhere(&self.local)]
        } else {
            declared
        }
    }

    /// Holds the node's own provider record of `key` with the addresses it
    /// declares; fails, when the store has no room for one more key, as
    /// [`Task::with_room`] says.
    pub(super) fn own_provider_record(&mut self, key: &RecordKey) -> kad_store::Result<()> {
        let record = ProviderRecord {
            key: key.clone(),
            provider: self.local,
            expires: None,
            addresses: self.declared(),
        };
        self.with_room(|kad| kad.store_mut().add_provider(record.clone()))
    }

    /// `insert`, which puts something in the store; if the store has no
    /// room, again once after dropping what `libp2p-kad` marked expired,
    /// which holds its place until something reads it.
    pub(super) fn with_room<T>(
        &mut self,
        mut insert: impl FnMut(&mut kad::Behaviour<MemoryStore>) -> kad_store::Result<T>,
    ) -> kad_store::Result<T> {
        if let Ok(done) = insert(self.kad()) {
            return Ok(done);
        }
        let purged = self.purge_expired();
        tracing::debug!(purged, "the DHT store was full: dropped what expired");
        insert(self.kad())
    }

    /// Drops from the store others' values and provider records that
    /// `libp2p-kad` marked expired; how many.
    fn purge_expired(&mut self) -> usize {
        let now = Instant::now();
        let store = self.swarm.behaviour_mut().kad.inner.store_mut();
        let expired: Vec<RecordKey> = store
            .records()
            .filter(|record| record.is_expired(now))
            .map(|record| record.key.clone())
            .collect();
        let mut purged = expired.len();
        for key in &expired {
            store.remove(key);
        }
        self.provider_keys.retain(|key| {
            for provider in store.providers(key) {
                if provider.is_expired(now) {
                    store.remove_provider(key, &provider.provider);
                    purged += 1;
                }
            }
            !store.providers(key).is_empty()
        });
        purged
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
    /// ([`Task::on_identify`]), and the node bootstraps through the first
    /// that does ([`Task::on_routing_updated`]).
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
    /// learns those that are [routable](Task::routable), and when it is the
    /// first saved contact to show it is a router, the node bootstraps
    /// through it.
    fn on_routing_updated(&mut self, peer: PeerId, is_new_peer: bool, addresses: &kad::Addresses) {
        if is_new_peer {
            tracing::debug!(%peer, "a DHT router in the routing table");
        }
        for addr in addresses.iter() {
            if let Some((named, socket)) = dial_target(addr)
                && named == peer
                && self.routable(socket)
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

    /// A step of query `id`, to the round or the lookup it belongs to; the
    /// last one with its requests to the network, the dials [`NoDial`]
    /// refused it left out.
    fn on_query(&mut self, id: QueryId, result: QueryResult, stats: &QueryStats, last: bool) {
        let requests = last.then(|| {
            let undialed = self.swarm.behaviour_mut().kad.take_undialed(&id);
            #[cfg(test)]
            self.queries_ended
                .push((id, stats.num_requests(), undialed));
            u64::from(stats.num_requests().saturating_sub(undialed))
        });
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
                        self.entered();
                    }
                }
            }
            QueryResult::GetProviders(result) => {
                if let Ok(GetProvidersOk::FoundProviders { providers, .. }) = &result {
                    self.hold_undialed(id, providers);
                }
                self.on_lookup_providers(id, result, stats, requests);
            }
            QueryResult::GetRecord(result) => self.on_lookup_record(id, result, requests),
            QueryResult::PutRecord(result) => self.on_round_put(id, &result),
            QueryResult::GetClosestPeers(result) => self.on_round_closest(id, result),
            QueryResult::StartProviding(result) => self.on_round_provided(id, result),
            QueryResult::RepublishRecord(Err(error)) => {
                tracing::debug!(%error, "replicating a record");
            }
            _ => {}
        }
        if last {
            self.release_undialed(id);
        }
    }

    /// Providers a `get_providers` query found: each one not in the
    /// routing table is held in [`NoDial`] until the query ends.
    fn hold_undialed(&mut self, id: QueryId, providers: &HashSet<PeerId>) {
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
            if *peer == local || routers.contains(peer) || held.contains(peer) {
                continue;
            }
            held.push(*peer);
            no_dial.hold(*peer);
        }
    }

    /// Query `id` ended: the providers it held in [`NoDial`] go.
    fn release_undialed(&mut self, id: QueryId) {
        let held = self.held_by_query.remove(&id).unwrap_or_default();
        let no_dial = &mut self.swarm.behaviour_mut().undialed;
        for peer in &held {
            no_dial.release(peer);
        }
    }

    /// A `PutRecord` to this node: stored only if [`validate::check_put`]
    /// says so.
    fn on_put(&mut self, source: PeerId, record: Record) {
        let now = self.clock.now_unix();
        let held = held_value(self.kad().store_mut(), &record.key);
        match validate::check_put(&record.key, &record.value, held.as_deref(), now) {
            Verdict::Store => {
                let changed = held.as_deref() != Some(record.value.as_slice());
                let (key, value) = (record.key.clone(), record.value.clone());
                // Too large a value counts as no room too: only a test's
                // store is smaller than `MAX_RECORD_BYTES`.
                if let Err(error) = self.with_room(|kad| kad.store_mut().put(record.clone())) {
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
        let store = self.swarm.behaviour_mut().kad.inner.store_mut();
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
    /// [`validate::check_provider`] says so, with the addresses
    /// [`validate::provider_addrs`] keeps.
    fn on_provider(&mut self, mut record: ProviderRecord) {
        let now = self.clock.now_unix();
        let max_keys = self.store_limits.max_provided_keys;
        let held_announce = agent_of(&record.provider)
            .and_then(|agent| held_value(self.kad().store_mut(), &announce_key(&agent)));
        let verdict =
            validate::check_provider(&record.key, &record.provider, held_announce.as_deref(), now);
        let (key, provider) = (record.key.clone(), record.provider);
        match verdict {
            Verdict::Store => {
                record.addresses = validate::provider_addrs(&provider, record.addresses);
                if let Err(error) =
                    self.with_room(|kad| kad.store_mut().add_provider(record.clone()))
                {
                    tracing::debug!(?error, "the store is full");
                    self.reject(Rejection::StoreFull, &provider, &key);
                    return;
                }
                let store = self.swarm.behaviour_mut().kad.inner.store_mut();
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
        let kad = &mut self.swarm.behaviour_mut().kad.inner;
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
            publication: self.publication.as_ref().map(Publication::status),
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

/// Whether `publication`'s records changed: `true` on the next change,
/// `false` once nothing can change them any more. Never, when the node
/// publishes nothing.
async fn next_change(publication: &mut Option<Publication>) -> bool {
    let Some(changes) = publication
        .as_mut()
        .and_then(|publication| publication.changes.as_mut())
    else {
        return future::pending().await;
    };
    changes.changed().await.is_ok()
}

/// Sleeps until `at`; never, without one.
async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => future::pending().await,
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
pub(super) mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use iroh::endpoint::{PortmapperConfig, presets};
    use iroh::protocol::Router;
    use iroh::{EndpointAddr, RelayMode, SecretKey, TransportAddr};
    use libp2p::futures::task::noop_waker;
    use pals_core::card_jws::CardHash;
    use pals_core::clock::SystemClock;
    use pals_core::grant::format_timestamp;
    use pals_core::identity::AgentKey;
    use pals_core::records::{
        ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact, RECORD_TTL_S,
    };
    use pals_core::signed::Signed;
    use tokio::sync::watch;

    use super::*;
    use crate::config::DhtCfg;
    use crate::dht::PublishedRecords;
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

    /// An endpoint with `key` on a free port of `ip`, with no relay.
    async fn endpoint_on(key: &AgentKey, ip: Ipv4Addr) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((ip, 0))
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    /// An endpoint with `key` on a free port of 127.0.0.1, as the daemon
    /// binds it without relay.
    pub(in crate::dht) async fn endpoint(key: &AgentKey) -> Endpoint {
        endpoint_on(key, Ipv4Addr::LOCALHOST).await
    }

    /// A task for `key` on `endpoint` with `cfg`, publishing `published`,
    /// at the time `clock` says.
    pub(in crate::dht) fn task_with(
        key: AgentKey,
        endpoint: Endpoint,
        cfg: DhtCfg,
        published: Option<Arc<dyn PublishedRecords>>,
        clock: Arc<dyn Clock>,
    ) -> (Task, IrohListener) {
        let key = Arc::new(key);
        let store = Arc::new(Store::open_in_memory(key.id()).unwrap());
        Task::new(DhtParts {
            endpoint,
            key,
            timing: DhtTiming::from_config(&cfg),
            cfg,
            quotas: KadQuotas::default(),
            store_limits: STORE_LIMITS,
            clock,
            store,
            published,
        })
        .unwrap()
    }

    /// A task for `key` on `endpoint` in `mode`, entering through
    /// `bootstrap`, publishing nothing.
    fn task(
        key: AgentKey,
        endpoint: Endpoint,
        mode: DhtMode,
        bootstrap: Vec<String>,
    ) -> (Task, IrohListener) {
        let cfg = DhtCfg {
            enabled: true,
            mode,
            bootstrap,
            ..DhtCfg::default()
        };
        task_with(key, endpoint, cfg, None, Arc::new(SystemClock))
    }

    /// A ticket of `endpoint` at `addr`.
    fn ticket(endpoint: &Endpoint, addr: SocketAddr) -> String {
        EndpointTicket::new(EndpointAddr::from_parts(
            endpoint.id(),
            [TransportAddr::Ip(addr)],
        ))
        .to_string()
    }

    /// `key`'s records, valid now, its announce listing `capabilities`,
    /// which never change.
    pub(in crate::dht) struct Fixed {
        current: (Vec<u8>, Vec<u8>),
        capabilities: Vec<String>,
        changes: watch::Sender<u64>,
    }

    impl Fixed {
        /// Issued a minute ago, for the whole life of a signed record.
        pub(in crate::dht) fn of(
            key: &AgentKey,
            capabilities: &[&str],
        ) -> Arc<dyn PublishedRecords> {
            Self::expiring(key, capabilities, SystemClock.now_unix(), RECORD_TTL_S - 60)
        }

        /// Issued a minute before `now`, in Unix seconds, expiring
        /// `expires_in` seconds after it.
        pub(in crate::dht) fn expiring(
            key: &AgentKey,
            capabilities: &[&str],
            now: i64,
            expires_in: i64,
        ) -> Arc<dyn PublishedRecords> {
            let issued_at = format_timestamp(now - 60).unwrap();
            let expires_at = format_timestamp(now + expires_in).unwrap();
            let contact = ContactRecord {
                schema: CONTACT_SCHEMA.to_owned(),
                agent_id: key.id(),
                iroh: IrohContact {
                    endpoint_id: key.id(),
                    relay_url: None,
                    direct_addrs: vec!["192.0.2.10:4433".to_owned()],
                },
                seq: 1,
                issued_at: issued_at.clone(),
                expires_at: expires_at.clone(),
            };
            let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
            let announce = AnnounceRecord {
                schema: ANNOUNCE_SCHEMA.to_owned(),
                agent_id: key.id(),
                seq: 1,
                agent_card_hash: format!("blake3:{}", "ab".repeat(32))
                    .parse::<CardHash>()
                    .unwrap(),
                capabilities: capabilities.clone(),
                discovery_alpn: "agentnet-discovery/0".to_owned(),
                issued_at,
                expires_at,
            };
            Arc::new(Self {
                current: (
                    Signed::issue(key, contact).unwrap().to_json(),
                    Signed::issue(key, announce).unwrap().to_json(),
                ),
                capabilities,
                changes: watch::channel(0).0,
            })
        }
    }

    impl PublishedRecords for Fixed {
        fn current(&self) -> (Vec<u8>, Vec<u8>) {
            self.current.clone()
        }

        fn capabilities(&self) -> Vec<String> {
            self.capabilities.clone()
        }

        fn changes(&self) -> watch::Receiver<u64> {
            self.changes.subscribe()
        }
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
        let searcher_key = AgentKey::generate();
        let searcher_endpoint = endpoint(&searcher_key).await;
        let (mut searcher, _listener) = task(
            searcher_key,
            searcher_endpoint,
            DhtMode::Client,
            vec![ticket(&router_endpoint, router_socket)],
        );
        let id = searcher.kad().get_providers(capability);
        let ended = tokio::time::timeout(PROMPTLY, async {
            loop {
                let event = searcher.swarm.select_next_some().await;
                searcher.on_swarm_event(event);
                if let Some(ended) = searcher.queries_ended.iter().find(|(q, ..)| *q == id) {
                    return *ended;
                }
            }
        })
        .await
        .expect("the lookup ends, without waiting on the client");
        // Two requests, the router's and the client's, and the client's was
        // refused before it left: one went to the network.
        let (_, requests, undialed) = ended;
        assert_eq!((requests, undialed), (2, 1));
        let no_dial = &searcher.swarm.behaviour().undialed;
        assert!(no_dial.held.is_empty());
    }

    #[test]
    fn a_refused_dial_counts_against_the_queries_that_picked_the_peer() {
        let local = PeerId::random();
        let mut config = kad_config(&DhtTiming::from_config(&DhtCfg::default()));
        config.set_periodic_bootstrap_interval(None);
        let mut kad = Kademlia::new(kad::Behaviour::with_config(
            local,
            MemoryStore::new(local),
            config,
        ));
        let provider = PeerId::random();
        kad.inner.add_address(
            &provider,
            "/ip4/192.0.2.1/udp/4433/quic-v1".parse().unwrap(),
        );
        let waker = noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        // Runs Kademlia until it dials the provider `dials` times.
        let mut run_until_dialed = |kad: &mut Kademlia, dials: usize| {
            let mut seen = 0;
            while seen < dials {
                match kad.poll(&mut cx) {
                    Poll::Ready(ToSwarm::Dial { opts }) if opts.get_peer_id() == Some(provider) => {
                        seen += 1;
                    }
                    Poll::Ready(_) => {}
                    Poll::Pending => panic!("Kademlia dialed the provider {seen} times"),
                }
            }
        };
        // Each dial refused, as the swarm reports it.
        let refuse = |kad: &mut Kademlia, cause: ConnectionDenied| {
            kad.on_swarm_event(FromSwarm::DialFailure(DialFailure {
                peer_id: Some(provider),
                error: &DialError::Denied { cause },
                connection_id: ConnectionId::new_unchecked(1),
            }));
        };

        // Two lookups that would ask the provider; one ends before it
        // picks it. The refusal is the other's alone.
        let picks = kad.inner.get_closest_peers(b"one".to_vec());
        let ends = kad.inner.get_closest_peers(b"two".to_vec());
        kad.inner.query_mut(&ends).unwrap().finish();
        run_until_dialed(&mut kad, 1);
        refuse(&mut kad, ConnectionDenied::new(NotDialed));
        assert_eq!(kad.take_undialed(&picks), 1);
        assert_eq!(kad.take_undialed(&ends), 0);
        assert_eq!(kad.take_undialed(&picks), 0, "taken once");

        // Two that both picked it, before either dial was refused: the
        // first refusal fails it in both, and the second in neither.
        let first = kad.inner.get_closest_peers(b"three".to_vec());
        let second = kad.inner.get_closest_peers(b"four".to_vec());
        run_until_dialed(&mut kad, 2);
        refuse(&mut kad, ConnectionDenied::new(NotDialed));
        refuse(&mut kad, ConnectionDenied::new(NotDialed));
        assert_eq!(kad.take_undialed(&first), 1);
        assert_eq!(kad.take_undialed(&second), 1);

        // A dial another behaviour refused is no refusal of NoDial's.
        let limited = kad.inner.get_closest_peers(b"five".to_vec());
        run_until_dialed(&mut kad, 1);
        refuse(
            &mut kad,
            ConnectionDenied::new(std::io::Error::other("a connection limit")),
        );
        assert_eq!(
            kad.inner.query(&limited).unwrap().stats().num_failures(),
            1,
            "the dial failed it"
        );
        assert_eq!(kad.take_undialed(&limited), 0);
    }

    #[tokio::test]
    async fn a_router_that_provides_answers_with_its_declared_addresses() {
        const PROMPTLY: Duration = Duration::from_secs(30);
        // A router bound to 0.0.0.0 that declares 127.0.0.1, and provides.
        let router_key = AgentKey::generate();
        let router_id = router_key.id();
        let router_peer = peer_id(&router_id);
        let router_endpoint = endpoint_on(&router_key, Ipv4Addr::UNSPECIFIED).await;
        let port = router_endpoint.bound_sockets()[0].port();
        let declared = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let cfg = DhtCfg {
            enabled: true,
            mode: DhtMode::Router,
            external_addrs: vec![declared],
            announce: true,
            ..DhtCfg::default()
        };
        let published = Fixed::of(&router_key, &["vision.ocr"]);
        let (mut router, listener) = task_with(
            router_key,
            router_endpoint.clone(),
            cfg,
            Some(published),
            Arc::new(SystemClock),
        );
        router.start_round();
        let capability = provider_key("vision.ocr").unwrap();
        let own = router.kad().store_mut().providers(&capability);
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].provider, router_peer);
        assert_eq!(own[0].addresses, [multiaddr_of(&router_peer, declared)]);
        let (_commands, queue) = mpsc::channel(1);
        let _router_task = tokio::spawn(router.run(queue));
        let _served = Router::builder(router_endpoint.clone())
            .accept(ALPN_KAD, listener)
            .spawn();

        // Another node's lookup gets from it no address but the declared
        // one: the addresses it would dial it at.
        let searcher_key = AgentKey::generate();
        let searcher_endpoint = endpoint(&searcher_key).await;
        let (mut searcher, _listener) = task(
            searcher_key,
            searcher_endpoint,
            DhtMode::Client,
            vec![ticket(&router_endpoint, declared)],
        );
        let (reply, _answer) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + PROMPTLY;
        searcher.start_lookup(capability, 8, deadline, reply);
        let addrs = tokio::time::timeout(PROMPTLY, async {
            loop {
                let event = searcher.swarm.select_next_some().await;
                let found = matches!(
                    &event,
                    SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                        result: QueryResult::GetProviders(Ok(GetProvidersOk::FoundProviders {
                            providers, ..
                        })),
                        ..
                    })) if providers.contains(&router_peer)
                );
                searcher.on_swarm_event(event);
                if found {
                    return searcher
                        .kad()
                        .handle_pending_outbound_connection(
                            ConnectionId::new_unchecked(0),
                            Some(router_peer),
                            &[],
                            Role::Dialer,
                        )
                        .unwrap();
                }
            }
        })
        .await
        .expect("the lookup finds the router");
        assert!(
            addrs.contains(&multiaddr_of(&router_peer, declared)),
            "{addrs:?}"
        );
        let unspecified = addrs.iter().find(|addr| {
            addr.iter().any(|part| match part {
                libp2p::multiaddr::Protocol::Ip4(ip) => ip.is_unspecified(),
                libp2p::multiaddr::Protocol::Ip6(ip) => ip.is_unspecified(),
                _ => false,
            })
        });
        assert_eq!(unspecified, None, "{addrs:?}");
    }

    #[tokio::test]
    async fn a_router_keeps_eight_addresses_of_a_provider_that_name_it() {
        let router_key = AgentKey::generate();
        let router_endpoint = endpoint(&router_key).await;
        let (mut router, _listener) =
            task(router_key, router_endpoint, DhtMode::Router, Vec::new());
        // The agent's announce, which its provider record needs.
        let agent = AgentKey::generate();
        let (_, announce) = Fixed::of(&agent, &["vision.ocr"]).current();
        router.on_put(
            PeerId::random(),
            Record::new(announce_key(&agent.id()), announce),
        );
        let peer = peer_id(&agent.id());
        let other = peer_id(&AgentKey::generate().id());
        let at = |n: u8| multiaddr_of(&peer, SocketAddr::from(([192, 0, 2, n], 4433)));
        let dns: Multiaddr = format!("/dns4/example.org/udp/4433/quic-v1/p2p/{peer}")
            .parse()
            .unwrap();
        // Among what it sends, ten addresses in the router form and the
        // client form name it; the others name another agent, have more
        // after `/p2p/`, no `/p2p/`, or no IP, and one comes twice.
        let mut sent = vec![
            multiaddr_of(&other, SocketAddr::from(([192, 0, 2, 99], 4433))),
            located_nowhere(&peer),
            dns,
            at(1),
            quic_addr(SocketAddr::from(([192, 0, 2, 98], 4433))),
            at(2),
            at(2),
            at(3).with(libp2p::multiaddr::Protocol::P2pCircuit),
        ];
        sent.extend((4..=10).map(at));
        let capability = provider_key("vision.ocr").unwrap();
        router.on_provider(ProviderRecord {
            key: capability.clone(),
            provider: peer,
            expires: None,
            addresses: sent,
        });
        let held = router.kad().store_mut().providers(&capability);
        assert_eq!(held.len(), 1, "{held:?}");
        // The first eight that name it, each once, in the order they came.
        let mut kept = vec![located_nowhere(&peer), at(1), at(2)];
        kept.extend((4..=8).map(at));
        assert_eq!(held[0].addresses, kept);
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
        no_dial.release(&peer);
        assert!(!dial(&mut no_dial, peer));
        no_dial.release(&peer);
        assert!(dial(&mut no_dial, peer));
    }
}
