//! [`IrohTransport`]: a libp2p `Transport` whose connections are Iroh's, on
//! the node's one endpoint and its one UDP port, with ALPN [`ALPN_KAD`]
//! (`docs/protocols/agentnet-kad-0.md`).
//!
//! - **Identity.** A node's `PeerId` is its Iroh `EndpointId`, which is the
//!   agent's Ed25519 public key ([`keys::peer_id`]). Iroh's TLS
//!   authenticates the remote end of every connection by that key (raw
//!   public keys, RFC 7250), so the transport adds no security handshake of
//!   its own, and libp2p never holds the secret key: it signs nothing. The
//!   `PeerId` libp2p gets is always the one of the endpoint Iroh
//!   authenticated, never the one an address said.
//! - **Addresses.** Two forms ([`multiaddr_of`], [`located_nowhere`]):
//!   `/ip4/<ip>/udp/<port>/quic-v1/p2p/<peer_id>` (or `/ip6/…`), an Iroh
//!   endpoint at that UDP address, and `/p2p/<peer_id>`, no location. Only
//!   the first is dialed ([`dial_target`]).
//! - **Dial.** `Endpoint::connect` with ALPN [`ALPN_KAD`], within
//!   [`DIAL_TIMEOUT`], to an `EndpointAddr` that carries the dialed address
//!   and every other one the [`AddressBook`] knows of that peer: Iroh picks
//!   the path, so a peer with two addresses is one connection, not two. A
//!   dial that reaches another endpoint fails in Iroh's handshake.
//! - **Listen.** The endpoint is already bound, and its router hands the
//!   connections of [`ALPN_KAD`] to an [`IrohListener`], the protocol
//!   handler this transport makes. The listener never waits: it queues a
//!   connection for the transport, up to [`KadQuotas::accept_queue`], and
//!   closes it with [`REFUSED`] when the queue is full. `listen_on` only
//!   names the endpoint's own socket, so that libp2p knows it; with no
//!   listener, an incoming connection is closed with [`REFUSED`] too.
//! - **Muxer.** Each connection is an [`IrohMuxer`]: QUIC multiplexes, so
//!   there is no muxer protocol; the streams the other side opens are held
//!   to the stream quotas of [`KadQuotas`], and reset when idle for the
//!   transport's idle timeout.
//!
//! [`Refusals`] counts what the listener and the muxers refused since the
//! transport was made.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use pals_core::clock::Clock;
use serde::Serialize;
use tokio::sync::mpsc::{self, error::TrySendError};

use super::keys::{self, agent_of};
use super::muxer::{IrohMuxer, StreamQuotas};
use crate::net::{agent_id_of, endpoint_id_of};

/// The ALPN of Kademlia over Iroh.
pub const ALPN_KAD: &[u8] = b"agentnet-kad/0";
/// How long a dial may take: `libp2p-quic`'s handshake timeout.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
/// The code a connection or a stream the node refuses is closed or reset
/// with.
pub const REFUSED: u32 = 1;

/// The most direct addresses the [`AddressBook`] keeps of one peer: the
/// most recently learned.
const ADDRESSES_PER_PEER: usize = 8;
/// The most peers the [`AddressBook`] keeps: the most recently learned.
const PEERS_KNOWN: usize = 1024;

/// What the connections of [`ALPN_KAD`] may make the node hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KadQuotas {
    /// Incoming connections queued for libp2p; one more is closed with
    /// [`REFUSED`]. At least 1.
    pub accept_queue: usize,
    /// Streams of one peer open at a time, over all its connections.
    pub peer_streams: usize,
    /// Streams one peer opens in a minute of the node's clock, over all its
    /// connections.
    pub peer_streams_per_minute: u32,
    /// Streams of all peers open at a time.
    pub global_streams: usize,
}

impl Default for KadQuotas {
    /// A queue of 32 connections; 8 streams at a time and 120 a minute per
    /// peer, 256 at a time in all.
    fn default() -> Self {
        Self {
            accept_queue: 32,
            peer_streams: 8,
            peer_streams_per_minute: 120,
            global_streams: 256,
        }
    }
}

/// The direct addresses known of each peer, which every dial to it
/// carries. Clones share them.
#[derive(Debug, Clone, Default)]
pub struct AddressBook {
    inner: Arc<Mutex<Book>>,
}

#[derive(Debug, Default)]
struct Book {
    peers: HashMap<PeerId, Known>,
    /// How many addresses were learned, to order the peers by when.
    learned: u64,
}

#[derive(Debug, Default)]
struct Known {
    /// Oldest first.
    addrs: VecDeque<SocketAddr>,
    /// When it was last learned, in [`Book::learned`].
    at: u64,
}

impl AddressBook {
    /// `peer` is reachable at `addr`. The book keeps the latest
    /// [`ADDRESSES_PER_PEER`] of each peer and [`PEERS_KNOWN`] peers,
    /// forgetting those learned longest ago.
    pub fn learn(&self, peer: PeerId, addr: SocketAddr) {
        let mut book = self.book();
        book.learned += 1;
        let at = book.learned;
        if !book.peers.contains_key(&peer) && book.peers.len() >= PEERS_KNOWN {
            let oldest = book
                .peers
                .iter()
                .min_by_key(|(_, known)| known.at)
                .map(|(peer, _)| *peer);
            if let Some(oldest) = oldest {
                book.peers.remove(&oldest);
            }
        }
        let known = book.peers.entry(peer).or_default();
        known.at = at;
        known.addrs.retain(|known| *known != addr);
        known.addrs.push_back(addr);
        if known.addrs.len() > ADDRESSES_PER_PEER {
            known.addrs.pop_front();
        }
    }

    /// The addresses known of `peer`, oldest first.
    pub fn of(&self, peer: &PeerId) -> Vec<SocketAddr> {
        self.book()
            .peers
            .get(peer)
            .map(|known| known.addrs.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Nothing is left half written while the lock is held: taken as it is
    /// after a panic.
    fn book(&self) -> MutexGuard<'_, Book> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What the [`IrohListener`] and the [`IrohMuxer`]s of one transport
/// refused, since it was made. Clones share the counts.
#[derive(Debug, Clone, Default)]
pub struct Refusals {
    inner: Arc<Counts>,
}

#[derive(Debug, Default)]
struct Counts {
    connections: AtomicU64,
    streams: AtomicU64,
    idle_streams: AtomicU64,
}

/// [`Refusals`] at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RefusedCounts {
    /// Incoming connections closed with [`REFUSED`]: the queue was full or
    /// nothing listened.
    pub connections: u64,
    /// Incoming streams reset with [`REFUSED`], beyond the stream quotas.
    pub streams: u64,
    /// Incoming streams reset with [`REFUSED`] after the idle timeout with
    /// no read or write.
    pub idle_streams: u64,
}

impl Refusals {
    /// The counts now.
    pub fn counts(&self) -> RefusedCounts {
        RefusedCounts {
            connections: self.inner.connections.load(Ordering::Relaxed),
            streams: self.inner.streams.load(Ordering::Relaxed),
            idle_streams: self.inner.idle_streams.load(Ordering::Relaxed),
        }
    }

    /// Closes `connection` with [`REFUSED`], and counts it.
    fn connection(&self, connection: &Connection, reason: &'static str) {
        self.inner.connections.fetch_add(1, Ordering::Relaxed);
        connection.close(VarInt::from_u32(REFUSED), reason.as_bytes());
        tracing::debug!(
            peer = %connection.remote_id(),
            reason,
            "an agentnet-kad/0 connection refused"
        );
    }

    /// Counts a stream refused.
    pub(crate) fn stream(&self) {
        self.inner.streams.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts a stream reset for being idle.
    pub(crate) fn idle_stream(&self) {
        self.inner.idle_streams.fetch_add(1, Ordering::Relaxed);
    }
}

/// What a connection upgrades to: the remote's `PeerId` and the muxer.
type Output = (PeerId, StreamMuxerBox);

/// The libp2p transport over one Iroh endpoint.
pub struct IrohTransport {
    endpoint: Endpoint,
    book: AddressBook,
    streams: Arc<StreamQuotas>,
    refusals: Refusals,
    /// The connections the [`IrohListener`] accepted.
    incoming: mpsc::Receiver<Connection>,
    listener: Option<ListenerId>,
    events: VecDeque<TransportEvent<<Self as Transport>::ListenerUpgrade, Error>>,
    /// The task that last polled and found nothing, to wake when an event
    /// is queued outside [`Transport::poll`].
    waker: Option<Waker>,
}

/// The protocol handler of [`ALPN_KAD`] on the endpoint's router: it hands
/// each connection, once Iroh authenticated it, to the [`IrohTransport`],
/// and never waits for room to do it.
#[derive(Debug, Clone)]
pub struct IrohListener {
    connections: mpsc::Sender<Connection>,
    refusals: Refusals,
}

/// Why the transport failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Iroh did not connect: no address answers, or the endpoint there is
    /// not the one dialed, or it does not serve [`ALPN_KAD`].
    #[error("Iroh did not connect: {0}")]
    Connect(ConnectError),
    /// The dial took longer than [`DIAL_TIMEOUT`].
    #[error("no connection in {} s", DIAL_TIMEOUT.as_secs())]
    Timeout,
    /// The connection is to another endpoint than the one dialed, which
    /// Iroh's handshake already rules out.
    #[error("connected to another endpoint: {0}")]
    WrongEndpoint(EndpointId),
    /// `listen_on` an address the endpoint is not bound to.
    #[error("the endpoint is not bound to {0}")]
    NotBound(Multiaddr),
    /// `listen_on` a second time: the endpoint is one listener.
    #[error("the endpoint is already listening")]
    AlreadyListening,
}

impl IrohTransport {
    /// The transport over `endpoint`, within `quotas` at the time `clock`
    /// says, resetting an incoming stream with no read or write for
    /// `idle_timeout`, dialing each peer at every address `book` knows of
    /// it; the handler the endpoint's router must serve at [`ALPN_KAD`] for
    /// it to get incoming connections; and what both refuse.
    pub fn new(
        endpoint: Endpoint,
        quotas: KadQuotas,
        clock: Arc<dyn Clock>,
        book: AddressBook,
        idle_timeout: Duration,
    ) -> (Self, IrohListener, Refusals) {
        let refusals = Refusals::default();
        let (connections, incoming) = mpsc::channel(quotas.accept_queue.max(1));
        let streams = StreamQuotas::new(quotas, clock, refusals.clone(), idle_timeout);
        let transport = Self {
            endpoint,
            book,
            streams: Arc::new(streams),
            refusals: refusals.clone(),
            incoming,
            listener: None,
            events: VecDeque::new(),
            waker: None,
        };
        let listener = IrohListener {
            connections,
            refusals: refusals.clone(),
        };
        (transport, listener, refusals)
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
        let peer = peer_of(&connection.remote_id());
        let send_back_addr = remote_ip(&connection).map_or_else(Multiaddr::empty, quic_addr);
        let local_addr = self
            .endpoint
            .bound_sockets()
            .first()
            .copied()
            .map_or_else(Multiaddr::empty, quic_addr);
        let muxer = IrohMuxer::new(connection, peer, Arc::clone(&self.streams));
        TransportEvent::Incoming {
            listener_id,
            upgrade: std::future::ready(Ok((peer, StreamMuxerBox::new(muxer)))),
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
            .flatten()
            .and_then(|(peer, socket)| Some((peer, socket, endpoint_of(&peer)?)));
        let Some((peer, socket, id)) = target else {
            return Err(TransportError::MultiaddrNotSupported(addr));
        };
        // Every address known of the peer, in one dial: Iroh picks the path.
        let known = self.book.of(&peer);
        let to = EndpointAddr::from_parts(
            id,
            std::iter::once(socket).chain(known).map(TransportAddr::Ip),
        );
        let endpoint = self.endpoint.clone();
        let streams = Arc::clone(&self.streams);
        Ok(async move {
            let connection = tokio::time::timeout(DIAL_TIMEOUT, endpoint.connect(to, ALPN_KAD))
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(Error::Connect)?;
            let remote = connection.remote_id();
            if remote != id {
                connection.close(VarInt::from_u32(REFUSED), b"");
                return Err(Error::WrongEndpoint(remote));
            }
            let peer = peer_of(&remote);
            let muxer = IrohMuxer::new(connection, peer, streams);
            Ok((peer, StreamMuxerBox::new(muxer)))
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
                    None => this.refusals.connection(&connection, "not listening"),
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
    /// Queues `connection` for the transport, or closes it with [`REFUSED`]
    /// when the queue is full or the transport is gone: it never waits.
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        match self.connections.try_send(connection) {
            Ok(()) => {}
            Err(TrySendError::Full(connection)) => {
                self.refusals.connection(&connection, "busy");
            }
            Err(TrySendError::Closed(connection)) => {
                self.refusals.connection(&connection, "not listening");
            }
        }
        Ok(())
    }
}

/// The router form: `/ip4/<ip>/udp/<port>/quic-v1/p2p/<peer>`, or `/ip6/…`,
/// an Iroh endpoint of `peer` at that UDP address.
pub fn multiaddr_of(peer: &PeerId, addr: SocketAddr) -> Multiaddr {
    quic_addr(addr).with(Protocol::P2p(*peer))
}

/// The client form: `/p2p/<peer>`, no location.
pub fn located_nowhere(peer: &PeerId) -> Multiaddr {
    Multiaddr::empty().with(Protocol::P2p(*peer))
}

/// The peer and the socket of an address in the router form, with nothing
/// after its `/p2p/` and an agent's `PeerId` there; `None` for any other
/// address, the client form included.
pub fn dial_target(addr: &Multiaddr) -> Option<(PeerId, SocketAddr)> {
    let mut parts = addr.iter();
    let socket = socket_prefix(&mut parts)?;
    let Some(Protocol::P2p(peer)) = parts.next() else {
        return None;
    };
    if parts.next().is_some() {
        return None;
    }
    endpoint_of(&peer)?;
    Some((peer, socket))
}

/// The `PeerId` of an Iroh endpoint: its agent's.
fn peer_of(endpoint: &EndpointId) -> PeerId {
    keys::peer_id(&agent_id_of(endpoint))
}

/// The Iroh endpoint a `PeerId` names, if it is an agent's.
fn endpoint_of(peer: &PeerId) -> Option<EndpointId> {
    agent_of(peer).map(|agent| endpoint_id_of(&agent))
}

/// `/ip4/<ip>/udp/<port>/quic-v1`, or `/ip6/…`: the router form without
/// its `/p2p/`, as [`Transport::listen_on`] takes the endpoint's socket.
pub(crate) fn quic_addr(addr: SocketAddr) -> Multiaddr {
    let ip = match addr.ip() {
        IpAddr::V4(ip) => Protocol::Ip4(ip),
        IpAddr::V6(ip) => Protocol::Ip6(ip),
    };
    Multiaddr::empty()
        .with(ip)
        .with(Protocol::Udp(addr.port()))
        .with(Protocol::QuicV1)
}

/// The socket of `/ip4/<ip>/udp/<port>/quic-v1`, nothing after it.
fn socket_of(addr: &Multiaddr) -> Option<SocketAddr> {
    let mut parts = addr.iter();
    let socket = socket_prefix(&mut parts)?;
    parts.next().is_none().then_some(socket)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, 1], port))
    }

    #[test]
    fn the_address_book_keeps_the_latest_of_each_peer() {
        let book = AddressBook::default();
        let peer = PeerId::random();
        assert!(book.of(&peer).is_empty());
        for port in 1..=10 {
            book.learn(peer, addr(port));
        }
        // The eight learned last, oldest first.
        let kept: Vec<_> = (3..=10).map(addr).collect();
        assert_eq!(book.of(&peer), kept);
        // Learning one again makes it the latest, once.
        book.learn(peer, addr(3));
        let kept: Vec<_> = (4..=10).chain([3]).map(addr).collect();
        assert_eq!(book.of(&peer), kept);
    }

    #[test]
    fn the_address_book_forgets_the_peer_learned_longest_ago() {
        let book = AddressBook::default();
        let peers: Vec<PeerId> = (0..PEERS_KNOWN).map(|_| PeerId::random()).collect();
        for peer in &peers {
            book.learn(*peer, addr(1));
        }
        // The first one learned again: the second is the oldest now.
        book.learn(peers[0], addr(2));
        let newcomer = PeerId::random();
        book.learn(newcomer, addr(3));
        assert_eq!(book.of(&newcomer), vec![addr(3)]);
        assert_eq!(book.of(&peers[0]), vec![addr(1), addr(2)]);
        assert!(book.of(&peers[1]).is_empty());
        assert_eq!(book.of(&peers[2]), vec![addr(1)]);
    }
}
