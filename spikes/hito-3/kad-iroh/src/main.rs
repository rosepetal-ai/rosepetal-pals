//! `kad-iroh`: `libp2p-kad` over a libp2p transport made of Iroh
//! connections.
//!
//! One stack: the node's Iroh endpoint, with the agent's key, on UDP at
//! `--listen`, with no relay, port mapping or address lookup, serves ALPN
//! `agentnet-kad/0`, and libp2p's Kademlia runs over its connections
//! ([`spike_kad_iroh::behaviour`]). The `PeerId` is the agent's key, so the
//! records need no identity link: an announcement is the signed contact,
//! the signed announce and one provider record per capability, all under
//! the agent's `PeerId` ([`spike_kad_iroh::validate`] has the keys). A
//! router's `bootstrap_addr` is every address of its endpoint, as
//! `/ip4/<ip>/udp/<port>/quic-v1/p2p/<PeerId>`, comma-separated.

use pals_core::identity::AgentKey;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use spike_common::{Backend, BackendStats, NodeConfig, RawSearch};
use spike_kad_iroh::behaviour::{self, Handle, Settings};

struct KadIroh {
    node: Handle,
}

impl Backend for KadIroh {
    async fn start(cfg: &NodeConfig, key: &AgentKey) -> anyhow::Result<Self> {
        let settings = Settings {
            role: cfg.role,
            listen: cfg.listen,
            bootstrap: cfg.bootstrap.clone(),
            ttl: cfg.ttl,
            republish: cfg.republish,
        };
        Ok(Self {
            node: behaviour::spawn(settings, key).await?,
        })
    }

    async fn announce(
        &mut self,
        capabilities: &[String],
        contact: &Signed<ContactRecord>,
        announce: &Signed<AnnounceRecord>,
    ) -> anyhow::Result<()> {
        self.node
            .announce(contact.to_json(), announce.to_json(), capabilities.to_vec())
            .await
    }

    async fn unannounce(&mut self) -> anyhow::Result<()> {
        self.node.unannounce().await
    }

    async fn search(&mut self, capability: &str, limit: usize) -> anyhow::Result<RawSearch> {
        self.node.search(capability.to_owned(), limit).await
    }

    fn stats(&self) -> BackendStats {
        self.node.stats()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    spike_common::run_node::<KadIroh>().await
}
