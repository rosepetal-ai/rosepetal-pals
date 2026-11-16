//! The DHT's keys and what a router stores under them: the validators of
//! spec §6.2, which `behaviour` runs on every inbound `PutRecord` and
//! `AddProvider` (`StoreInserts::FilterBoth`) before anything reaches the
//! store, and which a searcher runs on what it gets.
//!
//! Copied from kad-libp2p's `validate.rs`. What changed: the `PeerId` is
//! the agent's own key ([`peer_of`], [`agent_of`]), so there is no
//! identity link. The contact goes under its own key, and every record is
//! checked against the agent its key names, not against a link held.
//!
//! The keys:
//! - `/agentnet/contact/<peer_id>`: the agent's `Signed<ContactRecord>`, as
//!   `pals-core` writes it;
//! - `/agentnet/announce/<peer_id>`: its `Signed<AnnounceRecord>`, also as
//!   `pals-core` writes it;
//! - a capability's provider records: the SHA2-256 multihash (code `0x12`)
//!   of the UTF-8 bytes of the whole capability string (spec §5.1).
//!
//! A contact or an announce must verify as `pals-core`'s, inside its
//! validity window, and be signed by the agent whose `PeerId` its key
//! names. A provider record names only a `PeerId`, which `libp2p-kad`
//! already checked is the peer that sent it, and Iroh's TLS that it is
//! that agent's key. Its key is a hash, so a router can check only its
//! form and, when it holds that `PeerId`'s announce, that the announce
//! lists a capability with that hash.
//!
//! A record older than the one held, by `seq`, is [`Verdict::Stale`]: kept
//! out, so that a replicated copy never takes a newer one's place, but not
//! counted as rejected, since nothing in it is invalid.

use std::str::FromStr;

use libp2p::PeerId;
use libp2p::kad::RecordKey;
use pals_core::identity::AgentId;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::{Signed, SignedError, check_validity};
use sha2::{Digest, Sha256};

use crate::transport;

/// The prefix of a contact's key.
pub const CONTACT_PREFIX: &str = "/agentnet/contact/";
/// The prefix of an announce's key.
pub const ANNOUNCE_PREFIX: &str = "/agentnet/announce/";
/// The multihash code of SHA2-256, and its digest's length.
const SHA2_256: [u8; 2] = [0x12, 0x20];

/// The agent's `PeerId`: its own Ed25519 public key.
pub fn peer_of(agent: &AgentId) -> PeerId {
    let endpoint = iroh::EndpointId::from_bytes(agent.as_bytes())
        .expect("an agent id is an Ed25519 public key");
    transport::peer_id(&endpoint)
}

/// The agent a `PeerId` is, if it is an Ed25519 key's.
pub fn agent_of(peer: &PeerId) -> Option<AgentId> {
    let endpoint = transport::endpoint_id(peer)?;
    let hex: String = endpoint
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    AgentId::from_str(&hex).ok()
}

/// Where a peer's contact is.
pub fn contact_key(peer: &PeerId) -> RecordKey {
    RecordKey::new(&format!("{CONTACT_PREFIX}{peer}"))
}

/// Where a peer's announce is.
pub fn announce_key(peer: &PeerId) -> RecordKey {
    RecordKey::new(&format!("{ANNOUNCE_PREFIX}{peer}"))
}

/// The key of a capability's provider records.
pub fn provider_key(capability: &str) -> RecordKey {
    let mut key = SHA2_256.to_vec();
    key.extend_from_slice(&Sha256::digest(capability.as_bytes()));
    RecordKey::from(key)
}

/// What a value's key says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Contact(PeerId),
    Announce(PeerId),
}

impl Slot {
    pub fn peer(self) -> PeerId {
        match self {
            Self::Contact(peer) | Self::Announce(peer) => peer,
        }
    }
}

/// The slot `key` names, if it is one of the two kinds of value.
pub fn slot(key: &RecordKey) -> Option<Slot> {
    let text = std::str::from_utf8(key.as_ref()).ok()?;
    let peer = |rest: &str| {
        PeerId::from_str(rest)
            .ok()
            .filter(|peer| peer.to_base58() == rest)
    };
    if let Some(rest) = text.strip_prefix(CONTACT_PREFIX) {
        return peer(rest).map(Slot::Contact);
    }
    text.strip_prefix(ANNOUNCE_PREFIX)
        .and_then(peer)
        .map(Slot::Announce)
}

/// What a router does with a record offered to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Store,
    /// Keeps the newer one it holds.
    Stale,
    /// Refuses it, and counts it: why.
    Reject(&'static str),
}

/// The verdict on a `PutRecord` of `value` under `key` at `now_unix`;
/// `held` gives what the router holds under a key, unexpired.
pub fn check_put(
    key: &RecordKey,
    value: &[u8],
    held: impl Fn(&RecordKey) -> Option<Vec<u8>>,
    now_unix: i64,
) -> Verdict {
    let newer_held = |seq: u64, held_seq: Option<u64>| held_seq.is_some_and(|old| old > seq);
    match slot(key) {
        None => Verdict::Reject("unknown_key"),
        Some(Slot::Contact(peer)) => match contact_for(&peer, value, now_unix) {
            Err(why) => Verdict::Reject(why),
            Ok(contact) => {
                let old = held(key).and_then(|v| contact_for(&peer, &v, now_unix).ok());
                if newer_held(contact.doc.seq, old.map(|c| c.doc.seq)) {
                    Verdict::Stale
                } else {
                    Verdict::Store
                }
            }
        },
        Some(Slot::Announce(peer)) => match announce_for(&peer, value, now_unix) {
            Err(why) => Verdict::Reject(why),
            Ok(announce) => {
                let old = held(key).and_then(|v| announce_for(&peer, &v, now_unix).ok());
                if newer_held(announce.doc.seq, old.map(|a| a.doc.seq)) {
                    Verdict::Stale
                } else {
                    Verdict::Store
                }
            }
        },
    }
}

/// The verdict on an `AddProvider` of `provider` under `key` at
/// `now_unix`; `held` as in [`check_put`]. `libp2p-kad` has already dropped
/// one whose `provider` is not the peer that sent it.
pub fn check_provider(
    key: &RecordKey,
    provider: &PeerId,
    held: impl Fn(&RecordKey) -> Option<Vec<u8>>,
    now_unix: i64,
) -> Verdict {
    let bytes = key.as_ref();
    if bytes.len() != SHA2_256.len() + 32 || bytes[..2] != SHA2_256 {
        return Verdict::Reject("bad_provider_key");
    }
    if agent_of(provider).is_none() {
        return Verdict::Reject("not_an_agent");
    }
    if let Some(announce) =
        held(&announce_key(provider)).and_then(|v| announce_for(provider, &v, now_unix).ok())
        && !announce
            .doc
            .capabilities
            .iter()
            .any(|capability| provider_key(capability) == *key)
    {
        return Verdict::Reject("capability_not_announced");
    }
    Verdict::Store
}

/// `value` as `peer`'s contact: it verifies as `pals-core`'s, inside its
/// validity window, and `peer`'s agent signed it.
pub fn contact_for(
    peer: &PeerId,
    value: &[u8],
    now_unix: i64,
) -> Result<Signed<ContactRecord>, &'static str> {
    let contact = Signed::<ContactRecord>::from_json(value).map_err(|_| "bad_contact")?;
    let c = &contact.doc;
    signed_by(peer, &c.agent_id)?;
    in_window(&c.issued_at, &c.expires_at, now_unix)?;
    Ok(contact)
}

/// `value` as `peer`'s announce, checked as [`contact_for`] checks a
/// contact.
pub fn announce_for(
    peer: &PeerId,
    value: &[u8],
    now_unix: i64,
) -> Result<Signed<AnnounceRecord>, &'static str> {
    let announce = Signed::<AnnounceRecord>::from_json(value).map_err(|_| "bad_announce")?;
    let a = &announce.doc;
    signed_by(peer, &a.agent_id)?;
    in_window(&a.issued_at, &a.expires_at, now_unix)?;
    Ok(announce)
}

/// The record's signer, which `Signed::from_json` verified, is the agent
/// `peer` is.
fn signed_by(peer: &PeerId, signer: &AgentId) -> Result<(), &'static str> {
    if agent_of(peer) == Some(*signer) {
        Ok(())
    } else {
        Err("wrong_key")
    }
}

fn in_window(issued_at: &str, expires_at: &str, now_unix: i64) -> Result<(), &'static str> {
    check_validity(issued_at, expires_at, now_unix).map_err(|error| match error {
        SignedError::NotYetValid => "not_yet_valid",
        SignedError::Expired => "expired",
        _ => "malformed",
    })
}
