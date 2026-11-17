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
//!   sees it. A stream counts until libp2p drops its substream.
//! - **This side's own streams.** The node keeps at most
//!   [`OUTBOUND_STREAMS_PER_PEER`] of its own streams open to one peer,
//!   counting all its connections to it: half the 8 at a time that a peer
//!   with the default quotas takes, which leaves room for the time the
//!   other side takes to see a stream close, and for a second connection.
//!   Rounds of publication, lookups and a router's replication all go
//!   through this one cap. One more waits ([`StreamMuxer::poll_outbound`]
//!   is pending) until one of them closes, which wakes it: the cap refuses
//!   no stream. The wait is not free, though: `libp2p-swarm` starts a
//!   requested stream's timer before it asks the muxer for it, so a stream
//!   that waits past `libp2p-kad`'s 10 s request timeout fails that
//!   request. A well-behaved node is never refused for its own traffic.
//! - **Idle streams.** A stream the other side opened that goes the idle
//!   timeout with no read or write through it is reset with [`REFUSED`] and
//!   counted in [`Refusals`], and libp2p drops it, which frees its place in
//!   the quotas. `libp2p-kad` 0.49 waits for the next request on an
//!   incoming stream with no deadline of its own, so without this one peer
//!   could hold its quota of streams for as long as it liked.
//! - **Stream credit.** Each connection grants the other side credit for
//!   [`KadQuotas::peer_streams`] streams at a time, plus
//!   [`STREAM_HEADROOM`], so that one past the quota is refused, not left
//!   waiting, as the daemon's other ALPNs do. QUIC never takes back the
//!   credit of the handshake: the lower one applies as streams close.
//! - **Closing.** The connection closes when libp2p closes it
//!   ([`StreamMuxer::poll_close`]); when the other side or Iroh closes it,
//!   the pending `accept_bi` fails and libp2p drops it.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

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
/// The stream credit a connection grants beyond [`KadQuotas::peer_streams`]:
/// room for the streams past the quota to arrive and be refused.
pub const STREAM_HEADROOM: usize = 4;
/// This side's own streams open to one peer at a time, over all its
/// connections to it: half the default [`KadQuotas::peer_streams`].
pub const OUTBOUND_STREAMS_PER_PEER: usize = 4;

/// The stream quotas of every connection of one transport, and what they
/// refused.
pub(crate) struct StreamQuotas {
    quotas: KadQuotas,
    clock: Arc<dyn Clock>,
    refusals: Refusals,
    /// How long an admitted stream may go with no read or write.
    idle_timeout: Duration,
    used: Mutex<Used>,
    /// This side's own streams open to each peer, and the muxers waiting
    /// for one of them to close.
    own: Mutex<HashMap<PeerId, Own>>,
}

/// This side's own streams to one peer.
#[derive(Default)]
struct Own {
    open: usize,
    /// Woken, all of them, each time one of those streams closes.
    waiting: Vec<Waker>,
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
    /// `quotas` at the time `clock` says, with streams idle after
    /// `idle_timeout`, counting what they refuse in `refusals`.
    pub(crate) fn new(
        quotas: KadQuotas,
        clock: Arc<dyn Clock>,
        refusals: Refusals,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            quotas,
            clock,
            refusals,
            idle_timeout,
            used: Mutex::new(Used::default()),
            own: Mutex::new(HashMap::new()),
        }
    }

    /// A place for one more of this side's own streams to `peer`, as the
    /// module says; pending, and woken when one frees, while
    /// [`OUTBOUND_STREAMS_PER_PEER`] are open.
    fn claim_own(self: &Arc<Self>, peer: PeerId, cx: &mut Context<'_>) -> Poll<OwnSlot> {
        let mut own = self.own.lock().unwrap_or_else(PoisonError::into_inner);
        let of_peer = own.entry(peer).or_default();
        if of_peer.open < OUTBOUND_STREAMS_PER_PEER {
            of_peer.open += 1;
            return Poll::Ready(OwnSlot {
                quotas: Arc::clone(self),
                peer,
            });
        }
        if !of_peer
            .waiting
            .iter()
            .any(|waiting| waiting.will_wake(cx.waker()))
        {
            of_peer.waiting.push(cx.waker().clone());
        }
        Poll::Pending
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

/// One of this side's own streams to a peer, open or being opened: it
/// counts in [`OUTBOUND_STREAMS_PER_PEER`] until it is dropped, which wakes
/// whoever waits for a place.
struct OwnSlot {
    quotas: Arc<StreamQuotas>,
    peer: PeerId,
}

impl Drop for OwnSlot {
    fn drop(&mut self) {
        let waiting = {
            let mut own = self
                .quotas
                .own
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(of_peer) = own.get_mut(&self.peer) else {
                return;
            };
            of_peer.open = of_peer.open.saturating_sub(1);
            let waiting = std::mem::take(&mut of_peer.waiting);
            if of_peer.open == 0 {
                own.remove(&self.peer);
            }
            waiting
        };
        for waker in waiting {
            waker.wake();
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
    /// A stream of this side's being opened, with its place.
    outbound: Option<(OwnSlot, Streams)>,
    closing: Option<BoxFuture<'static, ConnectionError>>,
}

impl IrohMuxer {
    /// `connection` to `peer`, whose streams count in `quotas`, granting
    /// the other side credit for [`KadQuotas::peer_streams`] streams plus
    /// [`STREAM_HEADROOM`].
    pub(crate) fn new(connection: Connection, peer: PeerId, quotas: Arc<StreamQuotas>) -> Self {
        let credit = quotas.quotas.peer_streams.saturating_add(STREAM_HEADROOM);
        connection.set_max_concurrent_bi_streams(
            VarInt::from_u64(u64::try_from(credit).unwrap_or(u64::MAX)).unwrap_or(VarInt::MAX),
        );
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
                Some(slot) => return Poll::Ready(Ok(IrohSubstream::inbound(send, recv, slot))),
                None => this.quotas.refuse(this.peer, send, recv),
            }
        }
        // More may be waiting: come back after the other tasks.
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    /// A stream of this side's, once it has a place among
    /// [`OUTBOUND_STREAMS_PER_PEER`]: pending until then.
    fn poll_outbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();
        if this.outbound.is_none() {
            let slot = std::task::ready!(this.quotas.claim_own(this.peer, cx));
            let connection = this.connection.clone();
            let opening = async move { connection.open_bi().await }.boxed();
            this.outbound = Some((slot, opening));
        }
        let (_, opening) = this.outbound.as_mut().expect("being opened");
        let opened = std::task::ready!(opening.poll_unpin(cx));
        // A stream that failed to open gives its place back here.
        let (slot, _) = this.outbound.take().expect("being opened");
        let (send, recv) = opened?;
        Poll::Ready(Ok(IrohSubstream::outbound(send, recv, slot)))
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
    /// Its place in the quotas and its idle deadline, for a stream the
    /// other side opened.
    inbound: Option<Inbound>,
    /// Its place among this side's own streams, for one this side opened.
    _own: Option<OwnSlot>,
}

/// What a stream the other side opened holds: its place in the quotas, and
/// the deadline of its next read or write.
struct Inbound {
    slot: Slot,
    /// Made when the stream first waits, so that a substream needs a timer
    /// only once it is polled; `None` again once it passed.
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    /// Whether the deadline passed and the stream was reset.
    expired: bool,
}

impl IrohSubstream {
    /// A stream the other side opened, admitted in `slot`.
    fn inbound(send: SendStream, recv: RecvStream, slot: Slot) -> Self {
        Self {
            send,
            recv,
            closed: None,
            inbound: Some(Inbound {
                slot,
                deadline: None,
                expired: false,
            }),
            _own: None,
        }
    }

    /// A stream this side opened, in its place `own`.
    fn outbound(send: SendStream, recv: RecvStream, own: OwnSlot) -> Self {
        Self {
            send,
            recv,
            closed: None,
            inbound: None,
            _own: Some(own),
        }
    }

    /// A read or a write went through: the idle deadline starts again.
    fn active(&mut self) {
        if let Some(Inbound {
            slot,
            deadline: Some(deadline),
            ..
        }) = &mut self.inbound
        {
            let next = tokio::time::Instant::now() + slot.quotas.idle_timeout;
            deadline.as_mut().reset(next);
        }
    }

    /// A read or a write waits: once the idle deadline passes, the stream
    /// is reset with [`REFUSED`] and counted, and fails.
    fn waiting<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        let Some(inbound) = &mut self.inbound else {
            return Poll::Pending;
        };
        let idle_timeout = inbound.slot.quotas.idle_timeout;
        let deadline = inbound
            .deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(idle_timeout)));
        std::task::ready!(deadline.as_mut().poll(cx));
        inbound.expired = true;
        inbound.deadline = None;
        // Either fails only when the stream is already closed: nothing to
        // do then.
        let _ = self.send.reset(VarInt::from_u32(REFUSED));
        let _ = self.recv.stop(VarInt::from_u32(REFUSED));
        inbound.slot.quotas.refusals.idle_stream();
        tracing::debug!(
            peer = %inbound.slot.peer,
            "an idle agentnet-kad/0 stream reset"
        );
        Poll::Ready(Err(idle()))
    }

    /// Whether the stream was reset for being idle.
    fn expired(&self) -> bool {
        self.inbound.as_ref().is_some_and(|inbound| inbound.expired)
    }
}

/// The error of a stream reset for being idle.
fn idle() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "an idle stream, reset")
}

impl AsyncRead for IrohSubstream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.expired() {
            return Poll::Ready(Err(idle()));
        }
        match this.recv.poll_read(cx, buf) {
            Poll::Ready(read) => {
                this.active();
                Poll::Ready(read.map_err(Into::into))
            }
            Poll::Pending => this.waiting(cx),
        }
    }
}

impl AsyncWrite for IrohSubstream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.expired() {
            return Poll::Ready(Err(idle()));
        }
        match Pin::new(&mut this.send).poll_write(cx, buf) {
            Poll::Ready(written) => {
                this.active();
                Poll::Ready(written.map_err(Into::into))
            }
            Poll::Pending => this.waiting(cx),
        }
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
