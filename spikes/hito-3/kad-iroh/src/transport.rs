//! [`IrohTransport`]: a libp2p `Transport` whose connections are Iroh's,
//! on the node's one endpoint and its one UDP port.
//!
//! - **Identity.** A node's `PeerId` is its Iroh `EndpointId`, which is the
//!   agent's Ed25519 public key ([`peer_id`], [`endpoint_id`]). Iroh's TLS
//!   authenticates the remote end of every connection by that key (raw
//!   public keys, RFC 7250), so the transport adds no security handshake of
//!   its own, and libp2p never holds the secret key: it signs nothing.
//! - **Addresses.** A peer is `/ip4/<ip>/udp/<port>/quic-v1/p2p/<peer_id>`
//!   (or `/ip6/…`), the form of libp2p's QUIC: a dial goes to that
//!   `EndpointId` at that direct address only, as Iroh's address lookup and
//!   relays are off. An address with no `/p2p/` names no endpoint and is not
//!   dialed.
//! - **Dial.** `Endpoint::connect` with ALPN [`ALPN_KAD`], within
//!   [`DIAL_TIMEOUT`]. The `PeerId` libp2p gets is the one of the endpoint
//!   Iroh authenticated, never the one the address said: a dial that reaches
//!   another endpoint fails in Iroh's handshake.
//! - **Listen.** The endpoint is already bound, and its router hands the
//!   connections of [`ALPN_KAD`] to an [`IrohListener`], the protocol
//!   handler this transport makes, as the daemon serves each of its ALPNs.
//!   `listen_on` only names the endpoint's own socket, so that libp2p knows
//!   it; with no listener, an incoming connection is closed.
//! - **Muxer.** Each connection is an [`IrohMuxer`]: QUIC multiplexes, so
//!   there is no muxer protocol.

use std::collections::VecDeque;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use iroh::endpoint::{ConnectError, Connection, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use libp2p::core::muxing::StreamMuxerBox;
use libp2p::core::transport::{DialOpts, ListenerId, TransportError, TransportEvent};
use libp2p::core::{self, Transport};
use libp2p::futures::FutureExt;
use libp2p::futures::future::BoxFuture;
use libp2p::identity;
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use tokio::sync::mpsc;

use crate::muxer::IrohMuxer;

/// The ALPN of Kademlia over Iroh: what the daemon would serve besides its
/// own.
pub const ALPN_KAD: &[u8] = b"agentnet-kad/0";
/// How long a dial may take: `libp2p-quic`'s handshake timeout, so that the
/// two Kademlia candidates wait the same for a peer that does not answer.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
/// Incoming connections the listener queues for libp2p.
const QUEUED_CONNECTIONS: usize = 32;
/// The code a connection the transport will not use closes with.
const REFUSED: u32 = 1;

/// What a connection upgrades to: the remote's `PeerId` and the muxer.
type Output = (PeerId, StreamMuxerBox);

/// The libp2p transport over one Iroh endpoint.
pub struct IrohTransport {
    endpoint: Endpoint,
    /// The connections the [`IrohListener`] accepted.
    incoming: mpsc::Receiver<Connection>,
    listener: Option<ListenerId>,
    events: VecDeque<TransportEvent<<Self as Transport>::ListenerUpgrade, Error>>,
    /// The task that last polled and found nothing, to wake when an event
    /// is queued outside [`Transport::poll`].
    waker: Option<Waker>,
}

/// The protocol handler of [`ALPN_KAD`] on the endpoint's router: it hands
/// each connection, once Iroh authenticated it, to the [`IrohTransport`].
#[derive(Debug, Clone)]
pub struct IrohListener {
    connections: mpsc::Sender<Connection>,
}

/// Why the transport failed.
#[derive(Debug)]
pub enum Error {
    /// Iroh did not connect: the address does not answer, or the endpoint
    /// there is not the one dialed, or it does not serve [`ALPN_KAD`].
    Connect(ConnectError),
    /// The dial took longer than [`DIAL_TIMEOUT`].
    Timeout,
    /// The connection is to another endpoint than the one dialed, which
    /// Iroh's handshake already rules out.
    WrongEndpoint(EndpointId),
    /// `listen_on` an address the endpoint is not bound to.
    NotBound(Multiaddr),
    /// `listen_on` a second time: the endpoint is one listener.
    AlreadyListening,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(error) => write!(f, "Iroh did not connect: {error}"),
            Self::Timeout => write!(f, "no connection in {} s", DIAL_TIMEOUT.as_secs()),
            Self::WrongEndpoint(id) => write!(f, "connected to another endpoint: {id}"),
            Self::NotBound(addr) => write!(f, "the endpoint is not bound to {addr}"),
            Self::AlreadyListening => f.write_str("the endpoint is already listening"),
        }
    }
}

impl std::error::Error for Error {}

impl IrohTransport {
    /// The transport over `endpoint`, and the handler the endpoint's router
    /// must serve at [`ALPN_KAD`] for it to get incoming connections.
    pub fn new(endpoint: Endpoint) -> (Self, IrohListener) {
        let (connections, incoming) = mpsc::channel(QUEUED_CONNECTIONS);
        let transport = Self {
            endpoint,
            incoming,
            listener: None,
            events: VecDeque::new(),
            waker: None,
        };
        (transport, IrohListener { connections })
    }

    /// The local `PeerId`: the endpoint's key.
    pub fn local_peer_id(&self) -> PeerId {
        peer_id(&self.endpoint.id())
    }

    /// The local public key, as libp2p names it.
    pub fn local_public_key(&self) -> identity::PublicKey {
        public_key(&self.endpoint.id())
    }

    /// Queues `event` for [`Transport::poll`], and wakes the task waiting on
    /// it.
    fn queue(&mut self, event: TransportEvent<<Self as Transport>::ListenerUpgrade, Error>) {
        self.events.push_back(event);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    /// The upgrade of an accepted connection, with the addresses libp2p
    /// records for it.
    fn accepted(
        &self,
        listener_id: ListenerId,
        connection: Connection,
    ) -> TransportEvent<<Self as Transport>::ListenerUpgrade, Error> {
        let remote = connection.remote_id();
        let send_back_addr = remote_ip(&connection).map_or_else(Multiaddr::empty, quic_addr);
        let local_addr = self
            .endpoint
            .bound_sockets()
            .first()
            .copied()
            .map_or_else(Multiaddr::empty, quic_addr);
        let output = (
            peer_id(&remote),
            StreamMuxerBox::new(IrohMuxer::new(connection)),
        );
        TransportEvent::Incoming {
            listener_id,
            upgrade: std::future::ready(Ok(output)),
            local_addr,
            send_back_addr,
        }
    }
}

impl Transport for IrohTransport {
    type Output = Output;
    type Error = Error;
    type ListenerUpgrade = std::future::Ready<Result<Output, Error>>;
    type Dial = BoxFuture<'static, Result<Output, Error>>;

    /// Names the endpoint's own socket: `addr` must be the IP it is bound
    /// to, with its port or port 0.
    fn listen_on(&mut self, id: ListenerId, addr: Multiaddr) -> Result<(), TransportError<Error>> {
        let Some(wanted) = socket_of(&addr) else {
            return Err(TransportError::MultiaddrNotSupported(addr));
        };
        if self.listener.is_some() {
            return Err(TransportError::Other(Error::AlreadyListening));
        }
        let Some(bound) = self.endpoint.bound_sockets().into_iter().find(|bound| {
            bound.ip() == wanted.ip() && (wanted.port() == 0 || bound.port() == wanted.port())
        }) else {
            return Err(TransportError::Other(Error::NotBound(addr)));
        };
        self.listener = Some(id);
        self.queue(TransportEvent::NewAddress {
            listener_id: id,
            listen_addr: quic_addr(bound),
        });
        Ok(())
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        if self.listener != Some(id) {
            return false;
        }
        self.listener = None;
        self.queue(TransportEvent::ListenerClosed {
            listener_id: id,
            reason: Ok(()),
        });
        true
    }

    fn dial(
        &mut self,
        addr: Multiaddr,
        opts: DialOpts,
    ) -> Result<Self::Dial, TransportError<Error>> {
        // Iroh dials and accepts on its own; a libp2p hole punch, in which
        // the dialer acts as the listener, has no meaning here.
        let target = (opts.role == core::Endpoint::Dialer)
            .then(|| dial_target(&addr))
            .flatten();
        let Some((socket, id)) = target else {
            return Err(TransportError::MultiaddrNotSupported(addr));
        };
        let endpoint = self.endpoint.clone();
        Ok(async move {
            let addr = EndpointAddr::from_parts(id, [TransportAddr::Ip(socket)]);
            let connection = tokio::time::timeout(DIAL_TIMEOUT, endpoint.connect(addr, ALPN_KAD))
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(Error::Connect)?;
            let remote = connection.remote_id();
            if remote != id {
                connection.close(VarInt::from_u32(REFUSED), b"");
                return Err(Error::WrongEndpoint(remote));
            }
            Ok((
                peer_id(&remote),
                StreamMuxerBox::new(IrohMuxer::new(connection)),
            ))
        }
        .boxed())
    }

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Error>> {
        let this = self.get_mut();
        if let Some(event) = this.events.pop_front() {
            return Poll::Ready(event);
        }
        // `listen_on` and `remove_listener` queue events between polls.
        this.waker = Some(cx.waker().clone());
        loop {
            match this.incoming.poll_recv(cx) {
                Poll::Ready(Some(connection)) => match this.listener {
                    Some(id) => return Poll::Ready(this.accepted(id, connection)),
                    None => connection.close(VarInt::from_u32(REFUSED), b"not listening"),
                },
                // The router stopped: no connection will come again.
                Poll::Ready(None) => {
                    return match this.listener.take() {
                        Some(listener_id) => Poll::Ready(TransportEvent::ListenerClosed {
                            listener_id,
                            reason: Ok(()),
                        }),
                        None => Poll::Pending,
                    };
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl ProtocolHandler for IrohListener {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if let Err(unsent) = self.connections.send(connection).await {
            // The transport is gone.
            unsent.0.close(VarInt::from_u32(REFUSED), b"not listening");
        }
        Ok(())
    }
}

/// The `PeerId` of an Iroh endpoint, which is the agent's: its Ed25519
/// public key.
pub fn peer_id(endpoint: &EndpointId) -> PeerId {
    public_key(endpoint).to_peer_id()
}

/// The Iroh endpoint a `PeerId` names, if it is an Ed25519 key's.
pub fn endpoint_id(peer: &PeerId) -> Option<EndpointId> {
    // A `PeerId` of an Ed25519 key is the identity multihash (code 0) of
    // the key's protobuf encoding.
    let hash = peer.as_ref();
    if hash.code() != 0 {
        return None;
    }
    let key = identity::PublicKey::try_decode_protobuf(hash.digest())
        .ok()?
        .try_into_ed25519()
        .ok()?;
    EndpointId::from_bytes(&key.to_bytes()).ok()
}

/// `/ip4/<ip>/udp/<port>/quic-v1`, or `/ip6/…`.
pub fn quic_addr(addr: SocketAddr) -> Multiaddr {
    let ip = match addr.ip() {
        IpAddr::V4(ip) => Protocol::Ip4(ip),
        IpAddr::V6(ip) => Protocol::Ip6(ip),
    };
    Multiaddr::empty()
        .with(ip)
        .with(Protocol::Udp(addr.port()))
        .with(Protocol::QuicV1)
}

fn public_key(endpoint: &EndpointId) -> identity::PublicKey {
    let key = identity::ed25519::PublicKey::try_from_bytes(endpoint.as_bytes())
        .expect("an Iroh endpoint id is an Ed25519 public key");
    identity::PublicKey::from(key)
}

/// The socket of `/ip4/<ip>/udp/<port>/quic-v1`, nothing after it.
fn socket_of(addr: &Multiaddr) -> Option<SocketAddr> {
    let mut parts = addr.iter();
    let socket = socket_prefix(&mut parts)?;
    parts.next().is_none().then_some(socket)
}

/// The socket and the endpoint of
/// `/ip4/<ip>/udp/<port>/quic-v1/p2p/<peer_id>`, nothing after it.
fn dial_target(addr: &Multiaddr) -> Option<(SocketAddr, EndpointId)> {
    let mut parts = addr.iter();
    let socket = socket_prefix(&mut parts)?;
    let Some(Protocol::P2p(peer)) = parts.next() else {
        return None;
    };
    if parts.next().is_some() {
        return None;
    }
    Some((socket, endpoint_id(&peer)?))
}

fn socket_prefix<'a>(parts: &mut impl Iterator<Item = Protocol<'a>>) -> Option<SocketAddr> {
    let ip = match parts.next()? {
        Protocol::Ip4(ip) => IpAddr::V4(ip),
        Protocol::Ip6(ip) => IpAddr::V6(ip),
        _ => return None,
    };
    let Protocol::Udp(port) = parts.next()? else {
        return None;
    };
    matches!(parts.next()?, Protocol::QuicV1).then_some(SocketAddr::new(ip, port))
}

/// The remote's socket on the connection's selected path.
fn remote_ip(connection: &Connection) -> Option<SocketAddr> {
    let paths = connection.paths();
    let path = paths.iter().find(|path| path.is_selected())?;
    match path.remote_addr() {
        TransportAddr::Ip(addr) => Some(*addr),
        _ => None,
    }
}
