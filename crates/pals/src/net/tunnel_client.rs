//! Sends HTTP/1.1 requests to peers over Iroh, one exchange per stream.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::body::Body;
use http::header::{HOST, HeaderValue};
use http::{Request, Response};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId};

use super::{ALPN_A2A_HTTP, endpoint_id_hex};

/// How long dialing a peer may take before [`TunnelError::Timeout`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a request to a peer got no response.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// No connection or stream to the peer could be opened, or the
    /// connection was lost during the exchange.
    #[error("peer unreachable: {0}")]
    Unreachable(String),
    /// The HTTP exchange over the stream failed.
    #[error("tunnel protocol error: {0}")]
    Protocol(String),
    /// Dialing the peer took longer than allowed.
    #[error("timed out dialing the peer")]
    Timeout,
}

/// HTTP client for peers: each request travels on its own bidirectional
/// stream of a single cached Iroh connection per peer.
///
/// Cloning is cheap and clones share the connection cache.
#[derive(Debug, Clone)]
pub struct PeerHttp {
    endpoint: Endpoint,
    connections: Arc<Mutex<HashMap<EndpointId, Connection>>>,
}

impl PeerHttp {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            connections: Arc::default(),
        }
    }

    /// Sends `request` to `peer` and returns as soon as the response head
    /// arrives; the body streams from the tunnel as it is read.
    pub async fn send(
        &self,
        peer: EndpointAddr,
        mut request: Request<Body>,
    ) -> Result<Response<Body>, TunnelError> {
        let peer_id = peer.id;
        // HTTP/1.1 requires a Host header; the peer's id names the origin.
        if !request.headers().contains_key(HOST) {
            let host = HeaderValue::from_str(&endpoint_id_hex(&peer_id))
                .expect("hex digits are a valid header value");
            request.headers_mut().insert(HOST, host);
        }

        let (connection, (send, recv)) = self.open_stream(peer).await?;
        let io = TokioIo::new(tokio::io::join(recv, send));
        let (mut sender, exchange) = http1::handshake(io)
            .await
            .map_err(|error| TunnelError::Protocol(error.to_string()))?;
        // The exchange keeps running after `send` returns, to stream the
        // response body; it ends with the stream.
        tokio::spawn(async move {
            if let Err(error) = exchange.await {
                tracing::debug!(%error, "tunnel exchange failed");
            }
        });
        match sender.send_request(request).await {
            Ok(response) => Ok(response.map(Body::new)),
            Err(error) => Err(self.exchange_failed(&peer_id, &connection, &error)),
        }
    }

    /// Closes every cached connection; the next request dials again.
    pub async fn close_all(&self) {
        let connections: Vec<Connection> = self.lock().drain().map(|(_, c)| c).collect();
        for connection in &connections {
            connection.close(VarInt::from_u32(0), b"closed by peer client");
        }
        for connection in connections {
            connection.closed().await;
        }
    }

    async fn open_stream(
        &self,
        peer: EndpointAddr,
    ) -> Result<(Connection, (SendStream, RecvStream)), TunnelError> {
        let connection = self.connection(peer.clone()).await?;
        match connection.open_bi().await {
            Ok(stream) => Ok((connection, stream)),
            Err(_) => {
                // The cached connection died after it was looked up. Nothing
                // was sent on it, so dialing again is safe.
                self.forget(&peer.id, &connection);
                let connection = self.connection(peer).await?;
                let stream = connection
                    .open_bi()
                    .await
                    .map_err(|error| TunnelError::Unreachable(error.to_string()))?;
                Ok((connection, stream))
            }
        }
    }

    /// The cached live connection to `peer`, or a new one.
    async fn connection(&self, peer: EndpointAddr) -> Result<Connection, TunnelError> {
        let peer_id = peer.id;
        if let Some(connection) = self.cached(&peer_id) {
            return Ok(connection);
        }
        let connection =
            tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(peer, ALPN_A2A_HTTP))
                .await
                .map_err(|_| TunnelError::Timeout)?
                .map_err(|error| TunnelError::Unreachable(error.to_string()))?;

        let mut connections = self.lock();
        match connections.get(&peer_id) {
            // A concurrent request dialed the same peer first: use that
            // connection and let this one close when dropped.
            Some(existing) if existing.close_reason().is_none() => Ok(existing.clone()),
            _ => {
                connections.insert(peer_id, connection.clone());
                Ok(connection)
            }
        }
    }

    fn cached(&self, peer_id: &EndpointId) -> Option<Connection> {
        let mut connections = self.lock();
        let connection = connections.get(peer_id)?;
        if connection.close_reason().is_none() {
            return Some(connection.clone());
        }
        connections.remove(peer_id);
        None
    }

    /// Evicts `connection` if it is still the one cached for `peer_id`.
    fn forget(&self, peer_id: &EndpointId, connection: &Connection) {
        let mut connections = self.lock();
        if connections
            .get(peer_id)
            .is_some_and(|cached| cached.stable_id() == connection.stable_id())
        {
            connections.remove(peer_id);
        }
    }

    fn exchange_failed(
        &self,
        peer_id: &EndpointId,
        connection: &Connection,
        error: &hyper::Error,
    ) -> TunnelError {
        match connection.close_reason() {
            Some(reason) => {
                self.forget(peer_id, connection);
                TunnelError::Unreachable(reason.to_string())
            }
            None => TunnelError::Protocol(error.to_string()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<EndpointId, Connection>> {
        // The guarded map stays consistent even if a holder panicked.
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
