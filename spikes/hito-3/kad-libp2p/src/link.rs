//! The identity link `agentnet.spike-kadlink/0`, provisional: the record of
//! spec §6.2 that binds a libp2p `PeerId` to an agent and carries the
//! agent's contact.
//!
//! The node has two keys: the agent's, which is also its Iroh endpoint's,
//! and a libp2p Ed25519 key, whose `PeerId` is the node's name in the DHT.
//! In the spike the libp2p key derives from the agent's seed
//! ([`libp2p_keypair`]), so that a restarted node keeps its `PeerId`; the
//! product would generate it and keep it.
//!
//! The document, [`LinkDoc`], is
//! `{"schema", "peer_id", "agent_id", "contact", "issued_at", "expires_at"}`,
//! `contact` being the agent's `Signed<ContactRecord>` as a JSON object. It
//! travels as the JCS (RFC 8785) text of
//! `{"link": <document>, "libp2p_key", "libp2p_signature", "agent_signature"}`:
//! the libp2p public key in its protobuf encoding, the libp2p key's
//! signature of [`LIBP2P_DOMAIN`] `|| JCS(document)` and the agent key's of
//! [`AGENT_DOMAIN`] `|| JCS(document)`, all three in base64url without
//! padding. [`verify`] checks, the first failure winning: the size; one
//! spelling; the fields; the schema and the lifetime's form; that the
//! libp2p key is the one `peer_id` names (§6.2); both signatures; the
//! contact, as `pals-core` checks it, and that it is the same agent's; and
//! the validity windows of the link and of the contact.

use std::str::FromStr;

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use libp2p::PeerId;
use libp2p::identity::{Keypair, PublicKey};
use pals_core::grant::{format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs::{canonical, sign_domain, verify_domain};
use pals_core::records::{ContactRecord, RECORD_TTL_S};
use pals_core::signed::{Signed, SignedError, check_validity};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The value of [`LinkDoc::schema`].
pub const LINK_SCHEMA: &str = "agentnet.spike-kadlink/0";
/// The prefix of the bytes the libp2p key signs.
pub const LIBP2P_DOMAIN: &[u8] = b"agentnet.spike-kadlink/0\nlibp2p\n";
/// The prefix of the bytes the agent key signs.
pub const AGENT_DOMAIN: &[u8] = b"agentnet.spike-kadlink/0\nagent\n";
/// The largest link accepted, in bytes: a contact of at most
/// `pals_core::records::MAX_RECORD_BYTES` (4 KiB), the link's own fields,
/// the libp2p key and two signatures.
pub const MAX_LINK_BYTES: usize = 6 * 1024;
/// What the agent's seed is hashed with to give the libp2p key's seed.
const KEY_DERIVATION: &[u8] = b"agentnet.spike-kadlink/0\nlibp2p-key\n";

const WRAPPER: &str = "link";
const LIBP2P_KEY: &str = "libp2p_key";
const LIBP2P_SIGNATURE: &str = "libp2p_signature";
const AGENT_SIGNATURE: &str = "agent_signature";

/// What both keys sign.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkDoc {
    /// [`LINK_SCHEMA`].
    pub schema: String,
    /// The `PeerId` bound, in base58.
    pub peer_id: String,
    /// The agent bound.
    pub agent_id: AgentId,
    /// The agent's `Signed<ContactRecord>`, as the JSON object it travels as.
    pub contact: Value,
    pub issued_at: String,
    /// After `issued_at`, by at most a day, as `pals-core`'s records.
    pub expires_at: String,
}

/// A link that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub peer_id: PeerId,
    pub agent_id: AgentId,
    /// The agent's contact, verified.
    pub contact: Signed<ContactRecord>,
}

/// Why a link was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    TooLarge,
    Malformed,
    UnknownSchema,
    /// The libp2p key is not the one `peer_id` names.
    PeerIdMismatch,
    BadLibp2pSignature,
    BadAgentSignature,
    /// The contact does not verify as `pals-core`'s.
    BadContact,
    /// The contact is another agent's.
    AgentMismatch,
    Expired,
    NotYetValid,
}

impl LinkError {
    /// A short name, for logs and for the router's verdict.
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
            Self::UnknownSchema => "unknown_schema",
            Self::PeerIdMismatch => "peer_id_mismatch",
            Self::BadLibp2pSignature => "bad_libp2p_signature",
            Self::BadAgentSignature => "bad_agent_signature",
            Self::BadContact => "bad_contact",
            Self::AgentMismatch => "agent_mismatch",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
        }
    }
}

/// The node's libp2p key: Ed25519, its seed the SHA-256 of
/// `KEY_DERIVATION` and the agent's seed, so that it is never the agent's
/// own key and is the same on every start.
pub fn libp2p_keypair(agent: &AgentKey) -> Keypair {
    let mut hash = Sha256::new();
    hash.update(KEY_DERIVATION);
    hash.update(agent.to_bytes());
    let seed: [u8; 32] = hash.finalize().into();
    Keypair::ed25519_from_bytes(seed).expect("32 bytes are an Ed25519 seed")
}

/// The link of `libp2p`'s `PeerId` to `agent`, with `contact`, issued at
/// `now_unix` for a day: the value the node publishes.
pub fn issue(
    libp2p: &Keypair,
    agent: &AgentKey,
    contact: &Signed<ContactRecord>,
    now_unix: i64,
) -> anyhow::Result<Vec<u8>> {
    let at = |unix| format_timestamp(unix).context("the clock is out of range");
    let doc = LinkDoc {
        schema: LINK_SCHEMA.to_owned(),
        peer_id: libp2p.public().to_peer_id().to_base58(),
        agent_id: agent.id(),
        contact: serde_json::from_slice(&contact.to_json())?,
        issued_at: at(now_unix)?,
        expires_at: at(now_unix + RECORD_TTL_S)?,
    };
    sign(libp2p, agent, &doc)
}

/// `doc` signed with both keys, as it travels; [`issue`] without its
/// checks, so that a test can sign a link that does not verify.
pub fn sign(libp2p: &Keypair, agent: &AgentKey, doc: &LinkDoc) -> anyhow::Result<Vec<u8>> {
    let doc = serde_json::to_value(doc)?;
    let libp2p_signature = libp2p
        .sign(&signed_bytes(LIBP2P_DOMAIN, &doc))
        .context("the libp2p key cannot sign")?;
    let mut envelope = Map::new();
    envelope.insert(
        LIBP2P_KEY.to_owned(),
        Value::String(URL_SAFE_NO_PAD.encode(libp2p.public().encode_protobuf())),
    );
    envelope.insert(
        LIBP2P_SIGNATURE.to_owned(),
        Value::String(URL_SAFE_NO_PAD.encode(libp2p_signature)),
    );
    envelope.insert(
        AGENT_SIGNATURE.to_owned(),
        Value::String(sign_domain(agent, AGENT_DOMAIN, &doc)),
    );
    envelope.insert(WRAPPER.to_owned(), doc);
    Ok(canonical(&Value::Object(envelope)))
}

/// Parses a link and verifies it at `now_unix`, as the module says.
pub fn verify(bytes: &[u8], now_unix: i64) -> Result<Link, LinkError> {
    if bytes.len() > MAX_LINK_BYTES {
        return Err(LinkError::TooLarge);
    }
    let envelope: Value = serde_json::from_slice(bytes).map_err(|_| LinkError::Malformed)?;
    // One spelling: no whitespace, reordering or duplicate keys.
    if canonical(&envelope) != bytes {
        return Err(LinkError::Malformed);
    }
    let Value::Object(mut envelope) = envelope else {
        return Err(LinkError::Malformed);
    };
    let mut text = |field| match envelope.remove(field) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(LinkError::Malformed),
    };
    let (libp2p_key, libp2p_signature, agent_signature) = (
        text(LIBP2P_KEY)?,
        text(LIBP2P_SIGNATURE)?,
        text(AGENT_SIGNATURE)?,
    );
    let doc = envelope.remove(WRAPPER).ok_or(LinkError::Malformed)?;
    if !envelope.is_empty() {
        return Err(LinkError::Malformed);
    }

    let parsed = LinkDoc::deserialize(&doc).map_err(|_| LinkError::Malformed)?;
    let reserialized = serde_json::to_value(&parsed).map_err(|_| LinkError::Malformed)?;
    if canonical(&reserialized) != canonical(&doc) {
        return Err(LinkError::Malformed);
    }
    if parsed.schema != LINK_SCHEMA {
        return Err(LinkError::UnknownSchema);
    }
    let (issued_at, expires_at) = (
        parse_timestamp(&parsed.issued_at).ok_or(LinkError::Malformed)?,
        parse_timestamp(&parsed.expires_at).ok_or(LinkError::Malformed)?,
    );
    if expires_at <= issued_at || expires_at - issued_at > RECORD_TTL_S {
        return Err(LinkError::Malformed);
    }
    let peer_id = PeerId::from_str(&parsed.peer_id).map_err(|_| LinkError::Malformed)?;
    if peer_id.to_base58() != parsed.peer_id {
        return Err(LinkError::Malformed);
    }

    let public = URL_SAFE_NO_PAD
        .decode(&libp2p_key)
        .ok()
        .and_then(|bytes| PublicKey::try_decode_protobuf(&bytes).ok())
        .ok_or(LinkError::Malformed)?;
    if public.to_peer_id() != peer_id {
        return Err(LinkError::PeerIdMismatch);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(&libp2p_signature)
        .map_err(|_| LinkError::BadLibp2pSignature)?;
    if !public.verify(&signed_bytes(LIBP2P_DOMAIN, &doc), &signature) {
        return Err(LinkError::BadLibp2pSignature);
    }
    if !verify_domain(&parsed.agent_id, AGENT_DOMAIN, &doc, &agent_signature) {
        return Err(LinkError::BadAgentSignature);
    }

    let contact = Signed::<ContactRecord>::from_json(&canonical(&parsed.contact))
        .map_err(|_| LinkError::BadContact)?;
    if contact.doc.agent_id != parsed.agent_id {
        return Err(LinkError::AgentMismatch);
    }
    let c = &contact.doc;
    check_validity(&parsed.issued_at, &parsed.expires_at, now_unix)
        .and_then(|()| check_validity(&c.issued_at, &c.expires_at, now_unix))
        .map_err(|error| match error {
            SignedError::NotYetValid => LinkError::NotYetValid,
            SignedError::Expired => LinkError::Expired,
            _ => LinkError::Malformed,
        })?;
    Ok(Link {
        peer_id,
        agent_id: parsed.agent_id,
        contact,
    })
}

fn signed_bytes(domain: &[u8], doc: &Value) -> Vec<u8> {
    let mut bytes = domain.to_vec();
    bytes.extend_from_slice(&canonical(doc));
    bytes
}
