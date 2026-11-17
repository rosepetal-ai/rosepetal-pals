//! A rogue publisher for the hito 3 exit test
//! (`docs/runbooks/hito-3-descubrimiento-kademlia.md`, case 6): a DHT client
//! of its own, with no node around it, that enters the DHT through a router
//! and publishes capabilities with a contact record whose signature was
//! altered after signing. Its announce record is valid, so the routers keep
//! the announce and its provider records and refuse the contact,
//! `bad_contact`; a client that finds it discards it, `no valid contact`.
//!
//! ```text
//! cargo build --release --locked -p pals --examples
//! dht_rogue --bootstrap <router ticket> --addr <ip:port> [--listen <ip:port>]
//!           [--capability <capability>]... [--record-ttl-s 120] [--republish-s 40]
//! ```
//!
//! - `--bootstrap`: a router's ticket (`pals id --ticket` on it), the way in.
//! - `--addr`: where the contact record says the agent is; nobody can use
//!   it, since its signature does not verify.
//! - `--listen`: the UDP address of its Iroh endpoint (`0.0.0.0:4433`).
//! - `--capability`: what it announces (`vision.ocr`); repeatable.
//! - `--record-ttl-s`, `--republish-s`: `[dht] record_ttl_s` and
//!   `republish_s`, as the daemons of the test have them.
//!
//! It publishes with the daemon's own code ([`Dht`], [`PublishedRecords`]),
//! with a fresh key on every start. It prints `dht_rogue: agent <id>` once
//! it runs, then a line for each round of publication that ends, and runs
//! until SIGTERM or SIGINT.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, ensure};
use clap::Parser;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey};
use pals::config::{DhtCfg, DhtMode};
use pals::dht::swarm::STORE_LIMITS;
use pals::dht::transport::{ALPN_KAD, KadQuotas};
use pals::dht::{Dht, DhtParts, DhtTiming, PublishedRecords};
use pals_core::card_jws::CardHash;
use pals_core::clock::{Clock, SystemClock};
use pals_core::grant::format_timestamp;
use pals_core::identity::AgentKey;
use pals_core::records::{
    ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact, RECORD_TTL_S,
};
use pals_core::signed::Signed;
use pals_core::store::Store;
use tokio::sync::watch;

/// A DHT client that publishes a contact record with an altered signature.
#[derive(Parser)]
#[command(name = "dht_rogue")]
struct Args {
    /// A router's ticket, the way into the DHT
    #[arg(long)]
    bootstrap: String,
    /// Where the contact record says the agent is
    #[arg(long)]
    addr: SocketAddr,
    /// The UDP address of the Iroh endpoint
    #[arg(long, default_value = "0.0.0.0:4433")]
    listen: SocketAddr,
    /// A capability to announce; repeatable
    #[arg(long = "capability", default_value = "vision.ocr")]
    capabilities: Vec<String>,
    /// How long the routers keep a record
    #[arg(long, default_value_t = 120)]
    record_ttl_s: u64,
    /// How often the records are published again
    #[arg(long, default_value_t = 40)]
    republish_s: u64,
}

/// The records it publishes, fixed: a tampered contact and a valid
/// announce, both of `seq` 1 and valid for [`RECORD_TTL_S`].
struct RogueRecords {
    contact: Vec<u8>,
    announce: Vec<u8>,
    capabilities: Vec<String>,
    changes: watch::Sender<u64>,
}

impl PublishedRecords for RogueRecords {
    fn current(&self) -> (Vec<u8>, Vec<u8>) {
        (self.contact.clone(), self.announce.clone())
    }

    fn capabilities(&self) -> Vec<String> {
        self.capabilities.clone()
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

/// `key`'s contact record at `addr`, signed and then altered: the first
/// character of its signature changed, so that it no longer verifies.
fn tampered_contact(key: &AgentKey, addr: SocketAddr, now: i64) -> anyhow::Result<Vec<u8>> {
    let doc = ContactRecord {
        schema: CONTACT_SCHEMA.to_owned(),
        agent_id: key.id(),
        iroh: IrohContact {
            endpoint_id: key.id(),
            relay_url: None,
            direct_addrs: vec![addr.to_string()],
        },
        seq: 1,
        issued_at: format_timestamp(now).context("the time now")?,
        expires_at: format_timestamp(now + RECORD_TTL_S).context("the expiry")?,
    };
    let mut signed = Signed::issue(key, doc).context("signing the contact")?;
    let first = signed.signature.remove(0);
    signed
        .signature
        .insert(0, if first == 'A' { 'B' } else { 'A' });
    let tampered = signed.to_json();
    ensure!(
        Signed::<ContactRecord>::from_json(&tampered).is_err(),
        "the altered contact still verifies"
    );
    Ok(tampered)
}

/// `key`'s announce record listing `capabilities`, valid, naming a card
/// hash of no card: it has none.
fn announce(key: &AgentKey, capabilities: &[String], now: i64) -> anyhow::Result<Vec<u8>> {
    let card_hash: CardHash = format!("blake3:{}", blake3::hash(b"dht_rogue: no card").to_hex())
        .parse()
        .map_err(|err| anyhow::anyhow!("a card hash: {err:?}"))?;
    let doc = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: key.id(),
        seq: 1,
        agent_card_hash: card_hash,
        capabilities: capabilities.to_vec(),
        discovery_alpn: pals::discovery::DISCOVERY_ALPN.to_owned(),
        issued_at: format_timestamp(now).context("the time now")?,
        expires_at: format_timestamp(now + RECORD_TTL_S).context("the expiry")?,
    };
    Ok(Signed::issue(key, doc)
        .context("signing the announce")?
        .to_json())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    pals::install_crypto_provider();
    let key = AgentKey::generate();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let now = clock.now_unix();
    let records = Arc::new(RogueRecords {
        contact: tampered_contact(&key, args.addr, now)?,
        announce: announce(&key, &args.capabilities, now)?,
        capabilities: args.capabilities.clone(),
        changes: watch::channel(0).0,
    });

    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr(args.listen)
        .with_context(|| format!("invalid --listen {}", args.listen))?
        .bind()
        .await
        .context("cannot bind the Iroh endpoint")?;
    let cfg = DhtCfg {
        enabled: true,
        mode: DhtMode::Client,
        bootstrap: vec![args.bootstrap.clone()],
        announce: true,
        record_ttl_s: args.record_ttl_s,
        republish_s: args.republish_s,
        ..DhtCfg::default()
    };
    let (dht, listener) = Dht::spawn(DhtParts {
        endpoint: endpoint.clone(),
        key: Arc::new(AgentKey::from_bytes(&key.to_bytes())),
        timing: DhtTiming::from_config(&cfg),
        cfg,
        quotas: KadQuotas::default(),
        store_limits: STORE_LIMITS,
        clock,
        store: Arc::new(Store::open_in_memory(key.id())?),
        published: Some(records),
    })?;
    let router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
    println!(
        "dht_rogue: agent {}, announcing {} with an altered contact",
        key.id(),
        args.capabilities.join(", ")
    );

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("SIGTERM")?;
    let mut last_round = None;
    let mut every = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            _ = every.tick() => {
                let Ok(status) = dht.status().await else { break };
                let Some(publication) = status.publication else { continue };
                if publication.last_round_unix != last_round {
                    last_round = publication.last_round_unix;
                    println!(
                        "dht_rogue: round at {:?}, error {:?}, failed {:?}, routing peers {}",
                        publication.last_round_unix,
                        publication.last_error,
                        publication.failed_capabilities,
                        status.routing_peers
                    );
                }
            }
        }
    }
    dht.shutdown().await;
    router.shutdown().await.ok();
    println!("dht_rogue: stopped");
    Ok(())
}
