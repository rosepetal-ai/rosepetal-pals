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
//! provider records. [`transport`] and [`muxer`] are libp2p's transport
//! over the node's Iroh endpoint, with the address forms and the limits of
//! `agentnet-kad/0`'s connections and streams; their tests,
//! `crates/pals/tests/dht_transport.rs`, are the gate of any change of the
//! `libp2p` or `iroh` versions.

pub mod keys;
pub mod muxer;
pub mod transport;
