//! `kad-libp2p`: Iroh plus `libp2p-kad` on its own libp2p stack, the first
//! candidate of the hito 3 backend comparison; not product code.
//!
//! A library as well as the binary (`main.rs`), so that the tests reach
//! the link and its validators, and so that the candidate of `libp2p-kad`
//! over Iroh connections (task 4) can take [`behaviour`], [`link`] and
//! [`validate`] as they are: only the transport `behaviour::spawn` builds
//! is this candidate's own.

pub mod behaviour;
pub mod link;
pub mod validate;
