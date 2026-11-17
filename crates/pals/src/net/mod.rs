//! HTTP/1.1 between peers, carried by Iroh QUIC streams.
//!
//! The binding rules, shared by [`TunnelServer`] and [`PeerHttp`]:
//!
//! - Each bidirectional stream carries exactly one HTTP/1.1 exchange, one
//!   request and its response, without keep-alive.
//! - The server drops any `x-agentnet-peer` header the request carries and sets
//!   it to the connection's authenticated remote id, in lowercase hex.
//! - The response ends with the stream's FIN.
//! - A request body larger than [`TunnelLimits::max_body_bytes`] gets `413`;
//!   one that does not arrive whole within [`TunnelLimits::head_timeout`]
//!   of its head, `408`.
//!
//! An agent's id is its endpoint's id ([`agent_id_of`], [`endpoint_id_of`]);
//! [`peer_addr`] is where to dial one from its ticket, and [`contact_addr`]
//! from its signed contact record.

mod tunnel_client;
mod tunnel_server;

use std::net::SocketAddr;

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
use pals_core::identity::AgentId;
use pals_core::records::IrohContact;

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

/// The agent whose endpoint is `id`.
pub fn agent_id_of(id: &EndpointId) -> AgentId {
    endpoint_id_hex(id)
        .parse()
        .expect("an endpoint id is an Ed25519 public key")
}

/// The endpoint of agent `id`.
pub fn endpoint_id_of(id: &AgentId) -> EndpointId {
    EndpointId::from_bytes(id.as_bytes()).expect("an agent id is an Ed25519 public key")
}

/// Where to dial agent `id`: at the addresses of `ticket`, an endpoint
/// ticket of that agent, when there is one; else by its id alone, which
/// Iroh's address lookup resolves when the endpoint has one.
///
/// The dialed endpoint is always `id`, and the connection's handshake
/// proves that it is: a ticket that does not parse or names another
/// endpoint lends no addresses.
pub fn peer_addr(id: &AgentId, ticket: Option<&str>) -> EndpointAddr {
    let endpoint_id = endpoint_id_of(id);
    let addrs = ticket
        .and_then(|ticket| match ticket.parse::<EndpointTicket>() {
            Ok(ticket) if ticket.endpoint_addr().id == endpoint_id => {
                Some(ticket.endpoint_addr().addrs.clone())
            }
            Ok(_) => {
                tracing::warn!(peer = %id, "ignoring a ticket of another endpoint");
                None
            }
            Err(error) => {
                tracing::warn!(peer = %id, %error, "ignoring a ticket that does not parse");
                None
            }
        })
        .unwrap_or_default();
    EndpointAddr::from_parts(endpoint_id, addrs)
}

/// Where to dial the endpoint of `contact`, an agent's contact record:
/// its direct addresses and its relay, each that reads as one. The record
/// is verified before: its endpoint is its agent's own, and the
/// connection's handshake proves it.
pub fn contact_addr(contact: &IrohContact) -> EndpointAddr {
    let direct = contact
        .direct_addrs
        .iter()
        .filter_map(|addr| addr.parse::<SocketAddr>().ok())
        .map(TransportAddr::Ip);
    let relay = contact
        .relay_url
        .as_deref()
        .and_then(|url| url.parse::<RelayUrl>().ok())
        .map(TransportAddr::Relay);
    EndpointAddr::from_parts(endpoint_id_of(&contact.endpoint_id), direct.chain(relay))
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;
    use pals_core::identity::AgentKey;

    use super::*;

    #[test]
    fn agent_and_endpoint_ids_are_the_same_key() {
        let key = AgentKey::generate();
        let endpoint_id = SecretKey::from_bytes(&key.to_bytes()).public();
        assert_eq!(endpoint_id_of(&key.id()), endpoint_id);
        assert_eq!(agent_id_of(&endpoint_id), key.id());
    }

    #[test]
    fn peer_addr_takes_addresses_only_from_the_agents_own_ticket() {
        let id = AgentKey::generate().id();
        let socket = "127.0.0.1:4433".parse().unwrap();
        let own = EndpointAddr::new(endpoint_id_of(&id)).with_ip_addr(socket);
        let own_ticket = EndpointTicket::new(own.clone()).to_string();
        assert_eq!(peer_addr(&id, Some(&own_ticket)), own);
        assert_eq!(
            peer_addr(&id, Some(&own_ticket)).addrs,
            [TransportAddr::Ip(socket)].into()
        );

        let bare = EndpointAddr::new(endpoint_id_of(&id));
        assert_eq!(peer_addr(&id, None), bare);
        assert_eq!(peer_addr(&id, Some("not a ticket")), bare);
        let other = AgentKey::generate().id();
        let other_ticket =
            EndpointTicket::new(EndpointAddr::new(endpoint_id_of(&other)).with_ip_addr(socket))
                .to_string();
        assert_eq!(peer_addr(&id, Some(&other_ticket)), bare);
    }

    #[test]
    fn contact_addr_takes_the_direct_addresses_and_the_relay() {
        let id = AgentKey::generate().id();
        let contact = IrohContact {
            endpoint_id: id,
            relay_url: Some("https://relay.example.org./".to_owned()),
            direct_addrs: vec![
                "192.0.2.10:4433".to_owned(),
                "[2001:db8::10]:4433".to_owned(),
            ],
        };
        let addr = contact_addr(&contact);
        assert_eq!(addr.id, endpoint_id_of(&id));
        let relay: RelayUrl = "https://relay.example.org./".parse().unwrap();
        assert_eq!(
            addr.addrs,
            [
                TransportAddr::Ip("192.0.2.10:4433".parse().unwrap()),
                TransportAddr::Ip("[2001:db8::10]:4433".parse().unwrap()),
                TransportAddr::Relay(relay),
            ]
            .into()
        );
        let bare = IrohContact {
            relay_url: None,
            direct_addrs: Vec::new(),
            ..contact
        };
        assert_eq!(contact_addr(&bare), EndpointAddr::new(endpoint_id_of(&id)));
    }
}
