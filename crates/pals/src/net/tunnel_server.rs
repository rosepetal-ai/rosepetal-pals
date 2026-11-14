//! Serves an axum app to Iroh peers, one HTTP/1.1 exchange per stream.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::response::IntoResponse;
use http::header::{CONTENT_LENGTH, HeaderValue};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tower::ServiceExt;

use super::{PEER_HEADER, endpoint_id_hex};

/// Bounds on what one peer can make the server hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TunnelLimits {
    /// Largest request body accepted; a larger one gets `413`.
    pub max_body_bytes: usize,
    /// Exchanges served at the same time on one connection.
    pub max_streams_per_conn: usize,
    /// Time a stream has to deliver the complete request head, and then
    /// again the complete body; a body later than that gets `408`.
    pub head_timeout: Duration,
}

impl Default for TunnelLimits {
    /// 1 MiB bodies, 16 streams per connection, 10 s for the request head
    /// and 10 s for its body.
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
            max_streams_per_conn: 16,
            head_timeout: Duration::from_secs(10),
        }
    }
}

/// Iroh protocol handler that serves an axum app over the A2A HTTP binding.
///
/// Register it on [`ALPN_A2A_HTTP`](super::ALPN_A2A_HTTP). Every request
/// reaches the app with [`PEER_HEADER`] set to the connection's remote id.
#[derive(Debug, Clone)]
pub struct TunnelServer {
    app: axum::Router,
    limits: TunnelLimits,
}

impl TunnelServer {
    pub fn new(app: axum::Router, limits: TunnelLimits) -> Self {
        Self { app, limits }
    }
}

impl ProtocolHandler for TunnelServer {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = HeaderValue::from_str(&endpoint_id_hex(&connection.remote_id()))
            .expect("hex digits are a valid header value");
        let max_streams = self.limits.max_streams_per_conn;
        // Stop granting the peer credit for more open streams than the limit...
        connection.set_max_concurrent_bi_streams(
            VarInt::from_u64(max_streams as u64).unwrap_or(VarInt::MAX),
        );
        // ...and never serve more than the limit at once, whatever credit the
        // peer already received during the handshake.
        let slots = Arc::new(Semaphore::new(max_streams));
        let mut exchanges = JoinSet::new();
        loop {
            let slot = Arc::clone(&slots)
                .acquire_owned()
                .await
                .expect("the semaphore is never closed");
            let Ok((send, recv)) = connection.accept_bi().await else {
                // The connection is closed: no more streams will arrive.
                break;
            };
            let app = self.app.clone();
            let limits = self.limits;
            let peer = peer.clone();
            exchanges.spawn(async move {
                serve_exchange(app, limits, peer, send, recv).await;
                drop(slot);
            });
            // Reap finished exchanges so the set does not grow for as long as
            // the connection lives.
            while exchanges.try_join_next().is_some() {}
        }
        Ok(())
    }
}

/// Serves the single HTTP/1.1 exchange a stream carries. The response ends
/// with the stream's FIN, sent when hyper shuts the connection down.
async fn serve_exchange(
    app: axum::Router,
    limits: TunnelLimits,
    peer: HeaderValue,
    send: SendStream,
    recv: RecvStream,
) {
    let io = TokioIo::new(tokio::io::join(recv, send));
    let service = service_fn(move |request| handle(app.clone(), limits, peer.clone(), request));
    let exchange = http1::Builder::new()
        .keep_alive(false)
        .timer(TokioTimer::new())
        .header_read_timeout(limits.head_timeout)
        .serve_connection(io, service);
    if let Err(error) = exchange.await {
        tracing::debug!(%error, "tunnel exchange failed");
    }
}

async fn handle(
    app: axum::Router,
    limits: TunnelLimits,
    peer: HeaderValue,
    request: Request<Incoming>,
) -> Result<Response<Body>, std::convert::Infallible> {
    let max_body_bytes = limits.max_body_bytes;
    let (mut parts, body) = request.into_parts();
    let declared_length = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > max_body_bytes as u64) {
        return Ok(payload_too_large(max_body_bytes));
    }
    // A2A requests are small JSON documents, so the body is read here, bounded
    // in size and in time, instead of being streamed into the app: a peer that
    // stalls it would otherwise hold its stream's slot for good.
    let collected = tokio::time::timeout(
        limits.head_timeout,
        Limited::new(body, max_body_bytes).collect(),
    );
    let body = match collected.await {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(error)) if error.is::<LengthLimitError>() => {
            return Ok(payload_too_large(max_body_bytes));
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "unreadable request body");
            return Ok((StatusCode::BAD_REQUEST, "unreadable request body").into_response());
        }
        Err(_) => {
            tracing::debug!("a request body did not arrive in time");
            return Ok((StatusCode::REQUEST_TIMEOUT, "request body timed out").into_response());
        }
    };
    // `insert` replaces every value the peer sent: the app only ever sees the
    // id that the QUIC handshake authenticated.
    parts.headers.insert(PEER_HEADER, peer);
    let Ok(response) = app
        .oneshot(Request::from_parts(parts, Body::from(body)))
        .await;
    Ok(response)
}

fn payload_too_large(max_body_bytes: usize) -> Response<Body> {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("request body exceeds {max_body_bytes} bytes"),
    )
        .into_response()
}
