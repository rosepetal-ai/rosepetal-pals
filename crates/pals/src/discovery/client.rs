//! The requester side of `agentnet-discovery/0`.
//!
//! One exchange per stream, on one cached Iroh connection per peer on
//! [`ALPN_DISCOVERY`]: the request frame, closed with FIN, and then the
//! response frame, read only once its length prefix is within the smaller
//! of the request's `max_response_bytes` and [`MAX_RESPONSE_FRAME_BYTES`],
//! and followed by the stream's end and nothing else.
//!
//! Every response carries the peer's signed contact and announce records,
//! and the client takes none without them: each must verify
//! ([`Signed::from_json`]), be signed by the peer the connection
//! authenticated, and be in force, with [`CLOCK_SKEW_S`] of tolerance
//! ([`check_validity`]); then the store compares it with the newest it knows
//! of that peer ([`Store::peer_record_accept`]). A record that goes back is
//! [`DiscoveryError::Regression`], and the whole response is discarded.
//! A response that refuses is [`DiscoveryError::Remote`], once its records
//! passed, with the peer's message cut to [`MAX_ERROR_MESSAGE_BYTES`].
//!
//! A service query ([`DiscoveryClient::query`]) gets an offer, which the
//! client takes only once [`accept_offer`] does (signed by the peer asked,
//! for this agent, for this query, in force) and keeps
//! ([`Store::offer_received_put`]).
//!
//! [`CLOCK_SKEW_S`]: pals_core::grant::CLOCK_SKEW_S

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt, WriteError};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use pals_core::clock::{Clock, SystemClock};
use pals_core::discovery::query::{MAX_QUERY_BYTES, OfferError, ServiceOffer, ServiceQuery};
use pals_core::discovery::wire::{self, request, response};
use pals_core::discovery::{
    ALPN_DISCOVERY, DISCOVERY_VERSION, MAX_ERROR_MESSAGE_BYTES, MAX_REQUEST_FRAME_BYTES,
    MAX_RESPONSE_FRAME_BYTES, accept_offer, decode, encode_frame, frame_len,
};
use pals_core::grant::{SignedGrant, parse_timestamp};
use pals_core::identity::AgentId;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::{Signed, SignedDoc, check_validity};
use pals_core::store::{RecordAccept, Store};

use super::{ANNOUNCE, CONTACT};
use crate::net::endpoint_id_of;

/// How long dialing a peer may take, within the call's own time.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The code a response stream is stopped with when it is not read.
const NOT_READ: u32 = 0;

/// What a peer answered, with its records, verified and accepted.
#[derive(Debug, Clone)]
pub struct Reply {
    pub response: wire::Response,
    pub announce: Signed<AnnounceRecord>,
    pub contact: Signed<ContactRecord>,
    /// The request frame's bytes.
    pub bytes_sent: u64,
    /// The response frame's bytes.
    pub bytes_received: u64,
}

/// Why a call got no answer to use.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DiscoveryError {
    /// No connection or stream to the peer could be had, or the connection
    /// was lost; or this agent could not check the peer's records.
    #[error("peer unreachable: {0}")]
    Unreachable(String),
    /// The answer did not arrive within the call's time.
    #[error("the peer did not answer in time")]
    Timeout,
    /// The request, or the response, is larger than its limit.
    #[error("frame too large")]
    TooLarge,
    /// The exchange broke, or what came back is not a response.
    #[error("discovery protocol error: {0}")]
    Protocol(String),
    /// A record the response carries is not one of the peer's in force.
    #[error("bad record from the peer: {0}")]
    BadRecord(String),
    /// A record goes back from the newest known of the peer: the response
    /// is discarded.
    #[error("the peer's record goes back to before seq {known_seq}")]
    Regression { known_seq: u64 },
    /// The peer refused the request. Its message is the peer's, quoted, at
    /// most [`MAX_ERROR_MESSAGE_BYTES`].
    #[error("the peer refused: {:?} ({:?})", .0.message, .0.code())]
    Remote(wire::Error),
    /// The peer's answer to a service query is no offer this agent takes
    /// ([`accept_offer`]).
    #[error("the peer's offer is refused: {0}")]
    Offer(OfferError),
}

/// Calls peers on `agentnet-discovery/0`. Cloning is cheap, and clones
/// share the connections.
#[derive(Clone)]
pub struct DiscoveryClient {
    endpoint: Endpoint,
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    connections: Arc<Mutex<HashMap<EndpointId, Connection>>>,
}

impl fmt::Debug for DiscoveryClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveryClient")
            .field("endpoint", &self.endpoint.id())
            .finish_non_exhaustive()
    }
}

impl DiscoveryClient {
    /// Calls from `endpoint`, keeping peers' records in `store`, with the
    /// system's clock.
    pub fn new(endpoint: Endpoint, store: Arc<Store>) -> Self {
        Self {
            endpoint,
            store,
            clock: Arc::new(SystemClock),
            connections: Arc::default(),
        }
    }

    /// Judges records in force by `clock` instead.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Sends `req` to `peer`, dialed at the addresses of `addr`, and reads
    /// its answer, all within `timeout`.
    ///
    /// The connection is to `peer`'s own endpoint, whatever endpoint `addr`
    /// names: the handshake proves it is `peer` that answers.
    pub async fn call(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
        req: wire::Request,
        timeout: Duration,
    ) -> Result<Reply, DiscoveryError> {
        let frame = encode_frame(&req);
        if frame.len() - 4 > MAX_REQUEST_FRAME_BYTES {
            return Err(DiscoveryError::TooLarge);
        }
        let limit = match usize::try_from(req.max_response_bytes) {
            Ok(asked) if asked > 0 => asked.min(MAX_RESPONSE_FRAME_BYTES),
            _ => MAX_RESPONSE_FRAME_BYTES,
        };
        let addr = EndpointAddr::from_parts(endpoint_id_of(peer), addr.addrs);
        let (response, bytes_received) =
            tokio::time::timeout(timeout, self.exchange(addr, &frame, limit))
                .await
                .map_err(|_| DiscoveryError::Timeout)??;
        if response.version != DISCOVERY_VERSION {
            return Err(DiscoveryError::Protocol(format!(
                "a response of version {}",
                response.version
            )));
        }
        let (contact, announce) = self.accept_records(peer, &response).await?;
        match response.result {
            Some(response::Result::Error(error)) => Err(DiscoveryError::Remote(bounded(error))),
            Some(_) => Ok(Reply {
                response,
                announce,
                contact,
                bytes_sent: frame.len() as u64,
                bytes_received,
            }),
            None => Err(DiscoveryError::Protocol(
                "a response without a result".to_owned(),
            )),
        }
    }

    /// Asks `peer`, dialed at the addresses of `addr`, for an offer for
    /// `query`, presenting `grant` when given, within `timeout`; the offer,
    /// once [`accept_offer`] takes it at this agent's clock and the store
    /// keeps it under (`peer`, its `offer_id`), and the reply it came in.
    ///
    /// [`DiscoveryError::TooLarge`] for a query over [`MAX_QUERY_BYTES`],
    /// before anything is sent; [`DiscoveryError::Protocol`] for an answer
    /// that is neither an offer nor a refusal; [`DiscoveryError::Offer`] for
    /// an offer refused, which is not kept.
    pub async fn query(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
        query: &ServiceQuery,
        grant: Option<&SignedGrant>,
        timeout: Duration,
    ) -> Result<(Signed<ServiceOffer>, Reply), DiscoveryError> {
        let bytes = query.to_jcs();
        if bytes.len() > MAX_QUERY_BYTES {
            return Err(DiscoveryError::TooLarge);
        }
        let req = wire::Request {
            version: DISCOVERY_VERSION,
            max_response_bytes: 0,
            grant_token: grant.map(SignedGrant::to_token).unwrap_or_default(),
            op: Some(request::Op::ServiceQuery(wire::ServiceQuery {
                query: bytes,
            })),
        };
        let reply = self.call(peer, addr, req, timeout).await?;
        let Some(response::Result::ServiceOffer(offer)) = &reply.response.result else {
            return Err(DiscoveryError::Protocol(
                "an answer to a service query that is not an offer".to_owned(),
            ));
        };
        let now = self.clock.now_unix();
        let offer = accept_offer(offer, peer, self.store.owner(), query, now)
            .map_err(DiscoveryError::Offer)?;
        let kept = {
            let (store, offer) = (Arc::clone(&self.store), offer.clone());
            tokio::task::spawn_blocking(move || store.offer_received_put(&offer, now)).await
        };
        match kept {
            Ok(Ok(())) => Ok((offer, reply)),
            Ok(Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "cannot keep a peer's offer");
                Err(DiscoveryError::Unreachable(
                    "this agent cannot keep the peer's offer".to_owned(),
                ))
            }
            Err(err) => {
                tracing::warn!(error = %err, "keeping a peer's offer did not finish");
                Err(DiscoveryError::Unreachable(
                    "this agent cannot keep the peer's offer".to_owned(),
                ))
            }
        }
    }

    /// Closes every cached connection; the next call dials again.
    pub async fn close_all(&self) {
        let connections: Vec<Connection> = self.lock().drain().map(|(_, c)| c).collect();
        for connection in &connections {
            connection.close(VarInt::from_u32(0), b"closed by discovery client");
        }
        for connection in connections {
            connection.closed().await;
        }
    }

    /// `frame` on a new stream to `addr`, and the response read up to
    /// `limit` bytes of body, with the bytes of its frame.
    async fn exchange(
        &self,
        addr: EndpointAddr,
        frame: &[u8],
        limit: usize,
    ) -> Result<(wire::Response, u64), DiscoveryError> {
        let peer_id = addr.id;
        let (connection, (mut send, mut recv)) = self.open_stream(addr).await?;
        let broken = |what: String| self.broken(&peer_id, &connection, what);
        match send.write_all(frame).await {
            Ok(()) => {
                let _ = send.finish();
            }
            // A server that refuses without reading stops the stream; its
            // answer still comes.
            Err(WriteError::Stopped(_)) => {}
            Err(error) => return Err(broken(error.to_string())),
        }
        let mut prefix = [0u8; 4];
        recv.read_exact(&mut prefix)
            .await
            .map_err(|error| broken(format!("no response: {error}")))?;
        let len = match frame_len(prefix, limit) {
            Ok(len) => len,
            Err(_) => {
                let _ = recv.stop(VarInt::from_u32(NOT_READ));
                return Err(DiscoveryError::TooLarge);
            }
        };
        let mut body = vec![0u8; len];
        recv.read_exact(&mut body)
            .await
            .map_err(|error| broken(format!("a response cut short: {error}")))?;
        // One frame, then the stream's end: anything else is no response.
        match recv.read(&mut [0u8; 1]).await {
            Ok(None) => {}
            Ok(Some(_)) => {
                let _ = recv.stop(VarInt::from_u32(NOT_READ));
                return Err(DiscoveryError::Protocol(
                    "bytes after the response frame".to_owned(),
                ));
            }
            Err(error) => return Err(broken(format!("no end after the response: {error}"))),
        }
        let response = decode::<wire::Response>(&body)
            .map_err(|_| DiscoveryError::Protocol("not a response".to_owned()))?;
        Ok((response, 4 + len as u64))
    }

    /// The records of `response`, verified as `peer`'s in force and
    /// accepted by the store.
    async fn accept_records(
        &self,
        peer: &AgentId,
        response: &wire::Response,
    ) -> Result<(Signed<ContactRecord>, Signed<AnnounceRecord>), DiscoveryError> {
        let now = self.clock.now_unix();
        let contact = verified_record(
            "contact",
            &response.contact_record,
            peer,
            now,
            |doc: &ContactRecord| (&doc.issued_at, &doc.expires_at),
        )?;
        let announce = verified_record(
            "announce",
            &response.announce_record,
            peer,
            now,
            |doc: &AnnounceRecord| (&doc.issued_at, &doc.expires_at),
        )?;
        // Both verified: their expiry reads.
        let expiry = |at: &str| parse_timestamp(at).unwrap_or(i64::MIN);
        let records: [(&'static str, u64, i64, Vec<u8>); 2] = [
            (
                CONTACT,
                contact.doc.seq,
                expiry(&contact.doc.expires_at),
                response.contact_record.clone(),
            ),
            (
                ANNOUNCE,
                announce.doc.seq,
                expiry(&announce.doc.expires_at),
                response.announce_record.clone(),
            ),
        ];
        let accepted = {
            let store = Arc::clone(&self.store);
            let peer = *peer;
            tokio::task::spawn_blocking(move || {
                records
                    .into_iter()
                    .map(|(kind, seq, expires_at, json)| {
                        store.peer_record_accept(&peer, kind, seq, &json, expires_at, now)
                    })
                    .collect::<anyhow::Result<Vec<RecordAccept>>>()
            })
            .await
        };
        let accepted = match accepted {
            Ok(Ok(accepted)) => accepted,
            Ok(Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "cannot check a peer's records");
                return Err(DiscoveryError::Unreachable(
                    "this agent cannot check the peer's records".to_owned(),
                ));
            }
            Err(err) => {
                tracing::warn!(error = %err, "the check of a peer's records did not finish");
                return Err(DiscoveryError::Unreachable(
                    "this agent cannot check the peer's records".to_owned(),
                ));
            }
        };
        for accept in accepted {
            if let RecordAccept::Regression { known_seq } = accept {
                tracing::warn!(%peer, known_seq, "a peer's record goes back: response discarded");
                return Err(DiscoveryError::Regression { known_seq });
            }
        }
        Ok((contact, announce))
    }

    /// A new stream to `addr`, on the cached connection or a new one.
    async fn open_stream(
        &self,
        addr: EndpointAddr,
    ) -> Result<(Connection, (SendStream, RecvStream)), DiscoveryError> {
        let peer_id = addr.id;
        let connection = self.connection(addr.clone()).await?;
        match connection.open_bi().await {
            Ok(stream) => Ok((connection, stream)),
            Err(_) => {
                // The cached connection died after it was looked up; nothing
                // was sent on it, so dialing again is safe.
                self.forget(&peer_id, &connection);
                let connection = self.connection(addr).await?;
                let stream = connection
                    .open_bi()
                    .await
                    .map_err(|error| DiscoveryError::Unreachable(error.to_string()))?;
                Ok((connection, stream))
            }
        }
    }

    /// The cached live connection to `addr`'s endpoint, or a new one.
    async fn connection(&self, addr: EndpointAddr) -> Result<Connection, DiscoveryError> {
        let peer_id = addr.id;
        if let Some(connection) = self.cached(&peer_id) {
            return Ok(connection);
        }
        let connection =
            tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(addr, ALPN_DISCOVERY))
                .await
                .map_err(|_| DiscoveryError::Timeout)?
                .map_err(|error| DiscoveryError::Unreachable(error.to_string()))?;
        let mut connections = self.lock();
        match connections.get(&peer_id) {
            // A concurrent call dialed the same peer first: use that one, and
            // let this one close when dropped.
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

    /// Why an exchange on `connection` broke: the connection lost, which is
    /// forgotten, or `what`.
    fn broken(
        &self,
        peer_id: &EndpointId,
        connection: &Connection,
        what: String,
    ) -> DiscoveryError {
        match connection.close_reason() {
            Some(reason) => {
                self.forget(peer_id, connection);
                DiscoveryError::Unreachable(reason.to_string())
            }
            None => DiscoveryError::Protocol(what),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<EndpointId, Connection>> {
        // The map stays consistent even if a holder panicked.
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// `error` with its message cut to [`MAX_ERROR_MESSAGE_BYTES`], at a
/// character's end, as a server of this protocol sends it.
fn bounded(mut error: wire::Error) -> wire::Error {
    let mut end = error.message.len().min(MAX_ERROR_MESSAGE_BYTES);
    while !error.message.is_char_boundary(end) {
        end -= 1;
    }
    error.message.truncate(end);
    error
}

/// The record of `kind` in `json`, verified, signed by `peer` and in force
/// at `now_unix`; `times` gives its `issued_at` and `expires_at`.
fn verified_record<T: SignedDoc>(
    kind: &str,
    json: &[u8],
    peer: &AgentId,
    now_unix: i64,
    times: impl Fn(&T) -> (&String, &String),
) -> Result<Signed<T>, DiscoveryError> {
    let bad = |why: String| DiscoveryError::BadRecord(format!("{kind} record: {why}"));
    let signed = Signed::<T>::from_json(json).map_err(|err| bad(err.to_string()))?;
    if signed.doc.signer() != peer {
        return Err(bad("signed by another agent".to_owned()));
    }
    let (issued_at, expires_at) = times(&signed.doc);
    check_validity(issued_at, expires_at, now_unix).map_err(|err| bad(err.to_string()))?;
    Ok(signed)
}
