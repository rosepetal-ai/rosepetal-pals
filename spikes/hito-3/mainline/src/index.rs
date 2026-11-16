//! The capability index this candidate defines over Mainline, which has
//! none of its own (spec §6.1), and what a router accepts into it.
//!
//! Everything goes under the agent's own Ed25519 key, which is also its
//! Iroh endpoint and its `AgentId`:
//!
//! - **contact:** a BEP 44 mutable item with salt [`Kind::Contact`]'s,
//!   `agentnet:contact:v1`, whose value is the JSON of the agent's
//!   `Signed<ContactRecord>` and whose `seq` is the record's;
//! - **announce:** the same with salt `agentnet:announce:v1` and the
//!   `Signed<AnnounceRecord>`;
//! - **index:** one signed peer per capability (n0-mainline's
//!   `announce_signed_peer`, a draft BEP behind its `unstable_signed_peers`
//!   feature) under [`capability_hash`], the SHA-1 of the capability
//!   string. A router keeps the agent's key, its timestamp and its
//!   signature, so a search gets identities, not `ip:port` as with BEP 5's
//!   `announce_peer`, and nothing has to turn addresses into agents.
//!
//! The DHT checks BEP 44's signature and that the item's target is
//! `SHA-1(key + salt)`, and a signed peer's signature and that its
//! timestamp is within 45 s. What PALS needs beyond that, a router checks
//! here before the library sees the put ([`check_item`],
//! [`check_provider`]); each refusal has a reason, as `records_rejected`
//! counts them.

use anyhow::ensure;
use n0_mainline::{Id, MutableItem, SigningKey};
use pals_core::identity::AgentId;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::{Signed, check_validity};
use spike_common::records::{BAD_ANNOUNCE, BAD_CONTACT, EXPIRED};

/// BEP 44's limit on a value, which n0-mainline's server enforces (error
/// 205): smaller than `pals-core`'s 4 KiB for a record.
pub const MAX_VALUE_BYTES: usize = 1000;

/// An item under a salt this index does not define, or a put of a kind it
/// does not use (immutable items, BEP 5 peers).
pub const UNKNOWN_KEY: &str = "unknown_key";
/// The key of the put is not an Ed25519 point, so not an agent.
pub const BAD_KEY: &str = "bad_key";
/// The record inside is another agent's than the key that signed the item.
pub const WRONG_KEY: &str = "wrong_key";
/// BEP 44's `seq` is not the record's.
pub const SEQ_MISMATCH: &str = "seq_mismatch";
/// A signed peer for a capability that the agent's stored announce does
/// not list.
pub const CAPABILITY_NOT_ANNOUNCED: &str = "capability_not_announced";

/// The two items an agent publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Contact,
    Announce,
}

impl Kind {
    /// BEP 44's salt of this kind of item.
    pub fn salt(self) -> &'static [u8] {
        match self {
            Kind::Contact => b"agentnet:contact:v1",
            Kind::Announce => b"agentnet:announce:v1",
        }
    }

    /// The kind whose salt is `salt`; `None` for any other, and for none.
    pub fn of_salt(salt: Option<&[u8]>) -> Option<Kind> {
        [Kind::Contact, Kind::Announce]
            .into_iter()
            .find(|kind| salt == Some(kind.salt()))
    }
}

/// What a router takes from a put it accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub agent: AgentId,
    pub kind: Kind,
    /// For an announce, the infohash of each capability it lists; empty for
    /// a contact.
    pub capabilities: Vec<Id>,
}

/// The infohash of a capability: the SHA-1 of its whole string,
/// `agentnet:capability:v1:<id>`, whose prefix already sets PALS's
/// capabilities apart from anything else announced in a DHT.
pub fn capability_hash(capability: &str) -> Id {
    Id::from(
        sha1_smol::Sha1::from(capability.as_bytes())
            .digest()
            .bytes(),
    )
}

/// Where an agent's item of `kind` lives: BEP 44's `SHA-1(key + salt)`.
pub fn target(agent: &AgentId, kind: Kind) -> Id {
    MutableItem::target_from_key(agent.as_bytes(), Some(kind.salt()))
}

/// The item of `kind` that carries `json`, signed with the agent's key,
/// with the record's `seq`. Fails when `json` does not fit in
/// [`MAX_VALUE_BYTES`].
pub fn item(signer: &SigningKey, kind: Kind, json: &[u8], seq: u64) -> anyhow::Result<MutableItem> {
    ensure!(
        json.len() <= MAX_VALUE_BYTES,
        "the {kind:?} record is {} bytes, and a BEP 44 item holds at most {MAX_VALUE_BYTES}",
        json.len()
    );
    let seq = i64::try_from(seq)?;
    Ok(MutableItem::new(signer, json, seq, Some(kind.salt())))
}

/// Whether a router takes a mutable item, put under `key` and `salt` with
/// `value` and `seq`, at `now_unix`: its salt is one of [`Kind`]'s, its
/// value is that kind's signed record as `pals-core` checks it (signature,
/// canonical form, the record's own rules), of the agent whose key signed
/// the item, with that `seq`, and inside its validity window. The first
/// check that fails gives the reason.
pub fn check_item(
    key: &[u8; 32],
    salt: Option<&[u8]>,
    value: &[u8],
    seq: i64,
    now_unix: i64,
) -> Result<Accepted, &'static str> {
    let kind = Kind::of_salt(salt).ok_or(UNKNOWN_KEY)?;
    let agent = agent_of(key)?;
    let (record_agent, record_seq, (issued_at, expires_at), capabilities) = match kind {
        Kind::Contact => {
            let contact = Signed::<ContactRecord>::from_json(value).map_err(|_| BAD_CONTACT)?;
            let doc = contact.doc;
            (
                doc.agent_id,
                doc.seq,
                (doc.issued_at, doc.expires_at),
                Vec::new(),
            )
        }
        Kind::Announce => {
            let announce = Signed::<AnnounceRecord>::from_json(value).map_err(|_| BAD_ANNOUNCE)?;
            let doc = announce.doc;
            let capabilities = doc
                .capabilities
                .iter()
                .map(|c| capability_hash(c))
                .collect();
            (
                doc.agent_id,
                doc.seq,
                (doc.issued_at, doc.expires_at),
                capabilities,
            )
        }
    };
    if record_agent != agent {
        return Err(WRONG_KEY);
    }
    if i64::try_from(record_seq).ok() != Some(seq) {
        return Err(SEQ_MISMATCH);
    }
    check_validity(&issued_at, &expires_at, now_unix).map_err(|_| EXPIRED)?;
    Ok(Accepted {
        agent,
        kind,
        capabilities,
    })
}

/// Whether a router takes a signed peer of `key` under `info_hash`: the key
/// is an agent's and, when the router accepted an announce of that agent
/// (`announced`, the infohashes of the newest one's capabilities), the
/// announce lists a capability with that infohash. Without an announce it
/// cannot tell which capability the infohash is, nor whether the agent
/// offers it.
pub fn check_provider(
    key: &[u8; 32],
    info_hash: &Id,
    announced: Option<&[Id]>,
) -> Result<AgentId, &'static str> {
    let agent = agent_of(key)?;
    match announced {
        Some(capabilities) if !capabilities.contains(info_hash) => Err(CAPABILITY_NOT_ANNOUNCED),
        _ => Ok(agent),
    }
}

/// The agent whose public key is `key`.
pub fn agent_of(key: &[u8; 32]) -> Result<AgentId, &'static str> {
    let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    hex.parse().map_err(|_| BAD_KEY)
}
