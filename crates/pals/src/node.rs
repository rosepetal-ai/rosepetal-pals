//! [`Node`]: one running agent, the A2A server its peers reach over Iroh.

use std::net::Ipv4Addr;
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
use tokio::task::JoinHandle;

use crate::a2a::auth::{Authenticator, JSONRPC_PATH, REST_PATH};
use crate::a2a::{BindingExecutor, GuardedHandler, SqliteTaskStore, canonical_card};
use crate::config::{Config, DataDirLock, WorkerCfg};
use crate::net::{ALPN_A2A_HTTP, PeerHttp, TunnelLimits, TunnelServer};
use crate::worker::echo::EchoExecutor;

/// The agent's SQLite store, in the data directory.
pub const STORE_FILE: &str = "pals.db";

/// How often a running node purges the dedup rows older than
/// [`DEDUP_RETENTION_S`], besides once when it starts.
const DEDUP_PURGE_INTERVAL: Duration = Duration::from_secs(3600);

/// A running agent: its A2A server, served to peers over Iroh on
/// [`ALPN_A2A_HTTP`], and the client it reaches peers with.
///
/// The server is the SDK's, with the [canonical card](canonical_card) at
/// `/.well-known/agent-card.json`, JSON-RPC at `/jsonrpc` and HTTP+JSON at
/// `/rest`, behind grant authentication ([`crate::a2a::auth`]) and the
/// ownership of tasks and idempotency of messages ([`GuardedHandler`]), on
/// the durable task store.
pub struct Node {
    pub id: AgentId,
    pub store: Arc<Store>,
    /// Requests to peers, from this agent's own endpoint and identity.
    pub peer_http: PeerHttp,
    router: Router,
    /// Purges old dedup rows every [`DEDUP_PURGE_INTERVAL`] while the node
    /// runs.
    dedup_purge: AbortOnDrop,
    /// Held while the node runs.
    _lock: DataDirLock,
}

impl Node {
    /// Starts the agent of `cfg.data_dir`, which must exist.
    ///
    /// In order: takes the data directory's lock, failing with
    /// [`AlreadyRunning`](crate::config::AlreadyRunning) when another daemon
    /// holds it; loads the agent's key, creating it if missing; opens the
    /// store; fails the tasks a previous run left in progress; frees the
    /// dedup keys it left with no execution behind them, and purges the
    /// dedup rows older than [`DEDUP_RETENTION_S`]; and only then binds the
    /// Iroh endpoint with the agent's key and serves [`ALPN_A2A_HTTP`] on
    /// it. With `net.relay` the endpoint uses Iroh's N0 preset and its
    /// public relays; without it, it has no relay and is bound to 127.0.0.1
    /// only. While it runs, the node purges the dedup rows again every hour.
    pub async fn spawn(cfg: Config) -> anyhow::Result<Node> {
        let executor = match cfg.worker {
            WorkerCfg::Echo => EchoExecutor,
        };
        Self::spawn_with_executor(cfg, executor).await
    }

    /// [`Node::spawn`], with `executor` doing the work instead of the
    /// executor `cfg.worker` selects.
    pub async fn spawn_with_executor(
        cfg: Config,
        executor: impl AgentExecutor,
    ) -> anyhow::Result<Node> {
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
            // An unbound key belongs to a request that never ran: free it,
            // so that its retry runs instead of waiting for nothing.
            let unbound = store
                .dedup_clear_unbound()
                .context("cannot free the dedup keys a restart left unbound")?;
            if unbound > 0 {
                tracing::info!(unbound, "freed the dedup keys a restart left unbound");
            }
            purge_dedup(&store).context("cannot purge the old dedup rows")?;
            anyhow::Ok((lock, key, store))
        })
        .await??;
        let key = Arc::new(key);
        let store = Arc::new(store);
        let id = key.id();

        let endpoint = bind_endpoint(&cfg, &key).await?;
        ensure!(
            endpoint.id().as_bytes() == id.as_bytes(),
            "the Iroh endpoint id is not the agent id"
        );
        let limits = TunnelLimits {
            max_body_bytes: cfg.limits.max_body_bytes,
            ..TunnelLimits::default()
        };
        let app = app(&cfg, &key, &store, executor);
        let router = Router::builder(endpoint.clone())
            .accept(ALPN_A2A_HTTP, TunnelServer::new(app, limits))
            .spawn();
        let dedup_purge = AbortOnDrop(tokio::spawn(purge_dedup_hourly(Arc::clone(&store))));
        Ok(Node {
            id,
            store,
            peer_http: PeerHttp::new(endpoint),
            router,
            dedup_purge,
            _lock: lock,
        })
    }

    /// The address peers dial this node at: its id and the direct addresses
    /// and relay it is reachable on.
    pub fn addr(&self) -> EndpointAddr {
        let endpoint = self.router.endpoint();
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

    /// [`Node::addr`] as an Iroh endpoint ticket, to hand to a peer.
    pub fn ticket(&self) -> String {
        EndpointTicket::new(self.addr()).to_string()
    }

    /// Stops purging, closes the connections to peers, stops serving,
    /// closes the endpoint and releases the data directory.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        drop(self.dedup_purge);
        self.peer_http.close_all().await;
        self.router
            .shutdown()
            .await
            .context("the node did not shut down cleanly")?;
        Ok(())
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
        .nest(REST_PATH, rest_router(handler));
    Authenticator::new(
        id,
        &cfg.trusted_issuers,
        Arc::clone(store),
        cfg.limits.max_body_bytes,
    )
    .guard(app)
}
