//! The signed, hash-chained local event log and its frozen vector.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use pals_core::events::{ChainError, EVENT_SIG_DOMAIN, Event};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs;
use pals_core::store::Store;
use serde_json::{Value, json};

/// 2026-11-14T00:00:00Z.
const NOW: i64 = 1_794_614_400;

/// A store on disk, so a second connection can edit it underneath.
fn file_store() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let store = Store::open(&path).unwrap();
    (dir, path, store)
}

fn append_three(store: &Store, key: &AgentKey) -> Vec<Event> {
    ["grant.issued", "task.accepted", "task.terminal"]
        .iter()
        .zip(0..)
        .map(|(kind, i)| {
            store
                .event_append(key, kind, &json!({ "id": format!("x-{i}") }), NOW + i)
                .unwrap()
        })
        .collect()
}

/// The signed header of `e`.
fn header(e: &Event) -> Value {
    json!({
        "author": e.author,
        "seq": e.seq,
        "prev_hash": e.prev_hash,
        "payload_hash": e.payload_hash,
        "kind": e.kind,
        "created_at": e.created_at,
    })
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn edit(path: &PathBuf, sql: &str, author: &AgentId) {
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(conn.execute(sql, [author.to_string()]).unwrap(), 1, "{sql}");
}

#[test]
fn three_appends_form_a_valid_chain() {
    let store = Store::open_in_memory().unwrap();
    let key = AgentKey::generate();
    let id = key.id();
    let events = append_three(&store, &key);
    assert_eq!(store.events_verify(&id).unwrap(), 3);

    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1, 2]);
    for e in &events {
        assert_eq!(e.author, id);
        assert_eq!(e.payload_hash, blake3_hex(&jcs::canonical(&e.payload)));
        assert!(jcs::verify_domain(
            &id,
            EVENT_SIG_DOMAIN,
            &header(e),
            &e.signature
        ));
    }
    for pair in events.windows(2) {
        assert_eq!(
            pair[1].prev_hash,
            blake3_hex(&jcs::canonical(&header(&pair[0])))
        );
    }
    assert_eq!(events[2].created_at, "2026-11-14T00:00:02Z");

    assert_eq!(store.events_since(&id, 0).unwrap(), events);
    assert_eq!(store.events_since(&id, 2).unwrap(), events[2..]);
    assert!(store.events_since(&id, 3).unwrap().is_empty());
    assert_eq!(store.events_count("task.accepted").unwrap(), 1);
    assert_eq!(store.events_count("blob.shared").unwrap(), 0);

    // Each author has its own chain.
    let other = AgentKey::generate();
    let first = store
        .event_append(&other, "grant.issued", &json!({ "grant_id": "g-1" }), NOW)
        .unwrap();
    assert_eq!(first.seq, 0);
    assert_eq!(store.events_verify(&other.id()).unwrap(), 1);
    assert_eq!(store.events_verify(&id).unwrap(), 3);
    assert_eq!(store.events_count("grant.issued").unwrap(), 2);
    assert_eq!(store.events_verify(&AgentKey::generate().id()).unwrap(), 0);
}

#[test]
fn first_event_prev_hash_is_64_zeros() {
    let store = Store::open_in_memory().unwrap();
    let key = AgentKey::generate();
    let first = store
        .event_append(&key, "grant.issued", &json!({ "grant_id": "g-1" }), NOW)
        .unwrap();
    assert_eq!(first.seq, 0);
    assert_eq!(first.prev_hash, "0".repeat(64));
    assert_eq!(first.created_at, "2026-11-14T00:00:00Z");

    let second = store
        .event_append(&key, "grant.revoked", &json!({ "grant_id": "g-1" }), NOW)
        .unwrap();
    assert_eq!(second.prev_hash.len(), 64);
    assert!(
        second
            .prev_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_ne!(second.prev_hash, "0".repeat(64));
}

#[test]
fn tampered_payload_is_detected() {
    let (_dir, path, store) = file_store();
    let key = AgentKey::generate();
    append_three(&store, &key);
    edit(
        &path,
        r#"UPDATE events SET payload = '{"id":"forged"}' WHERE author = ?1 AND seq = 1"#,
        &key.id(),
    );
    assert_eq!(
        store.events_verify(&key.id()),
        Err(ChainError::PayloadHash { seq: 1 })
    );
}

#[test]
fn deleted_row_is_detected_as_gap() {
    let (_dir, path, store) = file_store();
    let key = AgentKey::generate();
    append_three(&store, &key);
    edit(
        &path,
        "DELETE FROM events WHERE author = ?1 AND seq = 1",
        &key.id(),
    );
    assert_eq!(
        store.events_verify(&key.id()),
        Err(ChainError::Gap { seq: 1 })
    );

    // The first event too.
    let other = AgentKey::generate();
    append_three(&store, &other);
    edit(
        &path,
        "DELETE FROM events WHERE author = ?1 AND seq = 0",
        &other.id(),
    );
    assert_eq!(
        store.events_verify(&other.id()),
        Err(ChainError::Gap { seq: 0 })
    );
}

#[test]
fn concurrent_appends_get_contiguous_seq() {
    let (_dir, _path, store) = file_store();
    let store = Arc::new(store);
    let key = Arc::new(AgentKey::generate());
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let store = Arc::clone(&store);
            let key = Arc::clone(&key);
            thread::spawn(move || {
                (0..25)
                    .map(|i| {
                        let payload = json!({ "task_id": format!("t-{t}-{i}") });
                        store
                            .event_append(&key, "task.accepted", &payload, NOW)
                            .unwrap()
                            .seq
                    })
                    .collect::<Vec<u64>>()
            })
        })
        .collect();
    let mut seqs: Vec<u64> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    seqs.sort_unstable();
    let expected: Vec<u64> = (0..200).collect();
    assert_eq!(seqs, expected);

    let id = key.id();
    let logged: Vec<u64> = store
        .events_since(&id, 0)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(logged, expected);
    assert_eq!(store.events_verify(&id).unwrap(), 200);
    assert_eq!(store.events_count("task.accepted").unwrap(), 200);
}

#[test]
fn frozen_vector_matches() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/event-v1.json");
    let vector: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap();
    let key = AgentKey::from_bytes(&[0x01; 32]);

    // The inputs the vector was frozen with.
    assert_eq!(vector["signing_key_hex"], json!(hex(&[0x01; 32])));
    assert_eq!(vector["now_unix"], json!(NOW));
    assert_eq!(vector["kind"], json!("grant.issued"));
    assert_eq!(vector["payload"], json!({ "grant_id": "g-vector-1" }));
    let header_value = &vector["header"];
    assert_eq!(header_value["author"], json!(key.id()));
    assert_eq!(
        header_value["author"],
        json!("8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c")
    );
    assert_eq!(header_value["seq"], json!(0));
    assert_eq!(header_value["prev_hash"], json!("0".repeat(64)));
    assert_eq!(header_value["kind"], json!("grant.issued"));
    assert_eq!(header_value["created_at"], json!("2026-11-14T00:00:00Z"));
    assert_eq!(header_value.as_object().unwrap().len(), 6);

    // Recomputed, byte for byte.
    let payload_canonical = jcs::canonical(&vector["payload"]);
    assert_eq!(
        json!(hex(&payload_canonical)),
        vector["payload_canonical_hex"]
    );
    assert_eq!(
        json!(blake3_hex(&payload_canonical)),
        header_value["payload_hash"]
    );
    let header_canonical = jcs::canonical(header_value);
    assert_eq!(
        json!(hex(&header_canonical)),
        vector["header_canonical_hex"]
    );
    assert_eq!(
        json!(jcs::sign_domain(&key, EVENT_SIG_DOMAIN, header_value)),
        vector["signature"]
    );
    assert_eq!(json!(blake3_hex(&header_canonical)), vector["header_hash"]);

    // The store writes exactly that event, and chains the next one to it.
    let store = Store::open_in_memory().unwrap();
    let event = store
        .event_append(&key, "grant.issued", &vector["payload"], NOW)
        .unwrap();
    assert_eq!(&header(&event), header_value);
    assert_eq!(event.payload, vector["payload"]);
    assert_eq!(json!(event.signature), vector["signature"]);
    let next = store
        .event_append(&key, "grant.revoked", &vector["payload"], NOW + 1)
        .unwrap();
    assert_eq!(json!(next.prev_hash), vector["header_hash"]);
    assert_eq!(store.events_verify(&key.id()).unwrap(), 2);
}
