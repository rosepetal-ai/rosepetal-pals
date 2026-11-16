//! `kad-libp2p`: Iroh plus `libp2p-kad` on its own libp2p stack, the first
//! candidate of the hito 3 backend comparison; not product code.
//!
//! A library as well as the binary (`main.rs`), so that the tests reach
//! the link and its validators, and so that the candidate of `libp2p-kad`
//! over Iroh connections (task 4) can start from them:
//! - [`link`] and [`validate`] know nothing of the transport and can be
//!   taken as they are;
//! - [`behaviour`]'s Kademlia configuration, its validation of what
//!   arrives, its search, its announce, its sweep and its counters can be
//!   taken too, but not what assumes QUIC over IP: the transport `spawn`
//!   builds and listens on (`quic_addr`), the node's own addresses
//!   (`Task::addrs_now`, `Task::refresh_external_addrs`, which provider
//!   records need), the router's `bootstrap_addr`, the address filter
//!   (`Task::routable`) and the dial addresses it gives Kademlia back
//!   (`Task::known_addrs`), all multiaddrs of IP and UDP. Task 4 has to
//!   replace them with what names a peer over Iroh. A search keeps itself
//!   from dialing the providers it finds with [`behaviour::NoDial`], which
//!   denies only dials and closes no connection, so it also holds when the
//!   connections are shared with other protocols; but it denies every dial
//!   to those peers while the search runs, not only the query's.

use std::time::{SystemTime, UNIX_EPOCH};

pub mod behaviour;
pub mod link;
pub mod validate;

/// Seconds since the Unix epoch, by the system clock; 0 before it.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
