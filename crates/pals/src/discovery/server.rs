//! The provider side of `agentnet-discovery/0`: the transport around
//! [`answer`], with its limits on size and time and its quotas.
//!
//! On every stream a peer opens, in order:
//!
//! 1. **Quotas** ([`Quotas`]), before anything is read: at most
//!    [`Quotas::peer_concurrent`] streams of one peer at a time and
//!    [`Quotas::peer_per_minute`] of its requests in the last minute of the
//!    node's [`Clock`], and at most [`Quotas::global_concurrent`] streams of
//!    all peers at a time. Beyond them the stream is answered
//!    `RATE_LIMITED`, with `retry_after_ms`, and never queued.
//! 2. **The request**, whole within [`Quotas::read_timeout`]: its length
//!    prefix, refused `TOO_LARGE` beyond [`MAX_REQUEST_FRAME_BYTES`] before
//!    any of the body is read, then its body and the stream's end. A frame
//!    that is not a request, is cut short or is followed by anything is
//!    `BAD_REQUEST`; one late, `BAD_REQUEST` too.
//! 3. **The answer**: [`answer`], off the async threads, at the node's
//!    [`Clock`]. Every response, refusals included, carries the node's
//!    signed records.
//!
//! The server holds the [`DiscoveryContext`] and nothing else of the node:
//! no client for peers, no worker, no blob store, no adapter. It cannot run
//! work, and it cannot pass a request on.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};
use pals_core::clock::Clock;
use pals_core::discovery::wire::{self, ErrorCode};
use pals_core::discovery::{
    DiscoveryContext, MAX_REQUEST_FRAME_BYTES, WireError, answer, decode, encode_frame,
    error_response, frame_len,
};
use pals_core::identity::AgentId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use crate::net::agent_id_of;

/// The window [`Quotas::peer_per_minute`] counts requests in, in seconds.
const MINUTE_S: i64 = 60;
/// When to try again after a refusal for streams open at once: a guess,
/// since only the streams' end frees them.
const RETRY_BUSY_MS: u32 = 1000;
/// The streams a connection may open beyond its peer's quota, so that one
/// more is refused rather than left waiting for stream credit.
const STREAM_HEADROOM: usize = 4;
/// Above this many peers tracked, those with nothing open and nothing in
/// their window are forgotten.
const PEERS_KEPT: usize = 1024;
/// The code a stream is stopped with when its request is not read whole.
const NOT_READ: u32 = 0;

/// What one peer, and all of them, may make the server do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quotas {
    /// Streams of one peer served at a time.
    pub peer_concurrent: usize,
    /// Requests of one peer in a minute.
    pub peer_per_minute: u32,
    /// Streams of all peers served at a time.
    pub global_concurrent: usize,
    /// Time a request has to arrive whole.
    pub read_timeout: Duration,
}

impl Default for Quotas {
    /// 4 streams and 60 requests a minute per peer, 64 streams in all, 5 s
    /// to send a request.
    fn default() -> Self {
        Self {
            peer_concurrent: 4,
            peer_per_minute: 60,
            global_concurrent: 64,
            read_timeout: Duration::from_secs(5),
        }
    }
}

/// Runs before [`answer`], once a request is past the quotas and read. For
/// tests only, which hold requests there to fill the quotas; a node runs
/// with none.
pub trait DiscoveryHook: Send + Sync {
    /// Awaited before the answer to `req` from `peer`.
    fn before_answer(&self, peer: &AgentId, req: &wire::Request) -> BoxFuture<'static, ()>;
}

/// The Iroh protocol handler of `agentnet-discovery/0`, for
/// [`pals_core::discovery::ALPN_DISCOVERY`]. Clones share the quotas.
#[derive(Clone)]
pub struct DiscoveryServer {
    inner: Arc<Inner>,
    hook: Option<Arc<dyn DiscoveryHook>>,
}

struct Inner {
    ctx: Arc<DiscoveryContext>,
    quotas: Quotas,
    clock: Arc<dyn Clock>,
    /// One permit per stream served, of all peers.
    streams: Arc<Semaphore>,
    peers: Mutex<HashMap<AgentId, PeerUse>>,
}

/// What a peer is using of its quotas.
#[derive(Debug, Default)]
struct PeerUse {
    /// Streams served now.
    open: usize,
    /// When its requests of the last minute were admitted, oldest first.
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

impl DiscoveryServer {
    /// Answers with `ctx` within `quotas`, at the time `clock` says.
    pub fn new(ctx: DiscoveryContext, quotas: Quotas, clock: Arc<dyn Clock>) -> Self {
        let streams = Arc::new(Semaphore::new(quotas.global_concurrent));
        Self {
            inner: Arc::new(Inner {
                ctx: Arc::new(ctx),
                quotas,
                clock,
                streams,
                peers: Mutex::new(HashMap::new()),
            }),
            hook: None,
        }
    }

    /// With `hook` run before every answer. Tests only.
    #[doc(hidden)]
    pub fn with_hook(mut self, hook: Arc<dyn DiscoveryHook>) -> Self {
        self.hook = Some(hook);
        self
    }

    /// One exchange: the request on `recv`, the response on `send`.
    async fn serve(&self, peer: AgentId, mut send: SendStream, mut recv: RecvStream) {
        let slot = match self.admit(peer) {
            Ok(slot) => slot,
            Err(refusal) => {
                // Not read, not queued.
                let _ = recv.stop(VarInt::from_u32(NOT_READ));
                return self.reply(&mut send, refusal).await;
            }
        };
        let read = tokio::time::timeout(self.inner.quotas.read_timeout, read_request(&mut recv));
        let request = match read.await {
            Ok(Ok(request)) => request,
            Ok(Err(refusal)) => {
                let _ = recv.stop(VarInt::from_u32(NOT_READ));
                return self.reply(&mut send, refusal).await;
            }
            Err(_) => {
                tracing::debug!(%peer, "a discovery request did not arrive in time");
                let _ = recv.stop(VarInt::from_u32(NOT_READ));
                let late = wire::Error::new(ErrorCode::BadRequest, "request timed out");
                return self.reply(&mut send, late).await;
            }
        };
        if let Some(hook) = &self.hook {
            hook.before_answer(&peer, &request).await;
        }
        let ctx = Arc::clone(&self.inner.ctx);
        let now = self.inner.clock.now_unix();
        let answered = tokio::task::spawn_blocking(move || answer(&ctx, &peer, request, now)).await;
        let response = answered.unwrap_or_else(|err| {
            tracing::warn!(error = %err, "a discovery answer did not finish");
            error_response(
                &self.inner.ctx,
                wire::Error::new(ErrorCode::Internal, "internal error"),
            )
        });
        write_response(&mut send, &response).await;
        drop(slot);
    }

    /// Sends `error`, with the node's records, as the response.
    async fn reply(&self, send: &mut SendStream, error: wire::Error) {
        write_response(send, &error_response(&self.inner.ctx, error)).await;
    }

    /// A stream of `peer`'s, within the quotas at the clock's time now; the
    /// refusal otherwise.
    fn admit(&self, peer: AgentId) -> Result<Slot, wire::Error> {
        let inner = &self.inner;
        let now = inner.clock.now_unix();
        let mut peers = inner.peers();
        if peers.len() > PEERS_KEPT {
            peers.retain(|_, used| !used.idle(now));
        }
        let used = peers.entry(peer).or_default();
        while used.recent.front().is_some_and(|&at| at <= now - MINUTE_S) {
            used.recent.pop_front();
        }
        let refused = if used.open >= inner.quotas.peer_concurrent {
            Some(RETRY_BUSY_MS)
        } else if used.recent.len() >= inner.quotas.peer_per_minute as usize {
            // When the oldest request leaves the window.
            let oldest = used.recent.front().copied().unwrap_or(now);
            let wait_s = (oldest + MINUTE_S - now).clamp(1, MINUTE_S);
            Some(u32::try_from(wait_s * 1000).unwrap_or(u32::MAX))
        } else {
            None
        };
        let permit = match refused {
            Some(retry_after_ms) => Err(retry_after_ms),
            None => Arc::clone(&inner.streams)
                .try_acquire_owned()
                .map_err(|_| RETRY_BUSY_MS),
        };
        match permit {
            Ok(permit) => {
                used.open += 1;
                used.recent.push_back(now);
                Ok(Slot {
                    inner: Arc::clone(inner),
                    peer,
                    _permit: permit,
                })
            }
            Err(retry_after_ms) => {
                if used.idle(now) {
                    peers.remove(&peer);
                }
                tracing::debug!(%peer, "a discovery stream over the quotas refused");
                let mut error = wire::Error::new(ErrorCode::RateLimited, "too many requests");
                error.retry_after_ms = retry_after_ms;
                Err(error)
            }
        }
    }
}

impl Inner {
    /// A panic while the lock was held leaves counts that only err on the
    /// side of refusing: taken as they are.
    fn peers(&self) -> MutexGuard<'_, HashMap<AgentId, PeerUse>> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stream being served, which counts in its peer's quota and in the
/// node's until it is dropped.
struct Slot {
    inner: Arc<Inner>,
    peer: AgentId,
    _permit: OwnedSemaphorePermit,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let now = self.inner.clock.now_unix();
        let mut peers = self.inner.peers();
        if let Some(used) = peers.get_mut(&self.peer) {
            used.open = used.open.saturating_sub(1);
            if used.idle(now) {
                peers.remove(&self.peer);
            }
        }
    }
}

impl ProtocolHandler for DiscoveryServer {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = agent_id_of(&connection.remote_id());
        // Enough stream credit for one stream past the peer's quota to be
        // refused, not left waiting.
        let credit = self.inner.quotas.peer_concurrent + STREAM_HEADROOM;
        connection
            .set_max_concurrent_bi_streams(VarInt::from_u64(credit as u64).unwrap_or(VarInt::MAX));
        let mut exchanges = JoinSet::new();
        while let Ok((send, recv)) = connection.accept_bi().await {
            let server = self.clone();
            exchanges.spawn(async move { server.serve(peer, send, recv).await });
            // Reap the finished ones, so the set does not grow for as long
            // as the connection lives.
            while exchanges.try_join_next().is_some() {}
        }
        // The connection is closed: the exchanges still running end with
        // the set.
        Ok(())
    }
}

impl fmt::Debug for DiscoveryServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveryServer")
            .field("quotas", &self.inner.quotas)
            .field("hook", &self.hook.is_some())
            .finish_non_exhaustive()
    }
}

/// The request `recv` carries: one frame, then the stream's end.
async fn read_request(recv: &mut RecvStream) -> Result<wire::Request, wire::Error> {
    let malformed = || wire::Error::new(ErrorCode::BadRequest, "malformed request");
    let mut prefix = [0u8; 4];
    recv.read_exact(&mut prefix)
        .await
        .map_err(|_| malformed())?;
    let len = frame_len(prefix, MAX_REQUEST_FRAME_BYTES).map_err(|_| {
        wire::Error::new(
            ErrorCode::TooLarge,
            &format!("request frame larger than {MAX_REQUEST_FRAME_BYTES} bytes"),
        )
    })?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(|_| malformed())?;
    // Nothing after the frame but the end of the stream.
    match recv.read(&mut [0u8; 1]).await {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => return Err(malformed()),
    }
    decode::<wire::Request>(&body).map_err(|err| match err {
        WireError::TooLarge | WireError::Malformed => malformed(),
    })
}

/// Sends `response` as the stream's one frame, and its end.
async fn write_response(send: &mut SendStream, response: &wire::Response) {
    if let Err(error) = send.write_all(&encode_frame(response)).await {
        tracing::debug!(%error, "a discovery response was not delivered");
        return;
    }
    let _ = send.finish();
}
