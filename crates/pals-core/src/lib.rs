//! PALS core: the parts of an agent that do not touch the network.
//!
//! [`identity`] holds the agent's persistent Ed25519 key, [`jcs`] canonical
//! JSON and the domain-separated signatures over it, [`grant`] the signed
//! direct grants, and [`store`] the local SQLite database: contacts, grants
//! and revocations.

pub mod grant;
pub mod identity;
pub mod jcs;
pub mod store;
