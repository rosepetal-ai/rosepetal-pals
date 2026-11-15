//! Agent Card signatures (A2A §8.4): JWS `EdDSA` over a canonical form of
//! the card, the card hash, and the limits every card must respect.
//!
//! **Signing.** The agent signs its own card with its Ed25519 key. The
//! protected header is exactly `{"alg":"EdDSA","kid":"<agent id>","typ":"JOSE"}`
//! (JCS), and the signature covers the JWS signing input
//! `BASE64URL(protected) || "." || BASE64URL(JCS(card without signatures))`:
//! a detached payload, so `signatures` holds `[{protected, signature}]` and
//! no unprotected `header`. The input starts with `eyJ`, never with an
//! `agentnet.` signing domain, so a card signature can never be taken for a
//! grant or an event signature, nor the reverse.
//!
//! **Canonical forms.** The reference SDKs do not canonicalize a card the
//! same way before checking its signature ([`CanonicalForm`]): Go takes the
//! JSON as received; JS and Python take it after a trip through the protobuf
//! types, which drops unknown fields and proto3 default values, and then
//! drop every empty value. The cards PALS signs are [`normalize`]d, so that
//! the forms are the same bytes and every SDK verifies them. A foreign card
//! is accepted with a valid signature over any of the three forms
//! (`docs/conformance/hito-2/card-signatures.md` records which SDK checks
//! which).
//!
//! **Verification.** A signature is valid when its protected header has
//! `alg` `EdDSA`, no `crit`, the expected `kid`, and the Ed25519 signature
//! (strict rules) verifies for one of the forms. The card is validated
//! first ([`validate_card`]): size, depth, string length, element counts and
//! an A2A 1.0 interface, before anything else reads it. The card a
//! verification returns is read from the bytes the valid signature covers,
//! never from the JSON received, except its `signatures`, which no
//! signature covers and which come from the JSON received as they are,
//! those that do not verify and their unprotected headers included: over a
//! protobuf form, that JSON may carry what the signature does not cover
//! (unknown keys, snake_case names that only the protobuf types read,
//! shapes that only `a2a-lf` reads), and none of it reaches the card.
//!
//! **Hash.** [`card_hash`] identifies the exact signed version of a card
//! (spec §9, `agent_card_hash`): BLAKE3 over the JCS of the card as received,
//! signatures included.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use a2a::{AgentCard, AgentCardSignature, SecurityRequirement};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::identity::{AgentId, AgentKey};
use crate::jcs::canonical;

/// The JWS algorithm of every card signature PALS makes or accepts. PyJWT
/// refuses the RFC 9864 name `Ed25519`.
pub const CARD_JWS_ALG: &str = "EdDSA";
/// The `typ` of the protected header PALS writes.
pub const CARD_JWS_TYP: &str = "JOSE";
/// The largest card accepted, serialized, in bytes (spec §7.3 and §10).
pub const MAX_CARD_BYTES: usize = 32 * 1024;
/// The most interfaces a card may declare.
pub const MAX_CARD_INTERFACES: usize = 8;
/// The most skills a card may declare.
pub const MAX_CARD_SKILLS: usize = 64;
/// The most extensions a card may declare.
pub const MAX_CARD_EXTENSIONS: usize = 16;
/// The most signatures a card may carry, and so the most that are tried.
pub const MAX_CARD_SIGNATURES: usize = 4;
/// The longest string in a card, keys included, in bytes.
pub const MAX_CARD_STRING_BYTES: usize = 4 * 1024;
/// The deepest nesting of arrays and objects in a card, the card itself
/// at 1.
pub const MAX_CARD_DEPTH: usize = 16;
/// The largest magnitude of a number in an extension's `params`: protobuf
/// carries them as doubles, which hold every integer up to here exactly.
pub const MAX_PARAMS_INTEGER: i64 = (1 << 53) - 1;

/// A canonical form of a card, the bytes a signature covers. Each starts by
/// removing the top-level `signatures`, and ends with JCS (RFC 8785).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalForm {
    /// The JSON as received, the form `a2a-go` v2.7.0 checks.
    Raw,
    /// The JSON after a trip through `a2a_pb::protojson::AgentCard`, which
    /// drops unknown fields and proto3 default values: the protobuf JSON a
    /// signer writes before removing empty values. None of the three pinned
    /// SDKs checks this form; it is accepted from foreign signers.
    ProtoJson,
    /// [`CanonicalForm::ProtoJson`] without any `null`, `""`, `[]` or `{}`,
    /// removed recursively: the form `a2a-sdk` 1.3.0 (`_clean_empty`) and
    /// `@a2a-js/sdk` 1.3.0 (`cleanEmpty`) check.
    ProtoJsonClean,
}

const FORMS: [CanonicalForm; 3] = [
    CanonicalForm::Raw,
    CanonicalForm::ProtoJson,
    CanonicalForm::ProtoJsonClean,
];

/// Signs the agent's cards with the agent's own key.
#[derive(Debug, Clone)]
pub struct CardSigner {
    key: Arc<AgentKey>,
}

impl CardSigner {
    pub fn new(key: Arc<AgentKey>) -> Self {
        Self { key }
    }

    /// The `kid` of the signatures: the agent id, in hex.
    pub fn kid(&self) -> String {
        self.key.id().to_string()
    }
}

/// The hash of a signed card: `blake3:` and 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CardHash(String);

impl CardHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CardHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The text is not a card hash.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a card hash is `blake3:` and 64 lowercase hex characters")]
pub struct InvalidCardHash;

impl FromStr for CardHash {
    type Err = InvalidCardHash;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.strip_prefix("blake3:").ok_or(InvalidCardHash)?;
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(InvalidCardHash);
        }
        Ok(Self(s.to_owned()))
    }
}

impl Serialize for CardHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CardHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// What a card's signature is worth to this agent. A PALS peer's card is
/// used only [`Verified`](SignatureStatus::Verified), with its agent id as
/// `kid`; a plain A2A agent's is reported, never required, and only a
/// `Verified` one is chosen automatically (`pals::gateway::plain`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SignatureStatus {
    /// Verified with the key `kid` names: a PALS peer's agent id, or the
    /// OKP Ed25519 key `kid` of a JWK Set of a plain contact's origin.
    Verified { kid: String },
    /// Signed, but not in a way this agent verifies, or not validly.
    Unverifiable,
    /// Not signed.
    Unsigned,
}

impl fmt::Display for SignatureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Verified { kid } => write!(f, "verified {kid}"),
            Self::Unverifiable => f.write_str("unverifiable"),
            Self::Unsigned => f.write_str("unsigned"),
        }
    }
}

/// A card whose signature by an agent has been verified.
///
/// `card` and `payload` are what the valid signature covers. `json` is the
/// card as received: with `form` [`CanonicalForm::Raw`], as every card PALS
/// signs verifies, all of it but `signatures` is signed; with another form
/// it may carry content the signature does not cover, so nothing should
/// read the card from it.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedCard {
    /// The card in `payload`, with the signatures received.
    pub card: AgentCard,
    /// The card as received, signatures included.
    pub json: Value,
    pub signer: AgentId,
    /// The form the valid signature covers.
    pub form: CanonicalForm,
    /// The bytes the valid signature covers, the JWS payload before
    /// base64url: `canonical_payload(&json, form)`.
    pub payload: Vec<u8>,
    /// [`card_hash`] of `json`, the card as received. A card that gained
    /// unsigned content on its way hashes differently from its signer's
    /// copy.
    pub hash: CardHash,
}

/// A card whose signature by a given Ed25519 key has been verified: the card
/// of a plain A2A contact, whose key is not an agent id. Its fields mean
/// what [`VerifiedCard`]'s do: `card` and `payload` are what the signature
/// covers, `json` and `hash` the card as received.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyVerifiedCard {
    /// The card in `payload`, with the signatures received.
    pub card: AgentCard,
    /// The card as received, signatures included.
    pub json: Value,
    pub kid: String,
    /// The form the valid signature covers.
    pub form: CanonicalForm,
    /// The bytes the valid signature covers, the JWS payload before
    /// base64url.
    pub payload: Vec<u8>,
    /// [`card_hash`] of `json`, the card as received.
    pub hash: CardHash,
}

/// Why a card was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CardError {
    #[error("card larger than {MAX_CARD_BYTES} bytes")]
    TooLarge,
    #[error("card is not JSON")]
    NotJson,
    #[error("invalid card: {0}")]
    Invalid(&'static str),
    #[error("card not normalized: {0}")]
    NotNormalized(&'static str),
    #[error("card unsigned")]
    Unsigned,
    /// An algorithm other than `EdDSA`, or a JWS feature this verifier does
    /// not implement (`crit`).
    #[error("unsupported card signature algorithm")]
    UnsupportedAlg,
    #[error("card signed by another key")]
    WrongSigner,
    #[error("bad card signature")]
    BadSignature,
}

/// The bytes a signature over `card` in `form` covers, before base64url:
/// the JCS of the card without its top-level `signatures`, as `form` sees
/// it.
///
/// [`CardError::Invalid`] when `card` is not an object or, for the protobuf
/// forms, not an `AgentCard` to the protobuf types.
pub fn canonical_payload(card: &Value, form: CanonicalForm) -> Result<Vec<u8>, CardError> {
    let Value::Object(object) = card else {
        return Err(CardError::Invalid("not a JSON object"));
    };
    let mut stripped = object.clone();
    stripped.remove("signatures");
    let stripped = Value::Object(stripped);
    match form {
        CanonicalForm::Raw => Ok(canonical(&stripped)),
        CanonicalForm::ProtoJson => Ok(canonical(&protojson(stripped)?)),
        CanonicalForm::ProtoJsonClean => {
            let cleaned = clean_empty(protojson(stripped)?).unwrap_or_else(|| json!({}));
            Ok(canonical(&cleaned))
        }
    }
}

/// `card` after a trip through the protobuf JSON types.
fn protojson(card: Value) -> Result<Value, CardError> {
    let card: a2a_pb::protojson::AgentCard =
        serde_json::from_value(card).map_err(|_| CardError::Invalid("not a protobuf AgentCard"))?;
    serde_json::to_value(card).map_err(|_| CardError::Invalid("not a protobuf AgentCard"))
}

/// `value` without `null`, `""`, `[]` or `{}`, recursively; `None` when
/// nothing is left, as the Python SDK's `_clean_empty` and the JS SDK's
/// `cleanEmpty`.
fn clean_empty(value: Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::Array(items) => {
            let items: Vec<Value> = items.into_iter().filter_map(clean_empty).collect();
            (!items.is_empty()).then_some(Value::Array(items))
        }
        Value::Object(map) => {
            let map: serde_json::Map<String, Value> = map
                .into_iter()
                .filter_map(|(k, v)| clean_empty(v).map(|v| (k, v)))
                .collect();
            (!map.is_empty()).then_some(Value::Object(map))
        }
        other => Some(other),
    }
}

/// `card` written so that its three canonical forms are the same bytes.
///
/// Drops `required: false` from extensions and turns every empty optional
/// into `None`: `documentationUrl`, `iconUrl`, `securitySchemes`,
/// `securityRequirements`, `signatures`, the extensions list, an extension's
/// `description` and `params`, an interface's `tenant`, and a skill's
/// `examples`, `inputModes`, `outputModes` and `securityRequirements`.
///
/// [`CardError::NotNormalized`] when something is left that the protobuf
/// JSON would omit and `a2a-lf` writes: an empty `name`, `description`,
/// `version`, `defaultInputModes`, `defaultOutputModes` or `skills`; an
/// empty `id`, `name`, `description` or `tags` of a skill; an empty `url`,
/// `protocolBinding` or `protocolVersion` of an interface; an empty `uri` of
/// an extension; an empty security requirement or scope list; extension
/// `params` with anything but non-empty strings, non-empty lists of them or
/// non-empty objects of those; and, as a last check, anything else that
/// keeps the three forms apart (an empty provider field, an empty string in
/// a list…).
pub fn normalize(mut card: AgentCard) -> Result<AgentCard, CardError> {
    use CardError::NotNormalized;

    none_if_empty_str(&mut card.documentation_url);
    none_if_empty_str(&mut card.icon_url);
    if card
        .security_schemes
        .as_ref()
        .is_some_and(HashMap::is_empty)
    {
        card.security_schemes = None;
    }
    none_if_empty_vec(&mut card.security_requirements);
    none_if_empty_vec(&mut card.signatures);
    none_if_empty_vec(&mut card.capabilities.extensions);
    for ext in card.capabilities.extensions.iter_mut().flatten() {
        none_if_empty_str(&mut ext.description);
        if ext.required == Some(false) {
            ext.required = None;
        }
        if ext.params.as_ref().is_some_and(HashMap::is_empty) {
            ext.params = None;
        }
    }
    for iface in &mut card.supported_interfaces {
        none_if_empty_str(&mut iface.tenant);
    }
    for skill in &mut card.skills {
        none_if_empty_vec(&mut skill.examples);
        none_if_empty_vec(&mut skill.input_modes);
        none_if_empty_vec(&mut skill.output_modes);
        none_if_empty_vec(&mut skill.security_requirements);
    }

    if card.name.is_empty() {
        return Err(NotNormalized("empty name"));
    }
    if card.description.is_empty() {
        return Err(NotNormalized("empty description"));
    }
    if card.version.is_empty() {
        return Err(NotNormalized("empty version"));
    }
    if card.default_input_modes.is_empty() || card.default_output_modes.is_empty() {
        return Err(NotNormalized("empty default modes"));
    }
    if card.skills.is_empty() {
        return Err(NotNormalized("no skills"));
    }
    for skill in &card.skills {
        if skill.id.is_empty()
            || skill.name.is_empty()
            || skill.description.is_empty()
            || skill.tags.is_empty()
        {
            return Err(NotNormalized(
                "skill with an empty id, name, description or tags",
            ));
        }
        security_requirements_normalized(skill.security_requirements.as_deref())?;
    }
    for iface in &card.supported_interfaces {
        if iface.url.is_empty()
            || iface.protocol_binding.is_empty()
            || iface.protocol_version.is_empty()
        {
            return Err(NotNormalized(
                "interface with an empty url, protocolBinding or protocolVersion",
            ));
        }
    }
    for ext in card.capabilities.extensions.iter().flatten() {
        if ext.uri.is_empty() {
            return Err(NotNormalized("extension with an empty uri"));
        }
        if !ext
            .params
            .iter()
            .flatten()
            .all(|(_, v)| params_value_normalized(v))
        {
            return Err(NotNormalized(
                "extension params other than non-empty strings, lists or objects of them",
            ));
        }
    }
    security_requirements_normalized(card.security_requirements.as_deref())?;

    let json = serde_json::to_value(&card).expect("an AgentCard always serializes");
    let forms: Result<Vec<Vec<u8>>, CardError> = FORMS
        .iter()
        .map(|form| canonical_payload(&json, *form))
        .collect();
    match forms {
        Ok(forms) if forms[0] == forms[1] && forms[1] == forms[2] => Ok(card),
        _ => Err(NotNormalized("canonical forms differ")),
    }
}

fn none_if_empty_str(field: &mut Option<String>) {
    if field.as_deref() == Some("") {
        *field = None;
    }
}

fn none_if_empty_vec<T>(field: &mut Option<Vec<T>>) {
    if field.as_ref().is_some_and(Vec::is_empty) {
        *field = None;
    }
}

fn security_requirements_normalized(
    requirements: Option<&[SecurityRequirement]>,
) -> Result<(), CardError> {
    for requirement in requirements.into_iter().flatten() {
        if requirement.is_empty() {
            return Err(CardError::NotNormalized("empty security requirement"));
        }
        if requirement
            .values()
            .any(|scopes| scopes.is_empty() || scopes.iter().any(String::is_empty))
        {
            return Err(CardError::NotNormalized("empty security scopes"));
        }
    }
    Ok(())
}

/// Whether `value` is a non-empty string, a non-empty list of non-empty
/// strings, or a non-empty object of such values.
fn params_value_normalized(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|s| !s.is_empty()))
        }
        Value::Object(map) => !map.is_empty() && map.values().all(params_value_normalized),
        _ => false,
    }
}

/// `card`, [`normalize`]d and signed by `signer`, replacing any signature it
/// had.
///
/// Refuses what [`verify_card`] would refuse: [`CardError::NotNormalized`]
/// first, then whatever [`validate_card`] finds in the signed card.
pub fn sign_card(signer: &CardSigner, card: AgentCard) -> Result<AgentCard, CardError> {
    let mut card = normalize(card)?;
    card.signatures = None;
    let json = serde_json::to_value(&card).expect("an AgentCard always serializes");
    let payload = canonical_payload(&json, CanonicalForm::Raw)?;
    let header = json!({ "alg": CARD_JWS_ALG, "kid": signer.kid(), "typ": CARD_JWS_TYP });
    let protected = URL_SAFE_NO_PAD.encode(canonical(&header));
    let payload = URL_SAFE_NO_PAD.encode(payload);
    let signature = signer.key.sign(&signing_input(&protected, &payload));
    card.signatures = Some(vec![AgentCardSignature {
        protected,
        signature: URL_SAFE_NO_PAD.encode(signature),
        header: None,
    }]);
    validate_card(&serde_json::to_value(&card).expect("an AgentCard always serializes"))?;
    Ok(card)
}

/// The JWS signing input, `BASE64URL(protected) || "." || BASE64URL(payload)`,
/// from both parts already in base64url.
fn signing_input(protected: &str, payload: &str) -> Vec<u8> {
    [protected.as_bytes(), b".", payload.as_bytes()].concat()
}

/// `json` as an `AgentCard`, when it is one PALS will read, its own or a
/// foreign one.
///
/// [`CardError::TooLarge`] when its compact serialization exceeds
/// [`MAX_CARD_BYTES`]. [`CardError::Invalid`] when it is not an object;
/// nests deeper than [`MAX_CARD_DEPTH`]; holds a string or key longer than
/// [`MAX_CARD_STRING_BYTES`]; has a number in an extension's `params` that
/// is not an integer within ±[`MAX_PARAMS_INTEGER`]; does not deserialize as
/// both `a2a::AgentCard` and `a2a_pb::protojson::AgentCard`; has more than
/// [`MAX_CARD_INTERFACES`] interfaces, [`MAX_CARD_SKILLS`] skills,
/// [`MAX_CARD_EXTENSIONS`] extensions or [`MAX_CARD_SIGNATURES`]
/// signatures; or has no interface with protocol version `1.0`.
pub fn validate_card(json: &Value) -> Result<AgentCard, CardError> {
    use CardError::Invalid;

    if !json.is_object() {
        return Err(Invalid("not a JSON object"));
    }
    let size = serde_json::to_vec(json)
        .expect("a serde_json::Value always serializes")
        .len();
    if size > MAX_CARD_BYTES {
        return Err(CardError::TooLarge);
    }
    check_depth_and_strings(json, 1)?;
    for ext in json
        .pointer("/capabilities/extensions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(params) = ext.get("params") {
            check_params_numbers(params)?;
        }
    }

    let card: AgentCard =
        serde_json::from_value(json.clone()).map_err(|_| Invalid("not an A2A AgentCard"))?;
    serde_json::from_value::<a2a_pb::protojson::AgentCard>(json.clone())
        .map_err(|_| Invalid("not a protobuf AgentCard"))?;
    check_content(&card)?;
    Ok(card)
}

/// The element counts and the A2A 1.0 interface that [`validate_card`]
/// requires of `card`.
fn check_content(card: &AgentCard) -> Result<(), CardError> {
    use CardError::Invalid;

    if card.supported_interfaces.len() > MAX_CARD_INTERFACES {
        return Err(Invalid("more than 8 interfaces"));
    }
    if card.skills.len() > MAX_CARD_SKILLS {
        return Err(Invalid("more than 64 skills"));
    }
    if card.capabilities.extensions.as_ref().map_or(0, Vec::len) > MAX_CARD_EXTENSIONS {
        return Err(Invalid("more than 16 extensions"));
    }
    if card.signatures.as_ref().map_or(0, Vec::len) > MAX_CARD_SIGNATURES {
        return Err(Invalid("more than 4 signatures"));
    }
    if !card
        .supported_interfaces
        .iter()
        .any(|iface| iface.protocol_version == a2a::VERSION)
    {
        return Err(Invalid("no A2A 1.0 interface"));
    }
    Ok(())
}

/// `value`, at nesting `depth`, has no deeper container than
/// [`MAX_CARD_DEPTH`] and no longer string or key than
/// [`MAX_CARD_STRING_BYTES`].
fn check_depth_and_strings(value: &Value, depth: usize) -> Result<(), CardError> {
    let too_long = |s: &str| s.len() > MAX_CARD_STRING_BYTES;
    match value {
        Value::String(s) if too_long(s) => Err(CardError::Invalid("string longer than 4 KiB")),
        Value::Array(_) | Value::Object(_) if depth > MAX_CARD_DEPTH => {
            Err(CardError::Invalid("nested deeper than 16"))
        }
        Value::Array(items) => items
            .iter()
            .try_for_each(|item| check_depth_and_strings(item, depth + 1)),
        Value::Object(map) => map.iter().try_for_each(|(key, item)| {
            if too_long(key) {
                return Err(CardError::Invalid("string longer than 4 KiB"));
            }
            check_depth_and_strings(item, depth + 1)
        }),
        _ => Ok(()),
    }
}

/// Every number in `params` is an integer within ±[`MAX_PARAMS_INTEGER`].
fn check_params_numbers(params: &Value) -> Result<(), CardError> {
    match params {
        Value::Number(n) => {
            let ok = if let Some(i) = n.as_i64() {
                i.unsigned_abs() <= MAX_PARAMS_INTEGER.unsigned_abs()
            } else if n.is_u64() {
                false
            } else {
                n.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.abs() <= MAX_PARAMS_INTEGER as f64)
            };
            if ok {
                Ok(())
            } else {
                Err(CardError::Invalid(
                    "params number that is not an integer within 2^53",
                ))
            }
        }
        Value::Array(items) => items.iter().try_for_each(check_params_numbers),
        Value::Object(map) => map.values().try_for_each(check_params_numbers),
        _ => Ok(()),
    }
}

/// `json` validated and verified as signed by the agent `expected`: one of
/// its signatures has `kid` equal to `expected` in hex and verifies under
/// `expected`'s key.
///
/// After [`validate_card`]'s errors: [`CardError::Unsigned`] without
/// signatures. Otherwise, when no signature is valid, the most telling
/// failure among them, in this order: [`CardError::BadSignature`] (a
/// signature with the expected `kid` that does not verify),
/// [`CardError::WrongSigner`] (one with another `kid`),
/// [`CardError::UnsupportedAlg`] (one with another `alg`, or with `crit`),
/// and [`CardError::Invalid`] (a protected header that is not base64url of a
/// JSON object).
///
/// The card returned is read from the bytes the valid signature covers
/// ([`VerifiedCard`]), and it must still have an A2A 1.0 interface.
pub fn verify_card(json: &Value, expected: &AgentId) -> Result<VerifiedCard, CardError> {
    let kid = expected.to_string();
    let signed = verify_signed(json, &kid, |msg, sig| expected.verify(msg, sig))?;
    Ok(VerifiedCard {
        card: signed.card,
        json: json.clone(),
        signer: *expected,
        form: signed.form,
        payload: signed.payload,
        hash: card_hash(json),
    })
}

/// `json` validated and verified as signed with `key` under `kid`, with the
/// same rules and errors as [`verify_card`].
pub fn verify_card_with_key(
    json: &Value,
    kid: &str,
    key: &VerifyingKey,
) -> Result<KeyVerifiedCard, CardError> {
    let signed = verify_signed(json, kid, |msg, sig| {
        key.verify_strict(msg, &Signature::from_bytes(sig)).is_ok()
    })?;
    Ok(KeyVerifiedCard {
        card: signed.card,
        json: json.clone(),
        kid: kid.to_owned(),
        form: signed.form,
        payload: signed.payload,
        hash: card_hash(json),
    })
}

/// What a valid signature of a card covers.
struct Signed {
    card: AgentCard,
    form: CanonicalForm,
    payload: Vec<u8>,
}

/// `json` validated, one of its signatures with `kid` verified, and the card
/// read from the bytes that signature covers, with the signatures received.
fn verify_signed(
    json: &Value,
    kid: &str,
    verify: impl Fn(&[u8], &[u8; 64]) -> bool,
) -> Result<Signed, CardError> {
    let received = validate_card(json)?;
    let signatures = received.signatures.unwrap_or_default();
    let (form, payload) = verify_signatures(json, &signatures, kid, verify)?;
    let mut card = card_in_payload(&payload, form)?;
    card.signatures = Some(signatures);
    check_content(&card)?;
    Ok(Signed {
        card,
        form,
        payload,
    })
}

/// The card in `payload`, the bytes a signature over `form` covers: how
/// `a2a-lf` reads them for [`CanonicalForm::Raw`], and how the protobuf
/// types do for the other forms, which may leave out fields `a2a-lf`
/// requires (an empty `capabilities`, empty `tags`) and never hold a key the
/// protobuf types do not read.
fn card_in_payload(payload: &[u8], form: CanonicalForm) -> Result<AgentCard, CardError> {
    let invalid = CardError::Invalid("signed content is not an AgentCard");
    match form {
        CanonicalForm::Raw => serde_json::from_slice(payload).map_err(|_| invalid),
        CanonicalForm::ProtoJson | CanonicalForm::ProtoJsonClean => {
            let value: Value = serde_json::from_slice(payload).map_err(|_| invalid)?;
            a2a_pb::protojson_conv::from_value::<AgentCard>(value).map_err(|_| invalid)
        }
    }
}

/// A canonical form of the card being verified: its bytes, and those bytes
/// in base64url.
struct Payload {
    form: CanonicalForm,
    bytes: Vec<u8>,
    base64: String,
}

/// The form in which one of `signatures`, with `kid`, verifies, and the
/// bytes it covers.
fn verify_signatures(
    json: &Value,
    signatures: &[AgentCardSignature],
    kid: &str,
    verify: impl Fn(&[u8], &[u8; 64]) -> bool,
) -> Result<(CanonicalForm, Vec<u8>), CardError> {
    if signatures.is_empty() {
        return Err(CardError::Unsigned);
    }
    // Each distinct payload once, in the order of `FORMS`: a normalized
    // card has a single one.
    let mut payloads: Vec<Payload> = Vec::with_capacity(FORMS.len());
    for form in FORMS {
        let bytes = canonical_payload(json, form)?;
        if payloads.iter().all(|seen| seen.bytes != bytes) {
            let base64 = URL_SAFE_NO_PAD.encode(&bytes);
            payloads.push(Payload {
                form,
                bytes,
                base64,
            });
        }
    }

    let mut worst = CardError::Invalid("malformed signature header");
    for signature in signatures.iter().take(MAX_CARD_SIGNATURES) {
        match check_signature(signature, kid, &payloads, &verify) {
            Ok(payload) => return Ok((payload.form, payload.bytes.clone())),
            Err(err) if rank(err) > rank(worst) => worst = err,
            Err(_) => {}
        }
    }
    Err(worst)
}

fn rank(err: CardError) -> u8 {
    match err {
        CardError::BadSignature => 3,
        CardError::WrongSigner => 2,
        CardError::UnsupportedAlg => 1,
        _ => 0,
    }
}

fn check_signature<'p>(
    signature: &AgentCardSignature,
    kid: &str,
    payloads: &'p [Payload],
    verify: &impl Fn(&[u8], &[u8; 64]) -> bool,
) -> Result<&'p Payload, CardError> {
    let malformed = CardError::Invalid("malformed signature header");
    let header = URL_SAFE_NO_PAD
        .decode(&signature.protected)
        .map_err(|_| malformed)?;
    let Ok(Value::Object(header)) = serde_json::from_slice::<Value>(&header) else {
        return Err(malformed);
    };
    if header.contains_key("crit")
        || header.get("alg").and_then(Value::as_str) != Some(CARD_JWS_ALG)
    {
        return Err(CardError::UnsupportedAlg);
    }
    if header.get("kid").and_then(Value::as_str) != Some(kid) {
        return Err(CardError::WrongSigner);
    }
    let sig: [u8; 64] = URL_SAFE_NO_PAD
        .decode(&signature.signature)
        .ok()
        .and_then(|sig| sig.try_into().ok())
        .ok_or(CardError::BadSignature)?;
    payloads
        .iter()
        .find(|payload| verify(&signing_input(&signature.protected, &payload.base64), &sig))
        .ok_or(CardError::BadSignature)
}

/// `bytes`, at most [`MAX_CARD_BYTES`], parsed as JSON and verified as
/// [`verify_card`] does: [`CardError::TooLarge`] before parsing, and
/// [`CardError::NotJson`] when they are not JSON.
pub fn parse_and_verify(bytes: &[u8], expected: &AgentId) -> Result<VerifiedCard, CardError> {
    if bytes.len() > MAX_CARD_BYTES {
        return Err(CardError::TooLarge);
    }
    let json: Value = serde_json::from_slice(bytes).map_err(|_| CardError::NotJson)?;
    verify_card(&json, expected)
}

/// The hash of `signed_card` as received: `blake3:` and the hex of BLAKE3
/// over its JCS, signatures included. It changes with any signature and
/// not with the order of keys.
pub fn card_hash(signed_card: &Value) -> CardHash {
    CardHash(format!(
        "blake3:{}",
        blake3::hash(&canonical(signed_card)).to_hex()
    ))
}
