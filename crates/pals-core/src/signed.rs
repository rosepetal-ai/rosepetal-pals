//! Signed JSON documents, each kind under its own signing domain: the base
//! of the records, views, offers and benchmark reports of milestone 2.
//!
//! A kind of document is a [`SignedDoc`]: its `schema`, its signing domain,
//! the key it travels under, its size limit and where its signer is named
//! in it. A signed document travels as the JCS (RFC 8785) text of
//! `{<WRAPPER>: <document>, "signature": <signature>}`, where the signature
//! is the signer's Ed25519 signature of `DOMAIN || JCS(document)`, in
//! base64url without padding ([`crate::jcs`]). The domain separates kinds:
//! a signature made for one is never valid for another, nor for a grant or
//! an event.
//!
//! [`Signed::from_json`] verifies the signature over the document as it
//! was received, with the key of the agent the document names at
//! [`SignedDoc::SIGNER_POINTER`], before deserializing it; unknown fields
//! are refused. A document has one spelling: the envelope must be canonical,
//! and the document, once deserialized, must serialize back to exactly what
//! was received (an absent optional field is not `null`), so
//! [`Signed::to_json`] gives back the bytes that were verified.
//!
//! Validity in time is the receiver's to check, with [`check_validity`]:
//! [`CLOCK_SKEW_S`] of tolerance at both ends, and only to refuse.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::grant::{CLOCK_SKEW_S, parse_timestamp};
use crate::identity::{AgentId, AgentKey};
use crate::jcs::{canonical, sign_domain, verify_domain};

/// A kind of signed document.
pub trait SignedDoc: Serialize + DeserializeOwned {
    /// The value of the document's top-level `schema` field.
    const SCHEMA: &'static str;
    /// The prefix of the bytes its signature covers, `agentnet.<kind>/<n>\n`.
    const DOMAIN: &'static [u8];
    /// The key the document travels under in the signed JSON.
    const WRAPPER: &'static str;
    /// The largest signed JSON accepted, in bytes.
    const MAX_BYTES: usize;
    /// The JSON pointer (RFC 6901) to the signer's [`AgentId`] in the
    /// document: `/agent_id`, or `/provenance/measured_by`.
    const SIGNER_POINTER: &'static str;

    /// The signer, the agent named at [`SignedDoc::SIGNER_POINTER`]. Only
    /// [`Signed::issue`] reads it.
    fn signer(&self) -> &AgentId;

    /// The rules of the kind its types do not enforce (element counts,
    /// timestamp spelling, ranges), as [`SignedError::Malformed`]. Checked
    /// by [`Signed::issue`] and, last, by [`Signed::from_json`]. None by
    /// default.
    fn validate(&self) -> Result<(), SignedError> {
        Ok(())
    }
}

/// A document and its signer's signature, in base64url without padding.
///
/// Only [`Signed::issue`] and [`Signed::from_json`] produce values whose
/// signature has been made or verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed<T> {
    pub doc: T,
    pub signature: String,
}

/// Why a signed document was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignedError {
    #[error("too large")]
    TooLarge,
    #[error("malformed")]
    Malformed,
    #[error("unknown schema")]
    UnknownSchema,
    #[error("bad signature")]
    BadSignature,
    #[error("expired")]
    Expired,
    #[error("not yet valid")]
    NotYetValid,
}

impl<T: SignedDoc> Signed<T> {
    /// Signs `doc` with `key`.
    ///
    /// Refuses what [`Signed::from_json`] would refuse:
    /// [`SignedError::BadSignature`] when `doc` names a signer other than
    /// `key`'s id, [`SignedError::UnknownSchema`] when its `schema` is not
    /// [`SignedDoc::SCHEMA`], [`SignedError::Malformed`] when it breaks
    /// [`SignedDoc::validate`], and [`SignedError::TooLarge`] when the signed
    /// JSON would exceed [`SignedDoc::MAX_BYTES`].
    pub fn issue(key: &AgentKey, doc: T) -> Result<Self, SignedError> {
        let value = serde_json::to_value(&doc).map_err(|_| SignedError::Malformed)?;
        // `from_json` reads the signer at the pointer, not from `signer`.
        if *doc.signer() != key.id() || signer_at::<T>(&value) != Some(key.id()) {
            return Err(SignedError::BadSignature);
        }
        if !has_schema::<T>(&value) {
            return Err(SignedError::UnknownSchema);
        }
        doc.validate()?;
        let signature = sign_domain(key, T::DOMAIN, &value);
        let signed = Self { doc, signature };
        if signed.to_json().len() > T::MAX_BYTES {
            return Err(SignedError::TooLarge);
        }
        Ok(signed)
    }

    /// `JCS({WRAPPER: doc, "signature": signature})`.
    pub fn to_json(&self) -> Vec<u8> {
        let doc = serde_json::to_value(&self.doc)
            .expect("a document that was signed or verified serializes");
        let mut envelope = Map::new();
        envelope.insert(T::WRAPPER.to_owned(), doc);
        envelope.insert(
            "signature".to_owned(),
            Value::String(self.signature.clone()),
        );
        canonical(&Value::Object(envelope))
    }

    /// Parses a signed document and verifies its signature.
    ///
    /// In this order, the first failure winning:
    /// 1. at most [`SignedDoc::MAX_BYTES`], before parsing
    ///    ([`SignedError::TooLarge`]);
    /// 2. canonical JSON of an object with exactly
    ///    [`SignedDoc::WRAPPER`] and a string `signature`
    ///    ([`SignedError::Malformed`]);
    /// 3. the signature of `DOMAIN || JCS(document as received)` by the
    ///    agent at [`SignedDoc::SIGNER_POINTER`] of the document as received
    ///    ([`SignedError::Malformed`] when no agent id is there,
    ///    [`SignedError::BadSignature`] when it does not verify);
    /// 4. deserialized with unknown fields refused, to exactly the document
    ///    received ([`SignedError::Malformed`]);
    /// 5. `schema` is [`SignedDoc::SCHEMA`] ([`SignedError::UnknownSchema`]);
    /// 6. [`SignedDoc::validate`].
    ///
    /// Validity in time is [`check_validity`]'s job.
    pub fn from_json(bytes: &[u8]) -> Result<Self, SignedError> {
        if bytes.len() > T::MAX_BYTES {
            return Err(SignedError::TooLarge);
        }
        let envelope: Value = serde_json::from_slice(bytes).map_err(|_| SignedError::Malformed)?;
        // One spelling: no whitespace, reordering or duplicate keys.
        if canonical(&envelope) != bytes {
            return Err(SignedError::Malformed);
        }
        let Value::Object(mut envelope) = envelope else {
            return Err(SignedError::Malformed);
        };
        let (Some(doc), Some(Value::String(signature)), true) = (
            envelope.remove(T::WRAPPER),
            envelope.remove("signature"),
            envelope.is_empty(),
        ) else {
            return Err(SignedError::Malformed);
        };

        let signer = signer_at::<T>(&doc).ok_or(SignedError::Malformed)?;
        if !verify_domain(&signer, T::DOMAIN, &doc, &signature) {
            return Err(SignedError::BadSignature);
        }

        let parsed = T::deserialize(&doc).map_err(|_| SignedError::Malformed)?;
        let reserialized = serde_json::to_value(&parsed).map_err(|_| SignedError::Malformed)?;
        if canonical(&reserialized) != canonical(&doc) {
            return Err(SignedError::Malformed);
        }
        if !has_schema::<T>(&doc) {
            return Err(SignedError::UnknownSchema);
        }
        parsed.validate()?;
        Ok(Self {
            doc: parsed,
            signature,
        })
    }
}

/// Whether `now_unix` falls in `[issued_at, expires_at)` widened by
/// [`CLOCK_SKEW_S`] at both ends; the timestamps in the form
/// [`crate::grant::format_timestamp`] writes.
///
/// [`SignedError::Malformed`] when a timestamp is in another form,
/// [`SignedError::NotYetValid`] before the window, [`SignedError::Expired`]
/// after it.
pub fn check_validity(issued_at: &str, expires_at: &str, now_unix: i64) -> Result<(), SignedError> {
    let issued_at = parse_timestamp(issued_at).ok_or(SignedError::Malformed)?;
    let expires_at = parse_timestamp(expires_at).ok_or(SignedError::Malformed)?;
    if now_unix < issued_at - CLOCK_SKEW_S {
        return Err(SignedError::NotYetValid);
    }
    if now_unix >= expires_at + CLOCK_SKEW_S {
        return Err(SignedError::Expired);
    }
    Ok(())
}

/// The agent id at `T`'s signer pointer in `doc`, if there is one.
fn signer_at<T: SignedDoc>(doc: &Value) -> Option<AgentId> {
    doc.pointer(T::SIGNER_POINTER)?.as_str()?.parse().ok()
}

fn has_schema<T: SignedDoc>(doc: &Value) -> bool {
    doc.get("schema").and_then(Value::as_str) == Some(T::SCHEMA)
}
