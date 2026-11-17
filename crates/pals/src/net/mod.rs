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
//! from its signed contact record, which another agent wrote: only at the
//! addresses [`routable`] keeps and an `https` relay.

mod tunnel_client;
mod tunnel_server;

use std::net::{IpAddr, SocketAddr};

use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, TransportAddr};
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

/// Whether `endpoint` is bound to loopback: only then is a peer's
/// loopback address one to dial.
pub fn bound_on_loopback(endpoint: &Endpoint) -> bool {
    endpoint
        .bound_sockets()
        .iter()
        .any(|socket| socket.ip().is_loopback())
}

/// Whether `ip`, an address another agent gave, is one to dial: not
/// unspecified, multicast nor link-local, and loopback only when this
/// node is bound there (`on_loopback`, [`bound_on_loopback`]). Private
/// ranges are: a LAN is one.
pub fn routable(ip: IpAddr, on_loopback: bool) -> bool {
    let ip = ip.to_canonical();
    let link_local = match ip {
        IpAddr::V4(ip) => ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unicast_link_local(),
    };
    !ip.is_unspecified() && !ip.is_multicast() && !link_local && (!ip.is_loopback() || on_loopback)
}

/// Where to dial the endpoint of `contact`, an agent's contact record:
/// its direct addresses that read as one and are [`routable`] from a node
/// bound on loopback or not (`on_loopback`), and its relay when it reads as
/// one and is `https`. The record is verified before: its endpoint is its
/// agent's own, and the connection's handshake proves it; what it names is
/// the agent's word all the same.
pub fn contact_addr(contact: &IrohContact, on_loopback: bool) -> EndpointAddr {
    let direct = contact
        .direct_addrs
        .iter()
        .filter_map(|addr| addr.parse::<SocketAddr>().ok())
        .filter(|addr| routable(addr.ip(), on_loopback))
        .map(TransportAddr::Ip);
    let relay = contact
        .relay_url
        .as_deref()
        .and_then(|url| url.parse::<RelayUrl>().ok())
        .filter(|url| url.scheme() == "https")
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
        let addr = contact_addr(&contact, false);
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
        assert_eq!(
            contact_addr(&bare, false),
            EndpointAddr::new(endpoint_id_of(&id))
        );
    }

    #[test]
    fn contact_addr_keeps_only_routable_addresses_and_an_https_relay() {
        let id = AgentKey::generate().id();
        let contact = IrohContact {
            endpoint_id: id,
            relay_url: Some("http://relay.example.org./".to_owned()),
            direct_addrs: vec![
                "169.254.10.1:4433".to_owned(),
                "[fe80::1]:4433".to_owned(),
                "0.0.0.0:4433".to_owned(),
                "[::]:4433".to_owned(),
                "224.0.0.251:4433".to_owned(),
                "[ff02::1]:4433".to_owned(),
                "127.0.0.1:4433".to_owned(),
                "[::1]:4433".to_owned(),
                "[::ffff:127.0.0.1]:4433".to_owned(),
                "192.0.2.10:4433".to_owned(),
                "10.213.73.20:4433".to_owned(),
            ],
        };
        let ip = |addr: &str| TransportAddr::Ip(addr.parse().unwrap());
        // An http relay, link-local, unspecified and multicast addresses
        // go; private ranges stay, and loopback only for a node bound
        // there.
        let elsewhere = contact_addr(&contact, false);
        assert_eq!(
            elsewhere.addrs,
            [ip("192.0.2.10:4433"), ip("10.213.73.20:4433")].into()
        );
        let on_loopback = contact_addr(&contact, true);
        assert_eq!(
            on_loopback.addrs,
            [
                ip("127.0.0.1:4433"),
                ip("[::1]:4433"),
                ip("[::ffff:127.0.0.1]:4433"),
                ip("192.0.2.10:4433"),
                ip("10.213.73.20:4433"),
            ]
            .into()
        );
        // Only an https relay is dialed.
        for (url, kept) in [
            ("https://relay.example.org./", true),
            ("http://relay.example.org./", false),
            ("ftp://relay.example.org./", false),
        ] {
            let relayed = IrohContact {
                relay_url: Some(url.to_owned()),
                direct_addrs: Vec::new(),
                ..contact.clone()
            };
            let addrs = contact_addr(&relayed, false).addrs;
            assert_eq!(addrs.len(), usize::from(kept), "{url}: {addrs:?}");
        }
    }
}
