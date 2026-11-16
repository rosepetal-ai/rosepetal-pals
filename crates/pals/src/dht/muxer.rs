//! [`IrohMuxer`]: an Iroh connection as libp2p's `StreamMuxer`, each
//! substream a bidirectional QUIC stream of the connection, with the stream
//! quotas of `agentnet-kad/0` on the streams the other side opens.
//!
//! - **No muxer protocol.** QUIC already multiplexes, so nothing is
//!   negotiated for the connection: a substream libp2p opens is `open_bi`,
//!   one it accepts is `accept_bi`, as `libp2p-quic` does over its own QUIC
//!   connections. A QUIC peer learns of a new stream only when the opener
//!   sends on it, which libp2p's multistream-select always does first.
//! - **Quotas.** Before libp2p gets a stream the other side opened, the
//!   muxer checks it against [`KadQuotas`]: at most
//!   [`KadQuotas::peer_streams`] of one peer at a time and
//!   [`KadQuotas::peer_streams_per_minute`] of its streams in the last
//!   minute of the node's [`Clock`], counting all of that peer's
//!   connections, and at most [`KadQuotas::global_streams`] of all peers at
//!   a time. A stream beyond them is reset with [`REFUSED`] as it is
//!   accepted, without reading it, and counted in [`Refusals`]; libp2p never
//!   sees it. A stream counts until libp2p drops its substream. The streams
//!   this side opens count in no quota.
//! - **Closing.** The connection closes when libp2p closes it
//!   ([`StreamMuxer::poll_close`]); when the other side or Iroh closes it,
//!   the pending `accept_bi` fails and libp2p drops it.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream, VarInt};
use libp2p::PeerId;
use libp2p::core::muxing::{StreamMuxer, StreamMuxerEvent};
use libp2p::futures::future::BoxFuture;
use libp2p::futures::{AsyncRead, AsyncWrite, FutureExt};
use pals_core::clock::Clock;

use super::transport::{KadQuotas, REFUSED, Refusals};

type Streams = BoxFuture<'static, Result<(SendStream, RecvStream), ConnectionError>>;

/// The code a connection closes with when libp2p closes it.
const CLOSED_BY_LIBP2P: u32 = 0;
/// The window [`KadQuotas::peer_streams_per_minute`] counts streams in, in
/// seconds.
const MINUTE_S: i64 = 60;
/// Above this many peers tracked, those with nothing open and nothing in
/// their window are forgotten.
const PEERS_KEPT: usize = 1024;
/// Streams refused in one poll before the muxer yields, so that a peer
/// opening streams as fast as it can does not hold the task that polls.
const REFUSALS_PER_POLL: usize = 16;

/// The stream quotas of every connection of one transport, and what they
/// refused.
pub(crate) struct StreamQuotas {
    quotas: KadQuotas,
    clock: Arc<dyn Clock>,
    refusals: Refusals,
    used: Mutex<Used>,
}

/// What the peers are using of the quotas.
#[derive(Default)]
struct Used {
    /// Streams of all peers open now.
    open: usize,
    peers: HashMap<PeerId, PeerUse>,
}

/// What one peer is using of its quotas, over all its connections.
#[derive(Default)]
struct PeerUse {
    /// Its streams open now.
    open: usize,
    /// When its streams of the last minute were admitted, oldest first.
    recent: VecDeque<i64>,
}

impl PeerUse {
    fn idle(&self, now_unix: i64) -> bool {
        self.open == 0
            && self
                .recent
                .back()
                .is_none_or(|&at| at <= now_unix - MINUTE_S)
    }
}

impl StreamQuotas {
    /// `quotas` at the time `clock` says, counting what they refuse in
    /// `refusals`.
    pub(crate) fn new(quotas: KadQuotas, clock: Arc<dyn Clock>, refusals: Refusals) -> Self {
        Self {
            quotas,
            clock,
            refusals,
            used: Mutex::new(Used::default()),
        }
    }

    /// A stream of `peer`'s, within the quotas at the clock's time now;
    /// `None` beyond them.
    fn admit(self: &Arc<Self>, peer: PeerId) -> Option<Slot> {
        let now = self.clock.now_unix();
        let mut used = self.used();
        if used.peers.len() > PEERS_KEPT {
            used.peers.retain(|_, peer| !peer.idle(now));
        }
        let Used { open, peers } = &mut *used;
        let of_peer = peers.entry(peer).or_default();
        while of_peer
            .recent
            .front()
            .is_some_and(|&at| at <= now - MINUTE_S)
        {
            of_peer.recent.pop_front();
        }
        let per_minute = usize::try_from(self.quotas.peer_streams_per_minute).unwrap_or(usize::MAX);
        if of_peer.open >= self.quotas.peer_streams
            || of_peer.recent.len() >= per_minute
            || *open >= self.quotas.global_streams
        {
            if of_peer.idle(now) {
                peers.remove(&peer);
            }
            return None;
        }
        of_peer.open += 1;
        of_peer.recent.push_back(now);
        *open += 1;
        Some(Slot {
            quotas: Arc::clone(self),
            peer,
        })
    }

    /// Resets a stream of `peer`'s beyond the quotas, unread, and counts it.
    fn refuse(&self, peer: PeerId, mut send: SendStream, mut recv: RecvStream) {
        self.refusals.stream();
        // Either fails only when the stream is already closed: nothing to
        // do then.
        let _ = send.reset(VarInt::from_u32(REFUSED));
        let _ = recv.stop(VarInt::from_u32(REFUSED));
        tracing::debug!(%peer, "an agentnet-kad/0 stream over the quotas reset");
    }

    /// A panic while the lock was held leaves counts that only err on the
    /// side of refusing: taken as they are.
    fn used(&self) -> MutexGuard<'_, Used> {
        self.used.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stream the other side opened, which counts in its peer's quotas and
/// in the node's until it is dropped.
struct Slot {
    quotas: Arc<StreamQuotas>,
    peer: PeerId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let now = self.quotas.clock.now_unix();
        let mut used = self.quotas.used();
        used.open = used.open.saturating_sub(1);
        if let Some(of_peer) = used.peers.get_mut(&self.peer) {
            of_peer.open = of_peer.open.saturating_sub(1);
            if of_peer.idle(now) {
                used.peers.remove(&self.peer);
            }
        }
    }
}

/// One Iroh connection, as libp2p's muxer.
pub struct IrohMuxer {
    connection: Connection,
    /// The other side, as Iroh authenticated it.
    peer: PeerId,
    quotas: Arc<StreamQuotas>,
    inbound: Option<Streams>,
    outbound: Option<Streams>,
    closing: Option<BoxFuture<'static, ConnectionError>>,
}

impl IrohMuxer {
    /// `connection` to `peer`, whose streams count in `quotas`.
    pub(crate) fn new(connection: Connection, peer: PeerId, quotas: Arc<StreamQuotas>) -> Self {
        Self {
            connection,
            peer,
            quotas,
            inbound: None,
            outbound: None,
            closing: None,
        }
    }
}

impl StreamMuxer for IrohMuxer {
    type Substream = IrohSubstream;
    type Error = ConnectionError;

    /// The next stream the other side opened that fits the quotas; those
    /// that do not are reset on the way.
    fn poll_inbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();
        for _ in 0..REFUSALS_PER_POLL {
            let inbound = this.inbound.get_or_insert_with(|| {
                let connection = this.connection.clone();
                async move { connection.accept_bi().await }.boxed()
            });
            let accepted = std::task::ready!(inbound.poll_unpin(cx));
            this.inbound = None;
            let (send, recv) = accepted?;
            match this.quotas.admit(this.peer) {
                Some(slot) => return Poll::Ready(Ok(IrohSubstream::new(send, recv, Some(slot)))),
                None => this.quotas.refuse(this.peer, send, recv),
            }
        }
        // More may be waiting: come back after the other tasks.
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn poll_outbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();
        let outbound = this.outbound.get_or_insert_with(|| {
            let connection = this.connection.clone();
            async move { connection.open_bi().await }.boxed()
        });
        let opened = std::task::ready!(outbound.poll_unpin(cx));
        this.outbound = None;
        let (send, recv) = opened?;
        Poll::Ready(Ok(IrohSubstream::new(send, recv, None)))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        let closing = this.closing.get_or_insert_with(|| {
            this.connection
                .close(VarInt::from_u32(CLOSED_BY_LIBP2P), b"");
            let connection = this.connection.clone();
            async move { connection.closed().await }.boxed()
        });
        match std::task::ready!(closing.poll_unpin(cx)) {
            ConnectionError::LocallyClosed => Poll::Ready(Ok(())),
            error => Poll::Ready(Err(error)),
        }
    }

    /// Nothing to report: Iroh moves a connection between paths itself,
    /// and libp2p's addresses name the endpoint, not a path.
    fn poll(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<StreamMuxerEvent, Self::Error>> {
        Poll::Pending
    }
}

/// A bidirectional QUIC stream of an Iroh connection, as a libp2p
/// substream. Closing it finishes its sending half.
pub struct IrohSubstream {
    send: SendStream,
    recv: RecvStream,
    /// How closing went, once it was asked: libp2p may ask again.
    closed: Option<Result<(), io::ErrorKind>>,
    /// Its place in the quotas, for a stream the other side opened.
    _slot: Option<Slot>,
}

impl IrohSubstream {
    fn new(send: SendStream, recv: RecvStream, slot: Option<Slot>) -> Self {
        Self {
            send,
            recv,
            closed: None,
            _slot: slot,
        }
    }
}

impl AsyncRead for IrohSubstream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.get_mut().recv.poll_read(cx, buf).map_err(Into::into)
    }
}

impl AsyncWrite for IrohSubstream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().send)
            .poll_write(cx, buf)
            .map_err(Into::into)
    }

    /// QUIC sends what was written without being asked.
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let closed = *this.closed.get_or_insert_with(|| {
            this.send
                .finish()
                .map_err(|error| io::Error::from(error).kind())
        });
        Poll::Ready(closed.map_err(io::Error::from))
    }
}
