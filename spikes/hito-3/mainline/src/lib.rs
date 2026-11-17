//! `mainline`: Iroh plus a private Mainline DHT, with a capability index of
//! its own, which Mainline does not have (spec §6.1).
//!
//! - [`index`]: the index, what goes under which key and what a router
//!   takes into it, without network;
//! - [`krpc`]: the KRPC messages a node reads before n0-mainline does;
//! - [`router`]: a router's checks before the library stores a put, and its
//!   view of what the library's store holds;
//! - [`node`]: the `Backend`.
//!
//! The library is n0-mainline, n0's Mainline DHT, with its
//! `unstable_signed_peers` feature: the index is of signed peers, which
//! return agent keys and not addresses.

pub mod index;
pub mod krpc;
pub mod node;
pub mod router;

pub use node::Mainline;
