//! `kad-libp2p`: Iroh plus `libp2p-kad` on its own libp2p stack.
//!
//! The node runs two stacks side by side, as the daemon would (spec §6.1):
//! - Iroh, as in the daemon with `relay = false` and as the baseline: an
//!   endpoint with the agent's key, on UDP at `--listen`, with no relay,
//!   port mapping or address lookup. The contact record names it; nothing
//!   dials it in the spike.
//! - libp2p, with its own Ed25519 key ([`link::libp2p_keypair`]), QUIC on
//!   UDP at `--listen`'s port plus one, and Kademlia ([`behaviour`]): the
//!   two stacks cannot share a UDP port.
//!
//! An announcement is three things in the DHT, under the node's `PeerId`:
//! the link ([`link`]), which carries the contact; the signed announce; and
//! one provider record per capability ([`validate`] has the keys). A
//! router's `bootstrap_addr` is every address its libp2p listens at, as
//! multiaddrs ending in `/p2p/<PeerId>`, comma-separated.

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, RelayMode, SecretKey};
use libp2p::identity::Keypair;
use pals_core::identity::AgentKey;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use spike_common::{Backend, BackendStats, NodeConfig, RawSearch};
use spike_kad_libp2p::behaviour::{self, Handle, Settings};
use spike_kad_libp2p::link;

struct KadLibp2p {
    libp2p: Handle,
    libp2p_key: Keypair,
    agent: AgentKey,
    /// Held so that it stays bound.
    _endpoint: Endpoint,
}

impl Backend for KadLibp2p {
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
        let port = cfg
            .listen
            .port()
            .checked_add(1)
            .context("libp2p listens at --listen's port plus one, and there is none")?;
        let libp2p_key = link::libp2p_keypair(key);
        let libp2p = behaviour::spawn(
            Settings {
                role: cfg.role,
                listen: SocketAddr::new(cfg.listen.ip(), port),
                bootstrap: cfg.bootstrap.clone(),
                ttl: cfg.ttl,
                republish: cfg.republish,
            },
            libp2p_key.clone(),
        )
        .await?;
        Ok(Self {
            libp2p,
            libp2p_key,
            agent: AgentKey::from_bytes(&key.to_bytes()),
            _endpoint: endpoint,
        })
    }

    async fn announce(
        &mut self,
        capabilities: &[String],
        contact: &Signed<ContactRecord>,
        announce: &Signed<AnnounceRecord>,
    ) -> anyhow::Result<()> {
        let link = link::issue(&self.libp2p_key, &self.agent, contact, now_unix())?;
        self.libp2p
            .announce(link, announce.to_json(), capabilities.to_vec())
            .await
    }

    async fn unannounce(&mut self) -> anyhow::Result<()> {
        self.libp2p.unannounce().await
    }

    async fn search(&mut self, capability: &str, limit: usize) -> anyhow::Result<RawSearch> {
        self.libp2p.search(capability.to_owned(), limit).await
    }

    fn stats(&self) -> BackendStats {
        self.libp2p.stats()
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    spike_common::run_node::<KadLibp2p>().await
}
