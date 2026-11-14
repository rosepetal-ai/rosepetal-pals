//! PALS core: the parts of an agent that do not touch the network.
//!
//! [`identity`] holds the agent's persistent Ed25519 key, [`jcs`] canonical
//! JSON and the domain-separated signatures over it, [`grant`] the signed
//! direct grants, [`card_jws`] the JWS signatures, limits and hash of Agent
//! Cards, [`signed`] the signed JSON documents of milestone 2, [`records`]
//! the signed contact and announce records, [`inventory`] what the agent
//! offers and with what (`agentnet.inventory/1`), who may see each part of
//! it, the scopes grants give and the signed, paginated views each scope
//! gets (`agentnet.inventory-view/1`), [`store`] the local SQLite database
//! (contacts, grants, revocations, the A2A tasks the agent serves, the
//! messages it has seen, the questions it asks, its own and its peers'
//! records, its peers' verified cards, the revisions of its own views and
//! the pages of its peers'), [`events`] the signed, hash-chained event log
//! kept in it, [`discovery`] the messages of `agentnet-discovery/0` and the
//! answers to them, which read and sign but never run work, and [`clock`]
//! the clock quotas and records follow, which tests set by hand.

pub mod card_jws;
pub mod clock;
pub mod discovery;
pub mod events;
pub mod grant;
pub mod identity;
pub mod inventory;
pub mod jcs;
pub mod records;
pub mod signed;
pub mod store;
