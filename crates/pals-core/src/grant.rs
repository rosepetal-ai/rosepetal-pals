//! Signed direct grants (`agentnet.grant/1`).
//!
//! An issuer allows a subject to perform some operations on some resources of
//! one project, served by an audience, between two instants. There is no
//! delegation.
//!
//! The grant travels as a token: base64url without padding of
//! `JCS({"grant": <grant>, "signature": <signature>})`, where the signature is
//! the issuer's Ed25519 signature of `GRANT_SIG_DOMAIN || JCS(grant)`, in
//! base64url without padding. [`SignedGrant::from_token`] verifies it over the
//! JSON value it received, before deserializing it into a [`Grant`].

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

use crate::identity::{AgentId, AgentKey};
use crate::jcs::{canonical, sign_domain, verify_domain};

/// The value of [`Grant::schema`].
pub const GRANT_SCHEMA: &str = "agentnet.grant/1";
/// The prefix of the bytes a grant signature covers.
pub const GRANT_SIG_DOMAIN: &[u8] = b"agentnet.grant/1\n";
/// How far the clocks of issuer and audience may disagree, in seconds. It
/// widens the validity window at both ends.
pub const CLOCK_SKEW_S: i64 = 60;
/// The largest token accepted, in bytes.
pub const MAX_TOKEN_BYTES: usize = 8 * 1024;

/// What a grant allows its subject to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operation {
    #[serde(rename = "task.send")]
    TaskSend,
    #[serde(rename = "task.read")]
    TaskRead,
    #[serde(rename = "task.cancel")]
    TaskCancel,
    #[serde(rename = "blob.fetch")]
    BlobFetch,
}

/// The signed content of a grant.
///
/// `resources` are `skill:ask` or `blob:<hash hex>` in milestone 1; one ending
/// in `*` covers every resource that starts with what precedes the `*`
/// (`blob:*`). `issued_at` and `expires_at` are RFC 3339 UTC with seconds,
/// exactly as [`format_timestamp`] writes them: `2026-11-14T00:00:00Z`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub schema: String,
    pub grant_id: String,
    pub issuer: AgentId,
    pub subject: AgentId,
    pub audience: AgentId,
    pub project: String,
    pub resources: Vec<String>,
    pub operations: Vec<Operation>,
    pub issued_at: String,
    pub expires_at: String,
}

/// A grant and its issuer's signature, in base64url without padding.
///
/// Only [`SignedGrant::issue`] and [`SignedGrant::from_token`] produce values
/// whose signature has been made or verified; [`SignedGrant::check`] relies
/// on that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignedGrant {
    pub grant: Grant,
    pub signature: String,
}

/// The request a grant is checked against.
#[derive(Debug, Clone, Copy)]
pub struct GrantCheck<'a> {
    /// The verifier's clock, in Unix seconds.
    pub now_unix: i64,
    /// The agent that serves the request.
    pub audience: &'a AgentId,
    /// The agent that makes the request.
    pub subject: &'a AgentId,
    /// Issuers whose grants the audience accepts.
    pub accepted_issuers: &'a [AgentId],
    pub operation: Operation,
    pub resource: &'a str,
    /// Whether the audience has revoked this grant.
    pub revoked: bool,
}

/// Why a grant was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    #[error("malformed")]
    Malformed,
    #[error("too large")]
    TooLarge,
    #[error("unknown schema")]
    UnknownSchema,
    #[error("bad signature")]
    BadSignature,
    #[error("issuer not accepted")]
    IssuerNotAccepted,
    #[error("wrong subject")]
    WrongSubject,
    #[error("wrong audience")]
    WrongAudience,
    #[error("not yet valid")]
    NotYetValid,
    #[error("expired")]
    Expired,
    #[error("revoked")]
    Revoked,
    #[error("operation not granted")]
    OperationNotGranted,
    #[error("resource not granted")]
    ResourceNotGranted,
}

impl GrantError {
    /// Whether the grant itself is unusable (HTTP 401), rather than valid but
    /// not covering the request (HTTP 403): false only for
    /// [`GrantError::OperationNotGranted`] and [`GrantError::ResourceNotGranted`].
    pub fn is_authentication(&self) -> bool {
        !matches!(self, Self::OperationNotGranted | Self::ResourceNotGranted)
    }
}

impl SignedGrant {
    /// Signs `grant` with `key`.
    ///
    /// Refuses what [`SignedGrant::from_token`] would refuse:
    /// [`GrantError::IssuerNotAccepted`] when `grant.issuer` is not `key`'s
    /// id, [`GrantError::UnknownSchema`] when `grant.schema` is not
    /// [`GRANT_SCHEMA`], [`GrantError::Malformed`] when a timestamp is not in
    /// the [`format_timestamp`] form, and [`GrantError::TooLarge`] when the
    /// token would exceed [`MAX_TOKEN_BYTES`].
    pub fn issue(key: &AgentKey, grant: Grant) -> Result<Self, GrantError> {
        if grant.issuer != key.id() {
            return Err(GrantError::IssuerNotAccepted);
        }
        if grant.schema != GRANT_SCHEMA {
            return Err(GrantError::UnknownSchema);
        }
        validate_timestamps(&grant)?;
        let value = serde_json::to_value(&grant).expect("a grant always serializes");
        let signature = sign_domain(key, GRANT_SIG_DOMAIN, &value);
        let signed = Self { grant, signature };
        if signed.to_token().len() > MAX_TOKEN_BYTES {
            return Err(GrantError::TooLarge);
        }
        Ok(signed)
    }

    /// base64url without padding of `JCS({"grant": …, "signature": …})`.
    pub fn to_token(&self) -> String {
        let envelope = json!({ "grant": self.grant, "signature": self.signature });
        URL_SAFE_NO_PAD.encode(canonical(&envelope))
    }

    /// Parses a token and verifies its signature.
    ///
    /// The token must be the canonical form [`SignedGrant::to_token`] writes,
    /// and the signature is verified over the received JSON value of the grant
    /// before that value is deserialized; unknown fields are refused. Validity
    /// against a request is [`SignedGrant::check`]'s job.
    pub fn from_token(token: &str) -> Result<Self, GrantError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(GrantError::TooLarge);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| GrantError::Malformed)?;
        let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| GrantError::Malformed)?;
        // One spelling per token: no duplicate keys, whitespace or reordering.
        if canonical(&envelope) != bytes {
            return Err(GrantError::Malformed);
        }
        let Value::Object(mut envelope) = envelope else {
            return Err(GrantError::Malformed);
        };
        let (Some(grant), Some(Value::String(signature)), true) = (
            envelope.remove("grant"),
            envelope.remove("signature"),
            envelope.is_empty(),
        ) else {
            return Err(GrantError::Malformed);
        };

        match grant.get("schema") {
            Some(Value::String(schema)) if schema == GRANT_SCHEMA => {}
            Some(Value::String(_)) => return Err(GrantError::UnknownSchema),
            _ => return Err(GrantError::Malformed),
        }
        let issuer: AgentId = grant
            .get("issuer")
            .and_then(Value::as_str)
            .and_then(|text| text.parse().ok())
            .ok_or(GrantError::Malformed)?;
        if !verify_domain(&issuer, GRANT_SIG_DOMAIN, &grant, &signature) {
            return Err(GrantError::BadSignature);
        }

        let grant: Grant = serde_json::from_value(grant).map_err(|_| GrantError::Malformed)?;
        validate_timestamps(&grant)?;
        Ok(Self { grant, signature })
    }

    /// Whether this grant covers the request `c`, within [`CLOCK_SKEW_S`] of
    /// its validity window.
    ///
    /// The first failing rule wins, in this order: revoked, issuer, audience,
    /// subject, not yet valid, expired, operation, resource.
    pub fn check(&self, c: &GrantCheck) -> Result<(), GrantError> {
        let grant = &self.grant;
        if c.revoked {
            return Err(GrantError::Revoked);
        }
        if !c.accepted_issuers.contains(&grant.issuer) {
            return Err(GrantError::IssuerNotAccepted);
        }
        if grant.audience != *c.audience {
            return Err(GrantError::WrongAudience);
        }
        if grant.subject != *c.subject {
            return Err(GrantError::WrongSubject);
        }
        let issued_at = parse_timestamp(&grant.issued_at).ok_or(GrantError::Malformed)?;
        let expires_at = parse_timestamp(&grant.expires_at).ok_or(GrantError::Malformed)?;
        if c.now_unix < issued_at - CLOCK_SKEW_S {
            return Err(GrantError::NotYetValid);
        }
        if c.now_unix >= expires_at + CLOCK_SKEW_S {
            return Err(GrantError::Expired);
        }
        if !grant.operations.contains(&c.operation) {
            return Err(GrantError::OperationNotGranted);
        }
        if !grant
            .resources
            .iter()
            .any(|granted| resource_matches(granted, c.resource))
        {
            return Err(GrantError::ResourceNotGranted);
        }
        Ok(())
    }
}

/// Equality, or a prefix match when `granted` ends in `*`.
fn resource_matches(granted: &str, requested: &str) -> bool {
    match granted.strip_suffix('*') {
        Some(prefix) => requested.starts_with(prefix),
        None => granted == requested,
    }
}

const TIMESTAMP: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

/// `unix_s` as a grant timestamp, RFC 3339 UTC with seconds
/// (`2026-11-14T00:00:00Z`); `None` outside the years 0000 to 9999.
pub fn format_timestamp(unix_s: i64) -> Option<String> {
    let text = OffsetDateTime::from_unix_timestamp(unix_s)
        .ok()?
        .format(TIMESTAMP)
        .ok()?;
    (text.len() == 20).then_some(text)
}

/// The Unix seconds of a grant timestamp; `None` unless `text` is exactly in
/// the form [`format_timestamp`] writes.
pub fn parse_timestamp(text: &str) -> Option<i64> {
    if text.len() != 20 {
        return None;
    }
    let at = PrimitiveDateTime::parse(text, TIMESTAMP).ok()?.assume_utc();
    (at.format(TIMESTAMP).ok()? == text).then(|| at.unix_timestamp())
}

fn validate_timestamps(grant: &Grant) -> Result<(), GrantError> {
    match (
        parse_timestamp(&grant.issued_at),
        parse_timestamp(&grant.expires_at),
    ) {
        (Some(_), Some(_)) => Ok(()),
        _ => Err(GrantError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant_for(issuer: &AgentKey) -> Grant {
        let other = AgentKey::generate().id();
        Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: "g-unit".to_owned(),
            issuer: issuer.id(),
            subject: other,
            audience: issuer.id(),
            project: "demo".to_owned(),
            resources: vec!["skill:ask".to_owned()],
            operations: vec![Operation::TaskSend],
            issued_at: "2026-11-14T00:00:00Z".to_owned(),
            expires_at: "2026-11-15T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn issue_refuses_a_grant_of_another_issuer() {
        let key = AgentKey::generate();
        let grant = grant_for(&AgentKey::generate());
        assert_eq!(
            SignedGrant::issue(&key, grant),
            Err(GrantError::IssuerNotAccepted)
        );
    }

    #[test]
    fn issue_refuses_what_from_token_would_refuse() {
        let key = AgentKey::generate();
        let grant = Grant {
            schema: "agentnet.grant/2".to_owned(),
            ..grant_for(&key)
        };
        assert_eq!(
            SignedGrant::issue(&key, grant),
            Err(GrantError::UnknownSchema)
        );
        let grant = Grant {
            expires_at: "2026-11-15T00:00:00+00:00".to_owned(),
            ..grant_for(&key)
        };
        assert_eq!(SignedGrant::issue(&key, grant), Err(GrantError::Malformed));
    }

    #[test]
    fn unknown_schema_is_reported_before_the_signature() {
        let key = AgentKey::generate();
        let mut envelope: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(
                    SignedGrant::issue(&key, grant_for(&key))
                        .unwrap()
                        .to_token(),
                )
                .unwrap(),
        )
        .unwrap();
        envelope["grant"]["schema"] = json!("agentnet.grant/2");
        let token = URL_SAFE_NO_PAD.encode(canonical(&envelope));
        assert_eq!(
            SignedGrant::from_token(&token),
            Err(GrantError::UnknownSchema)
        );
    }

    #[test]
    fn non_canonical_token_is_malformed() {
        let key = AgentKey::generate();
        let token = SignedGrant::issue(&key, grant_for(&key))
            .unwrap()
            .to_token();
        let text = String::from_utf8(URL_SAFE_NO_PAD.decode(&token).unwrap()).unwrap();
        let spaced = text.replacen(':', ": ", 1);
        assert_eq!(
            SignedGrant::from_token(&URL_SAFE_NO_PAD.encode(spaced)),
            Err(GrantError::Malformed)
        );
    }

    #[test]
    fn timestamps_have_one_spelling() {
        assert_eq!(parse_timestamp("2026-11-14T00:00:00Z"), Some(1_794_614_400));
        assert_eq!(
            format_timestamp(1_794_614_400).as_deref(),
            Some("2026-11-14T00:00:00Z")
        );
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        for text in [
            "2026-11-14T00:00:00+00:00",
            "2026-11-14T00:00:00.000Z",
            "2026-11-14t00:00:00z",
            "2026-11-14 00:00:00Z",
            "2026-11-14T24:00:00Z",
            "2026-02-30T00:00:00Z",
            "+026-11-14T00:00:00Z",
            "2026-11-14T00:00:60Z",
            "",
        ] {
            assert_eq!(parse_timestamp(text), None, "{text}");
        }
        assert_eq!(
            format_timestamp(-1),
            Some("1969-12-31T23:59:59Z".to_owned())
        );
        assert_eq!(format_timestamp(i64::MAX), None);
    }
}
