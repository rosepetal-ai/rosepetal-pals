//! The local gateway: the standard A2A entry to every peer.
//!
//! It listens on loopback only. A stock A2A client reaches any PALS peer at
//! `<gateway>/peers/<peer>`, where `<peer>` is a contact name or an agent id,
//! with the standard bindings; and a plain A2A contact, an agent with no
//! PALS behind it, at `<gateway>/peers/<name>` ([`plain`]). `<peer>` is an
//! agent id first, then a PALS contact's name, then a plain contact's:
//!
//! | Route | Behavior |
//! |-|-|
//! | `GET /healthz` | `200 {"agent_id", "name", "ticket"}` |
//! | `GET /peers/{peer}/.well-known/agent-card.json` | the peer's card, verified, as [`gateway_view`], with `Cache-Control: max-age=300` and an `ETag`; `304` when `If-None-Match` names it |
//! | `GET /peers/{peer}/signed-agent-card.json` | the peer's card, verified, as the peer signed it ([`signed_card_json`]): signatures and `iroh://` interfaces included, for tools that check the signature themselves |
//! | `POST /peers/{peer}/jsonrpc`, or `/jsonrpc/` | the peer's `/jsonrpc` |
//! | any method, `/peers/{peer}/rest/{*rest}` | the peer's `/rest/{rest}` |
//! | `GET /discovery/inventory/{peer}?sections=a,b&cursor=…` | a page of the peer's inventory view, over `agentnet-discovery/0`, verified, as the peer signed it |
//! | `POST /discovery/query/{peer}` | the body, an `agentnet.service-query/1`, asked of the peer over `agentnet-discovery/0`: its offer, accepted and kept, as the peer signed it |
//! | `POST /discovery/search` | `{"search": <request>, "ask"?, "wait_s"?}`: a search for a provider among the contacts ([`Searcher`]), and, with `ask`, the question asked of the one chosen |
//! | `POST /inventory/refresh` | runs this agent's inventory adapters now ([`InventoryState::refresh`]): `200` with the [`RefreshReport`], fixed codes only |
//! | `POST /blobs` | stores the raw body as a blob: `201 {"url", "hash", "size"}` |
//! | `GET /blobs/{holder}/{hash}` | the blob, fetched from its holder first if need be |
//! | any method, `/mcp` | the local [MCP server](crate::mcp), Streamable HTTP |
//!
//! A request to a peer travels through [`PeerHttp`], never to the node's own
//! app: method, path after `/peers/{peer}` and query exactly as received
//! (but for the slash after `jsonrpc`, which a client that joins a path to
//! the interface URL adds, as `httpx` does with a base URL),
//! end-to-end headers and the body; the response comes back the same way,
//! Server-Sent Events streamed as they come. The gateway drops the hop-by-hop
//! headers, `Host` and any `x-agentnet-*` header the client sent, which only
//! the peer's tunnel sets, and the cookies a peer sets: every peer shares the
//! gateway's origin. Nor does any `x-agentnet-*` header a peer answers with
//! reach the client: those the client gets are the gateway's own. A2A
//! bindings never redirect, so a peer's `3xx` is a protocol error, and its
//! `Location` never reaches the client.
//!
//! **Cards.** A peer's public card ([`PeerCards::public`]) is read up to
//! 32 KiB and used only once it is valid and signed by the peer (`kid` equal
//! to the agent id Iroh's handshake proved); then it is kept, as the peer's
//! `public` card, and served. One that is not gets `502 {"error":"peer card
//! invalid: <reason>"}`. The gateway does not compare it with the peer's
//! announce record: discovery does. A peer's answer to the extended card
//! (`GetExtendedAgentCard`, `GET /rest/extendedAgentCard` or
//! `/rest/agent-card/extended`) is verified the same way and goes on as its
//! [`gateway_view`], with [`CARD_HASH_HEADER`] naming the card verified
//! ([`checks`]); one that does not verify is a protocol error.
//!
//! **Inventory.** `GET /discovery/inventory/{peer}` asks the peer for a page
//! of its inventory view ([`DiscoveryClient`]), presenting the grant it
//! would present to the peer's A2A server (below), or none, and answers with
//! the page as the peer signed it, re-encoded in ASCII, once it verifies as
//! the peer's page for this agent ([`verify_page`]). `sections` are names
//! of the inventory's sections, comma separated (all when left out);
//! `cursor` is a page's `next_cursor`. The page is kept
//! ([`Store::inventory_cache_put`]) under the request and the grant it was
//! asked with, `public` without one, and served again while it lasts to a
//! request with the same grant: never to one with another grant, or none.
//! An unknown section is `400`; the peer's refusal is `400` for a bad
//! request or cursor, `429` with `Retry-After` when rate limited, `502`
//! otherwise, with its message cut to printable ASCII.
//!
//! **Queries.** `POST /discovery/query/{peer}` takes a service query as its
//! JSON body (at most [`MAX_QUERY_BODY_BYTES`], its JCS at most
//! [`MAX_QUERY_BYTES`], else `413`; one that is not a valid query is `400`
//! with the reason, before the peer is asked), asks the peer
//! ([`DiscoveryClient::query`]) presenting the grant it holds for the
//! query's `project` when it holds one, else the grant it would present to
//! the peer's A2A server, or none, and answers with the offer as the peer
//! signed it, re-encoded in ASCII, once the client accepted it, which keeps
//! it ([`Store::offer_received_put`]). A refused offer is `502`; the peer's
//! refusals and failures are answered as for the inventory.
//!
//! **Searches.** `POST /discovery/search` runs [`Searcher::search`] on the
//! body's `search` (at most [`MAX_QUERY_BODY_BYTES`]), within the limits of
//! `[discovery]` for the whole daemon, never the gateway's own times; with
//! `ask`, [`Searcher::search_then_ask`].
//!
//! When the request carries no `Authorization`, the gateway presents the
//! newest unexpired grant it holds for the peer that allows a task operation
//! on `skill:ask`, and answers `401 {"error":"no grant held for <agent_id>"}`
//! itself, without contacting the peer, when it holds none; a client's own
//! `Authorization` passes intact. It does not choose between projects: the
//! newest such grant wins.
//!
//! Files travel as blobs ([`crate::blobs`]), and a stock client sees them as
//! plain URLs of the gateway, `<gateway>/blobs/<holder>/<hash>`. The body of
//! a request, read whole up to `limits.max_body_bytes` (beyond, `413`), goes
//! to the peer with those URLs as `iroh-blob://` ones ([`rewrite::to_wire`]);
//! a file part that names a blob of this agent's must name one it holds
//! (else `400`), gets the blob's hash and size as its metadata, and the blob
//! is [shared](BlobService::share) with the peer for [`BLOB_SHARE_TTL_S`]
//! before the request leaves.
//!
//! A peer's response never reaches the client as the peer sent it
//! ([`checks`]): whether the client expects one JSON document or an event
//! stream comes from its own call, never from the peer's `Content-Type`; a
//! response with a `Content-Encoding` other than `identity` is refused; a
//! body is read whole, an event of a stream as it completes, either bounded
//! by [`MAX_REWRITE_BYTES`] (a body, or an event before its empty line,
//! that grows past it is refused); a successful body must be JSON, and an
//! event's data, when it has any, too; no task id in them may be unsafe
//! ([`is_safe_task_id`]), since a client resolving
//! `<gateway>/peers/<peer>/rest/tasks/<id>` with dot segments
//! (`../../../<other>/rest/tasks/x`) would reach another peer with the
//! grant the gateway holds for it. What goes on is the gateway's own ASCII
//! encoding of the JSON it checked, its blob URLs the gateway's
//! ([`rewrite::to_gateway`]): a body typed `application/json`, an event as
//! `data: <JSON>` or a bare keepalive. A body refused is `502
//! {"error":"peer protocol error"}`; a stream refused ends, broken, before
//! the event at fault, which never reaches the client.

//! `POST /blobs` takes the body as it is, up to `limits.max_blob_bytes`
//! (beyond, `413`), into the agent's store, and answers with the blob's
//! gateway URL. `GET /blobs/{holder}/{hash}` serves the blob from the store
//! as `application/octet-stream`, whatever it is; when the store lacks it,
//! the gateway first fetches it from `holder` within
//! `limits.blob_fetch_timeout_s`: `403` when the holder refuses, `502` when
//! it cannot be reached or the blob is larger than `limits.max_blob_bytes`,
//! `404` when this agent is the holder and has no such blob.
//!
//! A `<peer>` that is neither a contact nor an agent id gets
//! `404 {"error":"unknown peer"}`, and a plain contact on the routes only
//! PALS peers serve (the signed card, `/discovery/...`) `404 {"error":"not
//! a PALS peer"}`; a peer that cannot be reached, within the
//! 10 s that [`PeerHttp`] allows for dialing, `502 {"error":"peer
//! unreachable"}`; and a broken exchange `502 {"error":"peer protocol
//! error"}`.
//!
//! Every request, to any route, first passes the local guard: one with an
//! `Origin` header (a web page), with `Sec-Fetch-Site` or `Sec-Fetch-Dest`
//! (Fetch Metadata that only a browser sends, navigating too, where it sends
//! no `Origin`; `Sec-Fetch-Mode` alone is Node's `fetch`, and passes), or
//! with a `Host` other than `127.0.0.1`, `localhost` or `[::1]` with the
//! gateway's port (DNS rebinding) gets `403`. Every response, whoever wrote
//! it, carries `Content-Security-Policy: sandbox` and
//! `X-Content-Type-Options: nosniff`, so that a browser never runs what a
//! peer sends at the gateway's origin: a sandboxed page has an opaque
//! origin, and its requests carry `Origin: null`.

mod checks;
pub mod plain;
pub mod rewrite;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::{Body, BodyDataStream, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures::StreamExt;
use http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, HOST, HeaderMap, HeaderName, HeaderValue,
    IF_NONE_MATCH, ORIGIN, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS,
};
use http::{StatusCode, Uri};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use iroh::{Endpoint, EndpointAddr};
use iroh_blobs::Hash;
use iroh_blobs::api::TempTag;
use pals_core::clock::Clock;
use pals_core::discovery::DISCOVERY_VERSION;
use pals_core::discovery::query::{MAX_QUERY_BYTES, ServiceQuery};
use pals_core::discovery::selection::SearchRequest;
use pals_core::discovery::wire::{self, ErrorCode, request, response};
use pals_core::grant::{Grant, Operation, SignedGrant, parse_timestamp};
use pals_core::identity::AgentId;
use pals_core::inventory::Section;
use pals_core::inventory::view::{INVENTORY_SECTIONS, verify_page};
use pals_core::store::{A2aContact, Contact, Store};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use self::checks::{Binding, Call, CardOwner, EventFramer};
use crate::a2a::auth::ASK_RESOURCE;
use crate::a2a::gateway_view;
#[cfg(doc)]
use crate::a2a::is_safe_task_id;
use crate::blobs::{BLOB_META_KEY, BLOB_SHARE_TTL_S, BlobError, BlobRef, BlobService, parse_hash};
use crate::config::LimitsCfg;
use crate::discovery::cards::PUBLIC_CONTEXT;
use crate::discovery::{
    CardFetchError, DiscoveryClient, DiscoveryError, DiscoveryLimits, PeerCards, SearchError,
    Searcher, SearcherParts, signed_card_json,
};
use crate::inventory::InventoryState;
#[cfg(doc)]
use crate::inventory::RefreshReport;
use crate::net::{PeerHttp, TunnelError, peer_addr};
use crate::node::ticket_of;
use crate::outbox::{AskError, Outbox};

/// The response header in which the gateway names the hash of the extended
/// card it verified ([`pals_core::card_jws::card_hash`] of the card as the
/// peer signed it). Only the gateway sets it.
pub const CARD_HASH_HEADER: &str = "x-agentnet-card-hash";
/// How long a client may cache a peer's card view, as the SDK lets a node's
/// own card be cached.
const CARD_MAX_AGE: &str = "max-age=300";
/// The prefix of the headers only PALS sets.
const OWN_HEADER_PREFIX: &str = "x-agentnet-";
/// The Fetch Metadata headers only a browser adds to its requests. Not
/// `Sec-Fetch-Mode`: Node's built-in `fetch` sends it alone on every request,
/// and A2A clients use it.
const FETCH_METADATA: [&str; 2] = ["sec-fetch-site", "sec-fetch-dest"];
/// Response headers of a peer that would apply to every peer, since they all
/// share the gateway's origin.
const PEER_COOKIES: [&str; 2] = ["set-cookie", "set-cookie2"];
/// The largest response body, or event of a stream, the gateway takes from
/// a peer.
const MAX_REWRITE_BYTES: usize = 16 * 1024 * 1024;
/// The type of an event stream.
const EVENT_STREAM: &str = "text/event-stream";
/// The type of JSON.
const JSON: &str = "application/json";
/// The type of A2A's JSON, which a peer may answer with.
const A2A_JSON: &str = "application/a2a+json";
/// The type of every blob the gateway serves, whatever its content.
const OCTET_STREAM: &str = "application/octet-stream";
/// How long asking a peer for a page of its inventory, or for an offer, may
/// take, dialing included.
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest body of `POST /discovery/query/{peer}`: a query, however it
/// is spelled.
pub const MAX_QUERY_BODY_BYTES: usize = 64 * 1024;
/// The longest refusal message of a peer the gateway passes on.
const MAX_PEER_MESSAGE_BYTES: usize = 256;

/// Headers that describe one connection, not the request (RFC 9110 §7.6.1),
/// and are never forwarded, in either direction.
const HOP_BY_HOP: [&str; 10] = [
    "connection",
    "expect",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// What the gateway works with: this agent, its store (contacts and held
/// grants), its client for peers, its blobs, its limits and the address it
/// listens on.
///
/// Cloning is cheap.
#[derive(Clone)]
pub struct GatewayState {
    inner: Arc<Inner>,
}

struct Inner {
    id: AgentId,
    name: String,
    store: Arc<Store>,
    peer_http: PeerHttp,
    /// This agent's endpoint, whose ticket `/healthz` reports.
    endpoint: Endpoint,
    blobs: BlobService,
    limits: LimitsCfg,
    /// Where the gateway listens.
    local_addr: SocketAddr,
    /// The questions this agent asks its peers, through this gateway.
    outbox: Outbox,
    /// Peers' cards, read, verified and kept.
    cards: PeerCards,
    /// Discovery requests to peers.
    discovery: DiscoveryClient,
    /// The search for a provider among the contacts.
    searcher: Searcher,
    /// The HTTP client for plain A2A contacts ([`plain::http_client`]).
    plain_http: reqwest::Client,
    /// The node's clock, which pages of peers' views are judged by.
    clock: Arc<dyn Clock>,
    /// The only directory `pals_fetch` writes to, when configured.
    download_dir: Option<PathBuf>,
    /// This agent's inventory, whose adapters `POST /inventory/refresh`
    /// runs.
    inventory: InventoryState,
    /// Cancelled when the gateway stops: ends the MCP sessions.
    stopping: CancellationToken,
}

/// What the gateway of an agent works with besides its listener.
pub struct GatewayParts {
    pub id: AgentId,
    pub name: String,
    pub store: Arc<Store>,
    pub peer_http: PeerHttp,
    /// Discovery requests to peers.
    pub discovery: DiscoveryClient,
    /// The node's clock.
    pub clock: Arc<dyn Clock>,
    /// The agent's own endpoint.
    pub endpoint: Endpoint,
    pub blobs: BlobService,
    pub limits: LimitsCfg,
    /// What a search may take.
    pub discovery_limits: DiscoveryLimits,
    /// The agent's data directory, whose files are never sent.
    pub data_dir: PathBuf,
    /// The only directory `pals_fetch` writes to, when configured; else
    /// the default, resolved when it is needed.
    pub download_dir: Option<PathBuf>,
    /// This agent's inventory.
    pub inventory: InventoryState,
}

impl GatewayState {
    /// The gateway of the agent `parts` describe, listening on `local_addr`.
    pub fn new(parts: GatewayParts, local_addr: SocketAddr) -> Self {
        let GatewayParts {
            id,
            name,
            store,
            peer_http,
            discovery,
            clock,
            endpoint,
            blobs,
            limits,
            discovery_limits,
            data_dir,
            download_dir,
            inventory,
        } = parts;
        let outbox =
            Outbox::new(Arc::clone(&store), base_url(&local_addr)).with_data_dir(&data_dir);
        let cards = PeerCards::new(peer_http.clone(), Arc::clone(&store));
        let plain_http = plain::http_client();
        let searcher = Searcher::new(SearcherParts {
            store: Arc::clone(&store),
            cards: cards.clone(),
            discovery: discovery.clone(),
            outbox: outbox.clone(),
            plain_http: plain_http.clone(),
            clock: Arc::clone(&clock),
            limits: discovery_limits,
        });
        Self {
            inner: Arc::new(Inner {
                id,
                name,
                store,
                peer_http,
                endpoint,
                blobs,
                limits,
                local_addr,
                outbox,
                cards,
                discovery,
                searcher,
                plain_http,
                clock,
                download_dir,
                inventory,
                stopping: CancellationToken::new(),
            }),
        }
    }

    /// `http://<address the gateway listens on>`.
    pub fn base_url(&self) -> String {
        base_url(&self.inner.local_addr)
    }

    /// The questions this agent asks its peers, through this gateway.
    pub fn outbox(&self) -> &Outbox {
        &self.inner.outbox
    }

    /// The search for a provider among this agent's contacts.
    pub fn searcher(&self) -> &Searcher {
        &self.inner.searcher
    }

    /// The only directory `pals_fetch` writes to: the configured one, else
    /// the default; why there is none when the default has no home
    /// directory to be in.
    pub(crate) fn download_dir(&self) -> Result<PathBuf, String> {
        match &self.inner.download_dir {
            Some(dir) => Ok(dir.clone()),
            None => crate::config::default_download_dir().map_err(|err| format!("{err:#}")),
        }
    }

    /// Cancelled when the gateway stops.
    pub(crate) fn stopping(&self) -> &CancellationToken {
        &self.inner.stopping
    }

    /// The port the gateway listens on.
    pub(crate) fn port(&self) -> u16 {
        self.inner.local_addr.port()
    }

    /// The agent's files.
    pub(crate) fn blobs(&self) -> &BlobService {
        &self.inner.blobs
    }

    /// This agent: `{"agent_id", "name", "ticket"}`, the ticket with the
    /// addresses it is reachable at now.
    pub(crate) fn identity(&self) -> Value {
        let inner = &self.inner;
        json!({
            "agent_id": inner.id.to_string(),
            "name": inner.name,
            "ticket": ticket_of(&inner.endpoint),
        })
    }

    /// Every contact, by name, and whether this agent holds an unexpired
    /// grant for it that [serves tasks](serves_tasks).
    pub(crate) async fn contacts(&self) -> Result<Vec<(Contact, bool)>, StoreUnavailable> {
        let store = Arc::clone(&self.inner.store);
        blocking(move || {
            let now = chrono::Utc::now().timestamp();
            store
                .contacts()?
                .into_iter()
                .map(|contact| {
                    let held = store.grants_held_for(&contact.agent_id, now)?;
                    let has_grant = held.iter().any(|g| serves_tasks(&g.grant));
                    Ok((contact, has_grant))
                })
                .collect()
        })
        .await
    }

    /// Every plain A2A contact, by name.
    pub(crate) async fn a2a_contacts(&self) -> Result<Vec<A2aContact>, StoreUnavailable> {
        let store = Arc::clone(&self.inner.store);
        blocking(move || store.a2a_contacts()).await
    }

    /// The holder and hash of `url` when it is a blob URL of this gateway,
    /// spelled as the gateway writes them: `<base_url>/blobs/<holder>/<hash>`.
    pub(crate) fn blob_of_url(&self, url: &str) -> Option<(AgentId, Hash)> {
        rewrite::gateway_blob(url, &self.base_url())
    }

    /// Blob `hash` of `holder`, from this agent's store, into which the
    /// gateway first fetches it from `holder`, within
    /// `limits.blob_fetch_timeout_s`, when the store lacks it.
    pub(crate) async fn obtain_blob(
        &self,
        holder: AgentId,
        hash: Hash,
    ) -> Result<HeldBlob, Refusal> {
        let blobs = &self.inner.blobs;
        // Held from before the download until it has been read, whatever
        // tags hold it or not.
        let held = match blobs.protect(hash).await {
            Ok(held) => held,
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "cannot protect a blob to serve");
                return Err(Refusal::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error",
                ));
            }
        };
        if let Some(size) = blobs.complete_size(hash).await {
            return Ok(HeldBlob { hash, size, held });
        }
        if holder == self.inner.id {
            return Err(Refusal::new(StatusCode::NOT_FOUND, "blob not found"));
        }
        let limits = &self.inner.limits;
        // The URL declares no size: the limit bounds the download.
        let blob = BlobRef {
            holder,
            hash,
            size: limits.max_blob_bytes,
        };
        let addr = blobs.holder_addr(&holder).await;
        let timeout = Duration::from_secs(limits.blob_fetch_timeout_s);
        let fetched = tokio::time::timeout(timeout, blobs.fetch(&blob, addr, None))
            .await
            .unwrap_or(Err(BlobError::Timeout));
        if let Err(failure) = fetched {
            tracing::debug!(%holder, %hash, error = %failure, "the gateway could not fetch a blob");
            let (status, message) = match failure {
                BlobError::Denied => (StatusCode::FORBIDDEN, "the holder denied the blob"),
                BlobError::Unreachable | BlobError::Timeout => {
                    (StatusCode::BAD_GATEWAY, "peer unreachable")
                }
                BlobError::TooLarge => (StatusCode::BAD_GATEWAY, "blob too large"),
                BlobError::Other(_) => (StatusCode::BAD_GATEWAY, "blob transfer failed"),
            };
            return Err(Refusal::new(status, message));
        }
        match blobs.complete_size(hash).await {
            Some(size) => Ok(HeldBlob { hash, size, held }),
            None => Err(Refusal::new(
                StatusCode::BAD_GATEWAY,
                "blob transfer failed",
            )),
        }
    }

    /// The agent `peer` names, an agent id, a contact's name or a plain
    /// contact's name, in that order, and where to dial a PALS peer; `None`
    /// when it names no one.
    async fn resolve(&self, peer: String) -> Result<Option<Resolved>, StoreUnavailable> {
        let store = Arc::clone(&self.inner.store);
        blocking(move || {
            let (id, contact) = match peer.parse::<AgentId>() {
                Ok(id) => (id, store.contact_by_id(&id)?),
                Err(_) => match store.contact_by_name(&peer)? {
                    Some(contact) => (contact.agent_id, Some(contact)),
                    None => return Ok(store.a2a_contact_by_name(&peer)?.map(Resolved::Plain)),
                },
            };
            let ticket = contact.and_then(|contact| contact.ticket);
            let addr = peer_addr(&id, ticket.as_deref());
            Ok(Some(Resolved::Peer(Target { id, addr })))
        })
        .await
    }

    /// The PALS peer `peer` names; else the answer: `404` for no one, or for
    /// a plain contact, which serves no PALS route.
    async fn resolve_pals(&self, peer: String) -> Result<Target, Refusal> {
        match self.resolve(peer).await {
            Ok(Some(Resolved::Peer(target))) => Ok(target),
            Ok(Some(Resolved::Plain(_))) => {
                Err(Refusal::new(StatusCode::NOT_FOUND, "not a PALS peer"))
            }
            Ok(None) => Err(Refusal::new(StatusCode::NOT_FOUND, "unknown peer")),
            Err(StoreUnavailable) => Err(Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            )),
        }
    }

    /// The newest unexpired grant held for `peer` that [serves
    /// tasks](serves_tasks).
    async fn held_grant(&self, peer: AgentId) -> Result<Option<SignedGrant>, StoreUnavailable> {
        self.held_grant_for(peer, None).await
    }

    /// The newest unexpired grant held for `peer` that [serves
    /// tasks](serves_tasks) in `project`, when there is one; else, as
    /// [`GatewayState::held_grant`], the newest that serves tasks.
    async fn held_grant_for(
        &self,
        peer: AgentId,
        project: Option<String>,
    ) -> Result<Option<SignedGrant>, StoreUnavailable> {
        let store = Arc::clone(&self.inner.store);
        blocking(move || {
            let now = chrono::Utc::now().timestamp();
            let held: Vec<SignedGrant> = store
                .grants_held_for(&peer, now)?
                .into_iter()
                .filter(|g| serves_tasks(&g.grant))
                .collect();
            let for_project =
                project.and_then(|project| held.iter().position(|g| g.grant.project == project));
            Ok(match for_project {
                Some(at) => held.into_iter().nth(at),
                None => held.into_iter().next(),
            })
        })
        .await
    }

    /// `Bearer <token>` of [`GatewayState::held_grant`].
    async fn held_bearer(&self, peer: AgentId) -> Result<Option<HeaderValue>, StoreUnavailable> {
        let held = self.held_grant(peer).await?;
        Ok(held.map(|g| {
            HeaderValue::from_str(&format!("Bearer {}", g.to_token()))
                .expect("a base64url token is a valid header value")
        }))
    }
}

/// `http://<local_addr>`.
fn base_url(local_addr: &SocketAddr) -> String {
    format!("http://{local_addr}")
}

/// A blob of the store, kept in it for as long as this value lives.
pub(crate) struct HeldBlob {
    pub(crate) hash: Hash,
    pub(crate) size: u64,
    /// Keeps the blob from the store's garbage collection.
    pub(crate) held: TempTag,
}

/// Whether `grant` can serve a request of the bindings the gateway forwards:
/// it allows at least one task operation, on a resource that covers
/// `skill:ask`. A `blob.fetch` grant, for instance, cannot.
pub(crate) fn serves_tasks(grant: &Grant) -> bool {
    let task_op = grant.operations.iter().any(|op| {
        matches!(
            op,
            Operation::TaskSend | Operation::TaskRead | Operation::TaskCancel
        )
    });
    task_op && grant.covers_resource(ASK_RESOURCE)
}

/// A PALS peer, resolved.
struct Target {
    id: AgentId,
    addr: EndpointAddr,
}

/// What a `{peer}` names.
enum Resolved {
    Peer(Target),
    Plain(A2aContact),
}

/// The gateway's routes, behind the local guard.
pub fn router(state: GatewayState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/peers/{peer}/.well-known/agent-card.json", get(peer_card))
        .route(
            "/peers/{peer}/signed-agent-card.json",
            get(signed_peer_card),
        )
        .route("/peers/{peer}/jsonrpc", post(forward_jsonrpc))
        .route("/peers/{peer}/jsonrpc/", post(forward_jsonrpc))
        .route("/peers/{peer}/rest/{*rest}", any(forward_rest))
        .route("/discovery/inventory/{peer}", get(peer_inventory))
        .route("/discovery/query/{peer}", post(peer_query))
        .route("/discovery/search", post(discovery_search))
        .route("/inventory/refresh", post(inventory_refresh))
        .route("/blobs", post(upload_blob))
        .route("/blobs/{holder}/{hash}", get(download_blob))
        .route_service("/mcp", crate::mcp::service(state.clone()))
        .layer(middleware::from_fn_with_state(state.clone(), local_guard))
        // Outermost, so that it covers the guard's refusals too.
        .layer(middleware::from_fn(sandbox))
        .with_state(state)
}

/// Makes every response inert in a browser: a sandboxed document with an
/// opaque origin, its content type taken as declared. It replaces whatever
/// policy a peer sent.
async fn sandbox(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// Refuses requests from web pages, from browsers and from names other than
/// loopback's.
async fn local_guard(State(state): State<GatewayState>, request: Request, next: Next) -> Response {
    if request.headers().contains_key(ORIGIN) {
        return error(StatusCode::FORBIDDEN, "requests from web pages are refused");
    }
    if FETCH_METADATA
        .iter()
        .any(|name| request.headers().contains_key(*name))
    {
        return error(StatusCode::FORBIDDEN, "browser requests are not accepted");
    }
    if !is_local_host(
        request.headers(),
        request.uri(),
        state.inner.local_addr.port(),
    ) {
        return error(StatusCode::FORBIDDEN, "host not allowed");
    }
    next.run(request).await
}

/// Whether the request names the gateway by a loopback name and its port:
/// exactly one `Host` header, or an authority in the request target, and
/// every one present is `127.0.0.1:<port>`, `localhost:<port>` or
/// `[::1]:<port>`.
fn is_local_host(headers: &HeaderMap, uri: &Uri, port: u16) -> bool {
    let allowed = |host: &str| {
        let host = host.to_ascii_lowercase();
        [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]
        .contains(&host)
    };
    let mut hosts = headers.get_all(HOST).iter();
    let host = match (hosts.next(), hosts.next()) {
        (Some(host), None) => match host.to_str() {
            Ok(host) => Some(host),
            Err(_) => return false,
        },
        (None, _) => None,
        (Some(_), Some(_)) => return false,
    };
    let authority = uri.authority().map(|authority| authority.as_str());
    match (host, authority) {
        (None, None) => false,
        (host, authority) => host.is_none_or(allowed) && authority.is_none_or(allowed),
    }
}

async fn healthz(State(state): State<GatewayState>) -> Response {
    Json(state.identity()).into_response()
}

/// The peer's card as [`gateway_view`], read from the peer and verified as
/// its own ([`PeerCards::public`]), and kept; a plain contact's as
/// [`plain::plain_view`], read and checked ([`plain::fetch_plain_card`]).
///
/// The response may be cached for [`CARD_MAX_AGE`] and carries a strong
/// `ETag`, the BLAKE3 of its body; a request whose `If-None-Match` names it
/// gets `304 Not Modified` (A2A §8.6).
async fn peer_card(
    State(state): State<GatewayState>,
    Path(peer): Path<String>,
    headers: HeaderMap,
) -> Response {
    let view = match state.resolve(peer).await {
        Ok(Some(Resolved::Peer(target))) => {
            match state.inner.cards.public(&target.id, target.addr).await {
                // From what the signature covers, never from the JSON
                // received.
                Ok(verified) => gateway_view(&verified.card, &state.base_url(), &target.id),
                Err(failure) => return card_failure(&target.id, &failure),
            }
        }
        Ok(Some(Resolved::Plain(contact))) => match state.plain_card(&contact).await {
            Ok(plain) => plain::plain_view(&plain, &state.base_url(), &contact.name),
            Err(failure) => return plain::plain_failure(&contact.name, &failure),
        },
        Ok(None) => return unknown_peer(),
        Err(failure) => return failure.into_response(),
    };
    let body = checks::ascii_json(&serde_json::to_value(&view).expect("an Agent Card serializes"));
    let etag = format!("\"{}\"", blake3::hash(&body).to_hex());
    let etag = HeaderValue::from_str(&etag).expect("hex digits in quotes are a valid header");
    let cached = HeaderValue::from_static(CARD_MAX_AGE);
    if if_none_match(&headers, &etag) {
        return (
            StatusCode::NOT_MODIFIED,
            [(ETAG, etag), (CACHE_CONTROL, cached)],
        )
            .into_response();
    }
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static(JSON)),
            (ETAG, etag),
            (CACHE_CONTROL, cached),
        ],
        body,
    )
        .into_response()
}

/// The peer's card as the peer signed it ([`signed_card_json`]), read from
/// the peer and verified as its own ([`PeerCards::public`]), and kept: only
/// what its valid signature covers, and the signatures that verify,
/// re-encoded in ASCII. For tools that check a card's signature themselves;
/// PALS peers only.
async fn signed_peer_card(State(state): State<GatewayState>, Path(peer): Path<String>) -> Response {
    let target = match state.resolve_pals(peer).await {
        Ok(target) => target,
        Err(refusal) => return refusal.into_response(),
    };
    match state.inner.cards.public(&target.id, target.addr).await {
        Ok(verified) => (
            [(CONTENT_TYPE, HeaderValue::from_static(JSON))],
            checks::ascii_json(&signed_card_json(&verified)),
        )
            .into_response(),
        Err(failure) => card_failure(&target.id, &failure),
    }
}

/// The query of `GET /discovery/inventory/{peer}`.
#[derive(Debug, Deserialize)]
struct InventoryQuery {
    /// Section names, comma separated; all when left out.
    sections: Option<String>,
    /// A page's `next_cursor`.
    cursor: Option<String>,
}

/// A page of `peer`'s inventory view, verified as its page for this agent:
/// kept from an earlier request with the same grant while it lasts, else
/// asked for on `agentnet-discovery/0` with the grant the gateway holds for
/// the peer, if any, and kept.
async fn peer_inventory(
    State(state): State<GatewayState>,
    Path(peer): Path<String>,
    Query(query): Query<InventoryQuery>,
) -> Response {
    let Some(sections) = inventory_sections(query.sections.as_deref()) else {
        return error(StatusCode::BAD_REQUEST, "unknown section");
    };
    let names: Vec<String> = sections.iter().map(|s| section_name(*s)).collect();
    let cursor = query.cursor.unwrap_or_default();
    let target = match state.resolve_pals(peer).await {
        Ok(target) => target,
        Err(refusal) => return refusal.into_response(),
    };
    let grant = match state.held_grant(target.id).await {
        Ok(grant) => grant,
        Err(failure) => return failure.into_response(),
    };
    let context = grant.as_ref().map_or_else(
        || PUBLIC_CONTEXT.to_owned(),
        |g| format!("grant:{}/{}", g.grant.issuer, g.grant.grant_id),
    );
    let key = inventory_request_key(&names, &cursor);
    let (own, now) = (state.inner.id, state.inner.clock.now_unix());

    let kept = {
        let (store, context, key) = (Arc::clone(&state.inner.store), context.clone(), key.clone());
        blocking(move || store.inventory_cache(&target.id, &context, &key, now)).await
    };
    match kept {
        Ok(Some(page)) if verify_page(&page, &target.id, Some(&own), now).is_ok() => {
            return page_response(&page);
        }
        Ok(_) => {}
        Err(failure) => return failure.into_response(),
    }

    let req = wire::Request {
        version: DISCOVERY_VERSION,
        max_response_bytes: 0,
        grant_token: grant
            .as_ref()
            .map(SignedGrant::to_token)
            .unwrap_or_default(),
        op: Some(request::Op::GetInventory(wire::GetInventory {
            sections: names,
            cursor,
            page_entries: 0,
        })),
    };
    let discovery = &state.inner.discovery;
    let reply = match discovery
        .call(&target.id, target.addr.clone(), req, INVENTORY_TIMEOUT)
        .await
    {
        Ok(reply) => reply,
        Err(failure) => return discovery_failure(&target.id, &failure),
    };
    let Some(response::Result::InventoryPage(page)) = reply.response.result else {
        return error(StatusCode::BAD_GATEWAY, "peer protocol error");
    };
    let expires_at = match verify_page(&page, &target.id, Some(&own), now) {
        Ok(verified) => parse_timestamp(&verified.expires_at).unwrap_or(now),
        Err(err) => {
            tracing::debug!(peer = %target.id, error = %err, "an inventory page that does not verify");
            return error(
                StatusCode::BAD_GATEWAY,
                format!("peer inventory page invalid: {err}"),
            );
        }
    };
    let store = Arc::clone(&state.inner.store);
    let (kept_page, peer) = (page.clone(), target.id);
    // A page that cannot be kept is still good to serve.
    let _ =
        blocking(move || store.inventory_cache_put(&peer, &context, &key, &kept_page, expires_at))
            .await;
    page_response(&page)
}

/// `peer`'s offer for the query the body holds, accepted and kept by this
/// agent's discovery client, as the peer signed it.
async fn peer_query(
    State(state): State<GatewayState>,
    Path(peer): Path<String>,
    request: Request,
) -> Response {
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_QUERY_BODY_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "query too large");
    };
    let Ok(query) = serde_json::from_slice::<ServiceQuery>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid query: malformed query");
    };
    if query.to_jcs().len() > MAX_QUERY_BYTES {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "query too large");
    }
    if let Err(why) = query.validate() {
        return error(StatusCode::BAD_REQUEST, format!("invalid query: {why}"));
    }
    let target = match state.resolve_pals(peer).await {
        Ok(target) => target,
        Err(refusal) => return refusal.into_response(),
    };
    let grant = match state.held_grant_for(target.id, query.project.clone()).await {
        Ok(grant) => grant,
        Err(failure) => return failure.into_response(),
    };
    let discovery = &state.inner.discovery;
    match discovery
        .query(
            &target.id,
            target.addr.clone(),
            &query,
            grant.as_ref(),
            INVENTORY_TIMEOUT,
        )
        .await
    {
        Ok((offer, _)) => page_response(&offer.to_json()),
        Err(failure) => discovery_failure(&target.id, &failure),
    }
}

/// The body of `POST /discovery/search`: the search, and, to ask the
/// candidate chosen, the question and how long to wait for its answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchBody {
    search: SearchRequest,
    #[serde(default)]
    ask: Option<String>,
    /// Seconds; [`DEFAULT_SEARCH_WAIT_S`] by default, at most
    /// [`MAX_SEARCH_WAIT_S`].
    #[serde(default)]
    wait_s: Option<u64>,
}

/// How long `POST /discovery/search` with a question waits for the answer
/// when the body does not say.
const DEFAULT_SEARCH_WAIT_S: u64 = 600;
/// The longest it waits.
const MAX_SEARCH_WAIT_S: u64 = 7 * 24 * 3600;

/// `POST /inventory/refresh`: runs this agent's inventory adapters now,
/// after any run in progress, and answers with what they observed
/// ([`RefreshReport`]): fixed codes, never a raw error. `500` when the
/// store cannot keep the observations, which the views show all the same.
async fn inventory_refresh(State(state): State<GatewayState>) -> Response {
    match state.inner.inventory.refresh().await {
        Ok(report) => Json(report).into_response(),
        Err(err) => {
            tracing::warn!(error = ?err, "cannot keep the observations of the adapters");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the observations were made, but cannot be kept",
            )
        }
    }
}

/// `POST /discovery/search`: the search the body asks for, its outcome as
/// JSON ([`Searcher::search`]); with `ask`, the search and the candidate
/// chosen and asked, `{"search", "chosen"}`, or why none took it,
/// `{"search", "error", "tried", "discarded"}`, with `"kept"`, the message
/// id, when the question waits for `pals task resume`
/// ([`Searcher::search_then_ask`]). Everything a peer said is re-encoded
/// in ASCII. `429` when too many searches run, `400` for a search that
/// makes no query.
async fn discovery_search(State(state): State<GatewayState>, request: Request) -> Response {
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_QUERY_BODY_BYTES).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "search too large");
    };
    let body: SearchBody = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(err) => return error(StatusCode::BAD_REQUEST, format!("invalid search: {err}")),
    };
    let searcher = &state.inner.searcher;
    let Some(question) = body.ask else {
        return match searcher.search(body.search).await {
            Ok(outcome) => json_response(&json!(outcome)),
            Err(err) => search_failure(&err),
        };
    };
    let wait = Duration::from_secs(
        body.wait_s
            .unwrap_or(DEFAULT_SEARCH_WAIT_S)
            .clamp(1, MAX_SEARCH_WAIT_S),
    );
    match searcher.search_then_ask(body.search, question, wait).await {
        Ok((outcome, Ok(chosen))) => json_response(&json!({"search": outcome, "chosen": chosen})),
        Ok((outcome, Err(err))) => {
            let mut answer = json!({"search": outcome, "error": err.to_string()});
            match err {
                SearchError::NoProvider { tried, discarded } => {
                    answer["tried"] = json!(tried);
                    answer["discarded"] = json!(discarded);
                }
                SearchError::Ask {
                    error,
                    tried,
                    discarded,
                    ..
                } => {
                    answer["tried"] = json!(tried);
                    answer["discarded"] = json!(discarded);
                    if let AskError::Unreachable { message_id } = error {
                        answer["kept"] = json!(message_id);
                    }
                }
                _ => {}
            }
            json_response(&answer)
        }
        Err(err) => search_failure(&err),
    }
}

/// The answer to a search that has no outcome.
fn search_failure(err: &SearchError) -> Response {
    match err {
        SearchError::TooManySearches => {
            let mut response = error(StatusCode::TOO_MANY_REQUESTS, err.to_string());
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from_static("1"));
            response
        }
        SearchError::Invalid(_) => error(StatusCode::BAD_REQUEST, err.to_string()),
        _ => {
            tracing::warn!(error = %err, "a search failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// `value` as the gateway answers JSON: re-encoded in ASCII.
fn json_response(value: &Value) -> Response {
    (
        [(CONTENT_TYPE, HeaderValue::from_static(JSON))],
        checks::ascii_json(value),
    )
        .into_response()
}

/// A verified page, or offer, as the gateway serves it: its JSON,
/// re-encoded in ASCII.
fn page_response(page: &[u8]) -> Response {
    let value: Value = serde_json::from_slice(page).expect("a verified document is JSON");
    (
        [(CONTENT_TYPE, HeaderValue::from_static(JSON))],
        checks::ascii_json(&value),
    )
        .into_response()
}

/// The inventory sections a comma separated list names, in order and without
/// repeats; all of them when it names none; `None` when one is not an
/// inventory section.
fn inventory_sections(list: Option<&str>) -> Option<Vec<Section>> {
    let names: Vec<&str> = list
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        return Some(INVENTORY_SECTIONS.to_vec());
    }
    let mut sections = names
        .into_iter()
        .map(|name| {
            serde_json::from_value::<Section>(json!(name))
                .ok()
                .filter(|section| INVENTORY_SECTIONS.contains(section))
        })
        .collect::<Option<Vec<Section>>>()?;
    sections.sort_unstable();
    sections.dedup();
    Some(sections)
}

/// The name a section travels by.
fn section_name(section: Section) -> String {
    match serde_json::to_value(section) {
        Ok(Value::String(name)) => name,
        other => unreachable!("a section is named by a string, not {other:?}"),
    }
}

/// What a request for a page is kept under: `blake3:` and the hex of
/// BLAKE3 over the JCS of its sections and cursor.
fn inventory_request_key(sections: &[String], cursor: &str) -> String {
    let request = json!({"sections": sections, "cursor": cursor});
    format!(
        "blake3:{}",
        blake3::hash(&pals_core::jcs::canonical(&request)).to_hex()
    )
}

/// The answer to a discovery call to `peer` that got no page, or no offer.
fn discovery_failure(peer: &AgentId, failure: &DiscoveryError) -> Response {
    tracing::debug!(%peer, error = %failure, "no answer to use from the peer");
    match failure {
        DiscoveryError::Unreachable(_) | DiscoveryError::Timeout => {
            error(StatusCode::BAD_GATEWAY, "peer unreachable")
        }
        DiscoveryError::TooLarge | DiscoveryError::Protocol(_) => {
            error(StatusCode::BAD_GATEWAY, "peer protocol error")
        }
        DiscoveryError::BadRecord(_) => error(StatusCode::BAD_GATEWAY, "peer record invalid"),
        DiscoveryError::Regression { .. } => {
            error(StatusCode::BAD_GATEWAY, "peer record regression")
        }
        DiscoveryError::Offer(refused) => error(
            StatusCode::BAD_GATEWAY,
            format!("peer offer invalid: {refused}"),
        ),
        DiscoveryError::Report(why) => error(
            StatusCode::BAD_GATEWAY,
            format!("peer benchmark report invalid: {why}"),
        ),
        DiscoveryError::Remote(refusal) => {
            // The peer's words are data: printable ASCII only, and short.
            let said = crate::printable_ascii(&refusal.message, MAX_PEER_MESSAGE_BYTES);
            let message = format!("peer refused: {said}");
            match refusal.code() {
                ErrorCode::BadRequest | ErrorCode::InvalidCursor => {
                    error(StatusCode::BAD_REQUEST, message)
                }
                ErrorCode::RateLimited => {
                    let seconds = refusal.retry_after_ms.div_ceil(1000).max(1);
                    let mut response = error(StatusCode::TOO_MANY_REQUESTS, message);
                    response
                        .headers_mut()
                        .insert(RETRY_AFTER, HeaderValue::from(seconds));
                    response
                }
                _ => error(StatusCode::BAD_GATEWAY, message),
            }
        }
    }
}

/// Whether `If-None-Match` in `headers` names `etag`, or is `*`.
fn if_none_match(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let etag = etag.to_str().unwrap_or_default();
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|tag| tag.trim().trim_start_matches("W/"))
        .any(|tag| tag == "*" || tag == etag)
}

/// `POST /peers/{peer}/jsonrpc`, or `/jsonrpc/`: both go to the peer's
/// `/jsonrpc`, where its JSON-RPC binding is served.
async fn forward_jsonrpc(
    State(state): State<GatewayState>,
    Path(peer): Path<String>,
    mut request: Request,
) -> Response {
    let uri = request.uri();
    if let Some(path) = uri.path().strip_suffix('/') {
        let target = match uri.query() {
            Some(query) => format!("{path}?{query}"),
            None => path.to_owned(),
        };
        match target.parse() {
            Ok(target) => *request.uri_mut() = target,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid request target"),
        }
    }
    forward(state, peer, request, Binding::JsonRpc).await
}

async fn forward_rest(
    State(state): State<GatewayState>,
    Path((peer, _rest)): Path<(String, String)>,
    request: Request,
) -> Response {
    forward(state, peer, request, Binding::Rest).await
}

/// Forwards `request`, for `/peers/<peer>/<rest>`, to `/<rest>` of `peer`;
/// to a plain contact as [`plain::forward`] says.
async fn forward(
    state: GatewayState,
    peer: String,
    request: Request,
    binding: Binding,
) -> Response {
    let Some(target_uri) = peer_target(request.uri()) else {
        return error(StatusCode::BAD_REQUEST, "invalid request target");
    };
    let target = match state.resolve(peer).await {
        Ok(Some(Resolved::Peer(target))) => target,
        Ok(Some(Resolved::Plain(contact))) => {
            return plain::forward(&state, contact, target_uri, request, binding).await;
        }
        Ok(None) => return unknown_peer(),
        Err(failure) => return failure.into_response(),
    };
    let (parts, body) = request.into_parts();
    let mut headers = outgoing_headers(&parts.headers);
    if !headers.contains_key(AUTHORIZATION) {
        match state.held_bearer(target.id).await {
            Ok(Some(bearer)) => {
                headers.insert(AUTHORIZATION, bearer);
            }
            Ok(None) => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    format!("no grant held for {}", target.id),
                );
            }
            Err(failure) => return failure.into_response(),
        }
    }
    let (body, request_json) = match state.outgoing_body(&target.id, &mut headers, body).await {
        Ok(outgoing) => outgoing,
        Err(refused) => return refused.into_response(),
    };
    // What the client expects, from its own call.
    let call = match binding {
        Binding::JsonRpc => Call::jsonrpc(request_json.as_ref()),
        Binding::Rest => {
            let path = target_uri.path();
            Call::rest(&parts.method, path.strip_prefix("/rest").unwrap_or(path))
        }
    };
    let mut outgoing = http::Request::builder()
        .method(parts.method)
        .uri(target_uri)
        .body(body)
        .expect("a method and URI taken from a valid request");
    *outgoing.headers_mut() = headers;

    match state.inner.peer_http.send(target.addr, outgoing).await {
        // A redirect would send the client, and the grant the gateway holds
        // for another peer, wherever this peer chose.
        Ok(response) if response.status().is_redirection() => {
            let failure =
                TunnelError::Protocol(format!("the peer redirected ({})", response.status()));
            tunnel_failure(&target.id, &failure)
        }
        Ok(response) => {
            let (mut parts, body) = response.into_parts();
            parts.headers = incoming_headers(&parts.headers);
            match state
                .incoming_body(
                    CardOwner::Peer(&target.id),
                    binding,
                    call,
                    parts.status,
                    &mut parts.headers,
                    body,
                )
                .await
            {
                Ok(body) => Response::from_parts(parts, body),
                Err(failure) => tunnel_failure(&target.id, &failure),
            }
        }
        Err(error) => tunnel_failure(&target.id, &error),
    }
}

impl GatewayState {
    /// The body of a client's request as it goes to `peer`: its blob URLs as
    /// `iroh-blob://` ones, the file parts that name a blob of this agent's
    /// declaring its hash and size, and those blobs shared with `peer`; and
    /// the request as JSON, when it is. `headers` get the new
    /// `Content-Length`.
    async fn outgoing_body(
        &self,
        peer: &AgentId,
        headers: &mut HeaderMap,
        body: Body,
    ) -> Result<(Body, Option<Value>), Refusal> {
        let limit = self.inner.limits.max_body_bytes;
        let bytes = match Limited::new(body, limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(err) if err.downcast_ref::<LengthLimitError>().is_some() => {
                return Err(Refusal::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("request body larger than {limit} bytes"),
                ));
            }
            Err(_) => {
                return Err(Refusal::new(
                    StatusCode::BAD_REQUEST,
                    "cannot read the request body",
                ));
            }
        };
        // Not JSON: nothing of it is a file part.
        let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok((Body::from(bytes), None));
        };
        let rewritten = rewrite::rewrite_to_wire(&mut value, &self.base_url());
        let declared = self.declare_own_blobs(&mut value, peer).await?;
        if !rewritten && !declared {
            return Ok((Body::from(bytes), Some(value)));
        }
        let bytes = serde_json::to_vec(&value).expect("a JSON value serializes");
        headers.insert(CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        Ok((Body::from(bytes), Some(value)))
    }

    /// Gives every file part of `value` that names a blob of this agent's
    /// that blob's hash and size, and shares those blobs with `peer`;
    /// whether there were any.
    async fn declare_own_blobs(&self, value: &mut Value, peer: &AgentId) -> Result<bool, Refusal> {
        let own = self.inner.id;
        let mut hashes = BTreeSet::new();
        rewrite::for_each_blob_part(value, &mut |_, holder, hash| {
            if holder == own {
                hashes.insert(hash);
            }
        });
        let mut declared = BTreeMap::new();
        for hash in hashes {
            let Some(size) = self.inner.blobs.complete_size(hash).await else {
                return Err(Refusal::new(
                    StatusCode::BAD_REQUEST,
                    format!("no blob {hash} to send"),
                ));
            };
            let blob = BlobRef {
                holder: own,
                hash,
                size,
            };
            if let Err(err) = self.inner.blobs.share(&blob, peer, BLOB_SHARE_TTL_S).await {
                tracing::warn!(error = %format!("{err:#}"), "cannot share a blob");
                return Err(Refusal::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error",
                ));
            }
            declared.insert(hash, blob);
        }
        rewrite::for_each_blob_part(value, &mut |part, holder, hash| {
            if holder == own
                && let Some(blob) = declared.get(&hash)
            {
                declare(part, blob);
            }
        });
        Ok(!declared.is_empty())
    }

    /// The body of the response of `owner`, a peer or a plain contact, of
    /// `status`, to `call` on `binding`, as it goes to the client: checked
    /// and re-encoded, as [`checks`] says. `headers` get the type and length
    /// of what goes, and, for a peer's extended card, [`CARD_HASH_HEADER`].
    async fn incoming_body(
        &self,
        owner: CardOwner<'_>,
        binding: Binding,
        call: Call,
        status: StatusCode,
        headers: &mut HeaderMap,
        body: Body,
    ) -> Result<Body, TunnelError> {
        if headers
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
        {
            return Err(TunnelError::Protocol("an encoded response".to_owned()));
        }
        let media_type = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if call == Call::Streaming && status.is_success() && media_type == EVENT_STREAM {
            headers.remove(CONTENT_LENGTH);
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(EVENT_STREAM));
            return Ok(checked_event_stream(body, self.base_url(), binding));
        }
        let card = call == Call::ExtendedCard;
        let limit = if card {
            checks::MAX_CARD_ANSWER_BYTES
        } else {
            MAX_REWRITE_BYTES
        };
        let bytes = Limited::new(body, limit)
            .collect()
            .await
            .map_err(|err| TunnelError::Protocol(format!("unreadable response: {err}")))?
            .to_bytes();
        let mut value = match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(_)
                if status.is_success() && (card || !bytes.iter().all(u8::is_ascii_whitespace)) =>
            {
                return Err(TunnelError::Protocol(
                    "a successful response that is not JSON".to_owned(),
                ));
            }
            // An error a client takes no task id from, or nothing.
            Err(_) => return Ok(Body::from(bytes)),
        };
        let card_hash = if card && status.is_success() {
            checks::extended_card_view(&mut value, binding, owner, &self.base_url())
                .map_err(TunnelError::Protocol)?
        } else {
            None
        };
        if let Some(id) = checks::unsafe_id(&value, binding) {
            return Err(TunnelError::Protocol(format!("an unsafe id {id}")));
        }
        rewrite::rewrite_to_gateway(&mut value, &self.base_url());
        let bytes = checks::ascii_json(&value);
        let json_type = if media_type == A2A_JSON {
            A2A_JSON
        } else {
            JSON
        };
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(json_type));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        if let Some(hash) = card_hash {
            let hash = HeaderValue::from_str(hash.as_str()).expect("a card hash is header safe");
            headers.insert(CARD_HASH_HEADER, hash);
        }
        Ok(Body::from(bytes))
    }
}

/// Sets the hash and size of `blob` as the metadata of `part`, a file part.
fn declare(part: &mut Map<String, Value>, blob: &BlobRef) {
    let metadata = part
        .entry("metadata")
        .or_insert_with(|| Value::Object(Map::new()));
    if !metadata.is_object() {
        *metadata = Value::Object(Map::new());
    }
    if let Value::Object(metadata) = metadata {
        metadata.insert(BLOB_META_KEY.to_owned(), blob.meta());
    }
}

/// `body`, an event stream on `binding`, sent on event by event as each is
/// complete, [checked and rebuilt](checks::check_event): the stream ends,
/// broken, at the first event refused, before it, or once the event being
/// received grows past [`MAX_REWRITE_BYTES`] before its empty line. An
/// event the peer's stream ends in the middle of, before its empty line,
/// is no event: SSE discards it, and so does the gateway, ending the stream
/// as the peer did.
fn checked_event_stream(body: Body, base_url: String, binding: Binding) -> Body {
    struct Events {
        body: BodyDataStream,
        framer: EventFramer,
        /// Whether the peer's stream has ended.
        input_ended: bool,
        /// Whether this stream has ended, broken or not.
        done: bool,
        base_url: String,
        binding: Binding,
    }
    impl Events {
        /// `event`, checked and rebuilt, or the end of this stream.
        async fn send(self, event: &[u8]) -> Option<(io::Result<Bytes>, Self)> {
            match checks::check_event(event, &self.base_url, self.binding) {
                Ok(checked) => Some((Ok(checked), self)),
                Err(why) => self.fail(why).await,
            }
        }

        /// Ends this stream, broken, because of `why`, once what it sent
        /// before has had its chance to be flushed: the server sends what it
        /// holds when the body is not ready, and drops it on an error.
        async fn fail(mut self, why: String) -> Option<(io::Result<Bytes>, Self)> {
            tracing::debug!(%why, "a peer's event stream was cut");
            self.done = true;
            self.framer = EventFramer::new();
            tokio::task::yield_now().await;
            Some((Err(io::Error::other(why)), self))
        }
    }
    let events = Events {
        body: body.into_data_stream(),
        framer: EventFramer::new(),
        input_ended: false,
        done: false,
        base_url,
        binding,
    };
    let stream = futures::stream::unfold(events, |mut events| async move {
        loop {
            if events.done {
                return None;
            }
            if let Some(event) = events.framer.next_event(events.input_ended) {
                return events.send(&event).await;
            }
            if events.input_ended {
                // Whatever is left is cut off: dropped, unchecked, unsent.
                if let Some(cut) = events.framer.rest() {
                    tracing::debug!(bytes = cut.len(), "dropped an event cut off by the peer");
                }
                return None;
            }
            if events.framer.held() > MAX_REWRITE_BYTES {
                return events
                    .fail(format!("an event larger than {MAX_REWRITE_BYTES} bytes"))
                    .await;
            }
            match events.body.next().await {
                Some(Ok(chunk)) => events.framer.push(&chunk),
                Some(Err(err)) => return events.fail(format!("the stream broke: {err}")).await,
                None => events.input_ended = true,
            }
        }
    });
    Body::from_stream(stream)
}

/// Stores the request's body as a blob, up to `limits.max_blob_bytes`, as it
/// arrives: `201 {"url", "hash", "size"}`, its URL being the gateway's.
async fn upload_blob(State(state): State<GatewayState>, request: Request) -> Response {
    let limit = state.inner.limits.max_blob_bytes;
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit) {
        return too_large_blob(limit);
    }
    let too_large = Arc::new(AtomicBool::new(false));
    let unreadable = Arc::new(AtomicBool::new(false));
    let (over, broken) = (Arc::clone(&too_large), Arc::clone(&unreadable));
    let mut received: u64 = 0;
    let body = request.into_body().into_data_stream().map(move |chunk| {
        let chunk = chunk.map_err(|err| {
            broken.store(true, Ordering::SeqCst);
            io::Error::other(err)
        })?;
        received = received.saturating_add(chunk.len() as u64);
        if received > limit {
            over.store(true, Ordering::SeqCst);
            return Err(io::Error::other("blob too large"));
        }
        Ok(chunk)
    });
    match state.inner.blobs.add_stream(body).await {
        Ok(blob) => {
            let url = rewrite::gateway_url(&state.base_url(), &blob.holder, &blob.hash);
            let created = json!({"url": url, "hash": blob.hash.to_hex(), "size": blob.size});
            (StatusCode::CREATED, Json(created)).into_response()
        }
        Err(_) if too_large.load(Ordering::SeqCst) => too_large_blob(limit),
        Err(_) if unreadable.load(Ordering::SeqCst) => {
            error(StatusCode::BAD_REQUEST, "cannot read the request body")
        }
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "cannot store an uploaded blob");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

fn too_large_blob(limit: u64) -> Response {
    error(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!("blob larger than {limit} bytes"),
    )
}

/// Serves blob `hash`, fetching it from `holder` first when the store lacks
/// it.
async fn download_blob(
    State(state): State<GatewayState>,
    Path((holder, hash)): Path<(String, String)>,
) -> Response {
    let (Ok(holder), Some(hash)) = (holder.parse::<AgentId>(), parse_hash(&hash)) else {
        return error(StatusCode::NOT_FOUND, "not a blob URL");
    };
    match state.obtain_blob(holder, hash).await {
        Ok(blob) => serve_blob(&state.inner.blobs, blob),
        Err(refusal) => refusal.into_response(),
    }
}

/// `200` with the bytes of `blob`, streamed from the store, which keeps it
/// until the last of them is sent.
fn serve_blob(blobs: &BlobService, blob: HeldBlob) -> Response {
    let HeldBlob { hash, size, held } = blob;
    let content = blobs.read(hash, size).map(move |chunk| {
        let _held = &held;
        chunk
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, OCTET_STREAM)
        .header(CONTENT_LENGTH, size)
        .body(Body::from_stream(content))
        .expect("a valid response")
}

/// The path and query that a request for `/peers/<peer>/<rest>` is forwarded
/// with: `/<rest>` and the query, both exactly as received, still
/// percent-encoded.
fn peer_target(uri: &Uri) -> Option<Uri> {
    let after_peers = uri.path().strip_prefix("/peers/")?;
    let rest = &after_peers[after_peers.find('/')?..];
    let target = match uri.query() {
        Some(query) => format!("{rest}?{query}"),
        None => rest.to_owned(),
    };
    target.parse().ok()
}

/// The headers of a client's request that go on to the peer: the
/// [end-to-end](end_to_end) ones, but for the `x-agentnet-*` headers that
/// only the peer's tunnel and authentication set.
fn outgoing_headers(incoming: &HeaderMap) -> HeaderMap {
    let mut headers = end_to_end(incoming);
    headers.retain_names(|name| !name.as_str().starts_with(OWN_HEADER_PREFIX));
    headers
}

/// The headers of a peer's response that go on to the client: the
/// [end-to-end](end_to_end) ones, but for the cookies it sets, which would
/// apply to every peer, and for the `x-agentnet-*` headers, which only the
/// gateway sets.
fn incoming_headers(peer: &HeaderMap) -> HeaderMap {
    let mut headers = end_to_end(peer);
    headers.retain_names(|name| {
        !PEER_COOKIES.contains(&name.as_str()) && !name.as_str().starts_with(OWN_HEADER_PREFIX)
    });
    headers
}

/// `headers` without the hop-by-hop ones: [`HOP_BY_HOP`], those that
/// `Connection` names, and `Host`.
fn end_to_end(headers: &HeaderMap) -> HeaderMap {
    let named_by_connection: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    let mut kept = headers.clone();
    kept.retain_names(|name| {
        name != HOST && !HOP_BY_HOP.contains(&name.as_str()) && !named_by_connection.contains(name)
    });
    kept
}

/// `HeaderMap::retain` for whole names: drops every value of the names
/// `keep` refuses.
trait RetainNames {
    fn retain_names(&mut self, keep: impl FnMut(&HeaderName) -> bool);
}

impl RetainNames for HeaderMap {
    fn retain_names(&mut self, mut keep: impl FnMut(&HeaderName) -> bool) {
        let dropped: Vec<HeaderName> = self.keys().filter(|name| !keep(name)).cloned().collect();
        for name in dropped {
            self.remove(name);
        }
    }
}

/// The answer to a card `peer` did not give: [`tunnel_failure`]'s, but for
/// `502 {"error":"peer card invalid: <reason>"}` when what it gave is not a
/// card it signed.
fn card_failure(peer: &AgentId, failure: &CardFetchError) -> Response {
    tracing::debug!(%peer, error = %failure, "no valid card from the peer");
    match failure {
        CardFetchError::Unreachable(_) | CardFetchError::Timeout => {
            error(StatusCode::BAD_GATEWAY, "peer unreachable")
        }
        CardFetchError::Refused(_) | CardFetchError::Protocol(_) => {
            error(StatusCode::BAD_GATEWAY, "peer protocol error")
        }
        CardFetchError::Invalid(reason) => error(
            StatusCode::BAD_GATEWAY,
            format!("peer card invalid: {reason}"),
        ),
    }
}

/// The answer to a request the tunnel could not carry to `peer`.
fn tunnel_failure(peer: &AgentId, failure: &TunnelError) -> Response {
    tracing::debug!(%peer, error = %failure, "a gateway request did not reach the peer");
    match failure {
        TunnelError::Unreachable(_) | TunnelError::Timeout => {
            error(StatusCode::BAD_GATEWAY, "peer unreachable")
        }
        TunnelError::Protocol(_) => error(StatusCode::BAD_GATEWAY, "peer protocol error"),
    }
}

fn unknown_peer() -> Response {
    error(StatusCode::NOT_FOUND, "unknown peer")
}

/// `status` with `{"error": message}`.
fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// A request the gateway answers itself: `status` with `{"error": message}`.
pub(crate) struct Refusal(StatusCode, String);

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.1, self.0)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        error(self.0, self.1)
    }
}

/// The store could not be read; the gateway answers `500`.
pub(crate) struct StoreUnavailable;

impl IntoResponse for StoreUnavailable {
    fn into_response(self) -> Response {
        error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

/// Runs a store call off the async threads, logging a failure.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, StoreUnavailable> {
    let failed = |message: String| {
        tracing::warn!(error = %message, "the gateway cannot read the store");
        StoreUnavailable
    };
    match tokio::task::spawn_blocking(call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(failed(format!("{err:#}"))),
        Err(err) => Err(failed(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(uri: &str) -> Option<String> {
        peer_target(&uri.parse().unwrap()).map(|uri| uri.to_string())
    }

    #[test]
    fn target_keeps_the_rest_of_the_path_and_the_query_as_sent() {
        assert_eq!(target("/peers/bob/jsonrpc").as_deref(), Some("/jsonrpc"));
        assert_eq!(
            target("/peers/bob/rest/message:send").as_deref(),
            Some("/rest/message:send")
        );
        assert_eq!(
            target("/peers/b%2Fo/rest/tasks/a%2Fb%3Acancel?historyLength=0&x=%20").as_deref(),
            Some("/rest/tasks/a%2Fb%3Acancel?historyLength=0&x=%20")
        );
        assert_eq!(
            target("/peers/bob/rest/tasks/t1?").as_deref(),
            Some("/rest/tasks/t1?")
        );
        assert_eq!(target("/peers/bob"), None);
        assert_eq!(target("/healthz"), None);
    }

    #[test]
    fn only_loopback_names_with_the_port_are_local() {
        let local = |hosts: &[&str], uri: &str| {
            let mut headers = HeaderMap::new();
            for host in hosts {
                headers.append(HOST, HeaderValue::from_str(host).unwrap());
            }
            is_local_host(&headers, &uri.parse().unwrap(), 7420)
        };
        for host in [
            "127.0.0.1:7420",
            "localhost:7420",
            "LocalHost:7420",
            "[::1]:7420",
        ] {
            assert!(local(&[host], "/healthz"), "{host}");
        }
        for host in [
            "evil.example:7420",
            "127.0.0.1:7421",
            "127.0.0.1",
            "localhost",
            "127.0.0.2:7420",
            "localhost.evil.example:7420",
            "",
        ] {
            assert!(!local(&[host], "/healthz"), "{host}");
        }
        // Two Host headers, or none and no authority, are refused.
        assert!(!local(&["127.0.0.1:7420", "127.0.0.1:7420"], "/healthz"));
        assert!(!local(&[], "/healthz"));
        // An authority in the request target must be local too.
        assert!(local(&[], "http://127.0.0.1:7420/healthz"));
        assert!(!local(
            &["127.0.0.1:7420"],
            "http://evil.example:7420/healthz"
        ));
    }

    #[test]
    fn end_to_end_drops_hop_by_hop_and_connection_named_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "127.0.0.1:7420"),
            ("connection", "keep-alive, x-private"),
            ("x-private", "1"),
            ("keep-alive", "timeout=5"),
            ("transfer-encoding", "chunked"),
            ("te", "trailers"),
            ("upgrade", "h2c"),
            ("authorization", "Bearer t"),
            ("a2a-version", "1.0"),
            ("content-type", "application/json"),
            ("accept", "text/event-stream"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        let kept = end_to_end(&headers);
        let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["a2a-version", "accept", "authorization", "content-type"]
        );
    }

    #[test]
    fn a_peer_out_of_reach_or_broken_exchange_is_a_bad_gateway() {
        let peer = pals_core::identity::AgentKey::generate().id();
        for (failure, message) in [
            (
                TunnelError::Unreachable("gone".to_owned()),
                "peer unreachable",
            ),
            (TunnelError::Timeout, "peer unreachable"),
            (
                TunnelError::Protocol("garbled".to_owned()),
                "peer protocol error",
            ),
        ] {
            let response = tunnel_failure(&peer, &failure);
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let body = futures::executor::block_on(response.into_body().collect())
                .unwrap()
                .to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body, json!({ "error": message }));
        }
    }

    #[test]
    fn incoming_headers_drop_the_peers_cookies() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("set-cookie", "a=1"),
            ("set-cookie", "b=2"),
            ("set-cookie2", "c=3"),
            ("connection", "close"),
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        let kept = incoming_headers(&headers);
        let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["cache-control", "content-type"]);
    }

    #[test]
    fn incoming_headers_drop_the_peers_own_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-agentnet-card-hash", "blake3:forged"),
            ("x-agentnet-peer", "someone"),
            ("content-type", "application/json"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        let kept = incoming_headers(&headers);
        let names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        assert_eq!(names, ["content-type"]);
    }

    #[test]
    fn only_grants_for_a_task_operation_on_skill_ask_serve_tasks() {
        let issuer = pals_core::identity::AgentKey::generate().id();
        let grant = |operations: Vec<Operation>, resource: &str| Grant {
            schema: pals_core::grant::GRANT_SCHEMA.to_owned(),
            grant_id: "g".to_owned(),
            issuer,
            subject: issuer,
            audience: issuer,
            project: "demo".to_owned(),
            resources: vec![resource.to_owned()],
            operations,
            issued_at: "2026-11-14T00:00:00Z".to_owned(),
            expires_at: "2026-11-15T00:00:00Z".to_owned(),
        };
        for op in [
            Operation::TaskSend,
            Operation::TaskRead,
            Operation::TaskCancel,
        ] {
            assert!(serves_tasks(&grant(vec![op], "skill:ask")), "{op:?}");
        }
        assert!(serves_tasks(&grant(
            vec![Operation::BlobFetch, Operation::TaskRead],
            "skill:*"
        )));
        assert!(!serves_tasks(&grant(
            vec![Operation::BlobFetch],
            "skill:ask"
        )));
        assert!(!serves_tasks(&grant(vec![Operation::BlobFetch], "blob:*")));
        assert!(!serves_tasks(&grant(vec![Operation::TaskSend], "blob:*")));
        assert!(!serves_tasks(&grant(
            vec![Operation::TaskSend],
            "skill:other"
        )));
    }

    /// A blob URL between peers, and the same blob at the gateway `base`.
    fn blob_urls(base: &str) -> (String, String) {
        let holder = pals_core::identity::AgentKey::generate().id();
        let hash = Hash::new(b"a file");
        (
            crate::blobs::wire_url(&holder, &hash),
            rewrite::gateway_url(base, &holder, &hash),
        )
    }

    #[test]
    fn events_go_on_with_their_blob_urls_the_gateways() {
        let base = "http://127.0.0.1:7420";
        let (wire, local) = blob_urls(base);
        let event = |url: &str| json!({"result": {"artifact": {"parts": [{"url": url}]}}});
        let sent = format!("event: message\r\nid: 7\r\ndata:{}\r\n\r\n", event(&wire));
        let checked = checks::check_event(sent.as_bytes(), base, Binding::JsonRpc).unwrap();
        assert_eq!(
            String::from_utf8(checked.to_vec()).unwrap(),
            format!("data: {}\n\n", event(&local))
        );
        // A text that mentions a blob URL is no file part: it stays.
        let text = json!({"result": {"text": wire}});
        let checked = checks::check_event(
            format!("data: {text}\n\n").as_bytes(),
            base,
            Binding::JsonRpc,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(checked.to_vec()).unwrap(),
            format!("data: {text}\n\n")
        );
    }

    /// What `checked_event_stream` sends of `chunks`: the bytes, and whether
    /// it ended unbroken.
    async fn stream_through(chunks: Vec<Vec<u8>>) -> (Vec<u8>, bool) {
        let chunks: Vec<Result<Bytes, io::Error>> = chunks
            .into_iter()
            .map(|chunk| Ok(Bytes::from(chunk)))
            .collect();
        let body = Body::from_stream(futures::stream::iter(chunks));
        let mut stream =
            checked_event_stream(body, "http://127.0.0.1:7420".to_owned(), Binding::Rest)
                .into_data_stream();
        let mut received = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => received.extend_from_slice(&chunk),
                Err(_) => return (received, false),
            }
        }
        (received, true)
    }

    #[tokio::test]
    async fn event_streams_are_rewritten_across_chunks_as_they_come() {
        let base = "http://127.0.0.1:7420";
        let (wire, local) = blob_urls(base);
        let event = |url: &str| format!("data: {}\n\n", json!({"parts": [{"url": url}]}));
        let sent = format!("{}{}", event(&wire), event("https://example.com/x"));
        // Cut in chunks of 7 bytes, splitting lines and the URL.
        let chunks = sent.as_bytes().chunks(7).map(<[u8]>::to_vec).collect();
        let (received, unbroken) = stream_through(chunks).await;
        assert!(unbroken);
        let expected = format!("{}{}", event(&local), event("https://example.com/x"));
        assert_eq!(String::from_utf8(received).unwrap(), expected);

        // A last event cut off before its empty line is no event: SSE
        // discards it, and so does the gateway, safe or not, ending the
        // stream as the peer did.
        for (sent, kept) in [
            (format!("data: {}", json!({"url": wire})), String::new()),
            ("data: {\"taskId\": \"../x\"}".to_owned(), String::new()),
            (
                format!(
                    "{}data: {}\n",
                    event("https://example.com/x"),
                    json!({"n": 1})
                ),
                event("https://example.com/x"),
            ),
        ] {
            let (received, unbroken) = stream_through(vec![sent.clone().into_bytes()]).await;
            assert!(unbroken, "{sent}");
            assert_eq!(String::from_utf8(received).unwrap(), kept, "{sent}");
        }
    }

    #[tokio::test]
    async fn a_stream_ends_before_the_first_event_refused() {
        let task = |id: &str| json!({"task": {"id": id, "contextId": "c", "status": {}}});
        let safe = format!("data: {}\n\n", task("t-1"));
        let unsafe_event = json!({"statusUpdate": {"taskId": "../x", "contextId": "c"}});
        let after = format!("data: {}\n\n", task("t-2"));
        // Lines ended with LF, CRLF, or CR alone, as SSE allows.
        for ending in ["\n", "\r\n", "\r"] {
            let lines = |text: &str| text.replace('\n', ending);
            let sent = format!(
                "{}{}{}",
                lines(&safe),
                lines(&format!("data: {unsafe_event}\n\n")),
                lines(&after)
            );
            // Cut in chunks of 5 bytes, splitting lines and line ends.
            let chunks = sent.as_bytes().chunks(5).map(<[u8]>::to_vec).collect();
            let (received, unbroken) = stream_through(chunks).await;
            assert!(!unbroken, "{ending:?}");
            assert_eq!(String::from_utf8(received).unwrap(), safe, "{ending:?}");
        }
        // A byte order mark at the start does not hide the first event.
        let sent = format!("\u{feff}data: {unsafe_event}\n\n");
        let (received, unbroken) = stream_through(vec![sent.into_bytes()]).await;
        assert!(!unbroken);
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn an_event_that_never_ends_ends_the_stream() {
        let line = b"data: x\n".repeat(64 * 1024);
        let chunks = vec![line; MAX_REWRITE_BYTES / (8 * 64 * 1024) + 2];
        let (received, unbroken) = stream_through(chunks).await;
        assert!(!unbroken);
        assert!(received.is_empty());
    }

    #[test]
    fn declaring_a_blob_keeps_the_rest_of_the_metadata() {
        let blob = BlobRef {
            holder: pals_core::identity::AgentKey::generate().id(),
            hash: Hash::new(b"a file"),
            size: 6,
        };
        let mut part = json!({"url": blob.to_url(), "metadata": {"other": 1}});
        declare(part.as_object_mut().unwrap(), &blob);
        assert_eq!(part["metadata"]["other"], 1);
        assert_eq!(part["metadata"][BLOB_META_KEY], blob.meta());
        for metadata in [json!(null), json!("x"), json!([1])] {
            let mut part = json!({"url": blob.to_url(), "metadata": metadata});
            declare(part.as_object_mut().unwrap(), &blob);
            assert_eq!(part["metadata"], json!({ BLOB_META_KEY: blob.meta() }));
        }
        let mut part = json!({"url": blob.to_url()});
        declare(part.as_object_mut().unwrap(), &blob);
        assert_eq!(part["metadata"], json!({ BLOB_META_KEY: blob.meta() }));
    }

    #[test]
    fn outgoing_headers_drop_pals_own_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-agentnet-peer", "a forged peer"),
            ("x-agentnet-project", "a forged project"),
            ("x-agentnet-grant-id", "g-forged"),
            ("x-agentnet-peer", "another"),
            ("x-other", "kept"),
            ("authorization", "Bearer t"),
            ("connection", "close"),
        ] {
            headers.append(name, HeaderValue::from_static(value));
        }
        let kept = outgoing_headers(&headers);
        let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["authorization", "x-other"]);
        assert_eq!(kept["authorization"], "Bearer t");
    }
}
