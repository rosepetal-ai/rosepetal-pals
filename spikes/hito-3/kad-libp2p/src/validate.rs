//! The DHT's keys and what a router stores under them: the validators of
//! spec §6.2, which `behaviour` runs on every inbound `PutRecord` and
//! `AddProvider` (`StoreInserts::FilterBoth`) before anything reaches the
//! store, and which a searcher runs on what it gets.
//!
//! The keys:
//! - `/agentnet/link/<peer_id>`: the node's link ([`crate::link`]), which
//!   carries its contact;
//! - `/agentnet/announce/<peer_id>`: its `Signed<AnnounceRecord>`, as
//!   `pals-core` writes it;
//! - a capability's provider records: the SHA2-256 multihash (code `0x12`)
//!   of the UTF-8 bytes of the whole capability string (spec §5.1).
//!
//! Kademlia carries each record on its own and in any order (a publisher's
//! puts race, and routers replicate what they hold), so a router can demand
//! of each record only what it proves by itself, and refuses what
//! contradicts what it already holds:
//! - a link must verify whole, under its own `PeerId`'s key;
//! - an announce must verify as `pals-core`'s, and be of the agent the link
//!   held for that `PeerId` binds; with no link held the binding is the
//!   searcher's to check;
//! - a provider record names only a `PeerId`, which `libp2p-kad` already
//!   checked is the peer that sent it. Its key is a hash, so a router can
//!   check only its form and, when it holds that `PeerId`'s announce, that
//!   the announce lists a capability with that hash.
//!
//! A record older than the one held, by `seq` of the same agent, is
//! [`Verdict::Stale`]: kept out, so that a replicated copy never takes a
//! newer one's place, but not counted as rejected, since nothing in it is
//! invalid.

use std::str::FromStr;

use libp2p::PeerId;
use libp2p::kad::RecordKey;
use pals_core::identity::AgentId;
use pals_core::records::AnnounceRecord;
use pals_core::signed::{Signed, SignedError, check_validity};
use sha2::{Digest, Sha256};

use crate::link::{self, Link};

/// The prefix of a link's key.
pub const LINK_PREFIX: &str = "/agentnet/link/";
/// The prefix of an announce's key.
pub const ANNOUNCE_PREFIX: &str = "/agentnet/announce/";
/// The multihash code of SHA2-256, and its digest's length.
const SHA2_256: [u8; 2] = [0x12, 0x20];

/// Where a peer's link is.
pub fn link_key(peer: &PeerId) -> RecordKey {
    RecordKey::new(&format!("{LINK_PREFIX}{peer}"))
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
    Link(PeerId),
    Announce(PeerId),
}

/// The slot `key` names, if it is one of the two kinds of value.
pub fn slot(key: &RecordKey) -> Option<Slot> {
    let text = std::str::from_utf8(key.as_ref()).ok()?;
    let peer = |rest: &str| {
        PeerId::from_str(rest)
            .ok()
            .filter(|peer| peer.to_base58() == rest)
    };
    if let Some(rest) = text.strip_prefix(LINK_PREFIX) {
        return peer(rest).map(Slot::Link);
    }
    text.strip_prefix(ANNOUNCE_PREFIX)
        .and_then(peer)
        .map(Slot::Announce)
}

/// What a router does with a record offered to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Stores it; the agent it is about, when the record names one.
    Store(Option<AgentId>),
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
    match slot(key) {
        None => Verdict::Reject("unknown_key"),
        Some(Slot::Link(peer)) => {
            let link = match link_for(&peer, value, now_unix) {
                Ok(link) => link,
                Err(why) => return Verdict::Reject(why),
            };
            if let Some(old) = held(key).and_then(|v| link_for(&peer, &v, now_unix).ok())
                && old.agent_id == link.agent_id
                && old.contact.doc.seq > link.contact.doc.seq
            {
                return Verdict::Stale;
            }
            Verdict::Store(Some(link.agent_id))
        }
        Some(Slot::Announce(peer)) => {
            let announce = match announce_of(value, now_unix) {
                Ok(announce) => announce,
                Err(why) => return Verdict::Reject(why),
            };
            let agent = announce.doc.agent_id;
            if let Some(link) =
                held(&link_key(&peer)).and_then(|v| link_for(&peer, &v, now_unix).ok())
                && link.agent_id != agent
            {
                return Verdict::Reject("agent_mismatch");
            }
            if let Some(old) = held(key).and_then(|v| announce_of(&v, now_unix).ok())
                && old.doc.agent_id == agent
                && old.doc.seq > announce.doc.seq
            {
                return Verdict::Stale;
            }
            Verdict::Store(Some(agent))
        }
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
    if let Some(announce) =
        held(&announce_key(provider)).and_then(|v| announce_of(&v, now_unix).ok())
        && !announce
            .doc
            .capabilities
            .iter()
            .any(|capability| provider_key(capability) == *key)
    {
        return Verdict::Reject("capability_not_announced");
    }
    Verdict::Store(None)
}

/// `value` as `peer`'s link: it verifies, and it is `peer`'s.
pub fn link_for(peer: &PeerId, value: &[u8], now_unix: i64) -> Result<Link, &'static str> {
    let link = link::verify(value, now_unix).map_err(|error| error.code())?;
    if link.peer_id != *peer {
        return Err("wrong_key");
    }
    Ok(link)
}

/// `value` as an announce: it verifies as `pals-core`'s, inside its
/// validity window.
pub fn announce_of(value: &[u8], now_unix: i64) -> Result<Signed<AnnounceRecord>, &'static str> {
    let announce = Signed::<AnnounceRecord>::from_json(value).map_err(|_| "bad_announce")?;
    let a = &announce.doc;
    check_validity(&a.issued_at, &a.expires_at, now_unix).map_err(|error| match error {
        SignedError::NotYetValid => "not_yet_valid",
        SignedError::Expired => "expired",
        _ => "bad_announce",
    })?;
    Ok(announce)
}
