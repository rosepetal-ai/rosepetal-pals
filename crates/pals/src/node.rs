//! [`Node`]: one running agent, the A2A server its peers reach over Iroh.

use std::net::Ipv4Addr;
use std::sync::Arc;

use a2a_server::agent_card::agent_card_router;
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::rest::rest_router;
use a2a_server::{DefaultRequestHandler, StaticAgentCard};
use anyhow::{Context, ensure};
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::Store;

use crate::a2a::auth::{Authenticator, JSONRPC_PATH, REST_PATH};
use crate::a2a::{GuardedHandler, SqliteTaskStore, canonical_card};
use crate::config::{Config, DataDirLock, WorkerCfg};
use crate::net::{ALPN_A2A_HTTP, PeerHttp, TunnelLimits, TunnelServer};
use crate::worker::echo::EchoExecutor;

/// The agent's SQLite store, in the data directory.
pub const STORE_FILE: &str = "pals.db";

/// A running agent: its A2A server, served to peers over Iroh on
/// [`ALPN_A2A_HTTP`], and the client it reaches peers with.
///
/// The server is the SDK's, with the [canonical card](canonical_card) at
/// `/.well-known/agent-card.json`, JSON-RPC at `/jsonrpc` and HTTP+JSON at
/// `/rest`, behind grant authentication ([`crate::a2a::auth`]) and the
/// ownership of tasks ([`GuardedHandler`]), on the durable task store.
pub struct Node {
    pub id: AgentId,
    pub store: Arc<Store>,
    /// Requests to peers, from this agent's own endpoint and identity.
    pub peer_http: PeerHttp,
    router: Router,
    /// Held while the node runs.
    _lock: DataDirLock,
}

impl Node {
    /// Starts the agent of `cfg.data_dir`, which must exist.
    ///
    /// In order: takes the data directory's lock, failing with
    /// [`AlreadyRunning`](crate::config::AlreadyRunning) when another daemon
    /// holds it; loads the agent's key, creating it if missing; opens the
    /// store; fails the tasks a previous run left in progress; and only then
    /// binds the Iroh endpoint with the agent's key and serves
    /// [`ALPN_A2A_HTTP`] on it. With `net.relay` the endpoint uses Iroh's
    /// N0 preset and its public relays; without it, it has no relay and is
    /// bound to 127.0.0.1 only.
    pub async fn spawn(cfg: Config) -> anyhow::Result<Node> {
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
        let app = app(&cfg, &key, &store);
        let router = Router::builder(endpoint.clone())
            .accept(ALPN_A2A_HTTP, TunnelServer::new(app, limits))
            .spawn();
        Ok(Node {
            id,
            store,
            peer_http: PeerHttp::new(endpoint),
            router,
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

    /// Closes the connections to peers, stops serving, closes the endpoint
    /// and releases the data directory.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.peer_http.close_all().await;
        self.router
            .shutdown()
            .await
            .context("the node did not shut down cleanly")?;
        Ok(())
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
fn app(cfg: &Config, key: &Arc<AgentKey>, store: &Arc<Store>) -> axum::Router {
    let id = key.id();
    let card = canonical_card(cfg, &id);
    let tasks = SqliteTaskStore::new(Arc::clone(store)).with_events(Arc::clone(key));
    let executor = match cfg.worker {
        WorkerCfg::Echo => EchoExecutor,
    };
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
