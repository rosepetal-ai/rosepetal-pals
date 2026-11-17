//! What a router stores, with no network: the verdict on every value and
//! provider record offered to it, and the reason for each refusal. The
//! records are the hito 2 vectors `contact-v1` and `announce-v1`, at
//! 2026-11-14T12:00:00Z, and records issued here with the same key or
//! another; the provider keys are `kad-v0`'s.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use libp2p::PeerId;
use libp2p::kad::RecordKey;
use pals::dht::keys::{announce_key, contact_key, peer_id};
use pals::dht::validate::{
    Rejection, Verdict, announce_for, check_provider, check_put, contact_for,
};
use pals_core::card_jws::CardHash;
use pals_core::grant::{CLOCK_SKEW_S, format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs::{canonical, sign_domain};
use pals_core::records::{
    ANNOUNCE_SCHEMA, ANNOUNCE_SIG_DOMAIN, AnnounceRecord, CONTACT_SCHEMA, ContactRecord,
    IrohContact, MAX_ANNOUNCED_CAPABILITIES, MAX_RECORD_BYTES, RECORD_TTL_S,
};
use pals_core::signed::Signed;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The card hash of `announce-v1`.
const CARD_HASH: &str = "blake3:856acc066c7304adbae9c444204c10a0c2e3d113491fca62c1a06da74fedd4fe";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn vector(name: &str) -> Value {
    let path = format!(
        "{}/../pals-core/tests/vectors/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
}

/// A hito 2 vector's signed record as it travels: the JCS of its `signed`.
fn signed_of(vector: &Value) -> Vec<u8> {
    canonical(&vector["signed"])
}

/// The vectors' agent: its secret key is 32 bytes `0x01`.
fn vector_key() -> AgentKey {
    let key = AgentKey::from_bytes(&[0x01; 32]);
    assert_eq!(
        key.id().to_string(),
        vector("kad-v0")["agent_id"].as_str().unwrap()
    );
    key
}

fn other_key() -> AgentKey {
    AgentKey::from_bytes(&[0x02; 32])
}

/// 2026-11-14T12:00:00Z, inside both vectors' validity window.
fn now() -> i64 {
    parse_timestamp("2026-11-14T12:00:00Z").unwrap()
}

/// Valid at [`now`]: issued an hour before, for a record's whole lifetime.
fn fresh() -> (i64, i64) {
    let issued_at = now() - 3600;
    (issued_at, issued_at + RECORD_TTL_S)
}

/// A `kad-v0` provider key, by capability.
fn vector_provider_key(capability: &str) -> RecordKey {
    let vector = vector("kad-v0");
    let entry = vector["provider_keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["capability"] == capability)
        .expect("a capability of the vector");
    RecordKey::from(unhex(entry["key_hex"].as_str().unwrap()))
}

/// `signed` with the first byte of its signature flipped: the envelope and
/// the record stay well formed, and the signature no longer verifies.
fn tamper(signed: &[u8]) -> Vec<u8> {
    let mut envelope: Value = serde_json::from_slice(signed).unwrap();
    let mut signature = URL_SAFE_NO_PAD
        .decode(envelope["signature"].as_str().unwrap())
        .unwrap();
    signature[0] ^= 0x01;
    envelope["signature"] = Value::String(URL_SAFE_NO_PAD.encode(signature));
    canonical(&envelope)
}

/// The two values a router stores.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Contact,
    Announce,
}

const KINDS: [Kind; 2] = [Kind::Contact, Kind::Announce];

impl Kind {
    fn key(self, agent: &AgentId) -> RecordKey {
        match self {
            Self::Contact => contact_key(agent),
            Self::Announce => announce_key(agent),
        }
    }

    /// `key`'s signed record of this kind, of `seq`, valid in `window`.
    fn record(self, key: &AgentKey, seq: u64, window: (i64, i64)) -> Vec<u8> {
        match self {
            Self::Contact => contact(key, seq, window),
            Self::Announce => announce(key, seq, &["vision.ocr"], window),
        }
    }

    fn bad(self) -> Rejection {
        match self {
            Self::Contact => Rejection::BadContact,
            Self::Announce => Rejection::BadAnnounce,
        }
    }
}

fn contact(key: &AgentKey, seq: u64, (issued_at, expires_at): (i64, i64)) -> Vec<u8> {
    let doc = ContactRecord {
        schema: CONTACT_SCHEMA.to_owned(),
        agent_id: key.id(),
        iroh: IrohContact {
            endpoint_id: key.id(),
            relay_url: None,
            direct_addrs: vec!["192.0.2.10:4433".to_owned()],
        },
        seq,
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(expires_at).unwrap(),
    };
    Signed::issue(key, doc).unwrap().to_json()
}

fn announce(
    key: &AgentKey,
    seq: u64,
    capabilities: &[&str],
    (issued_at, expires_at): (i64, i64),
) -> Vec<u8> {
    let doc = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: key.id(),
        seq,
        agent_card_hash: CARD_HASH.parse::<CardHash>().unwrap(),
        capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
        discovery_alpn: "agentnet-discovery/0".to_owned(),
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(expires_at).unwrap(),
    };
    Signed::issue(key, doc).unwrap().to_json()
}

/// An announce of `count` capabilities, signed by hand: `Signed::issue`
/// refuses one of more than [`MAX_ANNOUNCED_CAPABILITIES`].
fn announce_of(key: &AgentKey, count: usize) -> (Vec<u8>, Vec<String>) {
    let capabilities: Vec<String> = (0..count).map(|i| format!("example:cap-{i}")).collect();
    let (issued_at, expires_at) = fresh();
    let doc = json!({
        "schema": ANNOUNCE_SCHEMA,
        "agent_id": key.id().to_string(),
        "seq": 1,
        "agent_card_hash": CARD_HASH,
        "capabilities": capabilities,
        "discovery_alpn": "agentnet-discovery/0",
        "issued_at": format_timestamp(issued_at).unwrap(),
        "expires_at": format_timestamp(expires_at).unwrap(),
    });
    let signature = sign_domain(key, ANNOUNCE_SIG_DOMAIN, &doc);
    let signed = canonical(&json!({ "record": doc, "signature": signature }));
    (signed, capabilities)
}

#[test]
fn vectors_are_stored_under_their_keys() {
    let agent = vector_key().id();
    let kad = vector("kad-v0");
    let (contact, announce) = (
        signed_of(&vector("contact-v1")),
        signed_of(&vector("announce-v1")),
    );

    let key = contact_key(&agent);
    assert_eq!(
        key.as_ref(),
        kad["contact_key"].as_str().unwrap().as_bytes()
    );
    assert_eq!(check_put(&key, &contact, None, now()), Verdict::Store);
    let record = contact_for(&agent, &contact, now()).unwrap();
    assert_eq!((record.doc.agent_id, record.doc.seq), (agent, 1));

    let key = announce_key(&agent);
    assert_eq!(
        key.as_ref(),
        kad["announce_key"].as_str().unwrap().as_bytes()
    );
    assert_eq!(check_put(&key, &announce, None, now()), Verdict::Store);
    let record = announce_for(&agent, &announce, now()).unwrap();
    assert_eq!(
        record.doc.capabilities,
        ["vision.ocr", "example:metal-plates"]
    );
}

#[test]
fn a_record_under_another_agents_key_is_wrong_key() {
    let other = other_key().id();
    let contact = signed_of(&vector("contact-v1"));
    let announce = signed_of(&vector("announce-v1"));
    let wrong = Verdict::Reject(Rejection::WrongKey);
    assert_eq!(
        check_put(&contact_key(&other), &contact, None, now()),
        wrong
    );
    assert_eq!(
        check_put(&announce_key(&other), &announce, None, now()),
        wrong
    );
    assert_eq!(
        contact_for(&other, &contact, now()),
        Err(Rejection::WrongKey)
    );
    assert_eq!(
        announce_for(&other, &announce, now()),
        Err(Rejection::WrongKey)
    );
}

#[test]
fn a_record_of_the_other_kind_is_refused() {
    let agent = vector_key().id();
    let contact = signed_of(&vector("contact-v1"));
    let announce = signed_of(&vector("announce-v1"));
    assert_eq!(
        check_put(&announce_key(&agent), &contact, None, now()),
        Verdict::Reject(Rejection::BadAnnounce)
    );
    assert_eq!(
        check_put(&contact_key(&agent), &announce, None, now()),
        Verdict::Reject(Rejection::BadContact)
    );
}

#[test]
fn a_tampered_contact_is_bad_contact() {
    let agent = vector_key().id();
    let contact = tamper(&signed_of(&vector("contact-v1")));
    assert_eq!(
        check_put(&contact_key(&agent), &contact, None, now()),
        Verdict::Reject(Rejection::BadContact)
    );
    let announce = tamper(&signed_of(&vector("announce-v1")));
    assert_eq!(
        check_put(&announce_key(&agent), &announce, None, now()),
        Verdict::Reject(Rejection::BadAnnounce)
    );
}

#[test]
fn a_value_over_4_kib_is_too_large_before_it_is_read() {
    assert_eq!(MAX_RECORD_BYTES, 4096);
    let agent = vector_key().id();
    let over = vec![b'x'; MAX_RECORD_BYTES + 1];
    let at_the_limit = vec![b'x'; MAX_RECORD_BYTES];
    for kind in KINDS {
        let key = kind.key(&agent);
        assert_eq!(
            check_put(&key, &over, None, now()),
            Verdict::Reject(Rejection::TooLarge),
            "{kind:?}"
        );
        assert_eq!(
            check_put(&key, &at_the_limit, None, now()),
            Verdict::Reject(kind.bad()),
            "{kind:?}"
        );
    }
    assert_eq!(contact_for(&agent, &over, now()), Err(Rejection::TooLarge));
    assert_eq!(announce_for(&agent, &over, now()), Err(Rejection::TooLarge));
    // The key is read first.
    assert_eq!(
        check_put(&RecordKey::new(&"/ipfs/x"), &over, None, now()),
        Verdict::Reject(Rejection::UnknownKey)
    );
}

#[test]
fn records_follow_the_clock_skew() {
    assert_eq!(CLOCK_SKEW_S, 60);
    let key = vector_key();
    let ahead = |s: i64| (now() + s, now() + s + RECORD_TTL_S);
    let behind = |s: i64| (now() - s - RECORD_TTL_S, now() - s);
    for kind in KINDS {
        let at = kind.key(&key.id());
        let verdict = |window| check_put(&at, &kind.record(&key, 1, window), None, now());
        assert_eq!(verdict(ahead(59)), Verdict::Store, "{kind:?}");
        assert_eq!(
            verdict(ahead(61)),
            Verdict::Reject(Rejection::NotYetValid),
            "{kind:?}"
        );
        assert_eq!(verdict(behind(59)), Verdict::Store, "{kind:?}");
        assert_eq!(
            verdict(behind(61)),
            Verdict::Reject(Rejection::Expired),
            "{kind:?}"
        );
    }
}

#[test]
fn a_lower_seq_waits_for_the_held_one() {
    let key = vector_key();
    for kind in KINDS {
        let at = kind.key(&key.id());
        let held = kind.record(&key, 5, fresh());
        let first = kind.record(&key, 1, fresh());
        assert_eq!(
            check_put(&at, &first, Some(&held), now()),
            Verdict::Stale,
            "{kind:?}"
        );
        assert_eq!(
            check_put(&at, &first, None, now()),
            Verdict::Store,
            "{kind:?}"
        );
        let next = kind.record(&key, 6, fresh());
        assert_eq!(
            check_put(&at, &next, Some(&held), now()),
            Verdict::Store,
            "{kind:?}"
        );
        // Until the held one expires.
        let expired = kind.record(&key, 5, (now() - 2 * RECORD_TTL_S, now() - RECORD_TTL_S));
        assert_eq!(
            check_put(&at, &first, Some(&expired), now()),
            Verdict::Store,
            "{kind:?}"
        );
    }
}

#[test]
fn the_same_record_again_is_stored() {
    let key = vector_key();
    for kind in KINDS {
        let at = kind.key(&key.id());
        let record = kind.record(&key, 5, fresh());
        assert_eq!(
            check_put(&at, &record, Some(&record), now()),
            Verdict::Store,
            "{kind:?}"
        );
    }
}

#[test]
fn another_record_with_the_held_seq_is_stale() {
    let key = vector_key();
    for kind in KINDS {
        let at = kind.key(&key.id());
        let held = kind.record(&key, 5, fresh());
        let (issued_at, expires_at) = fresh();
        let other = kind.record(&key, 5, (issued_at + 1, expires_at + 1));
        assert_ne!(held, other);
        assert_eq!(
            check_put(&at, &other, Some(&held), now()),
            Verdict::Stale,
            "{kind:?}"
        );
    }
}

#[test]
fn provider_without_its_announce_is_refused() {
    let agent = vector_key();
    let peer = peer_id(&agent.id());
    let ocr = vector_provider_key("vision.ocr");
    let refused = Verdict::Reject(Rejection::ProviderWithoutAnnounce);
    assert_eq!(check_provider(&ocr, &peer, None, now()), refused);

    // What the router holds must be a valid announce of that agent.
    let of_another = announce(&other_key(), 1, &["vision.ocr"], fresh());
    let expired = announce(
        &agent,
        1,
        &["vision.ocr"],
        (now() - 2 * RECORD_TTL_S, now() - RECORD_TTL_S),
    );
    let tampered = tamper(&signed_of(&vector("announce-v1")));
    let contact = signed_of(&vector("contact-v1"));
    for held in [of_another, expired, tampered, contact] {
        assert_eq!(check_provider(&ocr, &peer, Some(&held), now()), refused);
    }
}

#[test]
fn provider_of_a_capability_its_announce_does_not_list_is_refused() {
    let peer = peer_id(&vector_key().id());
    let announce = signed_of(&vector("announce-v1"));
    let held = Some(announce.as_slice());
    let not_announced = Verdict::Reject(Rejection::CapabilityNotAnnounced);

    let review = vector_provider_key("code.rust.review");
    assert_eq!(check_provider(&review, &peer, held, now()), not_announced);

    // The SHA-256 multihash of `vision.ocr` without its prefix.
    let mut bare = vec![0x12, 0x20];
    bare.extend_from_slice(&Sha256::digest(b"vision.ocr"));
    assert_eq!(
        check_provider(&RecordKey::from(bare), &peer, held, now()),
        not_announced
    );

    for capability in ["vision.ocr", "example:metal-plates"] {
        let key = vector_provider_key(capability);
        assert_eq!(
            check_provider(&key, &peer, held, now()),
            Verdict::Store,
            "{capability}"
        );
    }
}

#[test]
fn an_announce_lists_at_most_32_capabilities() {
    assert_eq!(MAX_ANNOUNCED_CAPABILITIES, 32);
    let key = vector_key();
    let peer = peer_id(&key.id());
    let at = announce_key(&key.id());

    let (most, capabilities) = announce_of(&key, MAX_ANNOUNCED_CAPABILITIES);
    assert_eq!(check_put(&at, &most, None, now()), Verdict::Store);
    for capability in &capabilities {
        let mut provider = vec![0x12, 0x20];
        provider.extend_from_slice(&Sha256::digest(capability.as_bytes()));
        assert_eq!(
            check_provider(&RecordKey::from(provider), &peer, Some(&most), now()),
            Verdict::Store,
            "{capability}"
        );
    }

    let (too_many, _) = announce_of(&key, MAX_ANNOUNCED_CAPABILITIES + 1);
    assert_eq!(
        check_put(&at, &too_many, None, now()),
        Verdict::Reject(Rejection::BadAnnounce)
    );
}

#[test]
fn a_provider_key_that_is_not_a_sha256_multihash_is_refused() {
    let peer = peer_id(&vector_key().id());
    let announce = signed_of(&vector("announce-v1"));
    let digest = Sha256::digest(b"agentnet:capability:v1:vision.ocr");
    let with = |prefix: &[u8], digest: &[u8]| RecordKey::from([prefix, digest].concat());
    let keys = [
        // The capability's string itself, unhashed.
        RecordKey::new(&"agentnet:capability:v1:vision.ocr"),
        // SHA2-512's code, the length of SHA2-256.
        with(&[0x13, 0x20], &digest),
        // A length that is not 32.
        with(&[0x12, 0x1f], &digest[..31]),
        with(&[0x12, 0x20], &digest[..31]),
        with(&[0x12, 0x20], &[&digest[..], &[0]].concat()),
        RecordKey::from(Vec::new()),
        contact_key(&vector_key().id()),
    ];
    for key in keys {
        assert_eq!(
            check_provider(&key, &peer, Some(&announce), now()),
            Verdict::Reject(Rejection::BadProviderKey),
            "{key:?}"
        );
    }
}

#[test]
fn a_provider_that_is_not_an_agent_is_refused() {
    let announce = signed_of(&vector("announce-v1"));
    let ocr = vector_provider_key("vision.ocr");
    assert_eq!(
        check_provider(&ocr, &PeerId::random(), Some(&announce), now()),
        Verdict::Reject(Rejection::NotAnAgent)
    );
}

#[test]
fn unknown_keys_are_refused() {
    let agent = vector_key().id();
    let contact = signed_of(&vector("contact-v1"));
    let keys = [
        RecordKey::new(&format!("/agentnet/link/{}", peer_id(&agent))),
        RecordKey::new(&"/ipfs/x"),
        vector_provider_key("vision.ocr"),
    ];
    for key in keys {
        assert_eq!(
            check_put(&key, &contact, None, now()),
            Verdict::Reject(Rejection::UnknownKey),
            "{key:?}"
        );
    }
}

#[test]
fn rejections_have_stable_names() {
    let names = [
        (Rejection::UnknownKey, "unknown_key"),
        (Rejection::TooLarge, "too_large"),
        (Rejection::WrongKey, "wrong_key"),
        (Rejection::BadContact, "bad_contact"),
        (Rejection::BadAnnounce, "bad_announce"),
        (Rejection::Expired, "expired"),
        (Rejection::NotYetValid, "not_yet_valid"),
        (Rejection::BadProviderKey, "bad_provider_key"),
        (Rejection::NotAnAgent, "not_an_agent"),
        (
            Rejection::ProviderWithoutAnnounce,
            "provider_without_announce",
        ),
        (
            Rejection::CapabilityNotAnnounced,
            "capability_not_announced",
        ),
        (Rejection::StoreFull, "store_full"),
    ];
    assert_eq!(Rejection::ALL, names.map(|(rejection, _)| rejection));
    // The protocol's table of reasons lists every one.
    let path = format!(
        "{}/../../docs/protocols/agentnet-kad-0.md",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = std::fs::read_to_string(path).expect("the document");
    for (rejection, name) in names {
        assert_eq!(rejection.as_str(), name);
        assert!(
            doc.contains(&format!("\n| `{name}` |")),
            "{name} is not listed"
        );
    }
}
