//! `kad-iroh`: `libp2p-kad` over a libp2p transport made of Iroh
//! connections, the second candidate of the hito 3 backend comparison; not
//! product code.
//!
//! One stack, one identity and one UDP port: the node's Iroh endpoint
//! ([`bind_endpoint`]) has the agent's key and serves ALPN
//! [`transport::ALPN_KAD`], and libp2p reaches peers only through it
//! ([`transport`], [`muxer`]). The `PeerId` is the agent's own Ed25519
//! public key, so a record under a `PeerId` names its agent with no
//! identity link.
//!
//! A library as well as the binary (`main.rs`), so that the tests reach the
//! transport, the validators and the swarm. [`behaviour`] and [`validate`]
//! are copies of kad-libp2p's, changed where that candidate assumed QUIC
//! over IP or an identity link; each module's header says what changed.

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, RelayMode, SecretKey};
use pals_core::identity::AgentKey;

pub mod behaviour;
pub mod muxer;
pub mod transport;
pub mod validate;

/// The node's Iroh endpoint, started as the daemon starts it with
/// `relay = false` and as the baseline does: the agent's key, UDP at
/// `listen` only, no relay, no port mapping and no address lookup. It
/// serves no ALPN until a router gives it one.
pub async fn bind_endpoint(listen: SocketAddr, key: &AgentKey) -> anyhow::Result<Endpoint> {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr(listen)
        .context("invalid listen address")?
        .bind()
        .await
        .context("cannot bind the Iroh endpoint")
}

/// Seconds since the Unix epoch, by the system clock; 0 before it.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
