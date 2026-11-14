//! HTTP/1.1 between peers, carried by Iroh QUIC streams.
//!
//! The binding rules, shared by [`TunnelServer`] and [`PeerHttp`]:
//!
//! - Each bidirectional stream carries exactly one HTTP/1.1 exchange, one
//!   request and its response, without keep-alive.
//! - The server drops any `x-agentnet-peer` header the request carries and sets
//!   it to the connection's authenticated remote id, in lowercase hex.
//! - The response ends with the stream's FIN.
//! - A request body larger than [`TunnelLimits::max_body_bytes`] gets `413`.

mod tunnel_client;
mod tunnel_server;

use iroh::EndpointId;

pub use tunnel_client::{PeerHttp, TunnelError};
pub use tunnel_server::{TunnelLimits, TunnelServer};

/// ALPN of the A2A HTTP binding over Iroh.
pub const ALPN_A2A_HTTP: &[u8] = b"agentnet-a2a-http/1";

/// Header that carries the requesting peer's id to the served app.
pub const PEER_HEADER: &str = "x-agentnet-peer";

/// An endpoint id as 64 lowercase hex characters.
fn endpoint_id_hex(id: &EndpointId) -> String {
    id.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
