//! What a router stores, and why it refuses the rest
//! (`docs/protocols/agentnet-kad-0.md`, section 6): pure validators that
//! the swarm runs on every inbound `PutRecord` and `AddProvider`
//! (`StoreInserts::FilterBoth`) before anything reaches its store. A
//! searcher reads the contacts and announces it gets with [`contact_for`]
//! and [`announce_for`].
//!
//! A value, in this order, the first failure winning:
//! 1. its key is [`slot`]'s, a contact's or an announce's
//!    ([`Rejection::UnknownKey`]), and the value at most
//!    [`MAX_RECORD_BYTES`], checked before it is read
//!    ([`Rejection::TooLarge`]);
//! 2. it is that agent's record: it passes [`Signed::from_json`]
//!    ([`Rejection::BadContact`], [`Rejection::BadAnnounce`]), the agent its
//!    key names signed it ([`Rejection::WrongKey`]), and it is inside its
//!    validity window, with [`CLOCK_SKEW_S`] at both ends
//!    ([`Rejection::NotYetValid`], [`Rejection::Expired`]);
//! 3. what the router holds under that key, when it is still a valid
//!    record of that agent, does not have a higher `seq`, nor the same
//!    `seq` with other bytes ([`Verdict::Stale`]). The same bytes again are
//!    [`Verdict::Store`]: a republication is never refused.
//!
//! A provider record, in this order:
//! 1. its key is a SHA2-256 multihash, 34 bytes
//!    ([`Rejection::BadProviderKey`]);
//! 2. its `PeerId` is an agent's ([`agent_of`], [`Rejection::NotAnAgent`]),
//!    which `libp2p-kad` has already checked is the peer that sent it, and
//!    Iroh's TLS that it holds that key;
//! 3. the router holds a valid announce of that agent
//!    ([`Rejection::ProviderWithoutAnnounce`]);
//! 4. that announce lists a capability `c` whose [`provider_key`] is the
//!    record's key ([`Rejection::CapabilityNotAnnounced`]).
//!
//! A router therefore admits an agent's provider records only for the
//! capabilities of its signed announce, at most
//! [`MAX_ANNOUNCED_CAPABILITIES`], and the publisher sends the announce
//! before its provider records. The check is made as each provider record
//! arrives: those already held outlive an announce that replaces the one
//! they were checked against, until they expire.
//!
//! A [`Verdict::Stale`] record is kept out, so that a replicated copy never
//! takes a newer one's place, and is not counted as rejected: nothing in it
//! is invalid. A [`Verdict::Reject`] is counted by its reason, and
//! [`Rejection::StoreFull`] is the swarm's, never a validator's: the store
//! that has no room left.
//!
//! [`CLOCK_SKEW_S`]: pals_core::grant::CLOCK_SKEW_S
//! [`MAX_ANNOUNCED_CAPABILITIES`]: pals_core::records::MAX_ANNOUNCED_CAPABILITIES

use libp2p::PeerId;
use libp2p::kad::RecordKey;
use pals_core::identity::AgentId;
use pals_core::records::{AnnounceRecord, ContactRecord, MAX_RECORD_BYTES};
use pals_core::signed::{Signed, SignedDoc, SignedError, check_validity};

use super::keys::{SHA2_256, Slot, agent_of, provider_key, slot};

/// What a router does with a record offered to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Stores it.
    Store,
    /// Keeps the one it holds, and does not count this one as rejected.
    Stale,
    /// Refuses it, and counts it by its reason.
    Reject(Rejection),
}

/// Why a router refuses a record. [`Rejection::as_str`] is the name in the
/// logs and in `pals dht status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rejection {
    /// A value under a key that is not a contact's or an announce's.
    UnknownKey,
    /// A value of more than [`MAX_RECORD_BYTES`].
    TooLarge,
    /// A record signed by an agent other than the one its key names.
    WrongKey,
    /// A value under a contact's key that is no signed contact.
    BadContact,
    /// A value under an announce's key that is no signed announce.
    BadAnnounce,
    /// A record past its validity window.
    Expired,
    /// A record before its validity window.
    NotYetValid,
    /// A provider record whose key is no SHA2-256 multihash.
    BadProviderKey,
    /// A provider record whose `PeerId` is no agent's.
    NotAnAgent,
    /// A provider record of an agent whose announce the router does not
    /// hold.
    ProviderWithoutAnnounce,
    /// A provider record of a capability the agent's announce does not
    /// list.
    CapabilityNotAnnounced,
    /// A record the store has no room for. The swarm's, never a
    /// validator's.
    StoreFull,
}

impl Rejection {
    /// Every rejection, in declaration order.
    pub const ALL: [Self; 12] = [
        Self::UnknownKey,
        Self::TooLarge,
        Self::WrongKey,
        Self::BadContact,
        Self::BadAnnounce,
        Self::Expired,
        Self::NotYetValid,
        Self::BadProviderKey,
        Self::NotAnAgent,
        Self::ProviderWithoutAnnounce,
        Self::CapabilityNotAnnounced,
        Self::StoreFull,
    ];

    /// Its stable name, in `snake_case`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown_key",
            Self::TooLarge => "too_large",
            Self::WrongKey => "wrong_key",
            Self::BadContact => "bad_contact",
            Self::BadAnnounce => "bad_announce",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::BadProviderKey => "bad_provider_key",
            Self::NotAnAgent => "not_an_agent",
            Self::ProviderWithoutAnnounce => "provider_without_announce",
            Self::CapabilityNotAnnounced => "capability_not_announced",
            Self::StoreFull => "store_full",
        }
    }
}

/// The verdict on a `PutRecord` of `value` under `key` at `now_unix`;
/// `held` is what the router holds under `key`, if anything.
pub fn check_put(key: &RecordKey, value: &[u8], held: Option<&[u8]>, now_unix: i64) -> Verdict {
    let Some(slot) = slot(key) else {
        return Verdict::Reject(Rejection::UnknownKey);
    };
    let seq_of = |bytes: &[u8]| match slot {
        Slot::Contact(agent) => contact_for(&agent, bytes, now_unix).map(|c| c.doc.seq),
        Slot::Announce(agent) => announce_for(&agent, bytes, now_unix).map(|a| a.doc.seq),
    };
    let seq = match seq_of(value) {
        Ok(seq) => seq,
        Err(why) => return Verdict::Reject(why),
    };
    // What is held counts only while it is valid.
    let held = held.and_then(|bytes| Some((seq_of(bytes).ok()?, bytes)));
    match held {
        Some((held_seq, bytes)) if held_seq > seq || (held_seq == seq && bytes != value) => {
            Verdict::Stale
        }
        _ => Verdict::Store,
    }
}

/// The verdict on an `AddProvider` of `provider` under `key` at
/// `now_unix`; `held_announce` is what the router holds under that
/// provider's announce key, if anything.
pub fn check_provider(
    key: &RecordKey,
    provider: &PeerId,
    held_announce: Option<&[u8]>,
    now_unix: i64,
) -> Verdict {
    let bytes = key.as_ref();
    if bytes.len() != SHA2_256.len() + 32 || !bytes.starts_with(&SHA2_256) {
        return Verdict::Reject(Rejection::BadProviderKey);
    }
    let Some(agent) = agent_of(provider) else {
        return Verdict::Reject(Rejection::NotAnAgent);
    };
    let Some(announce) = held_announce.and_then(|held| announce_for(&agent, held, now_unix).ok())
    else {
        return Verdict::Reject(Rejection::ProviderWithoutAnnounce);
    };
    let announced = announce
        .doc
        .capabilities
        .iter()
        .any(|capability| provider_key(capability).is_ok_and(|announced| announced == *key));
    if announced {
        Verdict::Store
    } else {
        Verdict::Reject(Rejection::CapabilityNotAnnounced)
    }
}

/// `value` as `agent`'s contact at `now_unix`: at most
/// [`MAX_RECORD_BYTES`], a signed contact, signed by `agent`, inside its
/// validity window.
pub fn contact_for(
    agent: &AgentId,
    value: &[u8],
    now_unix: i64,
) -> Result<Signed<ContactRecord>, Rejection> {
    let bad = Rejection::BadContact;
    let contact = signed_record::<ContactRecord>(value, bad)?;
    let c = &contact.doc;
    signed_by(agent, &c.agent_id)?;
    in_window(&c.issued_at, &c.expires_at, now_unix, bad)?;
    Ok(contact)
}

/// `value` as `agent`'s announce at `now_unix`, checked as [`contact_for`]
/// checks a contact.
pub fn announce_for(
    agent: &AgentId,
    value: &[u8],
    now_unix: i64,
) -> Result<Signed<AnnounceRecord>, Rejection> {
    let bad = Rejection::BadAnnounce;
    let announce = signed_record::<AnnounceRecord>(value, bad)?;
    let a = &announce.doc;
    signed_by(agent, &a.agent_id)?;
    in_window(&a.issued_at, &a.expires_at, now_unix, bad)?;
    Ok(announce)
}

/// `value` as a signed record, its size checked before it is read.
fn signed_record<T: SignedDoc>(value: &[u8], bad: Rejection) -> Result<Signed<T>, Rejection> {
    if value.len() > MAX_RECORD_BYTES {
        return Err(Rejection::TooLarge);
    }
    Signed::<T>::from_json(value).map_err(|_| bad)
}

/// The record's signer, which [`Signed::from_json`] verified, is `agent`.
fn signed_by(agent: &AgentId, signer: &AgentId) -> Result<(), Rejection> {
    if signer == agent {
        Ok(())
    } else {
        Err(Rejection::WrongKey)
    }
}

fn in_window(
    issued_at: &str,
    expires_at: &str,
    now_unix: i64,
    bad: Rejection,
) -> Result<(), Rejection> {
    check_validity(issued_at, expires_at, now_unix).map_err(|error| match error {
        SignedError::NotYetValid => Rejection::NotYetValid,
        SignedError::Expired => Rejection::Expired,
        _ => bad,
    })
}
