//! PALS core: the parts of an agent that do not touch the network.
//!
//! [`identity`] holds the agent's persistent Ed25519 key. Grants, the SQLite
//! store and the signed event log land here in later tasks.

pub mod identity;
