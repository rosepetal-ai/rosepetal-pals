//! PALS core: the parts of an agent that do not touch the network.
//!
//! [`identity`] holds the agent's persistent Ed25519 key, [`jcs`] canonical
//! JSON and the domain-separated signatures over it, [`grant`] the signed
//! direct grants, [`card_jws`] the JWS signatures, limits and hash of Agent
//! Cards, [`signed`] the signed JSON documents of milestone 2, [`records`]
//! the signed contact and announce records, [`inventory`] what the agent
//! offers and with what (`agentnet.inventory/1`), who may see each part of
//! it and the scopes grants give, [`store`] the local SQLite database
//! (contacts, grants, revocations, the A2A tasks the agent serves, the
//! messages it has seen, the questions it asks, its own and its peers'
//! records, and its peers' verified cards), and [`events`] the signed,
//! hash-chained event log kept in it.

pub mod card_jws;
pub mod events;
pub mod grant;
pub mod identity;
pub mod inventory;
pub mod jcs;
pub mod records;
pub mod signed;
pub mod store;
