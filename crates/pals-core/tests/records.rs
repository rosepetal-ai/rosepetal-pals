//! Signed contact and announce records: the signed-document envelope, its
//! domains, limits and validity, the frozen vectors, and how the store keeps
//! the agent's own records and its peers'.

use pals_core::events::EVENT_SIG_DOMAIN;
use pals_core::grant::{CLOCK_SKEW_S, GRANT_SIG_DOMAIN, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs;
use pals_core::records::{
    ANNOUNCE_SCHEMA, ANNOUNCE_SIG_DOMAIN, AnnounceRecord, CONTACT_SCHEMA, CONTACT_SIG_DOMAIN,
    ContactRecord, IrohContact, MAX_RECORD_BYTES, RECORD_RENEW_BEFORE_S, RECORD_TTL_S,
};
use pals_core::signed::{Signed, SignedDoc, SignedError, check_validity};
use pals_core::store::{RecordAccept, Store};
use serde_json::{Map, Value, json};

/// 2026-11-14T00:00:00Z.
const ISSUED_AT: i64 = 1_794_614_400;
/// 2026-11-15T00:00:00Z.
const EXPIRES_AT: i64 = ISSUED_AT + RECORD_TTL_S;
/// 2026-11-14T12:00:00Z.
const NOW: i64 = ISSUED_AT + 12 * 3600;
/// The hash of the frozen card vector, `card-jws-v1.json`.
const CARD_HASH: &str = "blake3:856acc066c7304adbae9c444204c10a0c2e3d113491fca62c1a06da74fedd4fe";

/// The key whose secret is 32 bytes of `byte`.
fn fixed_key(byte: u8) -> AgentKey {
    AgentKey::from_bytes(&[byte; 32])
}

fn ts(unix_s: i64) -> String {
    format_timestamp(unix_s).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The contact record of the frozen vector, for `id` and `seq`.
fn contact(id: AgentId, seq: u64) -> ContactRecord {
    ContactRecord {
        schema: CONTACT_SCHEMA.to_owned(),
        agent_id: id,
        iroh: IrohContact {
            endpoint_id: id,
            relay_url: Some("https://relay.example.org/".to_owned()),
            direct_addrs: vec![
                "192.0.2.10:4433".to_owned(),
                "[2001:db8::10]:4433".to_owned(),
            ],
        },
        seq,
        issued_at: ts(ISSUED_AT),
        expires_at: ts(EXPIRES_AT),
    }
}

/// The announce record of the frozen vector, for `id` and `seq`.
fn announce(id: AgentId, seq: u64) -> AnnounceRecord {
    AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: id,
        seq,
        agent_card_hash: CARD_HASH.parse().unwrap(),
        capabilities: vec!["vision.ocr".to_owned(), "example:metal-plates".to_owned()],
        discovery_alpn: "agentnet-discovery/0".to_owned(),
        issued_at: ts(ISSUED_AT),
        expires_at: ts(EXPIRES_AT),
    }
}

/// `JCS({wrapper: doc, "signature": signature})`.
fn envelope(wrapper: &str, doc: &Value, signature: &str) -> Vec<u8> {
    let mut map = Map::new();
    map.insert(wrapper.to_owned(), doc.clone());
    map.insert("signature".to_owned(), json!(signature));
    jcs::canonical(&Value::Object(map))
}

/// `doc` signed by `key` under `domain`, in `T`'s envelope, whatever the
/// document says.
fn sign_as<T: SignedDoc>(key: &AgentKey, domain: &[u8], doc: &Value) -> Vec<u8> {
    envelope(T::WRAPPER, doc, &jcs::sign_domain(key, domain, doc))
}

fn value_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn contact_and_announce_roundtrip() {
    let key = AgentKey::generate();

    let signed = Signed::issue(&key, contact(key.id(), 3)).unwrap();
    let bytes = signed.to_json();
    assert_eq!(
        value_of(&bytes),
        json!({"record": serde_json::to_value(&signed.doc).unwrap(), "signature": signed.signature})
    );
    // Canonical text, so one record has one spelling.
    assert_eq!(jcs::canonical(&value_of(&bytes)), bytes);
    let parsed = Signed::<ContactRecord>::from_json(&bytes).unwrap();
    assert_eq!(parsed, signed);
    assert_eq!(parsed.to_json(), bytes);

    // No relay and no direct address: `relay_url` is left out.
    let bare = ContactRecord {
        iroh: IrohContact {
            endpoint_id: key.id(),
            relay_url: None,
            direct_addrs: vec![],
        },
        ..contact(key.id(), 4)
    };
    let bare = Signed::issue(&key, bare).unwrap();
    assert_eq!(
        value_of(&bare.to_json())["record"]["iroh"],
        json!({"direct_addrs": [], "endpoint_id": key.id().to_string()})
    );
    assert_eq!(
        Signed::<ContactRecord>::from_json(&bare.to_json()).unwrap(),
        bare
    );

    let signed = Signed::issue(&key, announce(key.id(), 1)).unwrap();
    let bytes = signed.to_json();
    assert_eq!(
        value_of(&bytes),
        json!({"record": serde_json::to_value(&signed.doc).unwrap(), "signature": signed.signature})
    );
    let parsed = Signed::<AnnounceRecord>::from_json(&bytes).unwrap();
    assert_eq!(parsed, signed);
    assert_eq!(parsed.to_json(), bytes);
}

#[test]
fn tampered_record_is_bad_signature() {
    let key = AgentKey::generate();
    let tampered = |bytes: &[u8], edit: &dyn Fn(&mut Value)| {
        let mut value = value_of(bytes);
        edit(&mut value);
        jcs::canonical(&value)
    };

    let contact_bytes = Signed::issue(&key, contact(key.id(), 2)).unwrap().to_json();
    let edits: [&dyn Fn(&mut Value); 6] = [
        &|v| v["record"]["seq"] = json!(9),
        &|v| v["record"]["iroh"]["direct_addrs"][0] = json!("198.51.100.7:4433"),
        &|v| v["record"]["iroh"]["relay_url"] = json!("https://relay.example.net/"),
        &|v| {
            v["record"]["iroh"]
                .as_object_mut()
                .unwrap()
                .remove("relay_url");
        },
        &|v| v["record"]["expires_at"] = json!("2027-11-15T00:00:00Z"),
        &|v| v["signature"] = json!(jcs::sign_domain(&key, CONTACT_SIG_DOMAIN, &json!({}))),
    ];
    for edit in edits {
        assert_eq!(
            Signed::<ContactRecord>::from_json(&tampered(&contact_bytes, edit)),
            Err(SignedError::BadSignature)
        );
    }
    // A signature that is not one.
    for signature in ["", "not base64!", "AAAA"] {
        let bytes = tampered(&contact_bytes, &|v| v["signature"] = json!(signature));
        assert_eq!(
            Signed::<ContactRecord>::from_json(&bytes),
            Err(SignedError::BadSignature),
            "{signature:?}"
        );
    }

    let announce_bytes = Signed::issue(&key, announce(key.id(), 2))
        .unwrap()
        .to_json();
    let other_hash = format!("blake3:{}", "0".repeat(64));
    let edits: [&dyn Fn(&mut Value); 3] = [
        &|v| v["record"]["agent_card_hash"] = json!(other_hash),
        &|v| v["record"]["capabilities"][1] = json!("vision.detect"),
        &|v| v["record"]["discovery_alpn"] = json!("agentnet-discovery/1"),
    ];
    for edit in edits {
        assert_eq!(
            Signed::<AnnounceRecord>::from_json(&tampered(&announce_bytes, edit)),
            Err(SignedError::BadSignature)
        );
    }
}

#[test]
fn record_signed_for_one_domain_does_not_verify_in_another() {
    // The frozen wire identifiers.
    assert_eq!(CONTACT_SCHEMA, "agentnet.contact/1");
    assert_eq!(CONTACT_SIG_DOMAIN, b"agentnet.contact/1\n");
    assert_eq!(ANNOUNCE_SCHEMA, "agentnet.announce/1");
    assert_eq!(ANNOUNCE_SIG_DOMAIN, b"agentnet.announce/1\n");
    assert_eq!(
        (ContactRecord::DOMAIN, ContactRecord::WRAPPER),
        (CONTACT_SIG_DOMAIN, "record")
    );
    assert_eq!(
        (AnnounceRecord::DOMAIN, AnnounceRecord::WRAPPER),
        (ANNOUNCE_SIG_DOMAIN, "record")
    );

    let key = AgentKey::generate();
    // Both travel under `record`: only the domain tells them apart.
    let announce_bytes = Signed::issue(&key, announce(key.id(), 1))
        .unwrap()
        .to_json();
    assert_eq!(
        Signed::<ContactRecord>::from_json(&announce_bytes),
        Err(SignedError::BadSignature)
    );
    let contact_bytes = Signed::issue(&key, contact(key.id(), 1)).unwrap().to_json();
    assert_eq!(
        Signed::<AnnounceRecord>::from_json(&contact_bytes),
        Err(SignedError::BadSignature)
    );

    let doc = serde_json::to_value(contact(key.id(), 1)).unwrap();
    for domain in [
        ANNOUNCE_SIG_DOMAIN,
        GRANT_SIG_DOMAIN,
        EVENT_SIG_DOMAIN,
        b"agentnet.contact/2\n",
        b"agentnet.contact/1",
        b"",
    ] {
        assert_eq!(
            Signed::<ContactRecord>::from_json(&sign_as::<ContactRecord>(&key, domain, &doc)),
            Err(SignedError::BadSignature),
            "{}",
            String::from_utf8_lossy(domain)
        );
    }
    let doc = serde_json::to_value(announce(key.id(), 1)).unwrap();
    for domain in [CONTACT_SIG_DOMAIN, GRANT_SIG_DOMAIN, EVENT_SIG_DOMAIN] {
        assert_eq!(
            Signed::<AnnounceRecord>::from_json(&sign_as::<AnnounceRecord>(&key, domain, &doc)),
            Err(SignedError::BadSignature)
        );
    }
    // Under its own domain, the same document verifies.
    assert!(
        Signed::<AnnounceRecord>::from_json(&sign_as::<AnnounceRecord>(
            &key,
            ANNOUNCE_SIG_DOMAIN,
            &doc
        ))
        .is_ok()
    );
}

#[test]
fn record_naming_another_agent_is_bad_signature() {
    let a = AgentKey::generate();
    let b = AgentKey::generate();

    // B cannot issue a record that names A.
    assert_eq!(
        Signed::issue(&b, contact(a.id(), 1)),
        Err(SignedError::BadSignature)
    );
    assert_eq!(
        Signed::issue(&b, announce(a.id(), 1)),
        Err(SignedError::BadSignature)
    );

    // Nor does a record naming A, signed by B, verify; not even when its
    // endpoint is B's: the signer is the record's `agent_id`.
    for record in [
        contact(a.id(), 1),
        ContactRecord {
            iroh: IrohContact {
                endpoint_id: b.id(),
                ..contact(a.id(), 1).iroh
            },
            ..contact(a.id(), 1)
        },
    ] {
        let doc = serde_json::to_value(record).unwrap();
        let bytes = sign_as::<ContactRecord>(&b, CONTACT_SIG_DOMAIN, &doc);
        assert_eq!(
            Signed::<ContactRecord>::from_json(&bytes),
            Err(SignedError::BadSignature)
        );
    }
    let doc = serde_json::to_value(announce(a.id(), 1)).unwrap();
    let bytes = sign_as::<AnnounceRecord>(&b, ANNOUNCE_SIG_DOMAIN, &doc);
    assert_eq!(
        Signed::<AnnounceRecord>::from_json(&bytes),
        Err(SignedError::BadSignature)
    );
}

#[test]
fn contact_naming_another_endpoint_is_refused() {
    // An agent's Iroh endpoint key is its own key: A cannot advertise B's
    // endpoint as its own, though A signs the record validly.
    let a = AgentKey::generate();
    let b = AgentKey::generate();
    let elsewhere = ContactRecord {
        iroh: IrohContact {
            endpoint_id: b.id(),
            ..contact(a.id(), 1).iroh
        },
        ..contact(a.id(), 1)
    };

    assert_eq!(
        Signed::issue(&a, elsewhere.clone()),
        Err(SignedError::Malformed)
    );
    let doc = serde_json::to_value(&elsewhere).unwrap();
    let bytes = sign_as::<ContactRecord>(&a, CONTACT_SIG_DOMAIN, &doc);
    assert_eq!(
        Signed::<ContactRecord>::from_json(&bytes),
        Err(SignedError::Malformed)
    );

    // Its own endpoint, signed the same way, is accepted.
    let own = serde_json::to_value(contact(a.id(), 1)).unwrap();
    let bytes = sign_as::<ContactRecord>(&a, CONTACT_SIG_DOMAIN, &own);
    assert_eq!(
        Signed::<ContactRecord>::from_json(&bytes).unwrap(),
        Signed::issue(&a, contact(a.id(), 1)).unwrap()
    );
}

#[test]
fn expired_record_is_refused_beyond_skew() {
    assert_eq!(CLOCK_SKEW_S, 60);
    assert_eq!(RECORD_TTL_S, 24 * 3600);
    assert_eq!(RECORD_RENEW_BEFORE_S, 12 * 3600);

    let key = AgentKey::generate();
    let sent = Signed::issue(&key, contact(key.id(), 1)).unwrap();
    let received = Signed::<ContactRecord>::from_json(&sent.to_json()).unwrap();
    let (issued_at, expires_at) = (&received.doc.issued_at, &received.doc.expires_at);

    assert_eq!(
        check_validity(issued_at, expires_at, EXPIRES_AT + 61),
        Err(SignedError::Expired)
    );
    assert_eq!(
        check_validity(issued_at, expires_at, EXPIRES_AT + CLOCK_SKEW_S),
        Err(SignedError::Expired)
    );
    assert_eq!(
        check_validity(issued_at, expires_at, EXPIRES_AT + 59),
        Ok(())
    );
    assert_eq!(check_validity(issued_at, expires_at, NOW), Ok(()));
    assert_eq!(
        check_validity(issued_at, expires_at, ISSUED_AT - 59),
        Ok(())
    );
    assert_eq!(
        check_validity(issued_at, expires_at, ISSUED_AT - 61),
        Err(SignedError::NotYetValid)
    );

    for (issued_at, expires_at) in [
        ("2026-11-14T00:00:00+00:00", "2026-11-15T00:00:00Z"),
        ("2026-11-14T00:00:00Z", "2026-11-15"),
        ("", ""),
    ] {
        assert_eq!(
            check_validity(issued_at, expires_at, NOW),
            Err(SignedError::Malformed),
            "{issued_at} {expires_at}"
        );
    }
}

#[test]
fn unknown_field_is_malformed() {
    let key = AgentKey::generate();
    let contact_doc = serde_json::to_value(contact(key.id(), 1)).unwrap();
    let announce_doc = serde_json::to_value(announce(key.id(), 1)).unwrap();
    let edited = |doc: &Value, edit: &dyn Fn(&mut Value)| {
        let mut doc = doc.clone();
        edit(&mut doc);
        doc
    };
    let as_contact = |doc: &Value| {
        Signed::<ContactRecord>::from_json(&sign_as::<ContactRecord>(&key, CONTACT_SIG_DOMAIN, doc))
    };
    let as_announce = |doc: &Value| {
        Signed::<AnnounceRecord>::from_json(&sign_as::<AnnounceRecord>(
            &key,
            ANNOUNCE_SIG_DOMAIN,
            doc,
        ))
    };
    // The unedited documents verify, so each refusal below is the edit's.
    assert!(as_contact(&contact_doc).is_ok());
    assert!(as_announce(&announce_doc).is_ok());

    // Unknown fields, validly signed, are refused when deserializing.
    assert_eq!(
        as_contact(&edited(&contact_doc, &|d| d["via"] = json!("dht"))),
        Err(SignedError::Malformed)
    );
    assert_eq!(
        as_contact(&edited(&contact_doc, &|d| d["iroh"]["libp2p"] = json!("12D3"))),
        Err(SignedError::Malformed)
    );
    assert_eq!(
        as_announce(&edited(&announce_doc, &|d| d["price"] = json!(0))),
        Err(SignedError::Malformed)
    );

    // So is an envelope with anything but the record and its signature.
    let signed = Signed::issue(&key, contact(key.id(), 1)).unwrap();
    let mut extra = value_of(&signed.to_json());
    extra["note"] = json!("hi");
    assert_eq!(
        Signed::<ContactRecord>::from_json(&jcs::canonical(&extra)),
        Err(SignedError::Malformed)
    );
    for envelope in [
        envelope("contact", &contact_doc, &signed.signature),
        jcs::canonical(&json!({"record": contact_doc})),
        jcs::canonical(&json!({"record": contact_doc, "signature": 7})),
        jcs::canonical(&json!([contact_doc, signed.signature])),
        b"not json".to_vec(),
    ] {
        assert_eq!(
            Signed::<ContactRecord>::from_json(&envelope),
            Err(SignedError::Malformed)
        );
    }

    // One spelling per record: canonical JSON only, and the record exactly
    // as it serializes back, so `null` is not an absent relay.
    let text = String::from_utf8(signed.to_json()).unwrap();
    assert_eq!(
        Signed::<ContactRecord>::from_json(text.replacen(':', ": ", 1).as_bytes()),
        Err(SignedError::Malformed)
    );
    let no_relay = ContactRecord {
        iroh: IrohContact {
            relay_url: None,
            ..contact(key.id(), 1).iroh
        },
        ..contact(key.id(), 1)
    };
    let no_relay = serde_json::to_value(no_relay).unwrap();
    assert!(as_contact(&no_relay).is_ok());
    assert_eq!(
        as_contact(&edited(&no_relay, &|d| d["iroh"]["relay_url"] = Value::Null)),
        Err(SignedError::Malformed)
    );

    // Content outside the record's rules, validly signed.
    let contact_edits: [&dyn Fn(&mut Value); 9] = [
        &|d| d["agent_id"] = json!("not an agent id"),
        &|d| d["seq"] = json!(0),
        &|d| d["seq"] = json!(-1),
        &|d| d["seq"] = json!(1u64 << 53),
        &|d| d["issued_at"] = json!("2026-11-14T00:00:00.000Z"),
        &|d| d["expires_at"] = d["issued_at"].clone(),
        &|d| d["iroh"]["relay_url"] = json!(""),
        &|d| d["iroh"]["direct_addrs"] = json!(["relay.example.org:4433"]),
        &|d| d["iroh"]["direct_addrs"] = json!(vec!["192.0.2.1:1"; 9]),
    ];
    for (i, edit) in contact_edits.iter().enumerate() {
        assert_eq!(
            as_contact(&edited(&contact_doc, edit)),
            Err(SignedError::Malformed),
            "contact edit {i}"
        );
    }
    let at_limit = edited(&contact_doc, &|d| {
        d["iroh"]["direct_addrs"] = json!(vec!["192.0.2.1:1"; 8]);
    });
    assert!(as_contact(&at_limit).is_ok());
    let announce_edits: [&dyn Fn(&mut Value); 5] = [
        &|d| d["agent_card_hash"] = json!("sha256:00"),
        &|d| d["capabilities"] = json!(vec!["vision.ocr"; 33]),
        &|d| d["capabilities"] = json!([""]),
        &|d| d["discovery_alpn"] = json!(""),
        &|d| d["expires_at"] = json!("2026-11-13T00:00:00Z"),
    ];
    for (i, edit) in announce_edits.iter().enumerate() {
        assert_eq!(
            as_announce(&edited(&announce_doc, edit)),
            Err(SignedError::Malformed),
            "announce edit {i}"
        );
    }
    let at_limit = edited(&announce_doc, &|d| {
        d["capabilities"] = json!(vec!["vision.ocr"; 32]);
    });
    assert!(as_announce(&at_limit).is_ok());

    // A schema this build does not know, validly signed, is reported as such;
    // and nothing is issued under it.
    assert_eq!(
        as_contact(&edited(&contact_doc, &|d| d["schema"] = json!("agentnet.contact/2"))),
        Err(SignedError::UnknownSchema)
    );
    assert_eq!(
        Signed::issue(
            &key,
            ContactRecord {
                schema: "agentnet.contact/2".to_owned(),
                ..contact(key.id(), 1)
            }
        ),
        Err(SignedError::UnknownSchema)
    );
    // Nor is a record its receiver would refuse.
    assert_eq!(
        Signed::issue(&key, contact(key.id(), 0)),
        Err(SignedError::Malformed)
    );
    assert_eq!(
        Signed::issue(
            &key,
            AnnounceRecord {
                capabilities: vec!["vision.ocr".to_owned(); 33],
                ..announce(key.id(), 1)
            }
        ),
        Err(SignedError::Malformed)
    );
}

#[test]
fn record_over_4_kib_is_too_large() {
    assert_eq!(MAX_RECORD_BYTES, 4096);
    assert_eq!(ContactRecord::MAX_BYTES, MAX_RECORD_BYTES);
    assert_eq!(AnnounceRecord::MAX_BYTES, MAX_RECORD_BYTES);

    let key = AgentKey::generate();
    // A contact whose relay URL has `pad` bytes more than the shortest.
    let padded = |pad: usize| ContactRecord {
        iroh: IrohContact {
            relay_url: Some(format!("https://r{}.example.org/", "r".repeat(pad))),
            ..contact(key.id(), 1).iroh
        },
        ..contact(key.id(), 1)
    };
    let shortest = Signed::issue(&key, padded(0)).unwrap().to_json().len();
    let fill = MAX_RECORD_BYTES - shortest;

    let largest = Signed::issue(&key, padded(fill)).unwrap();
    assert_eq!(largest.to_json().len(), MAX_RECORD_BYTES);
    assert_eq!(
        Signed::<ContactRecord>::from_json(&largest.to_json()).unwrap(),
        largest
    );

    assert_eq!(
        Signed::issue(&key, padded(fill + 1)),
        Err(SignedError::TooLarge)
    );
    let doc = serde_json::to_value(padded(fill + 1)).unwrap();
    let bytes = sign_as::<ContactRecord>(&key, CONTACT_SIG_DOMAIN, &doc);
    assert_eq!(bytes.len(), MAX_RECORD_BYTES + 1);
    assert_eq!(
        Signed::<ContactRecord>::from_json(&bytes),
        Err(SignedError::TooLarge)
    );
    // The size is checked before anything is parsed.
    assert_eq!(
        Signed::<AnnounceRecord>::from_json(&vec![b'['; MAX_RECORD_BYTES + 1]),
        Err(SignedError::TooLarge)
    );
}

#[test]
fn frozen_vectors_match() {
    let read = |name: &str| -> Value {
        let path = format!("{}/tests/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
    };
    let key = fixed_key(0x01);
    let card_vector = read("card-jws-v1.json");

    let contact_vector = read("contact-v1.json");
    let announce_vector = read("announce-v1.json");
    for vector in [&contact_vector, &announce_vector] {
        assert_eq!(vector["signing_key_hex"], json!(hex(&[0x01; 32])));
        assert_eq!(vector["agent_id"], json!(key.id().to_string()));
        assert_eq!(vector["agent_id"], card_vector["agent_id"]);
        // canonical_hex is JCS of the record; signed is its envelope.
        assert_eq!(
            vector["canonical_hex"],
            json!(hex(&jcs::canonical(&vector["record"])))
        );
        assert_eq!(
            vector["signed"],
            json!({"record": vector["record"], "signature": vector["signature"]})
        );
    }

    // The inputs the vectors were frozen with.
    let contact_record = contact(key.id(), 1);
    assert_eq!(
        serde_json::to_value(&contact_record).unwrap(),
        contact_vector["record"]
    );
    let announce_record = announce(key.id(), 1);
    assert_eq!(
        serde_json::to_value(&announce_record).unwrap(),
        announce_vector["record"]
    );
    // The announce names the frozen card.
    assert_eq!(
        announce_vector["record"]["agent_card_hash"],
        card_vector["hash"]
    );

    // Recomputed, byte for byte, and the frozen bytes accepted as they are.
    let issued = Signed::issue(&key, contact_record).unwrap();
    assert_eq!(json!(issued.signature), contact_vector["signature"]);
    let frozen = jcs::canonical(&contact_vector["signed"]);
    assert_eq!(issued.to_json(), frozen);
    assert_eq!(Signed::<ContactRecord>::from_json(&frozen).unwrap(), issued);

    let issued = Signed::issue(&key, announce_record).unwrap();
    assert_eq!(json!(issued.signature), announce_vector["signature"]);
    let frozen = jcs::canonical(&announce_vector["signed"]);
    assert_eq!(issued.to_json(), frozen);
    assert_eq!(
        Signed::<AnnounceRecord>::from_json(&frozen).unwrap(),
        issued
    );
}

#[test]
fn own_seq_survives_reopen_and_never_repeats() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let me = AgentKey::generate();
    let record = |seq| {
        Signed::issue(&me, announce(me.id(), seq))
            .unwrap()
            .to_json()
    };

    {
        let store = Store::open(&path, me.id()).unwrap();
        assert_eq!(store.own_record("announce").unwrap(), None);
        assert_eq!(store.own_seq_next("announce").unwrap(), 1);
        assert_eq!(store.own_seq_next("announce").unwrap(), 2);
        // Each kind counts on its own.
        assert_eq!(store.own_seq_next("contact").unwrap(), 1);

        store
            .own_record_put("announce", 2, &record(2), NOW)
            .unwrap();
        assert_eq!(store.own_record("announce").unwrap(), Some((2, record(2))));
        // Only the last seq handed out can be stored: an older one would
        // go back, a newer one was never reserved.
        assert!(
            store
                .own_record_put("announce", 1, &record(1), NOW)
                .is_err()
        );
        assert!(
            store
                .own_record_put("announce", 3, &record(3), NOW)
                .is_err()
        );
        assert!(store.own_record_put("other", 1, b"{}", NOW).is_err());
        assert_eq!(store.own_record("announce").unwrap(), Some((2, record(2))));
        assert_eq!(store.own_record("other").unwrap(), None);
    }
    {
        let store = Store::open(&path, me.id()).unwrap();
        assert_eq!(store.own_record("announce").unwrap(), Some((2, record(2))));
        assert_eq!(store.own_seq_next("announce").unwrap(), 3);
        // Until the record that carries it is stored, there is none: the
        // last one stored carries an older seq.
        assert_eq!(store.own_record("announce").unwrap(), None);
    }
    {
        // As after a crash between reserving a seq and storing its record:
        // the seq is not handed out again.
        let store = Store::open(&path, me.id()).unwrap();
        assert_eq!(store.own_record("announce").unwrap(), None);
        assert_eq!(store.own_seq_next("announce").unwrap(), 4);
        store
            .own_record_put("announce", 4, &record(4), NOW)
            .unwrap();
        assert_eq!(store.own_seq_next("contact").unwrap(), 2);
    }
    let store = Store::open(&path, me.id()).unwrap();
    assert_eq!(store.own_record("announce").unwrap(), Some((4, record(4))));
    assert_eq!(store.own_record("contact").unwrap(), None);
    assert_eq!(store.own_seq_next("contact").unwrap(), 3);
}

#[test]
fn lower_seq_from_a_peer_is_a_regression() {
    let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
    let peer = AgentKey::generate();
    let other = AgentKey::generate();
    let record = |key: &AgentKey, seq| {
        Signed::issue(key, contact(key.id(), seq))
            .unwrap()
            .to_json()
    };
    let accept = |key: &AgentKey, kind: &str, seq| {
        store
            .peer_record_accept(&key.id(), kind, seq, &record(key, seq), EXPIRES_AT, NOW)
            .unwrap()
    };

    assert_eq!(store.peer_record(&peer.id(), "contact").unwrap(), None);
    assert_eq!(accept(&peer, "contact", 2), RecordAccept::New);
    assert_eq!(
        accept(&peer, "contact", 1),
        RecordAccept::Regression { known_seq: 2 }
    );
    // The older record is not kept.
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((2, record(&peer, 2)))
    );

    assert_eq!(accept(&peer, "contact", 3), RecordAccept::New);
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((3, record(&peer, 3)))
    );
    assert_eq!(
        accept(&peer, "contact", 2),
        RecordAccept::Regression { known_seq: 3 }
    );
    assert_eq!(accept(&peer, "contact", 3), RecordAccept::Same);

    // Each agent and each kind has its own sequence.
    assert_eq!(accept(&peer, "announce", 1), RecordAccept::New);
    assert_eq!(accept(&other, "contact", 1), RecordAccept::New);
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((3, record(&peer, 3)))
    );
    assert_eq!(
        store.peer_record(&other.id(), "contact").unwrap(),
        Some((1, record(&other, 1)))
    );
    assert_eq!(store.peer_record(&other.id(), "announce").unwrap(), None);
}

#[test]
fn same_seq_with_other_content_is_a_regression() {
    let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
    let peer = AgentKey::generate();
    let first = Signed::issue(&peer, contact(peer.id(), 5))
        .unwrap()
        .to_json();
    let moved = ContactRecord {
        iroh: IrohContact {
            direct_addrs: vec!["198.51.100.7:4433".to_owned()],
            ..contact(peer.id(), 5).iroh
        },
        ..contact(peer.id(), 5)
    };
    let moved = Signed::issue(&peer, moved).unwrap().to_json();
    assert_ne!(first, moved);
    let accept = |seq, json: &[u8]| {
        store
            .peer_record_accept(&peer.id(), "contact", seq, json, EXPIRES_AT, NOW)
            .unwrap()
    };

    assert_eq!(accept(5, &first), RecordAccept::New);
    assert_eq!(accept(5, &first), RecordAccept::Same);
    assert_eq!(accept(5, &moved), RecordAccept::Regression { known_seq: 5 });
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((5, first.clone()))
    );
    assert_eq!(accept(5, &first), RecordAccept::Same);

    let renewed = Signed::issue(&peer, contact(peer.id(), 6))
        .unwrap()
        .to_json();
    assert_eq!(accept(6, &renewed), RecordAccept::New);
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((6, renewed))
    );
}

#[test]
fn expired_peer_record_no_longer_blocks_a_lower_seq() {
    // An agent that lost its database but kept its key starts again at seq 1.
    let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
    let peer = AgentKey::generate();
    let old = Signed::issue(&peer, contact(peer.id(), 50))
        .unwrap()
        .to_json();
    assert_eq!(
        store
            .peer_record_accept(&peer.id(), "contact", 50, &old, EXPIRES_AT, NOW)
            .unwrap(),
        RecordAccept::New
    );

    // Its new first record, issued a whole lifetime after the known one
    // expired, and valid for a day from then.
    let forget_at = EXPIRES_AT + RECORD_TTL_S + CLOCK_SKEW_S;
    let reissued_at = forget_at - 3600;
    let fresh = ContactRecord {
        issued_at: ts(reissued_at),
        expires_at: ts(reissued_at + RECORD_TTL_S),
        ..contact(peer.id(), 1)
    };
    let fresh = Signed::issue(&peer, fresh).unwrap().to_json();
    let accept_at = |now| {
        store
            .peer_record_accept(
                &peer.id(),
                "contact",
                1,
                &fresh,
                reissued_at + RECORD_TTL_S,
                now,
            )
            .unwrap()
    };

    // A lower seq is a regression, and is not stored, while the known
    // record is valid, within the clock skew, and for a whole lifetime more:
    // a record issued before the known one, replayed, may still be valid
    // until then...
    for now in [
        EXPIRES_AT + CLOCK_SKEW_S - 1,
        EXPIRES_AT + CLOCK_SKEW_S,
        forget_at - 1,
    ] {
        assert_eq!(
            accept_at(now),
            RecordAccept::Regression { known_seq: 50 },
            "{now}"
        );
    }
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((50, old))
    );
    // ...and from then on the known record counts as none.
    assert_eq!(accept_at(forget_at), RecordAccept::New);
    assert_eq!(
        store.peer_record(&peer.id(), "contact").unwrap(),
        Some((1, fresh.clone()))
    );
    // From then on the new sequence is the one compared.
    assert_eq!(accept_at(forget_at + CLOCK_SKEW_S), RecordAccept::Same);
}

#[test]
fn peer_records_are_purged_once_forgotten() {
    let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
    let (gone, kept) = (AgentKey::generate(), AgentKey::generate());
    let take = |peer: &AgentKey, expires_at: i64| {
        let json = Signed::issue(peer, contact(peer.id(), 1))
            .unwrap()
            .to_json();
        let accept = store
            .peer_record_accept(&peer.id(), "contact", 1, &json, expires_at, NOW)
            .unwrap();
        assert_eq!(accept, RecordAccept::New);
    };
    take(&gone, EXPIRES_AT);
    take(&kept, EXPIRES_AT + 1);
    let forget_at = EXPIRES_AT + RECORD_TTL_S + CLOCK_SKEW_S;
    // Not while the record may still stop a replayed older one...
    assert_eq!(store.peer_records_purge(forget_at - 1).unwrap(), 0);
    assert!(store.peer_record(&gone.id(), "contact").unwrap().is_some());
    // ...and from when peer_record_accept forgets it, only it.
    assert_eq!(store.peer_records_purge(forget_at).unwrap(), 1);
    assert_eq!(store.peer_record(&gone.id(), "contact").unwrap(), None);
    assert!(store.peer_record(&kept.id(), "contact").unwrap().is_some());
    assert_eq!(store.peer_records_purge(forget_at + 1).unwrap(), 1);
    assert_eq!(store.peer_record(&kept.id(), "contact").unwrap(), None);
}

#[test]
fn record_lifetime_is_at_most_the_ttl() {
    let key = AgentKey::generate();
    let contact_for = |span: i64| ContactRecord {
        expires_at: ts(ISSUED_AT + span),
        ..contact(key.id(), 1)
    };
    let announce_for = |span: i64| AnnounceRecord {
        expires_at: ts(ISSUED_AT + span),
        ..announce(key.id(), 1)
    };
    let contact_bytes = |span| {
        let doc = serde_json::to_value(contact_for(span)).unwrap();
        sign_as::<ContactRecord>(&key, CONTACT_SIG_DOMAIN, &doc)
    };
    let announce_bytes = |span| {
        let doc = serde_json::to_value(announce_for(span)).unwrap();
        sign_as::<AnnounceRecord>(&key, ANNOUNCE_SIG_DOMAIN, &doc)
    };

    // RECORD_TTL_S from issued_at to expires_at, at most.
    assert!(Signed::issue(&key, contact_for(RECORD_TTL_S)).is_ok());
    assert!(Signed::<ContactRecord>::from_json(&contact_bytes(RECORD_TTL_S)).is_ok());
    assert!(Signed::issue(&key, announce_for(RECORD_TTL_S)).is_ok());
    assert!(Signed::<AnnounceRecord>::from_json(&announce_bytes(RECORD_TTL_S)).is_ok());

    // One second more is refused, on issue and on receipt.
    for span in [RECORD_TTL_S + 1, 365 * 24 * 3600] {
        assert_eq!(
            Signed::issue(&key, contact_for(span)),
            Err(SignedError::Malformed)
        );
        assert_eq!(
            Signed::<ContactRecord>::from_json(&contact_bytes(span)),
            Err(SignedError::Malformed)
        );
        assert_eq!(
            Signed::issue(&key, announce_for(span)),
            Err(SignedError::Malformed)
        );
        assert_eq!(
            Signed::<AnnounceRecord>::from_json(&announce_bytes(span)),
            Err(SignedError::Malformed)
        );
    }
}

#[test]
fn own_seq_carries_one_record() {
    let me = AgentKey::generate();
    let store = Store::open_in_memory(me.id()).unwrap();
    let moved = |seq| {
        let record = ContactRecord {
            iroh: IrohContact {
                direct_addrs: vec!["198.51.100.7:4433".to_owned()],
                ..contact(me.id(), seq).iroh
            },
            ..contact(me.id(), seq)
        };
        Signed::issue(&me, record).unwrap().to_json()
    };

    let seq = store.own_seq_next("contact").unwrap();
    let first = Signed::issue(&me, contact(me.id(), seq)).unwrap().to_json();
    store.own_record_put("contact", seq, &first, NOW).unwrap();
    // The same record again is harmless.
    store
        .own_record_put("contact", seq, &first, NOW + 1)
        .unwrap();
    // Another record under the same seq is refused: a peer that saw the
    // first would take it for a regression.
    assert!(
        store
            .own_record_put("contact", seq, &moved(seq), NOW + 2)
            .is_err()
    );
    assert_eq!(store.own_record("contact").unwrap(), Some((seq, first)));

    // A new seq carries the new content.
    let next = store.own_seq_next("contact").unwrap();
    store
        .own_record_put("contact", next, &moved(next), NOW + 3)
        .unwrap();
    assert_eq!(
        store.own_record("contact").unwrap(),
        Some((next, moved(next)))
    );
}
