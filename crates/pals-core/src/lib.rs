//! PALS core: the parts of an agent that do not touch the network.
//!
//! [`identity`] holds the agent's persistent Ed25519 key, [`jcs`] canonical
//! JSON and the domain-separated signatures over it, [`grant`] the signed
//! direct grants, [`card_jws`] the JWS signatures, limits and hash of Agent
//! Cards, [`store`] the local SQLite database (contacts, grants,
//! revocations, the A2A tasks the agent serves, the messages it has seen
//! and the questions it asks), and [`events`] the signed, hash-chained event
//! log kept in it.

pub mod card_jws;
pub mod events;
pub mod grant;
pub mod identity;
pub mod jcs;
pub mod store;
