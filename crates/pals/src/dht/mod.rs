//! The DHT of milestone 3: a private Kademlia network (`libp2p-kad`) whose
//! connections are the daemon's own Iroh connections, on the ALPN
//! `agentnet-kad/0`, and whose `PeerId` is the agent's own key
//! (`docs/protocols/agentnet-kad-0.md`). An agent finds there the providers
//! of a capability it has no contact with; what it gets is untrusted data,
//! checked before it is used.
//!
//! [`keys`] fixes what the network is addressed by, frozen by the vector
//! `crates/pals-core/tests/vectors/kad-v0.json`: the agent's `PeerId`, the
//! keys of its signed contact and announce, and the key of a capability's
//! provider records.

pub mod keys;
