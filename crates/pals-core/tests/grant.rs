//! Signed direct grants: token format, signature, validity rules and the
//! frozen vector.

use std::os::unix::fs::PermissionsExt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pals_core::grant::{
    CLOCK_SKEW_S, GRANT_SCHEMA, GRANT_SIG_DOMAIN, Grant, GrantCheck, GrantError, MAX_TOKEN_BYTES,
    Operation, SignedGrant,
};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs;
use serde_json::{Value, json};

/// 2026-11-14T00:00:00Z.
const ISSUED_AT: i64 = 1_794_614_400;
/// 2026-11-15T00:00:00Z.
const EXPIRES_AT: i64 = 1_794_700_800;

/// The key whose secret is 32 bytes of `byte`.
fn fixed_key(byte: u8) -> AgentKey {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.key");
    std::fs::write(&path, [byte; 32]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    AgentKey::load_or_create(&path).unwrap()
}

struct Parties {
    issuer: AgentKey,
    subject: AgentId,
    audience: AgentId,
}

impl Parties {
    fn new() -> Self {
        Self {
            issuer: AgentKey::generate(),
            subject: AgentKey::generate().id(),
            audience: AgentKey::generate().id(),
        }
    }

    fn grant(&self) -> Grant {
        Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: "g-1".to_owned(),
            issuer: self.issuer.id(),
            subject: self.subject,
            audience: self.audience,
            project: "demo".to_owned(),
            resources: vec!["skill:ask".to_owned(), "blob:*".to_owned()],
            operations: vec![Operation::TaskSend, Operation::TaskRead],
            issued_at: "2026-11-14T00:00:00Z".to_owned(),
            expires_at: "2026-11-15T00:00:00Z".to_owned(),
        }
    }

    fn signed(&self) -> SignedGrant {
        SignedGrant::issue(&self.issuer, self.grant()).unwrap()
    }

    /// A check that the signed grant passes.
    fn check<'a>(&'a self, accepted: &'a [AgentId]) -> GrantCheck<'a> {
        GrantCheck {
            now_unix: ISSUED_AT + 3600,
            audience: &self.audience,
            subject: &self.subject,
            accepted_issuers: accepted,
            operation: Operation::TaskSend,
            resource: "skill:ask",
            revoked: false,
        }
    }
}

fn decode_token(token: &str) -> Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token).unwrap()).unwrap()
}

fn encode_token(envelope: &Value) -> String {
    URL_SAFE_NO_PAD.encode(jcs::canonical(envelope))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn token_roundtrip() {
    let parties = Parties::new();
    let g = parties.signed();
    let token = g.to_token();
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "base64url without padding: {token}"
    );
    assert_eq!(
        SignedGrant::from_token(&g.to_token())
            .unwrap()
            .grant
            .grant_id,
        "g-1"
    );
    assert_eq!(SignedGrant::from_token(&token).unwrap(), g);
}

#[test]
fn any_tampered_field_breaks_signature() {
    let parties = Parties::new();
    let envelope = decode_token(&parties.signed().to_token());
    let other = AgentKey::generate().id().to_string();
    let tampers: Vec<(&str, Value)> = vec![
        ("grant_id", json!("g-2")),
        ("issuer", json!(other)),
        ("subject", json!(other)),
        ("audience", json!(other)),
        ("project", json!("other")),
        ("resources", json!(["skill:ask", "blob:*", "skill:other"])),
        (
            "operations",
            json!(["task.send", "task.read", "task.cancel"]),
        ),
        ("issued_at", json!("2026-11-13T00:00:00Z")),
        ("expires_at", json!("2026-12-15T00:00:00Z")),
    ];
    for (field, value) in tampers {
        let mut tampered = envelope.clone();
        tampered["grant"][field] = value;
        assert_eq!(
            SignedGrant::from_token(&encode_token(&tampered)),
            Err(GrantError::BadSignature),
            "tampered {field}"
        );
    }

    let mut tampered = envelope.clone();
    let mut sig = URL_SAFE_NO_PAD
        .decode(tampered["signature"].as_str().unwrap())
        .unwrap();
    sig[10] ^= 0x40;
    tampered["signature"] = json!(URL_SAFE_NO_PAD.encode(&sig));
    assert_eq!(
        SignedGrant::from_token(&encode_token(&tampered)),
        Err(GrantError::BadSignature),
        "tampered signature"
    );
}

#[test]
fn unknown_field_is_malformed() {
    let parties = Parties::new();
    let envelope = decode_token(&parties.signed().to_token());

    // Signed again by the issuer, so only the unknown field is wrong.
    let mut grant = envelope["grant"].clone();
    grant["delegate"] = json!(true);
    let signature = jcs::sign_domain(&parties.issuer, GRANT_SIG_DOMAIN, &grant);
    let token = encode_token(&json!({ "grant": grant, "signature": signature }));
    assert_eq!(SignedGrant::from_token(&token), Err(GrantError::Malformed));

    // Without signing again, the signature over the received value fails
    // first: the grant is never deserialized.
    let mut unsigned = envelope.clone();
    unsigned["grant"]["delegate"] = json!(true);
    assert_eq!(
        SignedGrant::from_token(&encode_token(&unsigned)),
        Err(GrantError::BadSignature)
    );

    // An unknown field next to the grant and its signature.
    let mut outer = envelope.clone();
    outer["delegate"] = json!(true);
    assert_eq!(
        SignedGrant::from_token(&encode_token(&outer)),
        Err(GrantError::Malformed)
    );
}

#[test]
fn expired_beyond_skew_is_expired() {
    let parties = Parties::new();
    let accepted = [parties.issuer.id()];
    let check = GrantCheck {
        now_unix: EXPIRES_AT + CLOCK_SKEW_S + 1,
        ..parties.check(&accepted)
    };
    assert_eq!(check.now_unix, EXPIRES_AT + 61);
    assert_eq!(parties.signed().check(&check), Err(GrantError::Expired));
}

#[test]
fn expiry_within_skew_is_accepted() {
    let parties = Parties::new();
    let accepted = [parties.issuer.id()];
    let check = GrantCheck {
        now_unix: EXPIRES_AT + 59,
        ..parties.check(&accepted)
    };
    assert_eq!(parties.signed().check(&check), Ok(()));
}

#[test]
fn issued_in_future_beyond_skew() {
    let parties = Parties::new();
    let accepted = [parties.issuer.id()];
    let g = parties.signed();
    let early = GrantCheck {
        now_unix: ISSUED_AT - 61,
        ..parties.check(&accepted)
    };
    assert_eq!(g.check(&early), Err(GrantError::NotYetValid));
    let within_skew = GrantCheck {
        now_unix: ISSUED_AT - 59,
        ..parties.check(&accepted)
    };
    assert_eq!(g.check(&within_skew), Ok(()));
}

#[test]
fn wrong_subject_audience_issuer() {
    let parties = Parties::new();
    let accepted = [parties.issuer.id()];
    let g = parties.signed();
    let stranger = AgentKey::generate().id();

    let check = GrantCheck {
        subject: &stranger,
        ..parties.check(&accepted)
    };
    assert_eq!(g.check(&check), Err(GrantError::WrongSubject));

    let check = GrantCheck {
        audience: &stranger,
        ..parties.check(&accepted)
    };
    assert_eq!(g.check(&check), Err(GrantError::WrongAudience));

    let others = [stranger];
    assert_eq!(
        g.check(&parties.check(&others)),
        Err(GrantError::IssuerNotAccepted)
    );
    assert_eq!(
        g.check(&parties.check(&[])),
        Err(GrantError::IssuerNotAccepted)
    );
    let several = [stranger, parties.issuer.id()];
    assert_eq!(g.check(&parties.check(&several)), Ok(()));

    for err in [
        GrantError::WrongSubject,
        GrantError::WrongAudience,
        GrantError::IssuerNotAccepted,
    ] {
        assert!(err.is_authentication(), "{err:?}");
    }
}

#[test]
fn operation_and_resource_matching() {
    let parties = Parties::new();
    let accepted = [parties.issuer.id()];
    let g = parties.signed();
    let with = |operation, resource| GrantCheck {
        operation,
        resource,
        ..parties.check(&accepted)
    };

    assert_eq!(g.check(&with(Operation::TaskSend, "skill:ask")), Ok(()));
    assert_eq!(g.check(&with(Operation::TaskRead, "skill:ask")), Ok(()));
    assert_eq!(g.check(&with(Operation::TaskRead, "blob:ab12")), Ok(()));
    assert_eq!(
        g.check(&with(Operation::TaskCancel, "skill:ask")),
        Err(GrantError::OperationNotGranted)
    );
    assert_eq!(
        g.check(&with(Operation::BlobFetch, "blob:ab12")),
        Err(GrantError::OperationNotGranted)
    );
    for resource in [
        "skill:other",
        "skill:as",
        "skill:ask2",
        "blo",
        "x:blob:ab12",
    ] {
        assert_eq!(
            g.check(&with(Operation::TaskSend, resource)),
            Err(GrantError::ResourceNotGranted),
            "{resource}"
        );
    }

    assert!(!GrantError::OperationNotGranted.is_authentication());
    assert!(!GrantError::ResourceNotGranted.is_authentication());

    let names: Vec<Value> = [
        Operation::TaskSend,
        Operation::TaskRead,
        Operation::TaskCancel,
        Operation::BlobFetch,
    ]
    .iter()
    .map(|op| serde_json::to_value(op).unwrap())
    .collect();
    assert_eq!(
        names,
        [
            json!("task.send"),
            json!("task.read"),
            json!("task.cancel"),
            json!("blob.fetch")
        ]
    );
}

#[test]
fn revoked_flag_wins() {
    let parties = Parties::new();
    let accepted = [parties.issuer.id()];
    let g = parties.signed();
    let revoked = GrantCheck {
        revoked: true,
        ..parties.check(&accepted)
    };
    assert_eq!(g.check(&revoked), Err(GrantError::Revoked));
    let revoked_and_expired = GrantCheck {
        now_unix: EXPIRES_AT + 3600,
        ..revoked
    };
    assert_eq!(g.check(&revoked_and_expired), Err(GrantError::Revoked));
    assert!(GrantError::Revoked.is_authentication());
}

#[test]
fn token_over_8k_is_too_large() {
    assert_eq!(MAX_TOKEN_BYTES, 8 * 1024);
    let parties = Parties::new();
    let token = parties.signed().to_token();

    let oversized = format!("{token}{}", "A".repeat(MAX_TOKEN_BYTES + 1 - token.len()));
    assert_eq!(oversized.len(), MAX_TOKEN_BYTES + 1);
    assert_eq!(
        SignedGrant::from_token(&oversized),
        Err(GrantError::TooLarge)
    );

    // At the limit the size is accepted and the content is judged instead.
    let at_limit = &oversized[..MAX_TOKEN_BYTES];
    assert_eq!(
        SignedGrant::from_token(at_limit),
        Err(GrantError::Malformed)
    );

    // A grant whose token would not fit is never issued.
    let mut big = parties.grant();
    big.resources = (0..200).map(|i| format!("blob:{i:064x}")).collect();
    assert_eq!(
        SignedGrant::issue(&parties.issuer, big),
        Err(GrantError::TooLarge)
    );
}

#[test]
fn frozen_vector_matches() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/grant-v1.json");
    let vector: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap();
    let key = fixed_key(0x01);
    assert_eq!(vector["signing_key_hex"], json!(hex(&[0x01; 32])));

    // The inputs the vector was frozen with.
    let grant_value = vector["grant"].clone();
    let grant: Grant = serde_json::from_value(grant_value.clone()).unwrap();
    let expected = Grant {
        schema: "agentnet.grant/1".to_owned(),
        grant_id: "g-vector-1".to_owned(),
        issuer: key.id(),
        subject: fixed_key(0x02).id(),
        audience: fixed_key(0x03).id(),
        project: "demo".to_owned(),
        resources: vec!["skill:ask".to_owned()],
        operations: vec![Operation::TaskSend, Operation::TaskRead],
        issued_at: "2026-11-14T00:00:00Z".to_owned(),
        expires_at: "2026-11-15T00:00:00Z".to_owned(),
    };
    assert_eq!(grant, expected);

    // Recomputed, byte for byte.
    let canonical = jcs::canonical(&grant_value);
    assert_eq!(json!(hex(&canonical)), vector["canonical_hex"]);
    let signature = jcs::sign_domain(&key, GRANT_SIG_DOMAIN, &grant_value);
    assert_eq!(json!(signature), vector["signature"]);
    let issued = SignedGrant::issue(&key, expected).unwrap();
    assert_eq!(json!(issued.signature), vector["signature"]);
    assert_eq!(json!(issued.to_token()), vector["token"]);

    // And the frozen token is accepted as is.
    let token = vector["token"].as_str().unwrap();
    assert_eq!(SignedGrant::from_token(token).unwrap(), issued);
}
