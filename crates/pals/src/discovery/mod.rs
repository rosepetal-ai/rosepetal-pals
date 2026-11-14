//! What agents learn of each other before any task: peers' Agent Cards, read
//! and verified ([`PeerCards`]), and this agent's own signed contact and
//! announce records, issued and kept current ([`Records`]), which the
//! discovery protocol, `agentnet-discovery/0`, carries in every answer.

pub mod cards;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use anyhow::{Context, anyhow, ensure};
use iroh::{EndpointAddr, TransportAddr};
use pals_core::card_jws::CardHash;
use pals_core::grant::{format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::{Inventory, Visibility};
use pals_core::records::{
    ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact,
    MAX_ANNOUNCED_CAPABILITIES, MAX_DIRECT_ADDRS, RECORD_RENEW_BEFORE_S, RECORD_TTL_S,
};
use pals_core::signed::{Signed, SignedDoc};
use pals_core::store::Store;

pub use cards::{CardFetchError, PeerCards, signed_card_json};

/// The ALPN of the discovery protocol, which the announce record names.
pub const DISCOVERY_ALPN: &str = "agentnet-discovery/0";
/// The store's name for the contact record.
const CONTACT: &str = "contact";
/// The store's name for the announce record.
const ANNOUNCE: &str = "announce";

/// Where this agent's endpoint can be dialed now, for its contact record.
pub type ContactSource = Arc<dyn Fn() -> IrohContact + Send + Sync>;

/// The capabilities of the public services of `inventory`, each once, in
/// the inventory's order: what the announce record lists.
///
/// An error when there are more than [`MAX_ANNOUNCED_CAPABILITIES`], which
/// an announce record cannot hold.
pub fn announced_capabilities(inventory: &Inventory) -> anyhow::Result<Vec<String>> {
    let mut capabilities: Vec<String> = Vec::new();
    let public = inventory
        .services
        .iter()
        .filter(|service| service.visibility == Visibility::Public);
    for capability in public.flat_map(|service| &service.capabilities) {
        if !capabilities.contains(capability) {
            capabilities.push(capability.clone());
        }
    }
    ensure!(
        capabilities.len() <= MAX_ANNOUNCED_CAPABILITIES,
        "the public services have {} capabilities, and an announce record lists at most \
         {MAX_ANNOUNCED_CAPABILITIES} public capabilities: show fewer services, or fewer \
         capabilities, in the inventory",
        capabilities.len()
    );
    Ok(capabilities)
}

/// The body of `id`'s contact record for an endpoint dialed at `addr`: its
/// first relay, and up to [`MAX_DIRECT_ADDRS`] of its IP addresses, sorted.
pub fn iroh_contact(id: AgentId, addr: &EndpointAddr) -> IrohContact {
    let mut relay_url = None;
    let mut direct = BTreeSet::new();
    for transport in &addr.addrs {
        match transport {
            TransportAddr::Ip(socket) => {
                let text = socket.to_string();
                // Only what a receiver reads back as a socket address.
                if text.parse::<SocketAddr>().is_ok() {
                    direct.insert(text);
                }
            }
            TransportAddr::Relay(url) if relay_url.is_none() => {
                relay_url = Some(url.to_string()).filter(|url| !url.is_empty());
            }
            _ => {}
        }
    }
    IrohContact {
        endpoint_id: id,
        relay_url,
        direct_addrs: direct.into_iter().take(MAX_DIRECT_ADDRS).collect(),
    }
}

/// This agent's own signed records, current: its contact record, where its
/// endpoint can be dialed, and its announce record, the hash of its public
/// card, its public capabilities and [`DISCOVERY_ALPN`]. Clones share them.
///
/// They are kept in memory as issued, and in the store for the next start,
/// which reads them back once ([`Store::own_record`]). A record is issued
/// again ([`Records::renew`]) when what it says changes or when less than
/// [`RECORD_RENEW_BEFORE_S`] are left of it; every record is issued with
/// the next `seq` of its kind ([`Store::own_seq_next`]) and the whole
/// [`RECORD_TTL_S`] from its `issued_at`, the time it is issued at.
#[derive(Clone)]
pub struct Records {
    inner: Arc<Inner>,
}

struct Inner {
    key: Arc<AgentKey>,
    store: Arc<Store>,
    contact_source: ContactSource,
    /// The announce record's content, fixed while the node runs.
    card_hash: CardHash,
    capabilities: Vec<String>,
    /// Held while records are issued, so that renewals never interleave.
    renewing: Mutex<()>,
    current: RwLock<Current>,
}

/// The records in force.
#[derive(Clone)]
struct Current {
    contact: Live<ContactRecord>,
    announce: Live<AnnounceRecord>,
}

/// A record, and its signed JSON.
#[derive(Clone)]
struct Live<T> {
    doc: T,
    json: Vec<u8>,
}

impl Records {
    /// The records of the agent of `key` at `now_unix`: the ones `store`
    /// kept from the last run when they still say the same and have at
    /// least [`RECORD_RENEW_BEFORE_S`] left, new ones otherwise. The contact
    /// record's body comes from `contact_source`, the announce record names
    /// `card_hash` and `capabilities` ([`announced_capabilities`]).
    pub fn start(
        key: Arc<AgentKey>,
        store: Arc<Store>,
        contact_source: ContactSource,
        card_hash: CardHash,
        capabilities: Vec<String>,
        now_unix: i64,
    ) -> anyhow::Result<Self> {
        let parts = Parts {
            key: &key,
            store: &store,
            contact_source: &contact_source,
            card_hash: &card_hash,
            capabilities: &capabilities,
        };
        let kept_contact = parts.kept::<ContactRecord>(CONTACT)?;
        let kept_announce = parts.kept::<AnnounceRecord>(ANNOUNCE)?;
        let contact = match parts.next_contact(kept_contact.as_ref(), now_unix)? {
            Some(issued) => issued,
            None => kept_contact.expect("kept, since nothing was issued"),
        };
        let announce = match parts.next_announce(kept_announce.as_ref(), now_unix)? {
            Some(issued) => issued,
            None => kept_announce.expect("kept, since nothing was issued"),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                key,
                store,
                contact_source,
                card_hash,
                capabilities,
                renewing: Mutex::new(()),
                current: RwLock::new(Current { contact, announce }),
            }),
        })
    }

    /// The contact and announce records in force, each as its signed JSON.
    pub fn current(&self) -> (Vec<u8>, Vec<u8>) {
        let current = self
            .inner
            .current
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        (current.contact.json.clone(), current.announce.json.clone())
    }

    /// Issues again, at `now_unix`, each record whose content changed (the
    /// addresses of the contact record) or that has less than
    /// [`RECORD_RENEW_BEFORE_S`] left; whether it issued any.
    pub fn renew(&self, now_unix: i64) -> anyhow::Result<bool> {
        let inner = &self.inner;
        let _renewing = inner
            .renewing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let held = inner
            .current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let parts = Parts {
            key: &inner.key,
            store: &inner.store,
            contact_source: &inner.contact_source,
            card_hash: &inner.card_hash,
            capabilities: &inner.capabilities,
        };
        let contact = parts.next_contact(Some(&held.contact), now_unix)?;
        let announce = parts.next_announce(Some(&held.announce), now_unix)?;
        let renewed = contact.is_some() || announce.is_some();
        if renewed {
            let mut current = inner
                .current
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(contact) = contact {
                current.contact = contact;
            }
            if let Some(announce) = announce {
                current.announce = announce;
            }
        }
        Ok(renewed)
    }
}

/// What issuing a record works with.
struct Parts<'a> {
    key: &'a AgentKey,
    store: &'a Store,
    contact_source: &'a ContactSource,
    card_hash: &'a CardHash,
    capabilities: &'a [String],
}

impl Parts<'_> {
    /// This agent's record of `kind` that the store kept, when it is one of
    /// this agent's that verifies; `None` otherwise.
    fn kept<T: SignedDoc + Clone>(&self, kind: &str) -> anyhow::Result<Option<Live<T>>> {
        let Some((seq, json)) = self.store.own_record(kind)? else {
            return Ok(None);
        };
        let Ok(signed) = Signed::<T>::from_json(&json) else {
            tracing::warn!(
                kind,
                seq,
                "the kept record does not verify: issuing a new one"
            );
            return Ok(None);
        };
        if *signed.doc.signer() != self.key.id() {
            tracing::warn!(
                kind,
                seq,
                "the kept record is another agent's: issuing a new one"
            );
            return Ok(None);
        }
        Ok(Some(Live {
            doc: signed.doc,
            json,
        }))
    }

    /// A new contact record when `held` says something else than the
    /// endpoint's addresses now, or is due; `None` when `held` stays.
    fn next_contact(
        &self,
        held: Option<&Live<ContactRecord>>,
        now_unix: i64,
    ) -> anyhow::Result<Option<Live<ContactRecord>>> {
        let iroh = (self.contact_source)();
        if let Some(held) = held
            && held.doc.iroh == iroh
            && !due(&held.doc.expires_at, now_unix)
        {
            return Ok(None);
        }
        let agent_id = self.key.id();
        self.issue(CONTACT, now_unix, |seq, issued_at, expires_at| {
            ContactRecord {
                schema: CONTACT_SCHEMA.to_owned(),
                agent_id,
                iroh,
                seq,
                issued_at,
                expires_at,
            }
        })
        .map(Some)
    }

    /// A new announce record when `held` names another card or other
    /// capabilities, or is due; `None` when `held` stays.
    fn next_announce(
        &self,
        held: Option<&Live<AnnounceRecord>>,
        now_unix: i64,
    ) -> anyhow::Result<Option<Live<AnnounceRecord>>> {
        if let Some(held) = held
            && held.doc.agent_card_hash == *self.card_hash
            && held.doc.capabilities == self.capabilities
            && held.doc.discovery_alpn == DISCOVERY_ALPN
            && !due(&held.doc.expires_at, now_unix)
        {
            return Ok(None);
        }
        let agent_id = self.key.id();
        self.issue(ANNOUNCE, now_unix, |seq, issued_at, expires_at| {
            AnnounceRecord {
                schema: ANNOUNCE_SCHEMA.to_owned(),
                agent_id,
                seq,
                agent_card_hash: self.card_hash.clone(),
                capabilities: self.capabilities.to_vec(),
                discovery_alpn: DISCOVERY_ALPN.to_owned(),
                issued_at,
                expires_at,
            }
        })
        .map(Some)
    }

    /// Issues the record `build` makes from the next `seq` of `kind`, issued
    /// at `now_unix` for the whole [`RECORD_TTL_S`], signs it and stores it.
    fn issue<T: SignedDoc>(
        &self,
        kind: &str,
        now_unix: i64,
        build: impl FnOnce(u64, String, String) -> T,
    ) -> anyhow::Result<Live<T>> {
        let issued_at = format_timestamp(now_unix)
            .with_context(|| format!("{now_unix} is not a time a record can be issued at"))?;
        let expires_at = now_unix
            .checked_add(RECORD_TTL_S)
            .and_then(format_timestamp)
            .with_context(|| format!("a record issued at {issued_at} expires out of range"))?;
        let seq = self.store.own_seq_next(kind)?;
        let signed = Signed::issue(self.key, build(seq, issued_at, expires_at))
            .map_err(|err| anyhow!("cannot sign this agent's {kind} record: {err}"))?;
        let json = signed.to_json();
        self.store.own_record_put(kind, seq, &json, now_unix)?;
        tracing::info!(kind, seq, "issued a signed record");
        Ok(Live {
            doc: signed.doc,
            json,
        })
    }
}

/// Whether a record expiring at `expires_at` has less than
/// [`RECORD_RENEW_BEFORE_S`] left at `now_unix`, or an expiry that does not
/// read.
fn due(expires_at: &str, now_unix: i64) -> bool {
    parse_timestamp(expires_at).is_none_or(|at| at - now_unix < RECORD_RENEW_BEFORE_S)
}
