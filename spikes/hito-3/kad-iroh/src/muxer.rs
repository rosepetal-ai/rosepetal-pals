//! [`IrohMuxer`]: an Iroh connection as libp2p's `StreamMuxer`, each
//! substream a bidirectional QUIC stream of the connection.
//!
//! QUIC already multiplexes, so there is no muxer protocol on the wire: a
//! substream libp2p opens is `open_bi`, one it accepts is `accept_bi`, as
//! `libp2p-quic` does over its own QUIC connections. A QUIC peer learns of
//! a new stream only when the opener sends on it, which libp2p's
//! multistream-select always does first. The connection closes when
//! libp2p closes it ([`StreamMuxer::poll_close`]); when the other side or
//! Iroh closes it, the pending `accept_bi` fails and libp2p drops it.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream, VarInt};
use libp2p::core::muxing::{StreamMuxer, StreamMuxerEvent};
use libp2p::futures::future::BoxFuture;
use libp2p::futures::{AsyncRead, AsyncWrite, FutureExt};

type Streams = BoxFuture<'static, Result<(SendStream, RecvStream), ConnectionError>>;

/// The code a connection closes with when libp2p closes it.
const CLOSED_BY_LIBP2P: u32 = 0;

/// One Iroh connection, as libp2p's muxer.
pub struct IrohMuxer {
    connection: Connection,
    inbound: Option<Streams>,
    outbound: Option<Streams>,
    closing: Option<BoxFuture<'static, ConnectionError>>,
}

impl IrohMuxer {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            inbound: None,
            outbound: None,
            closing: None,
        }
    }
}

impl StreamMuxer for IrohMuxer {
    type Substream = IrohSubstream;
    type Error = ConnectionError;

    fn poll_inbound(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        let this = self.get_mut();
        let inbound = this.inbound.get_or_insert_with(|| {
            let connection = this.connection.clone();
            async move { connection.accept_bi().await }.boxed()
        });
        let (send, recv) = std::task::ready!(inbound.poll_unpin(cx))?;
        this.inbound = None;
        Poll::Ready(Ok(IrohSubstream::new(send, recv)))
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
        let (send, recv) = std::task::ready!(outbound.poll_unpin(cx))?;
        this.outbound = None;
        Poll::Ready(Ok(IrohSubstream::new(send, recv)))
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
}

impl IrohSubstream {
    fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            closed: None,
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
