//! The `Backend`: an Iroh endpoint as in the baseline, and a node of a
//! private Mainline DHT (n0-mainline) beside it, with the capability index
//! of [`index`].
//!
//! - **Two sockets.** Iroh takes UDP at `--listen` (4433), the DHT the next
//!   port (4434): n0-mainline binds its own socket and cannot share Iroh's.
//!   A router's `bootstrap_addr` is every address of the node with the
//!   DHT's port, `ip:port` joined by commas, as the baseline's, because a
//!   router is on both networks and a node on one reaches it only at that
//!   network's address.
//! - **Private network.** The DHT starts with `--bootstrap`'s addresses as
//!   its whole bootstrap list, which replaces the library's default one
//!   (`router.bittorrent.com` and others); with none, it knows no one.
//! - **Modes.** A router runs in server mode from the start. Providers and
//!   clients run in n0-mainline's client mode, which is adaptive: after 15
//!   minutes, a client that has seen its own ping reach it at the address
//!   the others report becomes a server. In 0.7.2 that ping is never sent
//!   (`Core::update_address_votes_from_iterative_query` stores the new
//!   address before comparing with it), so they stay clients; [`Mode`]
//!   reports it.
//! - **Announce.** The contact and the announce go as BEP 44 items, and
//!   then a signed peer per capability, and the node puts them again every
//!   `--republish-s`. A provider whose addresses changed (scenario S7)
//!   starts a new DHT client first: the library's routing table keeps the
//!   addresses of the network it left, and offers no way to clear it.
//! - **Search.** `get_signed_peers` of the capability's infohash and, as
//!   each agent key arrives, up to `limit`, its contact and announce
//!   (`get_mutable`), kept at the highest `seq` among those that check.
//!
//! The library has no expiry for anything it stores (an LRU, bounded only
//! by its counts) and no request counts, so `--ttl-s` has nothing to set
//! and a search's `requests` is `None`.

use std::collections::HashSet;
use std::future::poll_fn;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use futures_core::Stream;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, RelayMode, SecretKey};
use n0_mainline::{
    Dht, Id, MutableItem, RequestFilter, RequestSpecific, ServerSettings, SigningKey,
};
use pals_core::identity::AgentKey;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use spike_common::{Backend, BackendStats, NodeConfig, RawCandidate, RawSearch, Role, local_addrs};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::index::{self, Kind};
use crate::router::{Outgoing, PROBE_INTERVAL, Router, now_unix};

/// How long a node with a bootstrap list waits, at start, for its first
/// lookup to fill its routing table.
const BOOTSTRAP_WAIT: Duration = Duration::from_secs(20);
/// How long a search may take; then it returns what it has.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a new DHT client may wait for the old one's port.
const REBIND_WAIT: Duration = Duration::from_secs(5);

/// A node of the candidate.
pub struct Mainline {
    role: Role,
    signer: SigningKey,
    /// The DHT's port: 0 for any.
    dht_port: u16,
    /// `--bootstrap`'s addresses, one by one.
    bootstrap: Vec<String>,
    republish: Duration,
    /// Present but on a move, while a new one starts.
    net: Option<Net>,
    router: Option<Arc<Router>>,
    routing_peers: Arc<AtomicU64>,
    bootstrap_addr: Option<String>,
    /// The contact's addresses at the last announce.
    contact_addrs: Vec<String>,
    republisher: Option<Task>,
    /// Held so that it stays bound.
    _endpoint: Endpoint,
}

/// A DHT node and the tasks that run beside it.
struct Net {
    dht: Dht,
    _tasks: Vec<Task>,
}

/// What an announce puts, again at every republication.
#[derive(Clone)]
struct Published {
    items: Vec<MutableItem>,
    /// The infohash of each capability.
    capabilities: Vec<Id>,
}

/// A task that ends with its handle.
struct Task(JoinHandle<()>);

/// A router's server filter until its hook is in place: the builder takes
/// no hook, so the router refuses every request until it has one, and
/// nothing reaches the store unchecked in between.
#[derive(Debug, Clone)]
struct UntilHooked(Arc<AtomicBool>);

impl RequestFilter for UntilHooked {
    fn allow_request(&self, _request: &RequestSpecific, _from: SocketAddrV4) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Backend for Mainline {
    async fn start(cfg: &NodeConfig, key: &AgentKey) -> anyhow::Result<Self> {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr(cfg.listen)
            .context("invalid listen address")?
            .bind()
            .await
            .context("cannot bind the Iroh endpoint")?;
        let dht_port = match cfg.listen.port() {
            0 => 0,
            port => port.checked_add(1).context("no port after --listen's")?,
        };
        let bootstrap: Vec<String> = cfg
            .bootstrap
            .iter()
            .flat_map(|addrs| addrs.split(','))
            .map(|addr| addr.trim().to_owned())
            .filter(|addr| !addr.is_empty())
            .collect();
        let router = (cfg.role == Role::Router).then(|| Arc::new(Router::default()));
        let routing_peers = Arc::new(AtomicU64::new(0));
        let net = Net::start(cfg.role, dht_port, &bootstrap, &router, &routing_peers).await?;
        let bootstrap_addr = match &router {
            Some(_) => {
                let port = net.dht.info().await?.local_addr().port();
                let addrs: Vec<String> = local_addrs(cfg.listen)
                    .into_iter()
                    .map(|addr| SocketAddr::new(addr.ip(), port).to_string())
                    .collect();
                Some(addrs.join(","))
            }
            None => None,
        };
        tracing::info!(?bootstrap_addr, ?bootstrap, role = ?cfg.role, "mainline started");
        Ok(Self {
            role: cfg.role,
            signer: SigningKey::from_bytes(&key.to_bytes()),
            dht_port,
            bootstrap,
            republish: cfg.republish,
            net: Some(net),
            router,
            routing_peers,
            bootstrap_addr,
            contact_addrs: Vec::new(),
            republisher: None,
            _endpoint: endpoint,
        })
    }

    async fn announce(
        &mut self,
        capabilities: &[String],
        contact: &Signed<ContactRecord>,
        announce: &Signed<AnnounceRecord>,
    ) -> anyhow::Result<()> {
        let published = Published {
            items: vec![
                index::item(
                    &self.signer,
                    Kind::Contact,
                    &contact.to_json(),
                    contact.doc.seq,
                )?,
                index::item(
                    &self.signer,
                    Kind::Announce,
                    &announce.to_json(),
                    announce.doc.seq,
                )?,
            ],
            capabilities: capabilities
                .iter()
                .map(|c| index::capability_hash(c))
                .collect(),
        };
        self.republisher = None;
        let addrs = &contact.doc.iroh.direct_addrs;
        if self.role != Role::Router
            && !self.contact_addrs.is_empty()
            && self.contact_addrs != *addrs
        {
            tracing::info!(from = ?self.contact_addrs, to = ?addrs, "the node moved: a new DHT client");
            // The old client's socket must be gone before the new one binds.
            self.net = None;
            let net = Net::start(
                self.role,
                self.dht_port,
                &self.bootstrap,
                &self.router,
                &self.routing_peers,
            )
            .await?;
            self.net = Some(net);
        }
        self.contact_addrs.clone_from(addrs);
        let dht = self.dht()?.clone();
        let result = publish(&dht, &self.signer, &published).await;
        let (signer, every) = (self.signer.clone(), self.republish);
        self.republisher = Some(Task(tokio::spawn(async move {
            let mut tick = tokio::time::interval_at((Instant::now() + every).into(), every);
            loop {
                tick.tick().await;
                if let Err(error) = publish(&dht, &signer, &published).await {
                    tracing::warn!("republication incomplete: {error:#}");
                }
            }
        })));
        result
    }

    async fn unannounce(&mut self) -> anyhow::Result<()> {
        // Mainline deletes nothing: what the routers hold stays as long as
        // the library keeps it.
        self.republisher = None;
        Ok(())
    }

    async fn search(&mut self, capability: &str, limit: usize) -> anyhow::Result<RawSearch> {
        let started = Instant::now();
        let dht = self.dht()?.clone();
        let mut peers = dht
            .get_signed_peers(index::capability_hash(capability))
            .await?;
        let mut lookups = JoinSet::new();
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        let mut peers_done = false;
        let deadline = tokio::time::sleep(SEARCH_TIMEOUT);
        tokio::pin!(deadline);
        while !(peers_done && lookups.is_empty()) {
            tokio::select! {
                batch = next(&mut peers), if !peers_done => match batch {
                    Some(batch) => {
                        for peer in batch {
                            let key = *peer.key();
                            if seen.len() < limit && seen.insert(key) {
                                lookups.spawn(lookup(dht.clone(), key, started));
                            }
                        }
                    }
                    None => peers_done = true,
                },
                Some(found) = lookups.join_next(), if !lookups.is_empty() => {
                    candidates.push(found?);
                }
                () = &mut deadline => {
                    tracing::warn!(capability, "the search ran out of time: returning what it has");
                    break;
                }
            }
        }
        Ok(RawSearch {
            candidates,
            requests: None,
        })
    }

    fn stats(&self) -> BackendStats {
        let mut stats = BackendStats {
            // UDP: Mainline has no connections, and nothing dials Iroh.
            connections: 0,
            routing_peers: self.routing_peers.load(Ordering::Relaxed),
            bootstrap_addr: self.bootstrap_addr.clone(),
            ..BackendStats::default()
        };
        if let Some(router) = &self.router {
            router.fill(&mut stats);
        }
        stats
    }
}

/// A node's DHT mode, as the library reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// It answers queries and stores for others.
    pub server: bool,
    /// It has seen its own ping reach it at the address the others report.
    /// A client that has will become a server at its next 15-minute
    /// refresh (the library's adaptive mode).
    pub reachable: bool,
}

impl Mainline {
    /// The DHT's mode now.
    pub async fn mode(&self) -> anyhow::Result<Mode> {
        let info = self.dht()?.info().await?;
        Ok(Mode {
            server: info.server_mode(),
            reachable: !info.firewalled(),
        })
    }

    fn dht(&self) -> anyhow::Result<&Dht> {
        match &self.net {
            Some(net) => Ok(&net.dht),
            None => bail!("the DHT client did not start again after the node moved"),
        }
    }
}

impl Net {
    /// A DHT node on `port` that joins through `bootstrap`, with its tasks:
    /// the stats every second and, on a router, its hook, its probes and
    /// its answers to what it refuses.
    async fn start(
        role: Role,
        port: u16,
        bootstrap: &[String],
        router: &Option<Arc<Router>>,
        routing_peers: &Arc<AtomicU64>,
    ) -> anyhow::Result<Self> {
        let mut builder = Dht::builder();
        // The whole list: the library's default nodes are never used.
        builder.bootstrap(bootstrap).port(port);
        let hooked = Arc::new(AtomicBool::new(false));
        if role == Role::Router {
            builder.server_mode().server_settings(ServerSettings {
                filter: Box::new(UntilHooked(Arc::clone(&hooked))),
                ..ServerSettings::default()
            });
        }
        let dht = bind(|| builder.build()).await?;
        let port = dht.info().await?.local_addr().port();
        let mut tasks = Vec::new();
        if let Some(router) = router {
            let (replies, mut outgoing) = mpsc::unbounded_channel::<Outgoing>();
            let sender = dht.clone();
            tasks.push(Task(tokio::spawn(async move {
                while let Some((bytes, to)) = outgoing.recv().await {
                    if sender.send_datagram(bytes, to).await.is_err() {
                        break;
                    }
                }
            })));
            let hook = router.hook(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port), replies);
            dht.set_datagram_hook(Some(hook)).await?;
            // The actor takes its messages in order: once it answers this
            // one, the hook is in place.
            dht.info().await?;
            hooked.store(true, Ordering::Release);
        }
        let (watcher, router, routing_peers) =
            (dht.clone(), router.clone(), Arc::clone(routing_peers));
        tasks.push(Task(tokio::spawn(async move {
            let itself = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
            let mut tick = tokio::time::interval(PROBE_INTERVAL);
            loop {
                tick.tick().await;
                let Ok(info) = watcher.info().await else {
                    break;
                };
                routing_peers.store(info.routing_table_size() as u64, Ordering::Relaxed);
                for probe in router.iter().flat_map(|router| router.probes()) {
                    if watcher.send_datagram(probe, itself).await.is_err() {
                        break;
                    }
                }
            }
        })));
        if !bootstrap.is_empty() {
            match timeout(BOOTSTRAP_WAIT, dht.bootstrapped()).await {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => tracing::warn!(?bootstrap, "the bootstrap found no one"),
                Ok(Err(error)) => bail!("the DHT stopped: {error}"),
                Err(_) => tracing::warn!(?bootstrap, "the bootstrap did not end in time"),
            }
        }
        Ok(Self { dht, _tasks: tasks })
    }
}

/// Builds the DHT, trying again while its port is still the old client's.
async fn bind(build: impl Fn() -> std::io::Result<Dht>) -> anyhow::Result<Dht> {
    let deadline = Instant::now() + REBIND_WAIT;
    loop {
        match build() {
            Ok(dht) => return Ok(dht),
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error).context("cannot bind the DHT's socket"),
        }
    }
}

/// Puts the items and then the signed peers, each set at once: the
/// announce first, so that a router holds it when the signed peers arrive.
/// Fails, naming each, when some did not reach any node.
///
/// A put that reached no node is tried once more after a lookup of its
/// target. The library puts through the nodes, and with the tokens, that
/// its last lookup of the target cached, for as long as one of those tokens
/// is under 5 minutes old (`get_cached_closest_nodes`, `Node::valid_token`).
/// A router that restarted since has a new token secret and refuses them
/// (error 203), and the library reports any such refusal as a timeout
/// (`PutQuery::check`). The lookup caches the nodes that answer now, with
/// fresh tokens.
async fn publish(dht: &Dht, signer: &SigningKey, published: &Published) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    let mut puts = JoinSet::new();
    for item in &published.items {
        let (dht, item) = (dht.clone(), item.clone());
        puts.spawn(async move {
            let salt = String::from_utf8_lossy(item.salt().unwrap_or_default()).into_owned();
            if dht.put_mutable(item.clone(), None).await.is_ok() {
                return Ok(());
            }
            if let Ok(mut found) = dht.get_mutable(item.key(), item.salt(), None).await {
                while next(&mut found).await.is_some() {}
            }
            dht.put_mutable(item, None)
                .await
                .map(drop)
                .map_err(|error| format!("{salt}: {error}"))
        });
    }
    while let Some(done) = puts.join_next().await {
        failures.extend(done?.err());
    }
    for info_hash in &published.capabilities {
        let (dht, signer, info_hash) = (dht.clone(), signer.clone(), *info_hash);
        puts.spawn(async move {
            if dht.announce_signed_peer(info_hash, &signer).await.is_ok() {
                return Ok(());
            }
            if let Ok(mut found) = dht.get_signed_peers(info_hash).await {
                while next(&mut found).await.is_some() {}
            }
            dht.announce_signed_peer(info_hash, &signer)
                .await
                .map(drop)
                .map_err(|error| format!("signed peer {info_hash}: {error}"))
        });
    }
    while let Some(done) = puts.join_next().await {
        failures.extend(done?.err());
    }
    ensure!(failures.is_empty(), "not stored: {}", failures.join("; "));
    Ok(())
}

/// One agent found under the capability's infohash: its contact and its
/// announce, each the newest that checks, and when the search had both.
async fn lookup(dht: Dht, key: [u8; 32], started: Instant) -> RawCandidate {
    let (contact, announce) = tokio::join!(
        fetch(&dht, &key, Kind::Contact, started),
        fetch(&dht, &key, Kind::Announce, started),
    );
    RawCandidate {
        found_ms: contact
            .found_ms
            .zip(announce.found_ms)
            .map(|(c, a)| c.max(a)),
        contact: contact.value,
        announce: announce.value,
    }
}

/// An item as a search found it.
struct Fetched {
    /// The newest item that checks or, if none does, the first one found.
    value: Option<Vec<u8>>,
    /// When the first item that checks arrived, from the search's start.
    found_ms: Option<u64>,
}

/// The agent `key`'s item of `kind`: every copy the routers answer with,
/// until the lookup ends.
async fn fetch(dht: &Dht, key: &[u8; 32], kind: Kind, started: Instant) -> Fetched {
    let mut fetched = Fetched {
        value: None,
        found_ms: None,
    };
    let Ok(mut items) = dht.get_mutable(key, Some(kind.salt()), None).await else {
        return fetched;
    };
    let mut newest: Option<i64> = None;
    while let Some(item) = next(&mut items).await {
        let checks =
            index::check_item(key, Some(kind.salt()), item.value(), item.seq(), now_unix()).is_ok();
        if checks {
            fetched.found_ms.get_or_insert_with(|| millis(started));
        }
        let better = match (checks, newest) {
            (true, Some(seq)) => item.seq() > seq,
            (true, None) => true,
            (false, _) => fetched.value.is_none(),
        };
        if better {
            if checks {
                newest = Some(item.seq());
            }
            fetched.value = Some(item.value().to_vec());
        }
    }
    fetched
}

async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
