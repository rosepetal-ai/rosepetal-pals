//! Agent Card signatures (JWS `EdDSA`): canonical forms, normalization,
//! validation limits, the card hash and the frozen vector.
//!
//! The agent key is 32 bytes of `0x01` unless a test says otherwise.

use std::collections::HashMap;
use std::sync::Arc;

use a2a::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill,
    HttpAuthSecurityScheme, SecurityScheme,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::SigningKey;
use pals_core::card_jws::{
    CARD_JWS_ALG, CARD_JWS_TYP, CanonicalForm, CardError, CardHash, CardSigner, MAX_CARD_BYTES,
    canonical_payload, card_hash, normalize, parse_and_verify, sign_card, validate_card,
    verify_card, verify_card_with_key,
};
use pals_core::events::EVENT_SIG_DOMAIN;
use pals_core::grant::GRANT_SIG_DOMAIN;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const BINDING_JSONRPC: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/jsonrpc";
const BINDING_HTTP_JSON: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/http-json";
const IDEMPOTENCY_EXTENSION: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/extensions/idempotency/v1";
const INVENTORY_EXTENSION: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/extensions/inventory/v1";

/// A named change to a card.
type Edit<T> = (&'static str, fn(&mut T));

const FORMS: [CanonicalForm; 3] = [
    CanonicalForm::Raw,
    CanonicalForm::ProtoJson,
    CanonicalForm::ProtoJsonClean,
];

/// The key whose secret is 32 bytes of `byte`.
fn fixed_key(byte: u8) -> Arc<AgentKey> {
    Arc::new(AgentKey::from_bytes(&[byte; 32]))
}

fn signer(byte: u8) -> (CardSigner, AgentId) {
    let key = fixed_key(byte);
    let id = key.id();
    (CardSigner::new(key), id)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The card of the frozen vector, for the agent `id`, as a builder would
/// write it: the idempotency extension still says `required: false`, which
/// normalization drops.
fn vector_card(id: &AgentId) -> AgentCard {
    let idempotency = AgentExtension {
        uri: IDEMPOTENCY_EXTENSION.to_owned(),
        description: Some(
            "Opt-in idempotency by messageId: a retry with the same messageId and content \
             attaches to the first request's task or answer instead of running again."
                .to_owned(),
        ),
        required: Some(false),
        params: None,
    };
    let inventory = AgentExtension {
        uri: INVENTORY_EXTENSION.to_owned(),
        description: Some(
            "Signed inventory views and service queries over agentnet-discovery/0.".to_owned(),
        ),
        required: None,
        params: Some(HashMap::from([
            ("profile".to_owned(), json!("agentnet.inventory/1")),
            ("discoveryAlpn".to_owned(), json!("agentnet-discovery/0")),
        ])),
    };
    let ask = AgentSkill {
        id: "ask".to_owned(),
        name: "Ask".to_owned(),
        description: "Answers a question with the context of one authorized project.".to_owned(),
        tags: vec!["ask".to_owned()],
        examples: None,
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    };
    let ocr = AgentSkill {
        id: "ocr-metal-v1".to_owned(),
        name: "OCR on metal parts".to_owned(),
        description: "Lee el texto grabado en piezas metálicas.".to_owned(),
        tags: vec!["agentnet:capability:v1:vision.ocr".to_owned()],
        examples: None,
        input_modes: Some(vec!["image/png".to_owned(), "image/jpeg".to_owned()]),
        output_modes: Some(vec!["text/plain".to_owned()]),
        security_requirements: None,
    };
    let grant_scheme = SecurityScheme::HttpAuth(HttpAuthSecurityScheme {
        scheme: "Bearer".to_owned(),
        description: None,
        bearer_format: Some("agentnet.grant/1".to_owned()),
    });
    AgentCard {
        name: "vector".to_owned(),
        description: "Frozen card vector".to_owned(),
        version: "0.0.0".to_owned(),
        supported_interfaces: vec![
            AgentInterface::new(format!("iroh://{id}/jsonrpc"), BINDING_JSONRPC),
            AgentInterface::new(format!("iroh://{id}/rest"), BINDING_HTTP_JSON),
        ],
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            extensions: Some(vec![idempotency, inventory]),
            extended_agent_card: Some(true),
        },
        default_input_modes: vec!["text/plain".to_owned(), "*/*".to_owned()],
        default_output_modes: vec![
            "text/plain".to_owned(),
            "application/octet-stream".to_owned(),
        ],
        skills: vec![ask, ocr],
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: Some(HashMap::from([("agentnetGrant".to_owned(), grant_scheme)])),
        security_requirements: Some(vec![HashMap::from([(
            "agentnetGrant".to_owned(),
            vec!["skill:ask".to_owned()],
        )])]),
        signatures: None,
    }
}

/// The vector card signed by the key of `byte`, as JSON, and that key's id.
fn signed_json(byte: u8) -> (Value, AgentId) {
    let (signer, id) = signer(byte);
    let signed = sign_card(&signer, vector_card(&id)).unwrap();
    (serde_json::to_value(signed).unwrap(), id)
}

/// The JWS signing input: `protected || "." || BASE64URL(payload)`.
fn signing_input(protected: &str, payload: &[u8]) -> Vec<u8> {
    format!("{protected}.{}", URL_SAFE_NO_PAD.encode(payload)).into_bytes()
}

fn eddsa_header(kid: &str) -> Value {
    json!({"alg": "EdDSA", "kid": kid, "typ": "JOSE"})
}

/// A signature entry made by `key` over `card` in `form`, with the protected
/// header `header`.
fn jws_entry(key: &AgentKey, header: &Value, card: &Value, form: CanonicalForm) -> Value {
    let protected = URL_SAFE_NO_PAD.encode(jcs::canonical(header));
    let payload = canonical_payload(card, form).unwrap();
    let signature = key.sign(&signing_input(&protected, &payload));
    json!({"protected": protected, "signature": URL_SAFE_NO_PAD.encode(signature)})
}

fn with_signatures(card: &Value, signatures: Vec<Value>) -> Value {
    let mut card = card.clone();
    card["signatures"] = Value::Array(signatures);
    card
}

fn unsigned(card: &Value) -> Value {
    let mut card = card.clone();
    card.as_object_mut().unwrap().remove("signatures");
    card
}

/// A card as another SDK might write it, with proto3 default values that a
/// normalized card never has: an extension with `"required": false` and a
/// security requirement with an empty scope list.
fn foreign_card() -> Value {
    json!({
        "name": "foreign",
        "description": "A card another SDK wrote",
        "version": "1.2.3",
        "supportedInterfaces": [{
            "url": "https://agent.example/a2a",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }],
        "capabilities": {
            "streaming": false,
            "extensions": [{"uri": "https://agent.example/ext/v1", "required": false}]
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{"id": "s", "name": "S", "description": "Does s.", "tags": ["s"]}],
        "securitySchemes": {"bearer": {"httpAuthSecurityScheme": {"scheme": "Bearer"}}},
        "securityRequirements": [{"schemes": {"bearer": {"list": []}}}]
    })
}

/// A JSON object nested `levels` deep: `{"a": {"a": … "leaf" …}}`.
fn nested(levels: usize) -> Value {
    (0..levels).fold(json!("leaf"), |inner, _| json!({ "a": inner }))
}

fn skill_json(i: usize, description: &str) -> Value {
    json!({"id": format!("s{i}"), "name": "S", "description": description, "tags": ["s"]})
}

/// `card`'s compact serialization grown to exactly `target` bytes with extra
/// skills, every string under 4 KiB.
fn padded_to(card: &Value, target: usize) -> Value {
    let mut card = card.clone();
    let base = serde_json::to_vec(&card).unwrap().len();
    let count = (target - base).div_ceil(3500);
    let skills = card["skills"].as_array_mut().unwrap();
    let first = skills.len();
    skills.extend((first..first + count).map(|i| skill_json(i, "")));
    let mut missing = target - serde_json::to_vec(&card).unwrap().len();
    let skills = card["skills"].as_array_mut().unwrap();
    for skill in &mut skills[first..] {
        let len = missing.min(4000);
        skill["description"] = json!("x".repeat(len));
        missing -= len;
    }
    assert_eq!(missing, 0);
    card
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut block = [0u8; 64];
    block[..key.len()].copy_from_slice(key);
    let ipad: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize()
        .to_vec()
}

/// `value` written with every object's keys in reverse order.
fn reversed_text(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .rev()
                .map(|(k, v)| format!("{}:{}", Value::String(k.clone()), reversed_text(v)))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(reversed_text).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

#[test]
fn sign_then_verify_roundtrip() {
    let (json, id) = signed_json(0x01);
    let verified = verify_card(&json, &id).unwrap();
    assert_eq!(verified.signer, id);
    assert_eq!(verified.form, CanonicalForm::Raw);
    assert_eq!(verified.json, json);
    assert_eq!(verified.hash, card_hash(&json));
    assert_eq!(serde_json::to_value(&verified.card).unwrap(), json);

    // The same, from the bytes a peer serves.
    let bytes = serde_json::to_vec_pretty(&json).unwrap();
    assert_eq!(parse_and_verify(&bytes, &id).unwrap().signer, id);
    assert_eq!(parse_and_verify(b"{not json", &id), Err(CardError::NotJson));
}

#[test]
fn protected_header_is_exactly_eddsa_kid_typ() {
    let (signer, id) = signer(0x01);
    assert_eq!(signer.kid(), id.to_string());
    let card = sign_card(&signer, vector_card(&id)).unwrap();
    let signatures = card.signatures.unwrap();
    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].header, None);
    let decoded =
        String::from_utf8(URL_SAFE_NO_PAD.decode(&signatures[0].protected).unwrap()).unwrap();
    assert_eq!(
        decoded,
        format!(r#"{{"alg":"EdDSA","kid":"{id}","typ":"JOSE"}}"#)
    );
    assert_eq!((CARD_JWS_ALG, CARD_JWS_TYP), ("EdDSA", "JOSE"));

    // Signing again replaces the signature instead of adding one.
    let mut resigned = vector_card(&id);
    resigned.signatures = Some(signatures.clone());
    let resigned = sign_card(&signer, resigned).unwrap();
    assert_eq!(resigned.signatures.unwrap(), signatures);
}

#[test]
fn any_tampered_field_breaks_the_signature() {
    let (json, id) = signed_json(0x01);
    let tampers: [Edit<Value>; 4] = [
        ("name", |c| c["name"] = json!("other")),
        ("skills[0].description", |c| {
            c["skills"][0]["description"] = json!("Something else.")
        }),
        ("capabilities.streaming", |c| {
            c["capabilities"]["streaming"] = json!(false)
        }),
        ("supportedInterfaces[0].url", |c| {
            c["supportedInterfaces"][0]["url"] = json!("iroh://elsewhere/jsonrpc")
        }),
    ];
    for (field, tamper) in tampers {
        let mut tampered = json.clone();
        tamper(&mut tampered);
        assert_eq!(
            verify_card(&tampered, &id).map(|v| v.signer),
            Err(CardError::BadSignature),
            "tampered {field}"
        );
    }

    let mut tampered = json.clone();
    let mut sig = URL_SAFE_NO_PAD
        .decode(tampered["signatures"][0]["signature"].as_str().unwrap())
        .unwrap();
    sig[10] ^= 0x40;
    tampered["signatures"][0]["signature"] = json!(URL_SAFE_NO_PAD.encode(&sig));
    assert_eq!(
        verify_card(&tampered, &id).map(|v| v.signer),
        Err(CardError::BadSignature),
        "tampered signature"
    );
}

#[test]
fn signature_by_another_key_is_wrong_signer() {
    let (json, k2) = signed_json(0x02);
    let k1 = fixed_key(0x01).id();
    assert_eq!(verify_card(&json, &k2).unwrap().signer, k2);
    assert_eq!(
        verify_card(&json, &k1).map(|v| v.signer),
        Err(CardError::WrongSigner)
    );
}

#[test]
fn kid_that_lies_is_bad_signature() {
    let (json, k1) = signed_json(0x01);
    let k2 = fixed_key(0x02);
    let card = unsigned(&json);
    let lying = jws_entry(
        &k2,
        &eddsa_header(&k1.to_string()),
        &card,
        CanonicalForm::Raw,
    );
    let lying = with_signatures(&card, vec![lying]);
    assert_eq!(
        verify_card(&lying, &k1).map(|v| v.signer),
        Err(CardError::BadSignature)
    );
}

#[test]
fn unsigned_card_is_unsigned() {
    let (json, id) = signed_json(0x01);
    let card = unsigned(&json);
    assert_eq!(
        verify_card(&card, &id).map(|v| v.signer),
        Err(CardError::Unsigned)
    );
    assert_eq!(
        verify_card(&with_signatures(&card, vec![]), &id).map(|v| v.signer),
        Err(CardError::Unsigned)
    );
}

#[test]
fn none_hs256_and_es256_are_refused() {
    let (json, id) = signed_json(0x01);
    let key = fixed_key(0x01);
    let card = unsigned(&json);
    let payload = canonical_payload(&card, CanonicalForm::Raw).unwrap();
    let entry = |header: Value, sign: &dyn Fn(&[u8]) -> Vec<u8>| {
        let protected = URL_SAFE_NO_PAD.encode(jcs::canonical(&header));
        let signature = sign(&signing_input(&protected, &payload));
        json!({"protected": protected, "signature": URL_SAFE_NO_PAD.encode(signature)})
    };
    let kid = id.to_string();
    let attempts = [
        (
            "none",
            entry(json!({"alg": "none", "kid": kid}), &|_| Vec::new()),
        ),
        (
            "HS256 keyed with the public key",
            entry(
                json!({"alg": "HS256", "kid": kid, "typ": "JOSE"}),
                &|input| hmac_sha256(id.as_bytes(), input),
            ),
        ),
        (
            "an Ed25519 signature labelled ES256",
            entry(
                json!({"alg": "ES256", "kid": kid, "typ": "JOSE"}),
                &|input| key.sign(input).to_vec(),
            ),
        ),
        (
            "the RFC 9864 name Ed25519",
            entry(
                json!({"alg": "Ed25519", "kid": kid, "typ": "JOSE"}),
                &|input| key.sign(input).to_vec(),
            ),
        ),
        (
            "no alg",
            entry(json!({"kid": kid, "typ": "JOSE"}), &|input| {
                key.sign(input).to_vec()
            }),
        ),
    ];
    for (what, signature) in attempts {
        assert_eq!(
            verify_card(&with_signatures(&card, vec![signature]), &id).map(|v| v.signer),
            Err(CardError::UnsupportedAlg),
            "{what}"
        );
    }
}

#[test]
fn crit_header_is_refused() {
    let (json, id) = signed_json(0x01);
    let key = fixed_key(0x01);
    let card = unsigned(&json);
    let header = json!({
        "alg": "EdDSA", "crit": ["exp"], "exp": 1_794_700_800, "kid": id.to_string(), "typ": "JOSE"
    });
    let signed = with_signatures(
        &card,
        vec![jws_entry(&key, &header, &card, CanonicalForm::Raw)],
    );
    assert_eq!(
        verify_card(&signed, &id).map(|v| v.signer),
        Err(CardError::UnsupportedAlg)
    );
}

#[test]
fn normalized_cards_are_fixed_points_of_every_form() {
    let (_, id) = signer(0x01);
    let normalized = normalize(vector_card(&id)).unwrap();
    assert_eq!(normalize(normalized.clone()).unwrap(), normalized);
    assert_eq!(
        normalized.capabilities.extensions.as_ref().unwrap()[0].required,
        None
    );

    let (signed, _) = signed_json(0x01);
    for card in [serde_json::to_value(&normalized).unwrap(), signed] {
        let raw = canonical_payload(&card, CanonicalForm::Raw).unwrap();
        assert_eq!(raw, jcs::canonical(&unsigned(&card)));
        for form in FORMS {
            assert_eq!(canonical_payload(&card, form).unwrap(), raw, "{form:?}");
        }
    }
}

#[test]
fn foreign_card_signed_over_the_python_form_verifies() {
    let key = fixed_key(0x07);
    let id = key.id();
    let card = foreign_card();
    let payloads: Vec<Vec<u8>> = FORMS
        .iter()
        .map(|form| canonical_payload(&card, *form).unwrap())
        .collect();
    assert_ne!(payloads[0], payloads[1]);
    assert_ne!(payloads[1], payloads[2]);
    assert_ne!(payloads[0], payloads[2]);
    // The Python SDK drops the empty scopes and the default `required`.
    let clean: Value = serde_json::from_slice(&payloads[2]).unwrap();
    assert_eq!(clean.get("securityRequirements"), None);
    assert_eq!(
        clean["capabilities"]["extensions"][0],
        json!({"uri": "https://agent.example/ext/v1"})
    );

    let entry = jws_entry(
        &key,
        &eddsa_header(&id.to_string()),
        &card,
        CanonicalForm::ProtoJsonClean,
    );
    let signed = with_signatures(&card, vec![entry]);
    let verified = verify_card(&signed, &id).unwrap();
    assert_eq!(verified.form, CanonicalForm::ProtoJsonClean);
    assert_eq!(verified.json, signed);
}

#[test]
fn foreign_card_signed_over_the_raw_form_verifies() {
    let key = fixed_key(0x07);
    let id = key.id();
    let card = foreign_card();
    let entry = jws_entry(
        &key,
        &eddsa_header(&id.to_string()),
        &card,
        CanonicalForm::Raw,
    );
    let signed = with_signatures(&card, vec![entry]);
    assert_eq!(verify_card(&signed, &id).unwrap().form, CanonicalForm::Raw);
}

#[test]
fn foreign_card_signed_over_the_protojson_form_verifies() {
    let key = fixed_key(0x07);
    let id = key.id();
    let card = foreign_card();
    let protojson: Value =
        serde_json::from_slice(&canonical_payload(&card, CanonicalForm::ProtoJson).unwrap())
            .unwrap();
    assert_eq!(
        protojson["securityRequirements"],
        json!([{"schemes": {"bearer": {}}}])
    );
    let entry = jws_entry(
        &key,
        &eddsa_header(&id.to_string()),
        &card,
        CanonicalForm::ProtoJson,
    );
    let signed = with_signatures(&card, vec![entry]);
    assert_eq!(
        verify_card(&signed, &id).unwrap().form,
        CanonicalForm::ProtoJson
    );
}

#[test]
fn normalize_refuses_empty_required_fields_and_empty_scopes() {
    let (_, id) = signer(0x01);

    // Empty optionals become `None`.
    let mut card = vector_card(&id);
    card.documentation_url = Some(String::new());
    card.capabilities.extensions.as_mut().unwrap()[1].description = Some(String::new());
    card.skills[0].examples = Some(Vec::new());
    card.supported_interfaces[0].tenant = Some(String::new());
    let mut expected = normalize(vector_card(&id)).unwrap();
    expected.capabilities.extensions.as_mut().unwrap()[1].description = None;
    assert_eq!(normalize(card).unwrap(), expected);

    let refused: [Edit<AgentCard>; 8] = [
        ("card description", |c| c.description = String::new()),
        ("card name", |c| c.name = String::new()),
        ("default input modes", |c| c.default_input_modes.clear()),
        ("skill description", |c| {
            c.skills[1].description = String::new()
        }),
        ("interface url", |c| {
            c.supported_interfaces[0].url = String::new()
        }),
        ("extension uri", |c| {
            c.capabilities.extensions.as_mut().unwrap()[0].uri = String::new()
        }),
        ("empty scopes", |c| {
            c.security_requirements = Some(vec![HashMap::from([(
                "agentnetGrant".to_owned(),
                Vec::new(),
            )])])
        }),
        ("params value that is not a string", |c| {
            c.capabilities.extensions.as_mut().unwrap()[1]
                .params
                .as_mut()
                .unwrap()
                .insert("pages".to_owned(), json!(3));
        }),
    ];
    for (what, spoil) in refused {
        let mut card = vector_card(&id);
        spoil(&mut card);
        assert!(
            matches!(normalize(card), Err(CardError::NotNormalized(_))),
            "{what}"
        );
    }
    // And sign_card normalizes first.
    let mut card = vector_card(&id);
    card.description = String::new();
    assert!(matches!(
        sign_card(&signer(0x01).0, card),
        Err(CardError::NotNormalized(_))
    ));
}

#[test]
fn empty_skill_tags_are_not_normalized() {
    let (_, id) = signer(0x01);
    let mut card = vector_card(&id);
    card.skills[1].tags.clear();
    assert!(matches!(normalize(card), Err(CardError::NotNormalized(_))));
}

#[test]
fn card_verifies_with_an_explicit_ed25519_key() {
    let signing = SigningKey::from_bytes(&[0x05; 32]);
    let key = AgentKey::from_bytes(&[0x05; 32]);
    let card = foreign_card();
    let entry = jws_entry(&key, &eddsa_header("k1"), &card, CanonicalForm::Raw);
    let signed = with_signatures(&card, vec![entry]);

    let verified = verify_card_with_key(&signed, "k1", &signing.verifying_key()).unwrap();
    assert_eq!(verified.kid, "k1");
    assert_eq!(verified.form, CanonicalForm::Raw);
    assert_eq!(verified.hash, card_hash(&signed));

    let other = SigningKey::from_bytes(&[0x06; 32]).verifying_key();
    assert_eq!(
        verify_card_with_key(&signed, "k1", &other).map(|v| v.kid),
        Err(CardError::BadSignature)
    );
    assert_eq!(
        verify_card_with_key(&signed, "k2", &signing.verifying_key()).map(|v| v.kid),
        Err(CardError::WrongSigner)
    );
}

#[test]
fn card_over_32_kib_is_too_large() {
    let (json, id) = signed_json(0x01);

    // Bytes on the wire, before parsing.
    let mut bytes = serde_json::to_vec(&json).unwrap();
    bytes.resize(MAX_CARD_BYTES, b' ');
    assert_eq!(parse_and_verify(&bytes, &id).unwrap().signer, id);
    bytes.push(b' ');
    assert_eq!(
        parse_and_verify(&bytes, &id).map(|v| v.signer),
        Err(CardError::TooLarge)
    );

    // A JSON value, serialized.
    let card = unsigned(&json);
    assert!(validate_card(&padded_to(&card, MAX_CARD_BYTES)).is_ok());
    assert_eq!(
        validate_card(&padded_to(&card, MAX_CARD_BYTES + 1)).map(|c| c.name),
        Err(CardError::TooLarge)
    );
}

#[test]
fn deep_or_huge_cards_are_invalid() {
    let (json, _) = signed_json(0x01);
    let card = unsigned(&json);
    let invalid = |card: &Value| matches!(validate_card(card), Err(CardError::Invalid(_)));

    // `params` sits at depth 5: card, capabilities, extensions, extension.
    let with_params = |params: Value| {
        let mut card = card.clone();
        card["capabilities"]["extensions"][1]["params"]["deep"] = params;
        card
    };
    assert!(validate_card(&with_params(nested(11))).is_ok());
    assert!(invalid(&with_params(nested(12))), "depth 17");

    let mut skills = card.clone();
    let list: Vec<Value> = (0..64).map(|i| skill_json(i, "d")).collect();
    skills["skills"] = Value::Array(list);
    assert!(validate_card(&skills).is_ok());
    skills["skills"]
        .as_array_mut()
        .unwrap()
        .push(skill_json(64, "d"));
    assert!(invalid(&skills), "65 skills");

    let max = (1i64 << 53) - 1;
    for ok in [json!(max), json!(-max), json!(0), json!(2.0)] {
        assert!(validate_card(&with_params(ok.clone())).is_ok(), "{ok}");
    }
    for bad in [json!(max + 1), json!(-max - 1), json!(1.5), json!(u64::MAX)] {
        assert!(invalid(&with_params(bad.clone())), "{bad}");
    }

    let mut not_a_card = card.clone();
    not_a_card["supportedInterfaces"] = json!("iroh://x");
    assert!(invalid(&not_a_card), "not an AgentCard");
    assert!(invalid(&json!(["a card"])), "not an object");
    let mut no_a2a_1 = card.clone();
    for iface in no_a2a_1["supportedInterfaces"].as_array_mut().unwrap() {
        iface["protocolVersion"] = json!("0.3");
    }
    assert!(invalid(&no_a2a_1), "no A2A 1.0 interface");
}

#[test]
fn more_than_4_signatures_is_invalid() {
    let (json, id) = signed_json(0x01);
    let signature = json["signatures"][0].clone();
    let four = with_signatures(&json, vec![signature.clone(); 4]);
    assert_eq!(verify_card(&four, &id).unwrap().signer, id);
    let five = with_signatures(&json, vec![signature; 5]);
    assert!(matches!(
        verify_card(&five, &id),
        Err(CardError::Invalid(_))
    ));
}

#[test]
fn more_than_8_interfaces_is_invalid() {
    let (json, _) = signed_json(0x01);
    let mut card = unsigned(&json);
    let iface = card["supportedInterfaces"][0].clone();
    card["supportedInterfaces"] = Value::Array(vec![iface.clone(); 8]);
    assert!(validate_card(&card).is_ok());
    card["supportedInterfaces"]
        .as_array_mut()
        .unwrap()
        .push(iface);
    assert!(matches!(validate_card(&card), Err(CardError::Invalid(_))));
}

#[test]
fn more_than_16_extensions_is_invalid() {
    let (json, _) = signed_json(0x01);
    let mut card = unsigned(&json);
    let ext = card["capabilities"]["extensions"][0].clone();
    card["capabilities"]["extensions"] = Value::Array(vec![ext.clone(); 16]);
    assert!(validate_card(&card).is_ok());
    card["capabilities"]["extensions"]
        .as_array_mut()
        .unwrap()
        .push(ext);
    assert!(matches!(validate_card(&card), Err(CardError::Invalid(_))));
}

#[test]
fn string_over_4_kib_is_invalid() {
    let (json, _) = signed_json(0x01);
    let mut card = unsigned(&json);
    card["skills"][0]["description"] = json!("x".repeat(4096));
    assert!(validate_card(&card).is_ok());
    card["skills"][0]["description"] = json!("x".repeat(4097));
    assert!(matches!(validate_card(&card), Err(CardError::Invalid(_))));

    // Object keys are strings too.
    let mut card = unsigned(&json);
    card["capabilities"]["extensions"][1]["params"]["k".repeat(4097)] = json!("v");
    assert!(matches!(validate_card(&card), Err(CardError::Invalid(_))));
}

#[test]
fn hash_covers_the_signatures_and_ignores_key_order() {
    let (json, _) = signed_json(0x01);
    let hash = card_hash(&json);
    let text = hash.to_string();
    assert_eq!(
        text,
        format!("blake3:{}", blake3::hash(&jcs::canonical(&json)).to_hex())
    );
    assert_eq!(text.len(), "blake3:".len() + 64);

    // Key order of the text received does not matter.
    let reordered = reversed_text(&json);
    assert_ne!(reordered, json.to_string());
    let reparsed: Value = serde_json::from_str(&reordered).unwrap();
    assert_eq!(card_hash(&reparsed), hash);

    // The signatures do.
    assert_ne!(card_hash(&unsigned(&json)), hash);
    let (other, _) = signed_json(0x02);
    let mut resigned = json.clone();
    resigned["signatures"] = other["signatures"].clone();
    assert_ne!(card_hash(&resigned), hash);

    // Text, parse and serde.
    assert_eq!(text.parse::<CardHash>().unwrap(), hash);
    assert_eq!(serde_json::to_value(&hash).unwrap(), json!(text));
    assert_eq!(
        serde_json::from_value::<CardHash>(json!(text)).unwrap(),
        hash
    );
    for bad in [
        text.to_uppercase(),
        text.replace("blake3:", "sha256:"),
        text[..text.len() - 1].to_owned(),
        format!("{text}0"),
        text.replace("blake3:", ""),
    ] {
        assert!(bad.parse::<CardHash>().is_err(), "{bad}");
    }
}

#[test]
fn jws_input_never_starts_like_an_agentnet_domain() {
    for byte in [0x01, 0x02, 0x03] {
        let (json, id) = signed_json(byte);
        let protected = json["signatures"][0]["protected"].as_str().unwrap();
        let payload = canonical_payload(&json, CanonicalForm::Raw).unwrap();
        let signing_input = signing_input(protected, &payload);
        // It is the input that was signed.
        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(json["signatures"][0]["signature"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(id.verify(&signing_input, &signature));

        assert!(signing_input.starts_with(b"eyJ"));
        for domain in [GRANT_SIG_DOMAIN, EVENT_SIG_DOMAIN] {
            assert!(!signing_input.starts_with(domain));
        }
        assert!(!signing_input.starts_with(b"agentnet."));
    }
}

#[test]
fn frozen_vector_matches() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/card-jws-v1.json"
    );
    let vector: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap();
    assert_eq!(vector["signing_key_hex"], json!(hex(&[0x01; 32])));
    let (signer, id) = signer(0x01);
    assert_eq!(vector["agent_id"], json!(id.to_string()));

    // The inputs the vector was frozen with, normalized.
    let normalized = normalize(vector_card(&id)).unwrap();
    assert_eq!(serde_json::to_value(&normalized).unwrap(), vector["card"]);

    // Recomputed, byte for byte.
    let payload = canonical_payload(&vector["card"], CanonicalForm::Raw).unwrap();
    assert_eq!(json!(hex(&payload)), vector["canonical_hex"]);
    let signed = serde_json::to_value(sign_card(&signer, vector_card(&id)).unwrap()).unwrap();
    assert_eq!(signed["signatures"][0]["protected"], vector["protected"]);
    assert_eq!(signed["signatures"][0]["signature"], vector["signature"]);
    assert_eq!(signed, vector["signed_card"]);
    assert_eq!(json!(card_hash(&signed).to_string()), vector["hash"]);

    // And the frozen card verifies as is, in every form.
    let frozen = &vector["signed_card"];
    let verified = verify_card(frozen, &id).unwrap();
    assert_eq!(verified.card, sign_card(&signer, vector_card(&id)).unwrap());
    assert_eq!(json!(verified.hash.to_string()), vector["hash"]);
    for form in FORMS {
        assert_eq!(
            json!(hex(&canonical_payload(frozen, form).unwrap())),
            vector["canonical_hex"],
            "{form:?}"
        );
    }
}
