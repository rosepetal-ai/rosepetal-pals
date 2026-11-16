//! Signed contact and announce records (`agentnet.contact/1`,
//! `agentnet.announce/1`).
//!
//! An agent issues both about itself and signs them with its own key, each
//! under its own domain ([`crate::signed::Signed`]). The contact record says how to reach
//! its Iroh endpoint; the announce record names the exact signed version of
//! its public Agent Card (`agent_card_hash`, [`crate::card_jws::card_hash`]),
//! the public capabilities it offers and the ALPN of its discovery protocol.
//! Both travel as `{"record": …, "signature": …}`.
//!
//! Each carries a `seq` its agent never repeats for that kind
//! ([`crate::store::Store::own_seq_next`]) and expires at most
//! [`RECORD_TTL_S`] after it was issued; the agent issues every record, in
//! `seq` order, with that whole lifetime, and a new one when less than
//! [`RECORD_RENEW_BEFORE_S`] remain, or when its content changes.
//! A receiver refuses one outside its validity window, with
//! [`crate::grant::CLOCK_SKEW_S`] of tolerance ([`check_validity`]), and one
//! that goes back from the record it knows of that agent and kind, until a
//! whole [`RECORD_TTL_S`] after that one expired
//! ([`crate::store::Store::peer_record_accept`]). In milestone 2 they travel
//! in every discovery response; the DHT will publish them later.
//!
//! [`check_validity`]: crate::signed::check_validity

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::card_jws::CardHash;
use crate::grant::parse_timestamp;
use crate::identity::AgentId;
use crate::signed::{SignedDoc, SignedError};

/// The value of [`ContactRecord::schema`].
pub const CONTACT_SCHEMA: &str = "agentnet.contact/1";
/// The prefix of the bytes a contact record signature covers.
pub const CONTACT_SIG_DOMAIN: &[u8] = b"agentnet.contact/1\n";
/// The value of [`AnnounceRecord::schema`].
pub const ANNOUNCE_SCHEMA: &str = "agentnet.announce/1";
/// The prefix of the bytes an announce record signature covers.
pub const ANNOUNCE_SIG_DOMAIN: &[u8] = b"agentnet.announce/1\n";
/// How long a record is valid after it is issued, in seconds.
pub const RECORD_TTL_S: i64 = 24 * 3600;
/// A record with less than this left before it expires is issued again, in
/// seconds.
pub const RECORD_RENEW_BEFORE_S: i64 = 12 * 3600;
/// The largest signed record accepted, in bytes.
pub const MAX_RECORD_BYTES: usize = 4 * 1024;
/// The most direct addresses a contact record lists.
pub const MAX_DIRECT_ADDRS: usize = 8;
/// The most capabilities an announce record lists.
pub const MAX_ANNOUNCED_CAPABILITIES: usize = 32;
/// The largest `seq`: every integer up to here is exact as a JSON number in
/// any language. The first is 1.
pub const MAX_RECORD_SEQ: u64 = (1 << 53) - 1;

/// How to reach an agent: its Iroh endpoint.
///
/// Timestamps are in the form [`crate::grant::format_timestamp`] writes;
/// `expires_at` comes after `issued_at`, by at most [`RECORD_TTL_S`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactRecord {
    /// [`CONTACT_SCHEMA`].
    pub schema: String,
    /// The agent the record is about, and its signer.
    pub agent_id: AgentId,
    pub iroh: IrohContact,
    /// From 1 to [`MAX_RECORD_SEQ`]; one more for every record the agent
    /// issues of this kind.
    pub seq: u64,
    pub issued_at: String,
    pub expires_at: String,
}

/// An Iroh endpoint and where it can be dialed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrohContact {
    /// The endpoint's id, which in PALS is the agent's own id: a record
    /// naming any other endpoint is malformed.
    pub endpoint_id: AgentId,
    /// The home relay, as Iroh writes its URL; left out when there is none,
    /// never empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// Up to [`MAX_DIRECT_ADDRS`] socket addresses, `192.0.2.10:4433` or
    /// `[2001:db8::10]:4433`.
    pub direct_addrs: Vec<String>,
}

/// What an agent offers and the card that describes it.
///
/// Timestamps as in [`ContactRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnounceRecord {
    /// [`ANNOUNCE_SCHEMA`].
    pub schema: String,
    /// The agent the record is about, and its signer.
    pub agent_id: AgentId,
    /// As in [`ContactRecord::seq`], counted apart.
    pub seq: u64,
    /// The hash of the agent's signed public card.
    pub agent_card_hash: CardHash,
    /// Up to [`MAX_ANNOUNCED_CAPABILITIES`] public capabilities, none empty.
    pub capabilities: Vec<String>,
    /// The ALPN of the agent's discovery protocol, `agentnet-discovery/0`.
    pub discovery_alpn: String,
    pub issued_at: String,
    pub expires_at: String,
}

impl SignedDoc for ContactRecord {
    const SCHEMA: &'static str = CONTACT_SCHEMA;
    const DOMAIN: &'static [u8] = CONTACT_SIG_DOMAIN;
    const WRAPPER: &'static str = "record";
    const MAX_BYTES: usize = MAX_RECORD_BYTES;
    const SIGNER_POINTER: &'static str = "/agent_id";

    fn signer(&self) -> &AgentId {
        &self.agent_id
    }

    fn validate(&self) -> Result<(), SignedError> {
        check_common(self.seq, &self.issued_at, &self.expires_at)?;
        let iroh = &self.iroh;
        // The agent's key is its endpoint's key: no agent speaks for
        // another's endpoint.
        if iroh.endpoint_id != self.agent_id
            || iroh.relay_url.as_deref() == Some("")
            || iroh.direct_addrs.len() > MAX_DIRECT_ADDRS
            || iroh
                .direct_addrs
                .iter()
                .any(|addr| addr.parse::<SocketAddr>().is_err())
        {
            return Err(SignedError::Malformed);
        }
        Ok(())
    }
}

impl SignedDoc for AnnounceRecord {
    const SCHEMA: &'static str = ANNOUNCE_SCHEMA;
    const DOMAIN: &'static [u8] = ANNOUNCE_SIG_DOMAIN;
    const WRAPPER: &'static str = "record";
    const MAX_BYTES: usize = MAX_RECORD_BYTES;
    const SIGNER_POINTER: &'static str = "/agent_id";

    fn signer(&self) -> &AgentId {
        &self.agent_id
    }

    fn validate(&self) -> Result<(), SignedError> {
        check_common(self.seq, &self.issued_at, &self.expires_at)?;
        if self.capabilities.len() > MAX_ANNOUNCED_CAPABILITIES
            || self.capabilities.iter().any(String::is_empty)
            || self.discovery_alpn.is_empty()
        {
            return Err(SignedError::Malformed);
        }
        Ok(())
    }
}

/// The `seq` range and the lifetime both records share: no record outlives
/// its issue by more than [`RECORD_TTL_S`]. That cap is necessary for a
/// receiver to forget a sequence ever, but not sufficient on its own: it
/// bounds when an older record expires only together with an `issued_at`
/// that never goes back, which is why the receiver waits a whole
/// [`RECORD_TTL_S`] past the known record's expiry
/// ([`crate::store::Store::peer_record_accept`]).
fn check_common(seq: u64, issued_at: &str, expires_at: &str) -> Result<(), SignedError> {
    match (parse_timestamp(issued_at), parse_timestamp(expires_at)) {
        (Some(issued_at), Some(expires_at))
            if issued_at < expires_at
                && expires_at - issued_at <= RECORD_TTL_S
                && (1..=MAX_RECORD_SEQ).contains(&seq) =>
        {
            Ok(())
        }
        _ => Err(SignedError::Malformed),
    }
}
