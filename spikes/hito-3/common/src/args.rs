//! The command line every candidate's binary takes.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::ensure;
use clap::Parser;
use pals_core::identity::AgentKey;

use crate::{NodeConfig, Role};

/// A node of the hito 3 backend comparison; its control API listens on
/// 127.0.0.1:7600.
#[derive(Debug, Parser)]
#[command(version)]
pub(crate) struct Cli {
    /// The node's name in the topology (r1, p1, c1…), for its logs.
    #[arg(long)]
    name: String,
    #[arg(long, value_enum)]
    role: Role,
    /// Where the backend listens, `ip:port`; with `0.0.0.0`, every local
    /// address goes in the contact record.
    #[arg(long)]
    listen: SocketAddr,
    /// A node to join through, in the backend's own form: what that node's
    /// `GET /stats` gives as `bootstrap_addr`. Repeatable.
    #[arg(long)]
    bootstrap: Vec<String>,
    /// A capability to announce at start, `agentnet:capability:v1:<id>`;
    /// providers only. Repeatable.
    #[arg(long = "announce", value_name = "CAPABILITY")]
    announce: Vec<String>,
    /// How long the DHT keeps an announcement, in seconds.
    #[arg(long)]
    ttl_s: u64,
    /// How often the backend renews its announcements, in seconds.
    #[arg(long)]
    republish_s: u64,
    /// The agent's Ed25519 secret key, 64 hex characters.
    #[arg(long, value_parser = parse_seed)]
    key_seed: [u8; 32],
    /// Publishes a contact whose signature does not verify (scenario S5);
    /// providers only.
    #[arg(long)]
    rogue: bool,
}

/// What [`Cli`] gives `run_node` beyond the backend's [`NodeConfig`].
pub(crate) struct Node {
    pub cfg: NodeConfig,
    pub key: AgentKey,
    pub announce: Vec<String>,
    pub rogue: bool,
}

impl Cli {
    pub(crate) fn into_node(self) -> anyhow::Result<Node> {
        ensure!(
            self.ttl_s > 0 && self.republish_s > 0,
            "--ttl-s and --republish-s must be positive"
        );
        ensure!(
            self.role == Role::Provider || (self.announce.is_empty() && !self.rogue),
            "only a provider takes --announce or --rogue"
        );
        Ok(Node {
            cfg: NodeConfig {
                name: self.name,
                role: self.role,
                listen: self.listen,
                bootstrap: self.bootstrap,
                ttl: Duration::from_secs(self.ttl_s),
                republish: Duration::from_secs(self.republish_s),
            },
            key: AgentKey::from_bytes(&self.key_seed),
            announce: self.announce,
            rogue: self.rogue,
        })
    }
}

fn parse_seed(text: &str) -> Result<[u8; 32], String> {
    let bad = || "a key seed is 64 hex characters".to_owned();
    if text.len() != 64 || !text.is_ascii() {
        return Err(bad());
    }
    let mut seed = [0u8; 32];
    for (byte, pair) in seed.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(extra: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec![
            "spike",
            "--name",
            "p1",
            "--role",
            "provider",
            "--listen",
            "0.0.0.0:4433",
            "--ttl-s",
            "120",
            "--republish-s",
            "40",
            "--key-seed",
        ];
        let seed = "01".repeat(32);
        argv.push(&seed);
        argv.extend_from_slice(extra);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn the_flags_make_the_node() {
        let node = cli(&[
            "--bootstrap",
            "a",
            "--bootstrap",
            "b",
            "--announce",
            "x",
            "--announce",
            "y",
        ])
        .unwrap()
        .into_node()
        .unwrap();
        assert_eq!(node.cfg.bootstrap, ["a", "b"]);
        assert_eq!(node.announce, ["x", "y"]);
        assert_eq!(node.cfg.ttl, Duration::from_secs(120));
        assert_eq!(node.cfg.republish, Duration::from_secs(40));
        assert_eq!(node.key.id(), AgentKey::from_bytes(&[1; 32]).id());
        assert!(!node.rogue);
    }

    #[test]
    fn a_seed_is_64_hex_characters() {
        assert!(parse_seed(&"ab".repeat(32)).is_ok());
        assert!(parse_seed(&"ab".repeat(31)).is_err());
        assert!(parse_seed(&"zz".repeat(32)).is_err());
        assert!(parse_seed(&"é".repeat(32)).is_err());
    }

    #[test]
    fn only_a_provider_announces() {
        let router = Cli::try_parse_from([
            "spike",
            "--name",
            "r1",
            "--role",
            "router",
            "--listen",
            "0.0.0.0:4433",
            "--ttl-s",
            "120",
            "--republish-s",
            "40",
            "--key-seed",
            &"01".repeat(32),
            "--rogue",
        ]);
        assert!(router.unwrap().into_node().is_err());
    }
}
