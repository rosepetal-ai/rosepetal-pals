//! Plain A2A contacts (H2-D3): agents that speak A2A v1.0 over HTTP with no
//! PALS daemon behind them, reached at the URL the operator saved, in the
//! outbound direction only. This agent's gateway and outbox call them; they
//! never reach this agent. A plain agent is at least as untrusted as a PALS
//! peer.
//!
//! **The contact** ([`new_contact`]): a name and a URL. The URL is
//! `https://<host>[:<port>][/<path>]`, the base of the agent, whose card is
//! at `<base>/.well-known/agent-card.json`, or its card's own URL (a path
//! ending in `.json`); `http://` only to `127.0.0.1`, `[::1]` or
//! `localhost`, for local agents; no credentials, query or fragment in it.
//! Its origin (scheme, host and port) is the agent's identity, and the only
//! origin ever called; it is never this agent's own gateway, a loopback
//! host on the gateway's port ([`OWN_GATEWAY`]), when the contact is saved
//! or used ([`fetch_contact_card`]). Credentials are none, or a bearer token read from
//! the environment variable the contact names when a request is sent; the
//! token is never kept. Files never go to a plain agent: they travel as
//! blobs, between PALS peers only.
//!
//! **The card** ([`fetch_plain_card`]) is read when the contact is saved and
//! at every use, with [`http_client`]: no proxy (the `*_PROXY` variables do
//! not count), no redirect followed (a `3xx` is a refusal), the platform's
//! certificate verifier, [`CONNECT_TIMEOUT`] to connect, [`CARD_TIMEOUT`] in
//! all, `A2A-Version: 1.0`, `Accept-Encoding: identity`, read up to
//! [`MAX_CARD_BYTES`] before it is parsed, and then validated
//! ([`validate_card`]). Of its interfaces, only
//! the `JSONRPC` and `HTTP+JSON` ones with protocol version `1.0`, no
//! tenant and the contact's origin are used, the first of each binding; the
//! others are ignored, so that no card ever sends the gateway to another
//! origin, such as the gateway itself or the local network. A card with
//! none is refused: `no A2A 1.0 interface at <origin>`.
//!
//! **Its signature** is reported, never required ([`SignatureStatus`]). A
//! signature counts when its protected header says `alg` `EdDSA`, no
//! `crit`, a `kid` and a `jku` of the contact's origin (so `https://`, or
//! `http://` for a loopback contact): the JWK Set there is read with the
//! card's rules, up to [`MAX_JWKS_BYTES`], and the key of that `kid` must be
//! an OKP Ed25519 one; then the card is [`Verified`](SignatureStatus::Verified)
//! when the signature verifies ([`verify_card_with_key`]), and the card used
//! is the one it covers. Any other key or algorithm (ES256, RS256...), a
//! `jku` elsewhere, a JWK Set that cannot be read, or a signature that does
//! not verify make it [`Unverifiable`](SignatureStatus::Unverifiable); none
//! at all, [`Unsigned`](SignatureStatus::Unsigned). Only a `Verified` plain
//! contact may be chosen automatically; any of them can be asked by name.
//! An absent `securityRequirements` says nothing of whether the agent needs
//! credentials: protobuf forms of a card drop empty requirements.
//!
//! **Through the gateway** (`forward`), `/peers/<name>/...` reaches the
//! contact `<name>`:
//!
//! - its card as [`plain_view`]: [`gateway_view_for`] with only the
//!   bindings it has, and no security;
//! - `jsonrpc` goes to its JSON-RPC URL, and `rest/{*rest}` to its
//!   HTTP+JSON URL followed by `/<rest>` and the query; a binding it lacks
//!   is `404`;
//! - the gateway's own answers name their reason: on HTTP+JSON as
//!   `{"error": <reason>}`, as for a peer; on JSON-RPC, whose pinned client
//!   reads no HTTP status, as a JSON-RPC error of the same HTTP status, of
//!   code [`REFUSED_CODE`] for a refusal (a `4xx` but `408` and `429`, as
//!   the outbox reads HTTP+JSON) and `InternalError` otherwise
//!   (`plain_answer`);
//! - the body is read whole up to `limits.max_body_bytes` (beyond, `413`),
//!   and one with a part that names a blob, of this gateway or between
//!   peers, gets `400 files go only to PALS peers`;
//! - the client's `Accept-Encoding` is replaced by `identity`, since the
//!   gateway refuses an encoded answer;
//! - the client's `Authorization` passes intact; without one, the contact's
//!   bearer token, read from its variable then (`401 bearer env var not
//!   set` when the variable is not set); no grant is ever sent; no
//!   `x-agentnet-*` header of the client's goes on, as to a peer;
//! - the card is read and checked before the call, which is sent only to
//!   the contact's origin; when the card cannot be read for want of the
//!   agent (no connection, no card in time), or the connection for the
//!   call cannot be made, nothing of the call was sent, and the answer is
//!   the `502` [`PLAIN_NOT_SENT`] ([`not_sent_message`]): `{"error": …}` on
//!   HTTP+JSON, the JSON-RPC error [`NOT_SENT_CODE`] on JSON-RPC. When the
//!   request carried a nonce ([`NOT_SENT_NONCE_HEADER`]), the refusal says
//!   it back, and it is the only answer that ever does: the agent never
//!   sees the nonce, so no answer of its own can hold it. To an agent that
//!   may run every copy, the outbox sends a question again on that nonce
//!   (or when it could not reach the gateway), never on the refusal's
//!   text;
//! - the answer goes through the same checks as a PALS peer's (`checks.rs`):
//!   unary or streaming from the client's request, bounded, task ids safe,
//!   re-encoded in ASCII, a redirect a `502`, cookies and `x-agentnet-*`
//!   headers dropped. And none of its bytes reach the client unchecked: an
//!   error body that is no JSON (invalid UTF-8 included) is replaced by the
//!   gateway's own (`replaced_body`), which names the status only, and so
//!   is any error with a string that says the text of [`PLAIN_NOT_SENT`] or
//!   that carries [`NOT_SENT_CODE`] (`forges_not_sent`); such a `2xx`
//!   answer is a protocol error, and such an event ends the stream. That
//!   keeps the agent from saying the gateway's words in one string, or its
//!   code; a client may still join several of the agent's strings into
//!   that text, which is why nothing is decided on it. Its `iroh-blob://`
//!   URLs stay as sent: a plain agent holds no PALS blob, and no blob is
//!   fetched on its word.
//!   An extended card answered is validated and served as its view,
//!   unsigned.

use std::sync::Arc;
use std::time::Duration;

use a2a::{AgentCard, TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC};
use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::VerifyingKey;
use http::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH};
use http::{HeaderValue, StatusCode, Uri};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use pals_core::card_jws::{
    CARD_JWS_ALG, CardError, CardHash, MAX_CARD_BYTES, MAX_CARD_SIGNATURES, card_hash,
    validate_card, verify_card_with_key,
};
use pals_core::identity::AgentId;
use pals_core::store::{A2aContact, PeerRef, Store, is_plain_contact_name};
use reqwest::Url;
use serde_json::Value;

use super::checks::{Binding, Call, CardOwner};
use super::{GatewayState, Refusal, error, incoming_headers, outgoing_headers, rewrite};
use crate::a2a::gateway_view_for;
use crate::discovery::cards::PUBLIC_CONTEXT;

/// The largest JWK Set read, in bytes.
pub const MAX_JWKS_BYTES: usize = 16 * 1024;
/// How long connecting to a plain agent may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long reading a plain agent's card may take in all, its JWK Set
/// included.
pub const CARD_TIMEOUT: Duration = Duration::from_secs(20);
/// The A2A protocol version asked for.
const A2A_VERSION: &str = "1.0";
/// The only content coding asked of a plain agent: the gateway reads no
/// other.
const IDENTITY: &str = "identity";
/// The well-known path of a card, after a contact's base URL.
const WELL_KNOWN_CARD: &str = "/.well-known/agent-card.json";
/// What a plain contact's URL looks like.
const URL_RULE: &str = "a plain A2A agent's URL is https://<host>[/<path>]";
/// Why an `http://` URL to any other host is refused.
pub const ONLY_LOOPBACK: &str = "http:// is only for 127.0.0.1, [::1] and localhost";
/// The gateway's `502` when it could not reach a plain agent and sent it
/// nothing of the call: its card could not be read, or the connection to
/// its origin could not be made. On HTTP+JSON, `{"error": …}`; on
/// JSON-RPC, a JSON-RPC error of the code [`NOT_SENT_CODE`]; in both, with
/// the request's nonce, if any ([`not_sent_message`]).
pub const PLAIN_NOT_SENT: &str = "plain agent unreachable: nothing was sent to it";
/// The request header in which the outbox gives the gateway the nonce of a
/// question it sends: [`NONCE_HEX_LEN`] lowercase hex digits, 128 random
/// bits, new for every send. Like every `x-agentnet-*` header, it never
/// reaches an agent; the gateway says it back in its [`PLAIN_NOT_SENT`],
/// and nowhere else.
pub const NOT_SENT_NONCE_HEADER: &str = "x-agentnet-not-sent-nonce";
/// How many lowercase hex digits a nonce has: 128 bits.
pub const NONCE_HEX_LEN: usize = 32;
/// The JSON-RPC error code of [`PLAIN_NOT_SENT`] on a plain contact's
/// JSON-RPC route, which no agent's answer carries through the gateway
/// (`forges_not_sent`).
pub const NOT_SENT_CODE: i32 = -32090;
/// The JSON-RPC error code of a refusal (a `4xx` but `408` and `429`) on a
/// plain contact's JSON-RPC route, the gateway's own or an agent's answer
/// it replaced: whose client reads no HTTP status, so that it knows a
/// refusal from a failure on the way, which is `InternalError`.
pub const REFUSED_CODE: i32 = -32091;
/// Why a contact whose origin is this agent's own gateway is refused.
pub const OWN_GATEWAY: &str =
    "a plain contact cannot be this agent's own gateway: its peers are reached at /peers already";

/// What a plain agent's card signature is worth: pals-core's, so that
/// the selection of providers reads it too.
pub use pals_core::card_jws::SignatureStatus;

/// A plain agent's card, read and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct PlainCard {
    /// The card: what its signature covers when it is
    /// [`Verified`](SignatureStatus::Verified), else as received.
    pub card: AgentCard,
    /// The card as received.
    pub json: Value,
    /// [`card_hash`] of `json`.
    pub hash: CardHash,
    pub signature: SignatureStatus,
    /// Its JSON-RPC interface on the contact's origin, if any.
    pub jsonrpc_url: Option<String>,
    /// Its HTTP+JSON interface on the contact's origin, if any.
    pub rest_url: Option<String>,
}

/// Why a plain agent's card could not be had.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlainError {
    /// The name, the URL or the variable cannot be a plain contact's.
    #[error("{0}")]
    BadContact(String),
    /// No connection to the agent.
    #[error("the agent cannot be reached: {0}")]
    Unreachable(String),
    /// The agent answered this status, not a card: a redirect, an error.
    #[error("{}", refused(*.0))]
    Refused(u16),
    /// What it answered is not a valid card.
    #[error("peer card invalid: {0}")]
    Invalid(CardError),
    /// The card has no interface this agent would call.
    #[error("no A2A 1.0 interface at {0}")]
    NoInterface(String),
    /// The exchange broke.
    #[error("broken exchange with the agent: {0}")]
    Protocol(String),
    /// The card did not arrive within [`CARD_TIMEOUT`].
    #[error("the card did not arrive in time")]
    Timeout,
}

fn refused(status: u16) -> String {
    if (300..400).contains(&status) {
        format!("the agent answered {status}, a redirect, which is not followed")
    } else {
        format!("the agent answered {status} instead of its card")
    }
}

/// The HTTP client for plain agents: no proxy, whatever the environment
/// says; no redirect followed; no request sent again by the client itself,
/// so that a connect error on a call means the call never connected and
/// the gateway's not-sent refusal holds (reqwest otherwise re-sends after
/// some HTTP/2 refusals, which a hostile agent can send after it ran the
/// question); [`CONNECT_TIMEOUT`] to connect; rustls with the platform's
/// certificate verifier.
pub fn http_client() -> reqwest::Client {
    crate::install_crypto_provider();
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("an HTTP client with no proxy builds once a crypto provider is installed")
}

/// The plain contact `name` for the agent at `url`, its bearer token in the
/// environment variable `bearer_env`, if any, saved at `now_unix`; refused
/// when the name, the URL or the variable's name cannot be a plain
/// contact's.
pub fn new_contact(
    name: &str,
    url: &str,
    bearer_env: Option<&str>,
    now_unix: i64,
) -> Result<A2aContact, PlainError> {
    if name.parse::<AgentId>().is_ok() {
        return Err(PlainError::BadContact(
            "a contact name cannot be an agent id: peers are reachable by their id already"
                .to_owned(),
        ));
    }
    if !is_plain_contact_name(name) {
        return Err(PlainError::BadContact(format!(
            "{name:?} cannot be a plain contact's name: 1 to 64 letters, digits, '-', '_' or \
             '.', not starting with '.'"
        )));
    }
    if let Some(var) = bearer_env
        && !is_env_var_name(var)
    {
        return Err(PlainError::BadContact(format!(
            "{var:?} is not the name of an environment variable: letters, digits and '_', not \
             starting with a digit"
        )));
    }
    let (card_url, origin) = contact_url(url)?;
    Ok(A2aContact {
        name: name.to_owned(),
        card_url: card_url.to_string(),
        origin,
        bearer_env: bearer_env.map(str::to_owned),
        added_at: now_unix,
    })
}

/// Whether `name` is a portable environment variable name.
fn is_env_var_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The card URL and the origin of a plain contact given as `text`: the
/// card's own URL when its path ends in `.json`, else
/// `<text>/.well-known/agent-card.json`.
fn contact_url(text: &str) -> Result<(Url, String), PlainError> {
    let url = Url::parse(text.trim()).map_err(|_| bad_url(&format!("{text:?} is not a URL")))?;
    check_target(&url)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(bad_url("no query or fragment"));
    }
    let origin = url.origin().ascii_serialization();
    if url.path().ends_with(".json") {
        return Ok((url, origin));
    }
    let mut card_url = url.clone();
    card_url.set_path(&format!(
        "{}{WELL_KNOWN_CARD}",
        url.path().trim_end_matches('/')
    ));
    Ok((card_url, origin))
}

fn bad_url(why: &str) -> PlainError {
    PlainError::BadContact(format!("{URL_RULE}: {why}"))
}

/// Whether `url` may be called: `https://`, or `http://` to a loopback
/// name; with no credentials.
fn check_target(url: &Url) -> Result<(), PlainError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad_url("no credentials in it"));
    }
    match url.scheme() {
        "https" if url.host_str().is_some() => Ok(()),
        "http" if matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")) => Ok(()),
        "http" => Err(bad_url(ONLY_LOOPBACK)),
        other => Err(bad_url(&format!("not {other}://"))),
    }
}

/// Whether `origin`, a contact's, is the gateway listening on loopback at
/// `gateway_port`: a loopback host (`localhost`, `127.0.0.0/8` or `[::1]`)
/// on that port, of any scheme.
pub fn is_own_gateway(origin: &str, gateway_port: u16) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    loopback && url.port_or_known_default() == Some(gateway_port)
}

/// [`fetch_plain_card`] by an agent whose gateway listens on loopback at
/// `gateway_port`: refused as [`OWN_GATEWAY`], before anything is sent,
/// when the contact's origin is that gateway's ([`is_own_gateway`]), which
/// would make an alias of a PALS peer that rides its grant, or a loop.
pub async fn fetch_contact_card(
    http: &reqwest::Client,
    contact: &A2aContact,
    gateway_port: u16,
) -> Result<PlainCard, PlainError> {
    if is_own_gateway(&contact.origin, gateway_port) {
        return Err(PlainError::BadContact(OWN_GATEWAY.to_owned()));
    }
    fetch_plain_card(http, contact).await
}

/// The origin of `url` as a contact's is written.
fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// `contact`'s card, read with `http` and checked as the [module](self)
/// says: its URL checked again first, as when it was saved, and nothing
/// sent when it fails.
pub async fn fetch_plain_card(
    http: &reqwest::Client,
    contact: &A2aContact,
) -> Result<PlainCard, PlainError> {
    let (card_url, origin) = contact_url(&contact.card_url)?;
    if origin != contact.origin {
        return Err(PlainError::BadContact(format!(
            "the card URL {} is not on the contact's origin {}",
            contact.card_url, contact.origin
        )));
    }
    let fetch = async {
        let bytes = read_bounded(http, card_url, MAX_CARD_BYTES).await?;
        let json: Value =
            serde_json::from_slice(&bytes).map_err(|_| PlainError::Invalid(CardError::NotJson))?;
        let received = validate_card(&json).map_err(PlainError::Invalid)?;
        let (card, signature) = signature_of(http, &origin, &json, received).await;
        let (jsonrpc_url, rest_url) = own_interfaces(&card, &origin);
        if jsonrpc_url.is_none() && rest_url.is_none() {
            return Err(PlainError::NoInterface(origin.clone()));
        }
        Ok(PlainCard {
            card,
            hash: card_hash(&json),
            json,
            signature,
            jsonrpc_url,
            rest_url,
        })
    };
    tokio::time::timeout(CARD_TIMEOUT, fetch)
        .await
        .unwrap_or(Err(PlainError::Timeout))
}

/// The body of `GET url`, with `A2A-Version: 1.0`, read up to `limit`
/// bytes: a declared length beyond it is refused before reading, and the
/// reading stops as soon as it goes past.
async fn read_bounded(
    http: &reqwest::Client,
    url: Url,
    limit: usize,
) -> Result<Vec<u8>, PlainError> {
    let mut response = http
        .get(url)
        .header("a2a-version", A2A_VERSION)
        .header(ACCEPT, "application/json")
        .header(ACCEPT_ENCODING, IDENTITY)
        .send()
        .await
        .map_err(|err| PlainError::Unreachable(err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(PlainError::Refused(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(PlainError::Invalid(CardError::TooLarge));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| PlainError::Protocol(err.to_string()))?
    {
        if body.len() + chunk.len() > limit {
            return Err(PlainError::Invalid(CardError::TooLarge));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The card to use of `received`, the card validated from `json`, and what
/// its signature is worth, for a contact at `origin`.
async fn signature_of(
    http: &reqwest::Client,
    origin: &str,
    json: &Value,
    received: AgentCard,
) -> (AgentCard, SignatureStatus) {
    let signatures = received.signatures.clone().unwrap_or_default();
    if signatures.is_empty() {
        return (received, SignatureStatus::Unsigned);
    }
    // Each JWK Set read once.
    let mut sets: Vec<(Url, Option<Value>)> = Vec::new();
    for signature in signatures.iter().take(MAX_CARD_SIGNATURES) {
        let Some((kid, jku)) = eddsa_kid_and_jku(&signature.protected) else {
            continue;
        };
        let Ok(jku) = Url::parse(&jku) else {
            continue;
        };
        if origin_of(&jku) != origin || check_target(&jku).is_err() {
            continue;
        }
        let set = match sets.iter().find(|(url, _)| *url == jku) {
            Some((_, set)) => set.clone(),
            None => {
                let set = read_bounded(http, jku.clone(), MAX_JWKS_BYTES)
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                sets.push((jku, set.clone()));
                set
            }
        };
        let Some(key) = set.as_ref().and_then(|set| okp_key(set, &kid)) else {
            continue;
        };
        if let Ok(verified) = verify_card_with_key(json, &kid, &key) {
            return (verified.card, SignatureStatus::Verified { kid });
        }
    }
    (received, SignatureStatus::Unverifiable)
}

/// The `kid` and the `jku` of a protected header, `protected` in
/// base64url, that says `alg` `EdDSA` and has no `crit`.
fn eddsa_kid_and_jku(protected: &str) -> Option<(String, String)> {
    let header = URL_SAFE_NO_PAD.decode(protected).ok()?;
    let Value::Object(header) = serde_json::from_slice::<Value>(&header).ok()? else {
        return None;
    };
    if header.contains_key("crit") || header.get("alg")?.as_str()? != CARD_JWS_ALG {
        return None;
    }
    let kid = header.get("kid")?.as_str()?;
    let jku = header.get("jku")?.as_str()?;
    Some((kid.to_owned(), jku.to_owned()))
}

/// The OKP Ed25519 key `kid` of the JWK Set `set`: the first key with that
/// `kid`, which must be one, for signatures (`use` and `alg`, when given,
/// `sig` and `EdDSA` or `Ed25519`).
fn okp_key(set: &Value, kid: &str) -> Option<VerifyingKey> {
    let key = set
        .get("keys")?
        .as_array()?
        .iter()
        .find(|key| key.get("kid").and_then(Value::as_str) == Some(kid))?;
    let field = |name: &str| key.get(name).and_then(Value::as_str);
    if field("kty")? != "OKP" || field("crv")? != "Ed25519" {
        return None;
    }
    if key
        .get("use")
        .is_some_and(|value| value.as_str() != Some("sig"))
        || key
            .get("alg")
            .is_some_and(|value| !matches!(value.as_str(), Some("EdDSA" | "Ed25519")))
    {
        return None;
    }
    let x: [u8; 32] = URL_SAFE_NO_PAD.decode(field("x")?).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&x).ok()
}

/// The first `JSONRPC` and the first `HTTP+JSON` interface of `card` with
/// protocol version 1.0, no tenant, and `origin` as theirs.
fn own_interfaces(card: &AgentCard, origin: &str) -> (Option<String>, Option<String>) {
    let (mut jsonrpc, mut rest) = (None, None);
    for iface in &card.supported_interfaces {
        if iface.protocol_version != a2a::VERSION
            || iface
                .tenant
                .as_deref()
                .is_some_and(|tenant| !tenant.is_empty())
        {
            continue;
        }
        let Ok(url) = Url::parse(&iface.url) else {
            continue;
        };
        if origin_of(&url) != origin
            || check_target(&url).is_err()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            continue;
        }
        let slot = match iface.protocol_binding.as_str() {
            TRANSPORT_PROTOCOL_JSONRPC => &mut jsonrpc,
            TRANSPORT_PROTOCOL_HTTP_JSON => &mut rest,
            _ => continue,
        };
        slot.get_or_insert_with(|| url.to_string());
    }
    (jsonrpc, rest)
}

/// The card of the plain contact `name`, read and checked, as the gateway at
/// `base_url` serves it: [`gateway_view_for`], declaring only the bindings
/// the agent has.
pub fn plain_view(plain: &PlainCard, base_url: &str, name: &str) -> AgentCard {
    served_view(&plain.card, plain, base_url, name)
}

/// `card`, a card of the plain contact `name` (its public card or its
/// extended one), as the gateway at `base_url` serves it:
/// [`gateway_view_for`], declaring only the bindings that `plain`, its
/// public card as read for the call, has, which are those the gateway
/// serves.
pub(crate) fn served_view(
    card: &AgentCard,
    plain: &PlainCard,
    base_url: &str,
    name: &str,
) -> AgentCard {
    let mut view = gateway_view_for(card, base_url, name);
    view.supported_interfaces
        .retain(|iface| match iface.protocol_binding.as_str() {
            TRANSPORT_PROTOCOL_JSONRPC => plain.jsonrpc_url.is_some(),
            TRANSPORT_PROTOCOL_HTTP_JSON => plain.rest_url.is_some(),
            _ => false,
        });
    view
}

/// Keeps `plain`, the card of the plain contact `name`, in `store`, as its
/// public card with no signer. A store that cannot be written is logged:
/// the card is still good to use.
pub async fn keep_plain_card(store: &Arc<Store>, name: &str, plain: &PlainCard) {
    let store = Arc::clone(store);
    let peer = format!("{}{name}", pals_core::store::PLAIN_PEER_PREFIX);
    let (json, hash) = (plain.json.clone(), plain.hash.clone());
    let kept = tokio::task::spawn_blocking(move || {
        let now = chrono::Utc::now().timestamp();
        store.peer_card_put(&peer, PUBLIC_CONTEXT, &json, &hash, None, now)
    })
    .await;
    match kept {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(error = %format!("{err:#}"), "cannot keep a plain contact's card")
        }
        Err(err) => tracing::warn!(error = %err, "cannot keep a plain contact's card"),
    }
}

impl GatewayState {
    /// The card of `contact`, read and checked now, and kept.
    pub(super) async fn plain_card(&self, contact: &A2aContact) -> Result<PlainCard, PlainError> {
        let plain = fetch_contact_card(&self.inner.plain_http, contact, self.port()).await?;
        keep_plain_card(&self.inner.store, &contact.name, &plain).await;
        Ok(plain)
    }
}

/// The answer to a card of the plain contact `name` the gateway could not
/// use: `502`, saying why when the card, or the contact, is at fault.
pub(super) fn plain_failure(name: &str, failure: &PlainError) -> Response {
    tracing::debug!(contact = name, error = %failure, "no usable card from a plain contact");
    error(StatusCode::BAD_GATEWAY, failure_message(failure))
}

/// What the gateway says of a card it could not use, `failure`.
fn failure_message(failure: &PlainError) -> String {
    match failure {
        PlainError::Unreachable(_) | PlainError::Timeout => "peer unreachable".to_owned(),
        PlainError::Refused(_) | PlainError::Protocol(_) => "peer protocol error".to_owned(),
        PlainError::Invalid(_) | PlainError::NoInterface(_) | PlainError::BadContact(_) => {
            failure.to_string()
        }
    }
}

/// The gateway's own answer, `status` and `message`, to a call on `binding`
/// for a plain contact whose request had the JSON-RPC `id`: on HTTP+JSON,
/// `{"error": message}`, as for a peer; on JSON-RPC, whose client reads no
/// HTTP status, a JSON-RPC error envelope of the same status, its code
/// that of `jsonrpc_code`: [`REFUSED_CODE`] for a `4xx` but `408` and
/// `429`, and `InternalError` for those two and the rest, so that a
/// refusal reads as one and a failure on the way as one.
pub(super) fn plain_answer(
    binding: Binding,
    id: Option<&Value>,
    status: StatusCode,
    message: impl Into<String>,
) -> Response {
    match binding {
        Binding::Rest => error(status, message),
        Binding::JsonRpc => jsonrpc_error(status, id, jsonrpc_code(status), &message.into()),
    }
}

/// The JSON-RPC error code that says, to a client that reads no HTTP
/// status, what `status` says: [`REFUSED_CODE`] for a refusal, a `4xx`
/// but `408` and `429`; `InternalError`, a failure on the way, for those
/// two and for the rest, as the outbox reads them on HTTP+JSON.
fn jsonrpc_code(status: StatusCode) -> i32 {
    let on_the_way = [StatusCode::REQUEST_TIMEOUT, StatusCode::TOO_MANY_REQUESTS];
    if status.is_client_error() && !on_the_way.contains(&status) {
        REFUSED_CODE
    } else {
        a2a::error_code::INTERNAL_ERROR
    }
}

/// The message of [`PLAIN_NOT_SENT`] for a request that carried `nonce`
/// ([`NOT_SENT_NONCE_HEADER`]): `<PLAIN_NOT_SENT> (nonce <nonce>)`, or the
/// text alone.
pub fn not_sent_message(nonce: Option<&str>) -> String {
    match nonce {
        Some(nonce) => format!("{PLAIN_NOT_SENT} (nonce {nonce})"),
        None => PLAIN_NOT_SENT.to_owned(),
    }
}

/// The nonce `headers`, a client's request's, carry
/// ([`NOT_SENT_NONCE_HEADER`]), when it is one: [`NONCE_HEX_LEN`]
/// lowercase hex digits, and nothing else, which the gateway may say back.
fn nonce_of(headers: &http::HeaderMap) -> Option<String> {
    let nonce = headers.get(NOT_SENT_NONCE_HEADER)?.to_str().ok()?;
    let hex = nonce
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    (nonce.len() == NONCE_HEX_LEN && hex).then(|| nonce.to_owned())
}

/// [`PLAIN_NOT_SENT`] on `binding`, for a request with the JSON-RPC `id`
/// and `nonce`: `502` with `{"error": …}`, or the JSON-RPC error
/// [`NOT_SENT_CODE`], of [`not_sent_message`].
fn not_sent(binding: Binding, id: Option<&Value>, nonce: Option<&str>) -> Response {
    let message = not_sent_message(nonce);
    match binding {
        Binding::Rest => error(StatusCode::BAD_GATEWAY, message),
        Binding::JsonRpc => jsonrpc_error(StatusCode::BAD_GATEWAY, id, NOT_SENT_CODE, &message),
    }
}

/// The body that stands, on `binding`, for a plain agent's answer of
/// `status` that the gateway does not pass on: one that is no JSON, or one
/// that says what only the gateway says ([`forges_not_sent`]). It names the
/// status and nothing the agent sent.
pub(super) fn replaced_body(binding: Binding, status: StatusCode) -> Vec<u8> {
    let message = format!("plain agent answered {}", status.as_u16());
    match binding {
        Binding::Rest => serde_json::to_vec(&serde_json::json!({ "error": message })),
        Binding::JsonRpc => {
            serde_json::to_vec(&jsonrpc_envelope(None, jsonrpc_code(status), &message))
        }
    }
    .expect("a JSON value serializes")
}

/// A JSON-RPC 2.0 error response for the request `id` (`null` when it is
/// not known).
fn jsonrpc_envelope(id: Option<&Value>, code: i32, message: &str) -> Value {
    let id = id
        .filter(|id| id.is_string() || id.is_number())
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

fn jsonrpc_error(status: StatusCode, id: Option<&Value>, code: i32, message: &str) -> Response {
    let body =
        serde_json::to_vec(&jsonrpc_envelope(id, code, message)).expect("a JSON value serializes");
    json_bytes_response(status, body)
}

/// `status` with `body`, exactly, typed `application/json`.
fn json_bytes_response(status: StatusCode, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// Whether `value`, a plain agent's answer, says what only the gateway may
/// say: a string or a key holding [`PLAIN_NOT_SENT`], once decoded, or a
/// `code` of [`NOT_SENT_CODE`], anywhere in it. The gateway never passes
/// such an answer on. What it decides is what a client reads; whether a
/// question goes again is never decided on it, but on the nonce.
pub(super) fn forges_not_sent(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains(PLAIN_NOT_SENT),
        Value::Array(items) => items.iter().any(forges_not_sent),
        Value::Object(members) => members.iter().any(|(key, member)| {
            key.contains(PLAIN_NOT_SENT)
                || (key == "code" && member.as_i64() == Some(i64::from(NOT_SENT_CODE)))
                || forges_not_sent(member)
        }),
        _ => false,
    }
}

/// Forwards `request`, for `/peers/<name>/<rest>` (`target` is `/<rest>`
/// and the query), to the plain contact `contact`, as the [module](self)
/// says.
pub(super) async fn forward(
    state: &GatewayState,
    contact: A2aContact,
    target: Uri,
    request: Request,
    binding: Binding,
) -> Response {
    let (parts, body) = request.into_parts();
    // Said back only if nothing is sent; never sent on (`outgoing_headers`).
    let nonce = nonce_of(&parts.headers);
    let mut headers = outgoing_headers(&parts.headers);
    // The body goes as read, and reqwest declares its length.
    headers.remove(CONTENT_LENGTH);
    // The gateway refuses an encoded answer: none is asked for, whatever
    // the client takes.
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static(IDENTITY));
    let limit = state.inner.limits.max_body_bytes;
    let bytes = match Limited::new(body, limit).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) if err.downcast_ref::<LengthLimitError>().is_some() => {
            return plain_answer(
                binding,
                None,
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body larger than {limit} bytes"),
            );
        }
        Err(_) => {
            return plain_answer(
                binding,
                None,
                StatusCode::BAD_REQUEST,
                "cannot read the request body",
            );
        }
    };
    let request_json = serde_json::from_slice::<Value>(&bytes).ok();
    // A JSON-RPC request's id, which its error answers.
    let id = request_json
        .as_ref()
        .and_then(|request| request.get("id"))
        .filter(|_| binding == Binding::JsonRpc);
    if request_json
        .as_ref()
        .is_some_and(|value| names_a_blob(value, &state.base_url()))
    {
        return plain_answer(
            binding,
            id,
            StatusCode::BAD_REQUEST,
            "files go only to PALS peers",
        );
    }
    if !headers.contains_key(AUTHORIZATION)
        && let Some(var) = &contact.bearer_env
    {
        match bearer_from(var) {
            Ok(bearer) => {
                headers.insert(AUTHORIZATION, bearer);
            }
            Err(refusal) => {
                return plain_answer(binding, id, refusal.status, refusal.message);
            }
        }
    }

    let plain = match state.plain_card(&contact).await {
        Ok(plain) => plain,
        // Nothing of the call was sent.
        Err(failure @ (PlainError::Unreachable(_) | PlainError::Timeout)) => {
            tracing::debug!(contact = %contact.name, error = %failure, "a plain contact cannot be reached");
            return not_sent(binding, id, nonce.as_deref());
        }
        Err(failure) => {
            tracing::debug!(contact = %contact.name, error = %failure, "no usable card from a plain contact");
            return plain_answer(
                binding,
                id,
                StatusCode::BAD_GATEWAY,
                failure_message(&failure),
            );
        }
    };
    let url = match target_url(&plain, binding, &target) {
        Ok(url) => url,
        Err(refusal) => return plain_answer(binding, id, refusal.status, refusal.message),
    };
    // What the client expects, from its own call.
    let call = match binding {
        Binding::JsonRpc => Call::jsonrpc(request_json.as_ref()),
        Binding::Rest => {
            let path = target.path();
            Call::rest(&parts.method, path.strip_prefix("/rest").unwrap_or(path))
        }
    };
    let sent = state
        .inner
        .plain_http
        .request(parts.method, url)
        .headers(headers)
        .body(bytes)
        .send()
        .await;
    let protocol_error = |why: String| {
        tracing::debug!(contact = %contact.name, %why, "a plain contact's answer was refused");
        plain_answer(binding, id, StatusCode::BAD_GATEWAY, "peer protocol error")
    };
    let response = match sent {
        Ok(response) => response,
        // No connection, so nothing of the call left.
        Err(err) if err.is_connect() => {
            tracing::debug!(contact = %contact.name, error = %err, "a plain contact cannot be reached");
            return not_sent(binding, id, nonce.as_deref());
        }
        Err(err) if err.is_timeout() => {
            tracing::debug!(contact = %contact.name, error = %err, "a plain contact did not answer");
            return plain_answer(binding, id, StatusCode::BAD_GATEWAY, "peer unreachable");
        }
        Err(err) => return protocol_error(err.to_string()),
    };
    let status = response.status();
    // A redirect would send the client wherever the agent chose.
    if status.is_redirection() {
        return protocol_error(format!("the agent redirected ({status})"));
    }
    let mut headers = incoming_headers(response.headers());
    let body = Body::from_stream(response.bytes_stream());
    let owner = CardOwner::Plain(&contact.name, &plain);
    match state
        .incoming_body(owner, binding, call, status, &mut headers, body)
        .await
    {
        Ok(body) => {
            let mut answer = Response::new(body);
            *answer.status_mut() = status;
            *answer.headers_mut() = headers;
            answer
        }
        Err(failure) => protocol_error(failure.to_string()),
    }
}

/// Whether `value`, a request's JSON, has a part whose `url` names a blob:
/// of the gateway at `base_url`, or between peers.
fn names_a_blob(value: &Value, base_url: &str) -> bool {
    let mut value = value.clone();
    let mut named = rewrite::rewrite_to_wire(&mut value, base_url);
    rewrite::for_each_blob_part(&mut value, &mut |_, _, _| named = true);
    named
}

/// `Bearer <token>`, the token read from the environment variable `var`
/// now; else the gateway's own `401`.
fn bearer_from(var: &str) -> Result<HeaderValue, Refusal> {
    let token = std::env::var(var)
        .ok()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| Refusal::new(StatusCode::UNAUTHORIZED, "bearer env var not set"))?;
    let mut bearer = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        Refusal::new(
            StatusCode::UNAUTHORIZED,
            "bearer env var does not hold a valid token",
        )
    })?;
    bearer.set_sensitive(true);
    Ok(bearer)
}

/// Where a call on `binding` to `target` goes: the card's JSON-RPC URL, or
/// its HTTP+JSON URL followed by the rest of `target`'s path and its query;
/// never another origin.
fn target_url(plain: &PlainCard, binding: Binding, target: &Uri) -> Result<Url, Refusal> {
    let (base, rest, what) = match binding {
        Binding::JsonRpc => (plain.jsonrpc_url.as_deref(), None, "JSON-RPC"),
        Binding::Rest => (
            plain.rest_url.as_deref(),
            Some(target.path().strip_prefix("/rest").unwrap_or_default()),
            "HTTP+JSON",
        ),
    };
    let Some(base) = base else {
        return Err(Refusal::new(
            StatusCode::NOT_FOUND,
            format!("no {what} interface"),
        ));
    };
    let base = Url::parse(base).expect("an interface URL kept parsed");
    let Some(rest) = rest else {
        return Ok(base);
    };
    let mut text = format!("{}{rest}", base.as_str().trim_end_matches('/'));
    if let Some(query) = target.query() {
        text.push('?');
        text.push_str(query);
    }
    match Url::parse(&text) {
        Ok(url) if origin_of(&url) == origin_of(&base) => Ok(url),
        _ => Err(Refusal::new(
            StatusCode::BAD_REQUEST,
            "invalid request target",
        )),
    }
}

/// The name a [`PeerRef`] is reached by at `<gateway>/peers/<segment>`: a
/// PALS peer's agent id in hex, a plain contact's name.
pub fn path_segment(peer: &PeerRef) -> String {
    match peer {
        PeerRef::Pals(id) => id.to_string(),
        PeerRef::Plain(contact) => contact.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_and_origin(text: &str) -> Result<(String, String), PlainError> {
        contact_url(text).map(|(url, origin)| (url.to_string(), origin))
    }

    #[test]
    fn a_contact_url_is_a_base_or_a_card_on_https_or_loopback_http() {
        let ok = |text: &str, card: &str, origin: &str| {
            assert_eq!(
                card_and_origin(text),
                Ok((card.to_owned(), origin.to_owned())),
                "{text}"
            );
        };
        ok(
            "https://agent.example",
            "https://agent.example/.well-known/agent-card.json",
            "https://agent.example",
        );
        ok(
            "https://agent.example:443/a2a/",
            "https://agent.example/a2a/.well-known/agent-card.json",
            "https://agent.example",
        );
        ok(
            "https://agent.example:8443/cards/me.json",
            "https://agent.example:8443/cards/me.json",
            "https://agent.example:8443",
        );
        ok(
            "http://127.0.0.1:7000",
            "http://127.0.0.1:7000/.well-known/agent-card.json",
            "http://127.0.0.1:7000",
        );
        ok(
            "http://[::1]:7000/x",
            "http://[::1]:7000/x/.well-known/agent-card.json",
            "http://[::1]:7000",
        );
        ok(
            "http://LOCALHOST:7000",
            "http://localhost:7000/.well-known/agent-card.json",
            "http://localhost:7000",
        );

        for text in [
            "http://10.0.0.1/",
            "http://example.com/",
            "http://127.0.0.2:7000/",
            "http://localhost.example/",
        ] {
            let err = card_and_origin(text).unwrap_err().to_string();
            assert!(err.contains(ONLY_LOOPBACK), "{text}: {err}");
        }
        for text in [
            "ftp://agent.example/",
            "file:///etc/passwd",
            "https://user:pw@agent.example/",
            "https://agent.example/?x=1",
            "https://agent.example/#x",
            "agent.example",
            "",
        ] {
            let err = card_and_origin(text).unwrap_err().to_string();
            assert!(err.starts_with(URL_RULE), "{text}: {err}");
        }
    }

    #[test]
    fn the_gateways_own_origin_is_known_by_any_loopback_name() {
        for origin in [
            "http://127.0.0.1:7420",
            "http://localhost:7420",
            "http://LOCALHOST:7420",
            "http://[::1]:7420",
            "https://127.0.0.1:7420",
            "https://127.1.2.3:7420",
        ] {
            assert!(is_own_gateway(origin, 7420), "{origin}");
        }
        for origin in [
            "http://127.0.0.1:7421",
            "https://agent.example:7420",
            "https://localhost",
            "http://10.0.0.1:7420",
            "not a url",
        ] {
            assert!(!is_own_gateway(origin, 7420), "{origin}");
        }
        assert!(is_own_gateway("https://localhost", 443));
    }

    #[test]
    fn contacts_refuse_ids_unsafe_names_and_odd_variables() {
        let id = pals_core::identity::AgentKey::generate().id().to_string();
        let url = "https://agent.example";
        assert!(new_contact("echo", url, Some("ECHO_TOKEN_1"), 5).is_ok());
        for name in [id.as_str(), "a/b", "..", ""] {
            assert!(new_contact(name, url, None, 5).is_err(), "{name:?}");
        }
        for var in ["", "1ABC", "A-B", "A=B", "A B"] {
            assert!(new_contact("echo", url, Some(var), 5).is_err(), "{var:?}");
        }
    }

    #[test]
    fn only_okp_ed25519_keys_for_signatures_are_taken() {
        let x = URL_SAFE_NO_PAD.encode(pals_core::identity::AgentKey::generate().id().as_bytes());
        let set = |key: Value| serde_json::json!({"keys": [key]});
        let okp = serde_json::json!({"kty": "OKP", "crv": "Ed25519", "kid": "k1", "x": x});
        assert!(okp_key(&set(okp.clone()), "k1").is_some());
        assert!(okp_key(&set(okp.clone()), "k2").is_none());
        for (field, value) in [
            ("kty", "EC"),
            ("crv", "X25519"),
            ("use", "enc"),
            ("alg", "ES256"),
            ("x", "AAAA"),
        ] {
            let mut key = okp.clone();
            key[field] = Value::from(value);
            assert!(okp_key(&set(key), "k1").is_none(), "{field}");
        }
    }

    #[test]
    fn only_eddsa_headers_with_a_kid_and_a_jku_count() {
        let encode = |header: Value| URL_SAFE_NO_PAD.encode(header.to_string());
        let jku = "https://agent.example/jwks.json";
        assert_eq!(
            eddsa_kid_and_jku(&encode(
                serde_json::json!({"alg": "EdDSA", "kid": "k1", "jku": jku})
            )),
            Some(("k1".to_owned(), jku.to_owned()))
        );
        for header in [
            serde_json::json!({"alg": "ES256", "kid": "k1", "jku": jku}),
            serde_json::json!({"alg": "EdDSA", "kid": "k1"}),
            serde_json::json!({"alg": "EdDSA", "jku": jku}),
            serde_json::json!({"alg": "EdDSA", "kid": "k1", "jku": jku, "crit": ["b64"]}),
        ] {
            assert_eq!(eddsa_kid_and_jku(&encode(header.clone())), None, "{header}");
        }
        assert_eq!(eddsa_kid_and_jku("not base64!"), None);
    }
}
