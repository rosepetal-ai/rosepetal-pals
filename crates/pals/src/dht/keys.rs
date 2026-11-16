//! The DHT's identity and keys, frozen by the vector
//! `crates/pals-core/tests/vectors/kad-v0.json`
//! (`docs/protocols/agentnet-kad-0.md`, `docs/extensions/capabilities-v1.md`):
//!
//! - **Identity.** An agent's `PeerId` is its own Ed25519 public key, the
//!   key of its Iroh endpoint: libp2p's protobuf encoding of that key under
//!   the identity multihash ([`peer_id`]). There is no second key and no
//!   link between identities to sign: Iroh's TLS authenticates that key on
//!   every connection, and libp2p never gets the secret key.
//! - **Values.** An agent's signed `agentnet.contact/1` lives under
//!   [`CONTACT_KEY_PREFIX`]`<peer_id>` and its `agentnet.announce/1` under
//!   [`ANNOUNCE_KEY_PREFIX`]`<peer_id>`, with the `PeerId` in base58.
//! - **Providers.** A capability's provider records live under the SHA2-256
//!   multihash (code `0x12`, length `0x20`) of the UTF-8 bytes of its full
//!   string ([`provider_key`]), as [`capability_tag`] writes it:
//!   `agentnet:capability:v1:<id>` for a capability of the common
//!   vocabulary, `<namespace>:<id>` unchanged for a namespaced one. Each
//!   capability has one spelling: its full tag is refused as a capability,
//!   since `agentnet` is no provider's namespace.
//!
//! Keys that come from the DHT are untrusted. A key names an agent only in
//! the one spelling this module writes ([`slot`], [`agent_of`]), so two
//! different keys never hold the same agent's value.

use std::str::FromStr;

use libp2p::PeerId;
use libp2p::identity::{self, ed25519};
use libp2p::kad::RecordKey;
use pals_core::identity::AgentId;
use pals_core::inventory::{InventoryError, capability_tag};
use sha2::{Digest, Sha256};

/// The prefix of the key of an agent's contact, before its `PeerId`.
pub const CONTACT_KEY_PREFIX: &str = "/agentnet/contact/";
/// The prefix of the key of an agent's announce, before its `PeerId`.
pub const ANNOUNCE_KEY_PREFIX: &str = "/agentnet/announce/";

/// The multihash code of the identity hash, the one under which a `PeerId`
/// carries an Ed25519 key.
const IDENTITY_HASH: u64 = 0x00;
/// The multihash code of SHA2-256 and its digest's length: the first two
/// bytes of a provider key.
pub(crate) const SHA2_256: [u8; 2] = [0x12, 0x20];

/// The agent's `PeerId`: its own Ed25519 public key.
pub fn peer_id(agent: &AgentId) -> PeerId {
    // libp2p checks the key with ed25519-dalek 3, the same check that
    // `AgentId` passed when it was parsed or derived.
    let key = ed25519::PublicKey::try_from_bytes(agent.as_bytes())
        .expect("an agent id is a valid Ed25519 public key");
    identity::PublicKey::from(key).to_peer_id()
}

/// The agent `peer` is, if it is the `PeerId` of an Ed25519 key, in the one
/// encoding [`peer_id`] writes. `None` for a `PeerId` of another kind of
/// key, of a hash of a key, or of the same key encoded differently.
pub fn agent_of(peer: &PeerId) -> Option<AgentId> {
    let hash = peer.as_ref();
    if hash.code() != IDENTITY_HASH {
        return None;
    }
    let key = identity::PublicKey::try_decode_protobuf(hash.digest())
        .ok()?
        .try_into_ed25519()
        .ok()?;
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let agent = AgentId::from_str(&hex).ok()?;
    (peer_id(&agent) == *peer).then_some(agent)
}

/// Where the agent's signed contact is: [`CONTACT_KEY_PREFIX`]`<peer_id>`.
pub fn contact_key(agent: &AgentId) -> RecordKey {
    RecordKey::new(&format!("{CONTACT_KEY_PREFIX}{}", peer_id(agent)))
}

/// Where the agent's signed announce is: [`ANNOUNCE_KEY_PREFIX`]`<peer_id>`.
pub fn announce_key(agent: &AgentId) -> RecordKey {
    RecordKey::new(&format!("{ANNOUNCE_KEY_PREFIX}{}", peer_id(agent)))
}

/// Where the providers of `capability` are: the SHA2-256 multihash of
/// [`capability_tag`]`(capability)`. Refused when `capability` is not one,
/// its full tag included.
pub fn provider_key(capability: &str) -> Result<RecordKey, InventoryError> {
    let tag = capability_tag(capability)?;
    let mut key = SHA2_256.to_vec();
    key.extend_from_slice(&Sha256::digest(tag.as_bytes()));
    Ok(RecordKey::from(key))
}

/// Whose value a key holds, and which of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Contact(AgentId),
    Announce(AgentId),
}

/// The value `key` names: `Some` only when it is exactly
/// [`contact_key`] or [`announce_key`] of an agent, with the canonical
/// base58 of its `PeerId` and nothing after it.
pub fn slot(key: &RecordKey) -> Option<Slot> {
    let text = std::str::from_utf8(key.as_ref()).ok()?;
    if let Some(rest) = text.strip_prefix(CONTACT_KEY_PREFIX) {
        return agent_named(rest).map(Slot::Contact);
    }
    text.strip_prefix(ANNOUNCE_KEY_PREFIX)
        .and_then(agent_named)
        .map(Slot::Announce)
}

/// The agent `text` names, if it is the base58 of that agent's `PeerId`.
fn agent_named(text: &str) -> Option<AgentId> {
    let peer = PeerId::from_str(text).ok()?;
    let agent = agent_of(&peer)?;
    (peer.to_base58() == text).then_some(agent)
}
