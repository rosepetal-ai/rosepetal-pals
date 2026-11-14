//! PALS core: the parts of an agent that do not touch the network.
//!
//! [`identity`] holds the agent's persistent Ed25519 key, [`jcs`] canonical
//! JSON and the domain-separated signatures over it, and [`grant`] the signed
//! direct grants. The SQLite store and the signed event log land here in later
//! tasks.

pub mod grant;
pub mod identity;
pub mod jcs;
