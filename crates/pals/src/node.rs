//! [`Node`]: one running agent, the A2A server its peers reach over Iroh, the
//! blobs it serves them, its signed cards and records, what it answers about
//! itself on `agentnet-discovery/0`, its DHT when it is on, and the local
//! gateway standard A2A clients reach its peers through.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::rest::rest_router;
use a2a_server::{AgentExecutor, DefaultRequestHandler};
use anyhow::{Context, ensure};
use axum::middleware::from_fn;
use futures::StreamExt;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, Watcher};
use iroh_tickets::endpoint::EndpointTicket;
use libp2p::kad::store::MemoryStoreConfig;
use pals_core::card_jws::CardSigner;
use pals_core::clock::{Clock, SystemClock};
use pals_core::discovery::{ALPN_DISCOVERY, DiscoveryContext, Load, LoadProbe, PUBLIC_CARD};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::view::{CursorKey, ViewCache};
use pals_core::store::{DEDUP_RETENTION_S, Store};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::a2a::auth::{Authenticator, JSONRPC_PATH, REST_PATH};
use crate::a2a::{
    BindingExecutor, CardSet, GuardedHandler, OfferCheck, SqliteTaskStore, echo_extensions,
    jsonrpc_shim, rest_shim,
};
use crate::blobs::{BLOBS_DIR, BlobService};
use crate::config::{Config, DataDirLock, DhtMode, WorkerCfg};
use crate::dht::swarm::STORE_LIMITS;
use crate::dht::transport::{ALPN_KAD, IrohListener, KadQuotas};
use crate::dht::{Dht, DhtParts, DhtTiming, PublishedRecords};
use crate::discovery::{
    ContactSource, DiscoveryClient, DiscoveryHook, DiscoveryLimits, DiscoveryServer, Quotas,
    Records, Searcher, announced_capabilities, iroh_contact, renew_on_change,
};
use crate::gateway::{self, GatewayParts, GatewayState};
use crate::inventory::mcp_adapter::MCP_ADAPTER_TIMEOUT;
use crate::inventory::{InventoryState, McpAdapter};
use crate::net::{ALPN_A2A_HTTP, PeerHttp, TunnelLimits, TunnelServer};
use crate::outbox::Outbox;
use crate::worker::TASKS_DIR;
use crate::worker::echo::EchoExecutor;
#[cfg(unix)]
use crate::worker::exec::ExecAdapter;
use crate::worker::tck::TckExecutor;
#[cfg(unix)]
use crate::worker::{Capacity, WorkerExecutor};

/// The agent's SQLite store, in the data directory.
pub const STORE_FILE: &str = "pals.db";

/// How often a running node purges the dedup rows older than
/// [`DEDUP_RETENTION_S`], besides once when it starts.
const DEDUP_PURGE_INTERVAL: Duration = Duration::from_secs(3600);

/// How often a running node issues again the records that need it
/// ([`Records::renew`]), besides once when it starts.
const RECORD_RENEW_INTERVAL: Duration = Duration::from_secs(3600);

/// How long a node keeps an offer it issued once it expired: a task that
/// names it is refused as `offer_expired`, not `offer_unknown`, meanwhile.
const OFFER_RETENTION_S: i64 = 24 * 3600;

/// How long [`Node::shutdown`] waits for the gateway's open connections to
/// finish before dropping them.
const GATEWAY_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// What a node runs with besides its configuration and executor: the clock
/// its quotas, records, pages, offers and observations follow, the load its
/// offers report, and, for tests, a hook in its discovery server, other
/// quotas, other limits for its searches, another time for its inventory
/// adapter, and other times, quotas and store limits for its DHT.
pub struct NodeOptions {
    pub clock: Arc<dyn Clock>,
    /// The worker's load; none for a worker without bounds.
    pub load: Option<Arc<dyn LoadProbe>>,
    /// Awaited before every discovery answer. Tests only.
    pub discovery_hook: Option<Arc<dyn DiscoveryHook>>,
    pub quotas: Quotas,
    /// The limits of the node's searches instead of those of `[discovery]`,
    /// beyond its caps: for tests on loaded hosts, which give candidates
    /// longer than a real search does.
    pub search_limits: Option<DiscoveryLimits>,
    /// How long the inventory adapter takes at most to observe one MCP
    /// connection, [`MCP_ADAPTER_TIMEOUT`] by default: for tests, which
    /// give up sooner on a server that hangs, or never.
    pub mcp_adapter_timeout: Duration,
    /// The DHT's times instead of those of `[dht]` ([`DhtTiming::from_config`]):
    /// for tests, which make them short.
    pub dht_timing: Option<DhtTiming>,
    /// The limits of the DHT's connections and streams.
    pub kad_quotas: KadQuotas,
    /// What the DHT's store holds at most, as a router.
    pub kad_store: MemoryStoreConfig,
}

impl Default for NodeOptions {
    /// The system's clock, no load, no hook, the default quotas, the
    /// search limits of the configuration, [`MCP_ADAPTER_TIMEOUT`], the
    /// DHT's times of the configuration, its default quotas and
    /// [`STORE_LIMITS`].
    fn default() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            load: None,
            discovery_hook: None,
            quotas: Quotas::default(),
            search_limits: None,
            mcp_adapter_timeout: MCP_ADAPTER_TIMEOUT,
            dht_timing: None,
            kad_quotas: KadQuotas::default(),
            kad_store: STORE_LIMITS,
        }
    }
}

/// A running agent: its A2A server, served to peers over Iroh on
/// [`ALPN_A2A_HTTP`], its blobs, served on [`iroh_blobs::ALPN`] to the peers
/// it shared them with, what it answers about itself on [`ALPN_DISCOVERY`]
/// ([`DiscoveryServer`]), the clients it reaches peers with, and its local
/// [gateway] on `gateway.listen`; with `[dht] enabled`, its [`Dht`] too,
/// served on [`ALPN_KAD`].
///
/// The server is the SDK's, with the signed [public card](CardSet::public)
/// at `/.well-known/agent-card.json` and the extended card of each request's
/// scope for `GetExtendedAgentCard` ([`CardSet`]), JSON-RPC at `/jsonrpc`
/// (behind `jsonrpc_shim`) and HTTP+JSON at `/rest` (behind
/// `rest_shim`), both with `echo_extensions`, behind grant
/// authentication ([`crate::a2a::auth`]) and the ownership of tasks,
/// idempotency of messages and the check of the offer a task names
/// ([`GuardedHandler`], [`OfferCheck`]), on the durable task store. Its
/// searches for a provider among its contacts ([`Searcher`]) keep the
/// limits of `[discovery]` for the whole node.
pub struct Node {
    pub id: AgentId,
    pub store: Arc<Store>,
    /// Requests to peers, from this agent's own endpoint and identity.
    pub peer_http: PeerHttp,
    /// The agent's files, in `<data_dir>/`[`BLOBS_DIR`].
    pub blobs: BlobService,
    /// Discovery requests to peers.
    discovery: DiscoveryClient,
    /// The questions this agent asks its peers, through its gateway.
    outbox: Outbox,
    /// The search for a provider among the contacts, through the outbox.
    searcher: Searcher,
    /// The inventory, as read when the node started, with what its adapters
    /// observe.
    inventory: InventoryState,
    /// The agent's cards, signed when the node started.
    cards: CardSet,
    /// The agent's own contact and announce records, current.
    records: Records,
    /// The DHT, when `[dht] enabled`.
    dht: Option<Dht>,
    router: Router,
    gateway: GatewayServer,
    /// Purges old dedup rows, expired pages of peers' views and expired
    /// offers every [`DEDUP_PURGE_INTERVAL`] while the node runs.
    dedup_purge: AbortOnDrop,
    /// Renews the records every [`RECORD_RENEW_INTERVAL`] while the node
    /// runs.
    records_renewal: AbortOnDrop,
    /// Renews the records when the endpoint's addresses change.
    address_renewal: AbortOnDrop,
    /// Runs the inventory adapters when the node starts and every
    /// `[inventory] refresh_interval_s` ([`InventoryState`]).
    inventory_refresh: AbortOnDrop,
    /// Held while the node runs.
    _lock: DataDirLock,
}

impl Node {
    /// Starts the agent of `cfg.data_dir`, which must exist, with the
    /// executor `cfg.worker` selects: [`EchoExecutor`], [`TckExecutor`] with
    /// the streaming timeout of its environment variable, or a
    /// `WorkerExecutor` running the `exec` adapter.
    ///
    /// In order: takes the data directory's lock, failing with
    /// [`AlreadyRunning`](crate::config::AlreadyRunning) when another daemon
    /// holds it; loads the agent's key, creating it if missing; opens the
    /// store; reads the inventory ([`InventoryState::load`]) and signs the
    /// cards it gives ([`CardSet::build`]), failing when the inventory is
    /// invalid, when a card breaks a card limit or when the public services
    /// have more capabilities than an announce record lists
    /// ([`announced_capabilities`]); fails the tasks a previous run left in
    /// progress and removes
    /// their working directories ([`TASKS_DIR`]); frees the dedup keys it
    /// left with no execution behind them, and purges the dedup rows older
    /// than [`DEDUP_RETENTION_S`], and the expired pages of peers' views and
    /// offers; logs how many questions of its own
    /// [outbox](Outbox) are still pending, and leaves them for
    /// [`Outbox::resume`]: it never sends them again on its own; binds the
    /// gateway's listener on `gateway.listen`, which must be a loopback
    /// address (port 0 picks a free one, see [`Node::gateway_url`]); binds
    /// the Iroh endpoint with the agent's key and opens the blob store in
    /// `<data_dir>/`[`BLOBS_DIR`]; takes up its contact and announce records,
    /// issuing those that need it ([`Records::start`]); and only then serves
    /// [`ALPN_A2A_HTTP`], [`iroh_blobs::ALPN`] and [`ALPN_DISCOVERY`] on the
    /// endpoint and starts the gateway, and then, in the background, the
    /// first run of the inventory adapters, which delays none of it
    /// ([`InventoryState::startup_refresh_done`]). With `net.relay` the
    /// endpoint uses Iroh's N0 preset and its public relays; without it, it
    /// has no relay and is bound to 127.0.0.1 only. With `net.listen`, it is
    /// bound to that address alone, relay or not. With `[dht] enabled`, it
    /// starts the DHT on the endpoint before it serves, once its records
    /// are in force, failing on a bootstrap ticket that is not one or is its
    /// own ([`Dht::spawn`]), and serves [`ALPN_KAD`] with the rest; with
    /// `[dht] announce` too, the DHT publishes those records, and again each
    /// time one is renewed ([`crate::dht::publish`]). While it runs, the node
    /// purges the dedup rows, the expired pages of peers' views and the
    /// expired offers again and renews its records every hour, renews them
    /// too whenever the endpoint's addresses change, and runs the inventory
    /// adapters every `[inventory] refresh_interval_s`. With the `exec`
    /// worker, its offers report the worker's load
    /// ([`WorkerExecutor::load_probe`]); with the others, a load without
    /// bounds.
    pub async fn spawn(cfg: Config) -> anyhow::Result<Node> {
        #[cfg(not(unix))]
        if matches!(cfg.worker, WorkerCfg::Exec(_)) {
            anyhow::bail!("the exec worker adapter needs a Unix system");
        }
        let opts = NodeOptions::default();
        let opened = Opened::open(&cfg, &opts).await?;
        match &cfg.worker {
            WorkerCfg::Echo => Ok(opened.start(&cfg, EchoExecutor, opts)),
            WorkerCfg::Tck => Ok(opened.start(&cfg, TckExecutor::from_env(), opts)),
            #[cfg(unix)]
            WorkerCfg::Exec(exec) => {
                let adapter = Arc::new(ExecAdapter::new(exec.clone(), &cfg.projects));
                let executor =
                    WorkerExecutor::new(adapter, &cfg, Capacity::from(exec), opened.blobs.clone());
                // Its offers say how busy it is.
                let opts = NodeOptions {
                    load: Some(executor.load_probe()),
                    ..opts
                };
                Ok(opened.start(&cfg, executor, opts))
            }
            #[cfg(not(unix))]
            WorkerCfg::Exec(_) => unreachable!("refused above"),
        }
    }

    /// [`Node::spawn`], with `executor` doing the work instead of the
    /// executor `cfg.worker` selects.
    pub async fn spawn_with_executor(
        cfg: Config,
        executor: impl AgentExecutor,
    ) -> anyhow::Result<Node> {
        Self::spawn_with_options(cfg, executor, NodeOptions::default()).await
    }

    /// [`Node::spawn_with_executor`], with `opts`.
    pub async fn spawn_with_options(
        cfg: Config,
        executor: impl AgentExecutor,
        opts: NodeOptions,
    ) -> anyhow::Result<Node> {
        Ok(Opened::open(&cfg, &opts).await?.start(&cfg, executor, opts))
    }

    /// The address peers dial this node at: its id and the direct addresses
    /// and relay it is reachable on, a DHT router's `[dht] external_addrs`
    /// among them.
    pub fn addr(&self) -> EndpointAddr {
        dialable_addr(self.router.endpoint(), self.external_addrs())
    }

    /// [`Node::addr`] as an Iroh endpoint ticket, to hand to a peer: a DHT
    /// router's invitation.
    pub fn ticket(&self) -> String {
        ticket_of(self.router.endpoint(), self.external_addrs())
    }

    /// The node's DHT, when `[dht] enabled`.
    pub fn dht(&self) -> Option<&Dht> {
        self.dht.as_ref()
    }

    /// A DHT router's `[dht] external_addrs`.
    fn external_addrs(&self) -> &[SocketAddr] {
        self.dht.as_ref().map_or(&[], Dht::external_addrs)
    }

    /// The URL of the local gateway, from the address it is bound to:
    /// `http://127.0.0.1:<port>` with the default `gateway.listen`.
    pub fn gateway_url(&self) -> String {
        self.gateway.url.clone()
    }

    /// The questions this agent asks its peers, through its gateway.
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// The client this agent asks its peers with on `agentnet-discovery/0`.
    pub fn discovery(&self) -> &DiscoveryClient {
        &self.discovery
    }

    /// The search for a provider among this agent's contacts, with the
    /// limits of `[discovery]` for the whole daemon.
    pub fn searcher(&self) -> &Searcher {
        &self.searcher
    }

    /// The agent's cards, signed when the node started.
    pub fn cards(&self) -> &CardSet {
        &self.cards
    }

    /// The agent's own contact and announce records, current.
    pub fn records(&self) -> &Records {
        &self.records
    }

    /// The inventory, as read when the node started, with what its adapters
    /// observe.
    pub fn inventory(&self) -> &InventoryState {
        &self.inventory
    }

    /// Drops the connections to peers this agent holds, as a network cut
    /// would; the next request to a peer dials it again.
    pub async fn close_peer_connections(&self) {
        self.peer_http.close_all().await;
    }

    /// Stops the gateway from accepting requests and ends its MCP sessions,
    /// stops purging and renewing records and running the inventory adapters
    /// (a server they started is killed), stops the DHT, closes the
    /// connections to peers, stops serving, closes the endpoint, lets the
    /// gateway's open connections finish for up to `GATEWAY_STOP_TIMEOUT`,
    /// stops the blob service, and only then lets go of the store and, last,
    /// of the data directory's lock.
    ///
    /// Every task of the node's own (the purge, the record renewal, the
    /// gateway, the blob provider's authorizer, the DHT) is over when it
    /// returns, so the store is closed then unless an execution still
    /// running or a connection still being torn down holds it: a node
    /// started on the same data directory in the same process opens the
    /// database alone once those are gone too.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        let Node {
            id: _,
            store,
            peer_http,
            blobs,
            discovery,
            outbox,
            searcher,
            inventory,
            cards,
            records,
            dht,
            router,
            gateway,
            dedup_purge,
            records_renewal,
            address_renewal,
            inventory_refresh,
            _lock: lock,
        } = self;
        let GatewayServer {
            url: _,
            stopping,
            stop,
            task: mut gateway_task,
        } = gateway;
        // The MCP sessions' streams would hold the server open.
        stopping.cancel();
        // The server may have stopped already; then there is nothing to tell.
        let _ = stop.send(());
        dedup_purge.stop().await;
        records_renewal.stop().await;
        address_renewal.stop().await;
        inventory_refresh.stop().await;
        if let Some(dht) = &dht {
            dht.shutdown().await;
        }
        peer_http.close_all().await;
        discovery.close_all().await;
        let stopped = router
            .shutdown()
            .await
            .context("the node did not shut down cleanly");
        if tokio::time::timeout(GATEWAY_STOP_TIMEOUT, &mut gateway_task.0)
            .await
            .is_err()
        {
            tracing::debug!("dropping the gateway connections still open");
            gateway_task.stop().await;
        }
        blobs.shutdown().await;
        drop((
            searcher, outbox, blobs, peer_http, discovery, inventory, cards, records, dht,
        ));
        tracing::debug!(
            handles = Arc::strong_count(&store),
            "the store's handles at shutdown, this one included"
        );
        drop(store);
        drop(lock);
        stopped
    }
}

/// What a node runs on, open and bound, before it serves anything.
struct Opened {
    lock: DataDirLock,
    key: Arc<AgentKey>,
    store: Arc<Store>,
    inventory: InventoryState,
    cards: CardSet,
    records: Records,
    listener: TcpListener,
    gateway_addr: SocketAddr,
    endpoint: Endpoint,
    blobs: BlobService,
    /// The DHT, running, and the handler of [`ALPN_KAD`] to serve.
    dht: Option<(Dht, IrohListener)>,
}

impl Opened {
    /// Everything [`Node::spawn`] does before it serves, at the time of
    /// `opts`' clock.
    async fn open(cfg: &Config, opts: &NodeOptions) -> anyhow::Result<Self> {
        ensure!(
            cfg.gateway.listen.ip().is_loopback(),
            "gateway.listen must be a loopback address, not {}",
            cfg.gateway.listen
        );
        let blocking_cfg = cfg.clone();
        let started = opts.clock.now_unix();
        let (adapter_clock, adapter_timeout) = (Arc::clone(&opts.clock), opts.mcp_adapter_timeout);
        let (lock, key, store, inventory, cards, capabilities) =
            tokio::task::spawn_blocking(move || {
                let cfg = blocking_cfg;
                let data_dir = &cfg.data_dir;
                let key_path = cfg.key_path();
                let lock = DataDirLock::acquire(data_dir)?;
                let key = AgentKey::load_or_create(&key_path)
                    .with_context(|| format!("cannot load {}", key_path.display()))?;
                let key = Arc::new(key);
                let store = Arc::new(Store::open(&data_dir.join(STORE_FILE), key.id())?);
                // Before anything is bound or changed: an inventory the node
                // cannot serve stops it here.
                let inventory = InventoryState::load_with(
                    &cfg,
                    Arc::clone(&store),
                    McpAdapter::new(adapter_timeout),
                    adapter_clock,
                )?;
                let snapshot = inventory.snapshot();
                let cards = CardSet::build(
                    &cfg,
                    &CardSigner::new(Arc::clone(&key)),
                    &snapshot.inventory,
                )?;
                let capabilities = announced_capabilities(&snapshot.inventory)?;
                let interrupted = store
                    .tasks_fail_interrupted(chrono::Utc::now().timestamp())
                    .context("cannot fail the tasks a restart interrupted")?;
                if interrupted > 0 {
                    tracing::info!(interrupted, "failed the tasks a restart interrupted");
                }
                remove_task_dirs(data_dir);
                // An unbound key belongs to a request that never ran: free it,
                // so that its retry runs instead of waiting for nothing.
                let unbound = store
                    .dedup_clear_unbound()
                    .context("cannot free the dedup keys a restart left unbound")?;
                if unbound > 0 {
                    tracing::info!(unbound, "freed the dedup keys a restart left unbound");
                }
                purge_dedup(&store).context("cannot purge the old dedup rows")?;
                purge_inventory_cache(&store, started)
                    .context("cannot purge the expired pages of peers' views")?;
                purge_offers(&store, started).context("cannot purge the expired offers")?;
                let pending = store
                    .outbox_pending()
                    .context("cannot read the outbox")?
                    .len();
                if pending > 0 {
                    tracing::info!(
                        pending,
                        "questions to peers still pending, left for `pals task resume`"
                    );
                }
                anyhow::Ok((lock, key, store, inventory, cards, capabilities))
            })
            .await??;

        let listener = TcpListener::bind(cfg.gateway.listen)
            .await
            .with_context(|| format!("cannot listen on {} for the gateway", cfg.gateway.listen))?;
        let gateway_addr = listener
            .local_addr()
            .context("the gateway listener has no address")?;

        let endpoint = bind_endpoint(cfg, &key).await?;
        ensure!(
            endpoint.id().as_bytes() == key.id().as_bytes(),
            "the Iroh endpoint id is not the agent id"
        );
        let blobs = BlobService::open(
            &cfg.data_dir.join(BLOBS_DIR),
            endpoint.clone(),
            Arc::clone(&store),
            Arc::clone(&key),
        )
        .await?;
        let records = {
            let id = key.id();
            let dialed = endpoint.clone();
            let external = router_external_addrs(cfg);
            let contact_source: ContactSource =
                Arc::new(move || iroh_contact(id, &dialable_addr(&dialed, &external)));
            let (key, store) = (Arc::clone(&key), Arc::clone(&store));
            let card_hash = cards.public().hash.clone();
            let now = opts.clock.now_unix();
            tokio::task::spawn_blocking(move || {
                Records::start(key, store, contact_source, card_hash, capabilities, now)
            })
            .await?
            .context("cannot issue this agent's signed records")?
        };
        let dht = if cfg.dht.enabled {
            let parts = DhtParts {
                endpoint: endpoint.clone(),
                key: Arc::clone(&key),
                cfg: cfg.dht.clone(),
                timing: opts
                    .dht_timing
                    .unwrap_or_else(|| DhtTiming::from_config(&cfg.dht)),
                quotas: opts.kad_quotas,
                store_limits: opts.kad_store.clone(),
                clock: Arc::clone(&opts.clock),
                store: Arc::clone(&store),
                // Only the public capabilities, which the records list
                // (H3-D4); nothing at all without `announce`.
                published: cfg
                    .dht
                    .announce
                    .then(|| Arc::new(records.clone()) as Arc<dyn PublishedRecords>),
            };
            Some(Dht::spawn(parts).context("cannot start the DHT")?)
        } else {
            None
        };
        Ok(Self {
            lock,
            key,
            store,
            inventory,
            cards,
            records,
            listener,
            gateway_addr,
            endpoint,
            blobs,
            dht,
        })
    }

    /// Serves the A2A server with `executor`, the blobs and the discovery
    /// protocol on the endpoint, and starts the gateway.
    fn start(self, cfg: &Config, executor: impl AgentExecutor, opts: NodeOptions) -> Node {
        let Self {
            lock,
            key,
            store,
            inventory,
            cards,
            records,
            listener,
            gateway_addr,
            endpoint,
            blobs,
            dht,
        } = self;
        let id = key.id();
        let limits = TunnelLimits {
            max_body_bytes: cfg.limits.max_body_bytes,
            ..TunnelLimits::default()
        };
        let NodeOptions {
            clock,
            load,
            discovery_hook,
            quotas,
            search_limits,
            // The inventory's adapter and the DHT have them already.
            mcp_adapter_timeout: _,
            dht_timing: _,
            kad_quotas: _,
            kad_store: _,
        } = opts;
        let offers = OfferCheck {
            cards: cards.clone(),
            inventory: inventory.clone(),
            clock: Arc::clone(&clock),
        };
        let app = app(cfg, &key, &store, offers, executor);
        let discovery_server = {
            let ctx = discovery_context(cfg, &key, &store, &inventory, &cards, &records, load);
            let server = DiscoveryServer::new(ctx, quotas, Arc::clone(&clock));
            match discovery_hook {
                Some(hook) => server.with_hook(hook),
                None => server,
            }
        };
        let router = Router::builder(endpoint.clone())
            .accept(ALPN_A2A_HTTP, TunnelServer::new(app, limits))
            .accept(iroh_blobs::ALPN, blobs.protocol())
            .accept(ALPN_DISCOVERY, discovery_server);
        let (router, dht) = match dht {
            Some((dht, listener)) => (router.accept(ALPN_KAD, listener), Some(dht)),
            None => (router, None),
        };
        let router = router.spawn();
        let peer_http = PeerHttp::new(endpoint.clone());
        let discovery = DiscoveryClient::new(endpoint.clone(), Arc::clone(&store))
            .with_clock(Arc::clone(&clock));
        // The endpoint's addresses now, and then each change of them: the
        // first relay or direct address it learns after it is bound, also
        // one learnt since the records were taken up, which the first item
        // catches (an item that changes nothing issues nothing).
        let address_changes = endpoint.watch_addr().stream().map(|_| ());
        let address_renewal = AbortOnDrop(tokio::spawn(renew_on_change(
            records.clone(),
            address_changes,
            Arc::clone(&clock),
        )));
        let state = GatewayState::new(
            GatewayParts {
                id,
                name: cfg.agent.name.clone(),
                store: Arc::clone(&store),
                peer_http: peer_http.clone(),
                discovery: discovery.clone(),
                clock: Arc::clone(&clock),
                endpoint,
                blobs: blobs.clone(),
                limits: cfg.limits.clone(),
                discovery_limits: search_limits
                    .unwrap_or_else(|| DiscoveryLimits::from_config(&cfg.discovery)),
                inventory: inventory.clone(),
                data_dir: cfg.data_dir.clone(),
                download_dir: cfg.mcp.download_dir.clone(),
                dht: dht.clone(),
            },
            gateway_addr,
        );
        let outbox = state.outbox().clone();
        let searcher = state.searcher().clone();
        let gateway = GatewayServer::start(listener, state);
        let dedup_purge = AbortOnDrop(tokio::spawn(purge_hourly(
            Arc::clone(&store),
            Arc::clone(&clock),
        )));
        let records_renewal =
            AbortOnDrop(tokio::spawn(renew_records_hourly(records.clone(), clock)));
        // Last, and in the background: nothing waits for the adapters.
        let inventory_refresh = AbortOnDrop(tokio::spawn(inventory.clone().keep_refreshed()));
        Node {
            id,
            store,
            peer_http,
            blobs,
            discovery,
            outbox,
            searcher,
            inventory,
            cards,
            records,
            dht,
            router,
            gateway,
            dedup_purge,
            records_renewal,
            address_renewal,
            inventory_refresh,
            _lock: lock,
        }
    }
}

/// What the node's discovery server answers with: the agent's key and
/// store, its inventory and records as they are at each answer (never as
/// they were at start), the hash of each card, the issuers it accepts
/// grants of (itself and `trusted_issuers`), whether it shows its public
/// view, this run's cursor key and page cache, the worker's load, and how
/// long its offers last.
fn discovery_context(
    cfg: &Config,
    key: &Arc<AgentKey>,
    store: &Arc<Store>,
    inventory: &InventoryState,
    cards: &CardSet,
    records: &Records,
    load: Option<Arc<dyn LoadProbe>>,
) -> DiscoveryContext {
    let mut card_hashes: BTreeMap<String, _> = cards.scoped_hashes();
    card_hashes.insert(PUBLIC_CARD.to_owned(), cards.public().hash.clone());
    let snapshot = {
        let inventory = inventory.clone();
        Arc::new(move || inventory.snapshot())
    };
    let current = {
        let records = records.clone();
        Arc::new(move || records.current())
    };
    DiscoveryContext {
        key: Arc::clone(key),
        store: Arc::clone(store),
        inventory: snapshot,
        card_hashes,
        records: current,
        accepted_issuers: std::iter::once(key.id())
            .chain(cfg.trusted_issuers.iter().copied())
            .collect(),
        public: cfg.discovery.public,
        cursor_key: CursorKey::random(),
        views: ViewCache::new(),
        load: load.unwrap_or_else(|| Arc::new(Load::unbounded())),
        offer_valid_for_ms: cfg.discovery.offer_valid_for_ms,
    }
}

/// The address peers dial `endpoint` at: its id, the direct addresses and
/// relay it is reachable on, and `external`, a DHT router's
/// `[dht] external_addrs`. Never an unspecified socket: one the endpoint is
/// bound to (`listen = "0.0.0.0:<port>"`) is left out.
pub(crate) fn dialable_addr(endpoint: &Endpoint, external: &[SocketAddr]) -> EndpointAddr {
    let mut addr = endpoint.addr();
    // The bound sockets are dialable when the endpoint is bound to a
    // specific address, as without a relay.
    for socket in endpoint.bound_sockets() {
        if !socket.ip().is_unspecified() {
            addr = addr.with_ip_addr(socket);
        }
    }
    for socket in external {
        addr = addr.with_ip_addr(*socket);
    }
    addr
}

/// [`dialable_addr`] as an Iroh endpoint ticket, to hand to a peer.
pub(crate) fn ticket_of(endpoint: &Endpoint, external: &[SocketAddr]) -> String {
    EndpointTicket::new(dialable_addr(endpoint, external)).to_string()
}

/// The `[dht] external_addrs` of a node that runs the DHT as a router: what
/// its address carries besides its own; none otherwise.
fn router_external_addrs(cfg: &Config) -> Vec<SocketAddr> {
    if cfg.dht.enabled && cfg.dht.mode == DhtMode::Router {
        cfg.dht.external_addrs.clone()
    } else {
        Vec::new()
    }
}

/// The gateway's HTTP server, running on its own task.
struct GatewayServer {
    url: String,
    /// Ends the MCP sessions.
    stopping: CancellationToken,
    /// Tells the server to stop accepting and to close its connections once
    /// their requests are answered.
    stop: oneshot::Sender<()>,
    /// Aborted if the node is dropped without [`Node::shutdown`].
    task: AbortOnDrop,
}

impl GatewayServer {
    fn start(listener: TcpListener, state: GatewayState) -> Self {
        let url = state.base_url();
        let stopping = state.stopping().clone();
        let (stop, stopped) = oneshot::channel::<()>();
        let serve = axum::serve(listener, gateway::router(state)).with_graceful_shutdown(async {
            // Dropping the sender stops the server as well.
            let _ = stopped.await;
        });
        let task = tokio::spawn(async move {
            if let Err(error) = serve.await {
                tracing::warn!(%error, "the gateway stopped serving");
            }
        });
        Self {
            url,
            stopping,
            stop,
            task: AbortOnDrop(task),
        }
    }
}

/// Removes the working directories that tasks a previous run interrupted
/// left in `data_dir` ([`TASKS_DIR`]). Failing to is not fatal: each task
/// makes its own directory anew.
fn remove_task_dirs(data_dir: &Path) {
    let tasks = data_dir.join(TASKS_DIR);
    match std::fs::remove_dir_all(&tasks) {
        Ok(()) => tracing::info!("removed the working directories of interrupted tasks"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            dir = %tasks.display(),
            error = %err,
            "cannot remove the working directories of interrupted tasks"
        ),
    }
}

/// Deletes the dedup rows older than [`DEDUP_RETENTION_S`].
fn purge_dedup(store: &Store) -> anyhow::Result<()> {
    let purged = store.dedup_purge(chrono::Utc::now().timestamp() - DEDUP_RETENTION_S)?;
    if purged > 0 {
        tracing::debug!(purged, "purged old dedup rows");
    }
    Ok(())
}

/// Deletes the pages of peers' views expired at `now_unix`.
fn purge_inventory_cache(store: &Store, now_unix: i64) -> anyhow::Result<()> {
    let purged = store.inventory_cache_purge(now_unix)?;
    if purged > 0 {
        tracing::debug!(purged, "purged expired pages of peers' views");
    }
    Ok(())
}

/// Deletes the offers this agent issued that expired more than
/// [`OFFER_RETENTION_S`] before `now_unix`, and those it received that
/// expired by then.
fn purge_offers(store: &Store, now_unix: i64) -> anyhow::Result<()> {
    let issued = store.offers_issued_purge(now_unix - OFFER_RETENTION_S)?;
    let received = store.offers_received_purge(now_unix)?;
    if issued + received > 0 {
        tracing::debug!(issued, received, "purged expired offers");
    }
    Ok(())
}

/// [`purge_dedup`], and [`purge_inventory_cache`] and [`purge_offers`] at
/// `clock`'s time, every [`DEDUP_PURGE_INTERVAL`], for as long as it runs. A
/// failed purge is logged, and the next one tries again.
async fn purge_hourly(store: Arc<Store>, clock: Arc<dyn Clock>) {
    let start = tokio::time::Instant::now() + DEDUP_PURGE_INTERVAL;
    let mut ticks = tokio::time::interval_at(start, DEDUP_PURGE_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let store = Arc::clone(&store);
        let now = clock.now_unix();
        let purged = tokio::task::spawn_blocking(move || {
            purge_dedup(&store).context("cannot purge the old dedup rows")?;
            purge_inventory_cache(&store, now)
                .context("cannot purge the expired pages of peers' views")?;
            purge_offers(&store, now).context("cannot purge the expired offers")
        })
        .await;
        match purged {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!(error = %format!("{err:#}"), "purge failed"),
            Err(err) => tracing::warn!(error = %err, "the purge did not finish"),
        }
    }
}

/// [`Records::renew`] at `clock`'s time every [`RECORD_RENEW_INTERVAL`], for
/// as long as it runs. A failed renewal is logged, and the next one tries
/// again; the records in force stay meanwhile.
async fn renew_records_hourly(records: Records, clock: Arc<dyn Clock>) {
    let start = tokio::time::Instant::now() + RECORD_RENEW_INTERVAL;
    let mut ticks = tokio::time::interval_at(start, RECORD_RENEW_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let records = records.clone();
        let now = clock.now_unix();
        let renewed = tokio::task::spawn_blocking(move || records.renew(now)).await;
        match renewed {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "cannot renew this agent's records")
            }
            Err(err) => tracing::warn!(error = %err, "the record renewal did not finish"),
        }
    }
}

/// A spawned task, aborted when this value is dropped.
struct AbortOnDrop(JoinHandle<()>);

impl AbortOnDrop {
    /// Aborts the task, unless it is over, and waits until it is. Not for a
    /// task already awaited to its end.
    async fn stop(mut self) {
        self.0.abort();
        // Cancelled, or over before; either way it is done.
        let _ = (&mut self.0).await;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The agent's Iroh endpoint, with the agent's key as its identity: bound
/// to `net.listen` alone when it is set, with the relay or without it;
/// otherwise as in milestone 2, the N0 preset's sockets with the relay and
/// a free port of 127.0.0.1 without it.
async fn bind_endpoint(cfg: &Config, key: &AgentKey) -> anyhow::Result<Endpoint> {
    let secret_key = SecretKey::from_bytes(&key.to_bytes());
    let builder = if cfg.net.relay {
        Endpoint::builder(presets::N0).secret_key(secret_key)
    } else {
        Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
    };
    let bind_addr = match cfg.net.listen {
        Some(listen) => Some(listen),
        None if !cfg.net.relay => Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
        None => None,
    };
    let builder = match bind_addr {
        Some(addr) => builder
            .clear_ip_transports()
            .bind_addr(addr)
            .with_context(|| format!("invalid bind address {addr}"))?,
        None => builder,
    };
    builder
        .bind()
        .await
        .context("cannot bind the Iroh endpoint")
}

/// The A2A server the node serves to its peers, checking again with
/// `offers` the offer a task names.
fn app(
    cfg: &Config,
    key: &Arc<AgentKey>,
    store: &Arc<Store>,
    offers: OfferCheck,
    executor: impl AgentExecutor,
) -> axum::Router {
    let cards = offers.cards.clone();
    let id = key.id();
    let tasks = SqliteTaskStore::new(Arc::clone(store)).with_events(Arc::clone(key));
    // Each execution is bound to its request before it runs.
    let executor = BindingExecutor::new(executor, Arc::clone(store));
    // The SDK refuses what the card does not declare (push notifications),
    // and answers `GetExtendedAgentCard` with the card of the request's
    // scope, which the grant layer set.
    let inner = DefaultRequestHandler::new(executor, tasks)
        .with_capabilities(cards.public().card.capabilities.clone())
        .with_extended_agent_card_resolver(cards.clone());
    let handler = Arc::new(
        GuardedHandler::new(inner, Arc::clone(store), Arc::clone(key)).with_offers(offers),
    );
    let app = axum::Router::new()
        .merge(agent_card_router(Arc::new(cards.clone())))
        .nest(
            JSONRPC_PATH,
            jsonrpc_router(Arc::clone(&handler))
                .layer(from_fn(echo_extensions))
                .layer(from_fn(jsonrpc_shim)),
        )
        .nest(
            REST_PATH,
            rest_router(handler)
                .layer(from_fn(echo_extensions))
                .layer(from_fn(rest_shim)),
        );
    Authenticator::new(
        id,
        &cfg.trusted_issuers,
        Arc::clone(store),
        cfg.limits.max_body_bytes,
    )
    .guard(app)
}
