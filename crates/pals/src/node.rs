//! [`Node`]: one running agent, the A2A server its peers reach over Iroh, the
//! blobs it serves them, and the local gateway standard A2A clients reach its
//! peers through.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::rest::rest_router;
use a2a_server::{AgentExecutor, DefaultRequestHandler, StaticAgentCard};
use anyhow::{Context, ensure};
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::{DEDUP_RETENTION_S, Store};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::a2a::auth::{Authenticator, JSONRPC_PATH, REST_PATH};
use crate::a2a::{BindingExecutor, GuardedHandler, SqliteTaskStore, canonical_card, rest_shim};
use crate::blobs::{BLOBS_DIR, BlobService};
use crate::config::{Config, DataDirLock, WorkerCfg};
use crate::gateway::{self, GatewayParts, GatewayState};
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

/// How long [`Node::shutdown`] waits for the gateway's open connections to
/// finish before dropping them.
const GATEWAY_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// A running agent: its A2A server, served to peers over Iroh on
/// [`ALPN_A2A_HTTP`], its blobs, served on [`iroh_blobs::ALPN`] to the peers
/// it shared them with, the client it reaches peers with, and its local
/// [gateway](crate::gateway) on `gateway.listen`.
///
/// The server is the SDK's, with the [canonical card](canonical_card) at
/// `/.well-known/agent-card.json`, JSON-RPC at `/jsonrpc` and HTTP+JSON at
/// `/rest` (behind [`rest_shim`]), behind grant authentication
/// ([`crate::a2a::auth`]) and the ownership of tasks and idempotency of
/// messages ([`GuardedHandler`]), on the durable task store.
pub struct Node {
    pub id: AgentId,
    pub store: Arc<Store>,
    /// Requests to peers, from this agent's own endpoint and identity.
    pub peer_http: PeerHttp,
    /// The agent's files, in `<data_dir>/`[`BLOBS_DIR`].
    pub blobs: BlobService,
    /// The questions this agent asks its peers, through its gateway.
    outbox: Outbox,
    router: Router,
    gateway: GatewayServer,
    /// Purges old dedup rows every [`DEDUP_PURGE_INTERVAL`] while the node
    /// runs.
    dedup_purge: AbortOnDrop,
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
    /// store; fails the tasks a previous run left in progress and removes
    /// their working directories ([`TASKS_DIR`]); frees the dedup keys it
    /// left with no execution behind them, and purges the dedup rows older
    /// than [`DEDUP_RETENTION_S`]; logs how many questions of its own
    /// [outbox](Outbox) are still pending, and leaves them for
    /// [`Outbox::resume`]: it never sends them again on its own; binds the
    /// gateway's listener on `gateway.listen`, which must be a loopback
    /// address (port 0 picks a free one, see [`Node::gateway_url`]); binds
    /// the Iroh endpoint with the agent's key and opens the blob store in
    /// `<data_dir>/`[`BLOBS_DIR`]; and only then serves [`ALPN_A2A_HTTP`]
    /// and [`iroh_blobs::ALPN`] on the endpoint and starts the gateway. With
    /// `net.relay` the endpoint uses Iroh's N0 preset and its public relays;
    /// without it, it has no relay and is bound to 127.0.0.1 only. While it
    /// runs, the node purges the dedup rows again every hour.
    pub async fn spawn(cfg: Config) -> anyhow::Result<Node> {
        #[cfg(not(unix))]
        if matches!(cfg.worker, WorkerCfg::Exec(_)) {
            anyhow::bail!("the exec worker adapter needs a Unix system");
        }
        let opened = Opened::open(&cfg).await?;
        match &cfg.worker {
            WorkerCfg::Echo => Ok(opened.start(&cfg, EchoExecutor)),
            WorkerCfg::Tck => Ok(opened.start(&cfg, TckExecutor::from_env())),
            #[cfg(unix)]
            WorkerCfg::Exec(exec) => {
                let adapter = Arc::new(ExecAdapter::new(exec.clone(), &cfg.projects));
                let executor =
                    WorkerExecutor::new(adapter, &cfg, Capacity::from(exec), opened.blobs.clone());
                Ok(opened.start(&cfg, executor))
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
        Ok(Opened::open(&cfg).await?.start(&cfg, executor))
    }

    /// The address peers dial this node at: its id and the direct addresses
    /// and relay it is reachable on.
    pub fn addr(&self) -> EndpointAddr {
        dialable_addr(self.router.endpoint())
    }

    /// [`Node::addr`] as an Iroh endpoint ticket, to hand to a peer.
    pub fn ticket(&self) -> String {
        ticket_of(self.router.endpoint())
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

    /// Drops the connections to peers this agent holds, as a network cut
    /// would; the next request to a peer dials it again.
    pub async fn close_peer_connections(&self) {
        self.peer_http.close_all().await;
    }

    /// Stops the gateway from accepting requests and ends its MCP sessions,
    /// stops purging, closes the connections to peers, stops serving,
    /// closes the endpoint, lets the gateway's open connections finish for
    /// up to [`GATEWAY_STOP_TIMEOUT`], and releases the data directory.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        let GatewayServer {
            url: _,
            stopping,
            stop,
            mut task,
        } = self.gateway;
        // The MCP sessions' streams would hold the server open.
        stopping.cancel();
        // The server may have stopped already; then there is nothing to tell.
        let _ = stop.send(());
        drop(self.dedup_purge);
        self.peer_http.close_all().await;
        let stopped = self
            .router
            .shutdown()
            .await
            .context("the node did not shut down cleanly");
        if tokio::time::timeout(GATEWAY_STOP_TIMEOUT, &mut task.0)
            .await
            .is_err()
        {
            tracing::debug!("dropping the gateway connections still open");
        }
        stopped
    }
}

/// What a node runs on, open and bound, before it serves anything.
struct Opened {
    lock: DataDirLock,
    key: Arc<AgentKey>,
    store: Arc<Store>,
    listener: TcpListener,
    gateway_addr: SocketAddr,
    endpoint: Endpoint,
    blobs: BlobService,
}

impl Opened {
    /// Everything [`Node::spawn`] does before it serves.
    async fn open(cfg: &Config) -> anyhow::Result<Self> {
        ensure!(
            cfg.gateway.listen.ip().is_loopback(),
            "gateway.listen must be a loopback address, not {}",
            cfg.gateway.listen
        );
        let data_dir = cfg.data_dir.clone();
        let key_path = cfg.key_path();
        let (lock, key, store) = tokio::task::spawn_blocking(move || {
            let lock = DataDirLock::acquire(&data_dir)?;
            let key = AgentKey::load_or_create(&key_path)
                .with_context(|| format!("cannot load {}", key_path.display()))?;
            let store = Store::open(&data_dir.join(STORE_FILE), key.id())?;
            let interrupted = store
                .tasks_fail_interrupted(chrono::Utc::now().timestamp())
                .context("cannot fail the tasks a restart interrupted")?;
            if interrupted > 0 {
                tracing::info!(interrupted, "failed the tasks a restart interrupted");
            }
            remove_task_dirs(&data_dir);
            // An unbound key belongs to a request that never ran: free it,
            // so that its retry runs instead of waiting for nothing.
            let unbound = store
                .dedup_clear_unbound()
                .context("cannot free the dedup keys a restart left unbound")?;
            if unbound > 0 {
                tracing::info!(unbound, "freed the dedup keys a restart left unbound");
            }
            purge_dedup(&store).context("cannot purge the old dedup rows")?;
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
            anyhow::Ok((lock, key, store))
        })
        .await??;
        let key = Arc::new(key);
        let store = Arc::new(store);

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
        Ok(Self {
            lock,
            key,
            store,
            listener,
            gateway_addr,
            endpoint,
            blobs,
        })
    }

    /// Serves the A2A server with `executor` and the blobs on the endpoint,
    /// and starts the gateway.
    fn start(self, cfg: &Config, executor: impl AgentExecutor) -> Node {
        let Self {
            lock,
            key,
            store,
            listener,
            gateway_addr,
            endpoint,
            blobs,
        } = self;
        let id = key.id();
        let limits = TunnelLimits {
            max_body_bytes: cfg.limits.max_body_bytes,
            ..TunnelLimits::default()
        };
        let app = app(cfg, &key, &store, executor);
        let router = Router::builder(endpoint.clone())
            .accept(ALPN_A2A_HTTP, TunnelServer::new(app, limits))
            .accept(iroh_blobs::ALPN, blobs.protocol())
            .spawn();
        let peer_http = PeerHttp::new(endpoint.clone());
        let state = GatewayState::new(
            GatewayParts {
                id,
                name: cfg.agent.name.clone(),
                store: Arc::clone(&store),
                peer_http: peer_http.clone(),
                endpoint,
                blobs: blobs.clone(),
                limits: cfg.limits.clone(),
                data_dir: cfg.data_dir.clone(),
            },
            gateway_addr,
        );
        let outbox = state.outbox().clone();
        let gateway = GatewayServer::start(listener, state);
        let dedup_purge = AbortOnDrop(tokio::spawn(purge_dedup_hourly(Arc::clone(&store))));
        Node {
            id,
            store,
            peer_http,
            blobs,
            outbox,
            router,
            gateway,
            dedup_purge,
            _lock: lock,
        }
    }
}

/// The address peers dial `endpoint` at: its id and the direct addresses and
/// relay it is reachable on.
fn dialable_addr(endpoint: &Endpoint) -> EndpointAddr {
    let mut addr = endpoint.addr();
    // The bound sockets are dialable when the endpoint is bound to a
    // specific address, as without a relay.
    for socket in endpoint.bound_sockets() {
        if !socket.ip().is_unspecified() {
            addr = addr.with_ip_addr(socket);
        }
    }
    addr
}

/// [`dialable_addr`] as an Iroh endpoint ticket, to hand to a peer.
pub(crate) fn ticket_of(endpoint: &Endpoint) -> String {
    EndpointTicket::new(dialable_addr(endpoint)).to_string()
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

/// [`purge_dedup`] every [`DEDUP_PURGE_INTERVAL`], for as long as it runs. A
/// failed purge is logged, and the next one tries again.
async fn purge_dedup_hourly(store: Arc<Store>) {
    let start = tokio::time::Instant::now() + DEDUP_PURGE_INTERVAL;
    let mut ticks = tokio::time::interval_at(start, DEDUP_PURGE_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let store = Arc::clone(&store);
        let purged = tokio::task::spawn_blocking(move || purge_dedup(&store)).await;
        match purged {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "cannot purge the old dedup rows")
            }
            Err(err) => tracing::warn!(error = %err, "the dedup purge did not finish"),
        }
    }
}

/// A spawned task, aborted when this value is dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The agent's Iroh endpoint, with the agent's key as its identity.
async fn bind_endpoint(cfg: &Config, key: &AgentKey) -> anyhow::Result<Endpoint> {
    let secret_key = SecretKey::from_bytes(&key.to_bytes());
    let endpoint = if cfg.net.relay {
        Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .bind()
            .await
    } else {
        Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .context("invalid bind address")?
            .bind()
            .await
    };
    endpoint.context("cannot bind the Iroh endpoint")
}

/// The A2A server the node serves to its peers.
fn app(
    cfg: &Config,
    key: &Arc<AgentKey>,
    store: &Arc<Store>,
    executor: impl AgentExecutor,
) -> axum::Router {
    let id = key.id();
    let card = canonical_card(cfg, &id);
    let tasks = SqliteTaskStore::new(Arc::clone(store)).with_events(Arc::clone(key));
    // Each execution is bound to its request before it runs.
    let executor = BindingExecutor::new(executor, Arc::clone(store));
    // The SDK refuses what the card does not declare (push notifications,
    // the extended card).
    let inner =
        DefaultRequestHandler::new(executor, tasks).with_capabilities(card.capabilities.clone());
    let handler = Arc::new(GuardedHandler::new(
        inner,
        Arc::clone(store),
        Arc::clone(key),
    ));
    let app = axum::Router::new()
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(card))))
        .nest(JSONRPC_PATH, jsonrpc_router(Arc::clone(&handler)))
        .nest(
            REST_PATH,
            rest_router(handler).layer(axum::middleware::from_fn(rest_shim)),
        );
    Authenticator::new(
        id,
        &cfg.trusted_issuers,
        Arc::clone(store),
        cfg.limits.max_body_bytes,
    )
    .guard(app)
}
