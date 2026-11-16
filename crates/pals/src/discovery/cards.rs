//! Peers' Agent Cards, as this agent reads them: the public card at
//! `/.well-known/agent-card.json`, and the extended card a grant lets it
//! see, at `GET /rest/extendedAgentCard`, both over the A2A tunnel.
//!
//! A card is read up to [`MAX_CARD_BYTES`], within [`CARD_TIMEOUT`], and
//! used only once it is valid and signed by the peer that answered: a
//! signature whose `kid` is the peer's agent id, the one Iroh's handshake
//! proved ([`parse_and_verify`]). One that is altered, signed by another
//! key or too large is refused, and never kept. A verified card is kept in
//! the store ([`Store::peer_card_put`]) as [`signed_card_json`] gives it, in
//! the context it was read in: [`PUBLIC_CONTEXT`], or
//! `grant:<issuer>/<grant_id>` for an extended card.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use http::header::AUTHORIZATION;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use iroh::EndpointAddr;
use pals_core::card_jws::{
    CardError, MAX_CARD_BYTES, MAX_CARD_SIGNATURES, VerifiedCard, card_hash, parse_and_verify,
    verify_card,
};
use pals_core::grant::SignedGrant;
use pals_core::identity::AgentId;
use pals_core::store::Store;
use serde_json::{Map, Value};

use crate::net::{PeerHttp, TunnelError};

/// How long reading a card may take in all, dialing included.
pub const CARD_TIMEOUT: Duration = Duration::from_secs(20);
/// The context a peer's public card is kept in.
pub const PUBLIC_CONTEXT: &str = "public";
/// Where a peer serves its public card.
const WELL_KNOWN_CARD: &str = "/.well-known/agent-card.json";
/// Where a peer serves its extended card, on its HTTP+JSON binding.
const EXTENDED_CARD: &str = "/rest/extendedAgentCard";
/// The A2A protocol version asked for.
const A2A_VERSION: &str = "1.0";

/// Why a peer's card could not be had.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CardFetchError {
    /// No connection to the peer, or it was lost.
    #[error("peer unreachable: {0}")]
    Unreachable(String),
    /// The peer answered with this status, not with a card: `401` or `403`
    /// to a grant it does not take, a redirect, an error.
    #[error("the peer refused the card request ({0})")]
    Refused(u16),
    /// What the peer answered is not a card it signed.
    #[error("peer card invalid: {0}")]
    Invalid(CardError),
    /// The HTTP exchange with the peer broke.
    #[error("broken exchange with the peer: {0}")]
    Protocol(String),
    /// The card did not arrive within [`CARD_TIMEOUT`].
    #[error("the card did not arrive in time")]
    Timeout,
}

impl From<TunnelError> for CardFetchError {
    fn from(err: TunnelError) -> Self {
        match err {
            TunnelError::Unreachable(why) => Self::Unreachable(why),
            TunnelError::Protocol(why) => Self::Protocol(why),
            TunnelError::Timeout => Self::Timeout,
        }
    }
}

/// Reads peers' cards and keeps the verified ones. Clones share the client
/// and the store.
#[derive(Clone)]
pub struct PeerCards {
    http: PeerHttp,
    store: Arc<Store>,
}

impl PeerCards {
    /// Reads cards with `http` and keeps them in `store`.
    pub fn new(http: PeerHttp, store: Arc<Store>) -> Self {
        Self { http, store }
    }

    /// The public card of `peer`, dialed at `addr`, verified as signed by
    /// `peer`, and kept in [`PUBLIC_CONTEXT`].
    pub async fn public(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
    ) -> Result<VerifiedCard, CardFetchError> {
        Ok(self.public_sized(peer, addr).await?.0)
    }

    /// [`PeerCards::public`], with the bytes of the card's body.
    pub async fn public_sized(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
    ) -> Result<(VerifiedCard, u64), CardFetchError> {
        let request = http::Request::get(WELL_KNOWN_CARD)
            .header("a2a-version", A2A_VERSION)
            .body(Body::empty())
            .expect("a valid request");
        let (verified, size) = self.fetch(peer, addr, request).await?;
        self.keep(peer, PUBLIC_CONTEXT.to_owned(), &verified).await;
        Ok((verified, size))
    }

    /// The extended card `grant` lets this agent see of `peer`, dialed at
    /// `addr`, verified as signed by `peer`, and kept in the context
    /// `grant:<issuer>/<grant_id>`. The peer chooses what it shows from the
    /// grant: a card never widens what the grant allows.
    pub async fn extended(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
        grant: &SignedGrant,
    ) -> Result<VerifiedCard, CardFetchError> {
        Ok(self.extended_sized(peer, addr, grant).await?.0)
    }

    /// [`PeerCards::extended`], with the bytes of the card's body.
    pub async fn extended_sized(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
        grant: &SignedGrant,
    ) -> Result<(VerifiedCard, u64), CardFetchError> {
        let request = http::Request::get(EXTENDED_CARD)
            .header("a2a-version", A2A_VERSION)
            .header(AUTHORIZATION, format!("Bearer {}", grant.to_token()))
            .body(Body::empty())
            .expect("a grant token is a valid header value");
        let (verified, size) = self.fetch(peer, addr, request).await?;
        let context = format!("grant:{}/{}", grant.grant.issuer, grant.grant.grant_id);
        self.keep(peer, context, &verified).await;
        Ok((verified, size))
    }

    /// The card `request` gets from `peer`, read up to [`MAX_CARD_BYTES`]
    /// and verified, within [`CARD_TIMEOUT`], and the bytes of its body.
    async fn fetch(
        &self,
        peer: &AgentId,
        addr: EndpointAddr,
        request: http::Request<Body>,
    ) -> Result<(VerifiedCard, u64), CardFetchError> {
        let exchange = async {
            let response = self.http.send(addr, request).await?;
            let status = response.status();
            if !status.is_success() {
                return Err(CardFetchError::Refused(status.as_u16()));
            }
            let body = Limited::new(response.into_body(), MAX_CARD_BYTES)
                .collect()
                .await
                .map_err(|err| {
                    if err.downcast_ref::<LengthLimitError>().is_some() {
                        CardFetchError::Invalid(CardError::TooLarge)
                    } else {
                        CardFetchError::Protocol(err.to_string())
                    }
                })?
                .to_bytes();
            let verified = parse_and_verify(&body, peer).map_err(CardFetchError::Invalid)?;
            Ok((verified, body.len() as u64))
        };
        tokio::time::timeout(CARD_TIMEOUT, exchange)
            .await
            .unwrap_or(Err(CardFetchError::Timeout))
    }

    /// Keeps `verified`, a card of `peer`, in `context`. A store that cannot
    /// be written is logged: the card is still good to use.
    async fn keep(&self, peer: &AgentId, context: String, verified: &VerifiedCard) {
        let store = Arc::clone(&self.store);
        let signed = signed_card_json(verified);
        let hash = card_hash(&signed);
        let (peer, signer) = (peer.to_string(), verified.signer);
        let kept = tokio::task::spawn_blocking(move || {
            let now = chrono::Utc::now().timestamp();
            store.peer_card_put(&peer, &context, &signed, &hash, Some(&signer), now)
        })
        .await;
        match kept {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(error = %format!("{err:#}"), "cannot keep a peer's card")
            }
            Err(err) => tracing::warn!(error = %err, "cannot keep a peer's card"),
        }
    }
}

/// `verified` as its signer signed it, with nothing that is not signed: the
/// bytes the valid signature covers ([`VerifiedCard::payload`]), with the
/// received signatures that verify on their own over those same bytes, each
/// as its `protected` header and `signature` only (an unprotected `header`
/// is not signed).
///
/// A card PALS signs verifies over the JSON as received, with one
/// signature: its signed JSON is the card as received.
pub fn signed_card_json(verified: &VerifiedCard) -> Value {
    let mut card: Map<String, Value> = serde_json::from_slice(&verified.payload)
        .expect("the bytes a signature covers are a JSON object");
    let received = verified
        .json
        .get("signatures")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let signatures: Vec<Value> = received
        .iter()
        .take(MAX_CARD_SIGNATURES)
        .filter(|signature| {
            let mut alone = verified.json.clone();
            alone["signatures"] = Value::Array(vec![(*signature).clone()]);
            verify_card(&alone, &verified.signer)
                .is_ok_and(|checked| checked.payload == verified.payload)
        })
        .map(|signature| {
            let mut kept = Map::new();
            for field in ["protected", "signature"] {
                if let Some(value) = signature.get(field) {
                    kept.insert(field.to_owned(), value.clone());
                }
            }
            Value::Object(kept)
        })
        .collect();
    card.insert("signatures".to_owned(), Value::Array(signatures));
    Value::Object(card)
}

#[cfg(test)]
mod tests {
    use pals_core::card_jws::{CanonicalForm, CardSigner};
    use pals_core::identity::AgentKey;
    use pals_core::inventory::Inventory;
    use serde_json::json;

    use super::*;
    use crate::a2a::CardSet;

    fn signed(key: &AgentKey) -> Value {
        let cfg: crate::config::Config = toml::from_str("[agent]\nname = \"bob\"\n").unwrap();
        let signer = CardSigner::new(Arc::new(AgentKey::from_bytes(&key.to_bytes())));
        let cards = CardSet::build(&cfg, &signer, &Inventory::default()).unwrap();
        cards.public().json.clone()
    }

    #[test]
    fn a_card_signed_as_received_is_kept_as_received() {
        let key = AgentKey::generate();
        let card = signed(&key);
        let verified = verify_card(&card, &key.id()).unwrap();
        assert_eq!(verified.form, CanonicalForm::Raw);
        assert_eq!(signed_card_json(&verified), card);
    }

    #[test]
    fn only_signed_content_and_valid_signatures_are_kept() {
        let key = AgentKey::generate();
        let card = signed(&key);
        let valid = card["signatures"][0].clone();

        // An unprotected header, and a signature of another key, ride along.
        let mut received = card.clone();
        let mut with_header = valid.clone();
        with_header["header"] = json!({"kid": "someone else", "note": "unsigned"});
        let other = signed(&AgentKey::generate())["signatures"][0].clone();
        received["signatures"] = json!([other, with_header]);
        let verified = verify_card(&received, &key.id()).unwrap();
        assert_eq!(signed_card_json(&verified), card);

        // Content the signature does not cover, which only a protobuf form
        // drops: a key no A2A type has.
        let mut extra = card.clone();
        extra["unsigned"] = json!("dropped");
        // It verifies over its protobuf form: the received JSON is not what
        // was signed, the payload is.
        let verified = verify_card(&extra, &key.id()).unwrap();
        assert_ne!(verified.form, CanonicalForm::Raw);
        let kept = signed_card_json(&verified);
        assert!(kept.get("unsigned").is_none(), "{kept}");
        assert_eq!(kept, card);
        assert_eq!(kept["signatures"], json!([valid]));
    }
}
