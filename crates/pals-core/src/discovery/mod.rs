//! `agentnet-discovery/0`: what an agent answers about itself before any
//! task, on an Iroh ALPN of its own (`docs/protocols/agentnet-discovery-0.md`).
//!
//! Each bidirectional stream carries one exchange: a request frame, which
//! the requester closes with FIN, and a response frame, which the server
//! closes with FIN ([`wire`]). A request is at most
//! [`MAX_REQUEST_FRAME_BYTES`], a response at most
//! [`MAX_RESPONSE_FRAME_BYTES`]; a length prefix over the limit is refused
//! before its body is read ([`frame_len`]).
//!
//! The answer is [`answer`]'s, here, in a crate that opens no sockets and
//! holds no endpoint, worker, executor or adapter: answering reads the
//! inventory, the store, the agent's own signed records and the worker's
//! load counters ([`LoadProbe`]), and signs with the agent's key, and that
//! is all it can do. It never runs work and never asks another peer. A
//! service query ([`query`]) is matched on that metadata only
//! ([`matching`]). The transport, the limits on time and the quotas are the
//! daemon's (`pals::discovery::DiscoveryServer`).
//!
//! What a requester sees comes from the grant it presents, verified as the
//! HTTP layer of milestone 1 verifies it ([`crate::grant::accept_grant`]),
//! and from nothing else it sends.

mod answer;
pub mod matching;
pub mod query;
pub mod wire;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub use answer::{answer, error_response};
pub use matching::{Evaluation, evaluate};
pub use query::{
    MAX_OFFER_BYTES, MAX_OFFER_VALID_FOR_MS, MAX_QUERY_BYTES, OfferError, ServiceOffer,
    ServiceQuery, accept_offer,
};
pub use wire::{WireError, decode, encode_frame, frame_len};

use crate::card_jws::{CardHash, MAX_CARD_BYTES};
use crate::identity::{AgentId, AgentKey};
use crate::inventory::InventorySnapshot;
use crate::inventory::view::{
    CursorKey, MAX_PAGE_BYTES, MAX_PAGE_ENTRIES, MAX_QUERY_TOTAL_BYTES, ViewCache,
};
use crate::store::Store;

/// The ALPN of the protocol.
pub const ALPN_DISCOVERY: &[u8] = b"agentnet-discovery/0";
/// The version of the protocol this agent speaks, the only one.
pub const DISCOVERY_VERSION: u32 = 0;
/// The largest request body: a grant of 8 KiB and a query of 8 KiB, with
/// room for the rest.
pub const MAX_REQUEST_FRAME_BYTES: usize = 20 * 1024;
/// The largest response body: a page of 64 KiB, with the records and the
/// rest.
pub const MAX_RESPONSE_FRAME_BYTES: usize = 80 * 1024;
/// The longest error message.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 256;
/// The scope key under which [`DiscoveryContext::card_hashes`] holds the
/// public card's hash, which every scope without a card of its own gets.
pub const PUBLIC_CARD: &str = "public";

/// The limits a server announces in every response.
pub fn server_limits() -> wire::Limits {
    let limit = |bytes: usize| u32::try_from(bytes).expect("the limits fit in 32 bits");
    wire::Limits {
        max_request_bytes: limit(MAX_REQUEST_FRAME_BYTES),
        max_response_bytes: limit(MAX_RESPONSE_FRAME_BYTES),
        max_query_bytes: limit(MAX_QUERY_BYTES),
        max_offer_bytes: limit(MAX_OFFER_BYTES),
        max_page_bytes: limit(MAX_PAGE_BYTES),
        max_page_entries: limit(MAX_PAGE_ENTRIES),
        max_query_total_bytes: limit(MAX_QUERY_TOTAL_BYTES),
        max_card_bytes: limit(MAX_CARD_BYTES),
    }
}

/// How busy the agent's worker is, read from its counters: tasks running and
/// waiting, and how many of each it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Load {
    pub running: u32,
    pub queued: u32,
    pub max_concurrent: u32,
    pub max_queue: u32,
}

impl Load {
    /// A worker with nothing running and no bounds.
    pub fn unbounded() -> Self {
        Self {
            running: 0,
            queued: 0,
            max_concurrent: u32::MAX,
            max_queue: u32::MAX,
        }
    }
}

/// Reads a worker's [`Load`]. It only reads counters: it never runs, queues
/// or waits for work.
pub trait LoadProbe: Send + Sync {
    fn load(&self) -> Load;
}

/// A load that never changes.
impl LoadProbe for Load {
    fn load(&self) -> Load {
        *self
    }
}

/// What [`answer`] answers with: the agent's key and store, its inventory
/// and records as they are now, the hashes of its cards, the issuers whose
/// grants it accepts, whether it shows its public view to requesters
/// without a grant, this run's cursor key and page cache, the worker's load
/// and how long its offers last.
pub struct DiscoveryContext {
    pub key: Arc<AgentKey>,
    pub store: Arc<Store>,
    /// The inventory and its observations now.
    pub inventory: Arc<dyn Fn() -> Arc<InventorySnapshot> + Send + Sync>,
    /// The hash of the card of each scope with a card of its own, by
    /// [`crate::inventory::Scope::key`], and the public card's under
    /// [`PUBLIC_CARD`], which every other scope gets.
    pub card_hashes: BTreeMap<String, CardHash>,
    /// The agent's signed contact and announce records in force, as their
    /// JSON: read on every answer, so that an answer carries the records
    /// renewed since the context was built.
    pub records: Arc<dyn Fn() -> (Vec<u8>, Vec<u8>) + Send + Sync>,
    /// The issuers whose grants the agent accepts: itself and those it
    /// trusts.
    pub accepted_issuers: Vec<AgentId>,
    /// Whether a requester without a grant sees the public view; without
    /// it, it sees an empty one.
    pub public: bool,
    pub cursor_key: CursorKey,
    pub views: ViewCache,
    /// The worker's load, for service offers.
    pub load: Arc<dyn LoadProbe>,
    /// How long a service offer lasts, from 1 to
    /// [`MAX_OFFER_VALID_FOR_MS`]; rounded up to whole seconds.
    pub offer_valid_for_ms: u32,
}

impl fmt::Debug for DiscoveryContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveryContext")
            .field("agent", &self.key.id())
            .field("card_hashes", &self.card_hashes)
            .field("accepted_issuers", &self.accepted_issuers)
            .field("public", &self.public)
            .field("views", &self.views)
            .field("offer_valid_for_ms", &self.offer_valid_for_ms)
            .finish_non_exhaustive()
    }
}
