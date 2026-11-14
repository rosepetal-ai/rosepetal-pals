//! The signed, hash-chained local event log (`agentnet.event/1`).
//!
//! Each agent appends its own events. An event signs the header
//! `{author, seq, prev_hash, payload_hash, kind, created_at}`:
//!
//! - `seq` counts the author's events from 0, without gaps;
//! - `payload_hash` is the BLAKE3 hash of `JCS(payload)`;
//! - `prev_hash` is the BLAKE3 hash of `JCS(header)` of the author's previous
//!   event, or 64 zeros for the first one;
//! - `created_at` is RFC 3339 UTC with seconds, as
//!   [`format_timestamp`] writes it.
//!
//! Hashes are lowercase hex. The signature is the author's Ed25519 signature
//! of `EVENT_SIG_DOMAIN || JCS(header)`, in base64url without padding.
//!
//! Payloads are minimal evidence: identifiers, hashes and states, never the
//! text of a question or the content of a file. Milestone 1 kinds are
//! `grant.issued`, `grant.revoked`, `task.accepted`, `task.terminal` and
//! `blob.shared`.

use anyhow::Context;
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};

use crate::grant::format_timestamp;
use crate::identity::{AgentId, AgentKey};
use crate::jcs::{canonical, sign_domain, verify_domain};
use crate::store::Store;

/// The prefix of the bytes an event signature covers.
pub const EVENT_SIG_DOMAIN: &[u8] = b"agentnet.event/1\n";

/// The `prev_hash` of an author's first event.
const FIRST_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One entry of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub author: AgentId,
    pub seq: u64,
    pub prev_hash: String,
    pub payload_hash: String,
    pub kind: String,
    pub created_at: String,
    /// In the form `JCS(payload)` parses back to.
    pub payload: Value,
    pub signature: String,
}

/// Where an author's chain first fails to verify.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    #[error("event {seq} is missing")]
    Gap { seq: u64 },
    #[error("event {seq} does not hash-link to the previous event")]
    PrevHash { seq: u64 },
    #[error("the payload of event {seq} does not match its hash")]
    PayloadHash { seq: u64 },
    #[error("the signature of event {seq} does not verify")]
    Signature { seq: u64 },
    /// The log could not be read, so it was not verified.
    #[error("cannot read the event log: {0}")]
    Storage(String),
}

fn header(
    author: &AgentId,
    seq: u64,
    prev_hash: &str,
    payload_hash: &str,
    kind: &str,
    created_at: &str,
) -> Value {
    json!({
        "author": author,
        "seq": seq,
        "prev_hash": prev_hash,
        "payload_hash": payload_hash,
        "kind": kind,
        "created_at": created_at,
    })
}

fn hash_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The columns of an event row, in [`Row`]'s order.
const COLUMNS: &str = "seq, prev_hash, payload_hash, kind, created_at, payload, signature";

/// An event row as stored, before any check. `payload` is the stored JCS
/// text.
struct StoredEvent {
    seq: i64,
    prev_hash: String,
    payload_hash: String,
    kind: String,
    created_at: String,
    payload: String,
    signature: String,
}

impl StoredEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get(0)?,
            prev_hash: row.get(1)?,
            payload_hash: row.get(2)?,
            kind: row.get(3)?,
            created_at: row.get(4)?,
            payload: row.get(5)?,
            signature: row.get(6)?,
        })
    }

    /// The signed header of this row, as event `seq` of `author`.
    fn header(&self, author: &AgentId, seq: u64) -> Value {
        header(
            author,
            seq,
            &self.prev_hash,
            &self.payload_hash,
            &self.kind,
            &self.created_at,
        )
    }

    fn into_event(self, author: AgentId) -> anyhow::Result<Event> {
        Ok(Event {
            author,
            seq: u64::try_from(self.seq).context("negative seq in the event log")?,
            payload: serde_json::from_str(&self.payload)
                .with_context(|| format!("event {} has a payload that is not JSON", self.seq))?,
            prev_hash: self.prev_hash,
            payload_hash: self.payload_hash,
            kind: self.kind,
            created_at: self.created_at,
            signature: self.signature,
        })
    }
}

impl Store {
    /// Signs and appends the next event of `key`'s author.
    ///
    /// One `IMMEDIATE` transaction reads the author's last event and writes
    /// the next one, so concurrent appends, from this process or another,
    /// get contiguous `seq`s.
    pub fn event_append(
        &self,
        key: &AgentKey,
        kind: &str,
        payload: &Value,
        now_unix: i64,
    ) -> anyhow::Result<Event> {
        let created_at =
            format_timestamp(now_unix).with_context(|| format!("no timestamp for {now_unix}"))?;
        let author = key.id();
        let payload_text =
            String::from_utf8(canonical(payload)).expect("JCS output is always UTF-8");
        let payload_hash = hash_hex(payload_text.as_bytes());

        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let last = tx
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM events WHERE author = ?1 ORDER BY seq DESC LIMIT 1"
                ),
                [&author],
                StoredEvent::from_row,
            )
            .optional()?;
        let (seq, prev_hash) = match last {
            None => (0, FIRST_PREV_HASH.to_owned()),
            Some(last) => {
                let last_seq = u64::try_from(last.seq).context("negative seq in the event log")?;
                let prev_hash = hash_hex(&canonical(&last.header(&author, last_seq)));
                (last_seq + 1, prev_hash)
            }
        };
        let signed = header(&author, seq, &prev_hash, &payload_hash, kind, &created_at);
        let signature = sign_domain(key, EVENT_SIG_DOMAIN, &signed);
        tx.execute(
            "INSERT INTO events
                 (author, seq, prev_hash, payload_hash, kind, created_at, payload, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                author,
                i64::try_from(seq)?,
                prev_hash,
                payload_hash,
                kind,
                created_at,
                payload_text,
                signature
            ],
        )?;
        tx.commit()?;

        Ok(Event {
            author,
            seq,
            prev_hash,
            payload_hash,
            kind: kind.to_owned(),
            created_at,
            payload: serde_json::from_str(&payload_text)?,
            signature,
        })
    }

    /// Verifies `author`'s whole chain and returns how many events it holds.
    ///
    /// The first failure wins, checked event by event from `seq` 0 and, in
    /// each event, in this order: gap, previous hash, payload hash,
    /// signature. Events removed from the end of the chain leave no trace.
    pub fn events_verify(&self, author: &AgentId) -> Result<u64, ChainError> {
        let storage = |err: rusqlite::Error| ChainError::Storage(err.to_string());
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM events WHERE author = ?1 ORDER BY seq"
            ))
            .map_err(storage)?;
        let rows = stmt
            .query_map([author], StoredEvent::from_row)
            .map_err(storage)?;

        let mut prev_hash = FIRST_PREV_HASH.to_owned();
        let mut verified = 0;
        for row in rows {
            let row = row.map_err(storage)?;
            let seq = verified;
            if u64::try_from(row.seq) != Ok(seq) {
                return Err(ChainError::Gap { seq });
            }
            if row.prev_hash != prev_hash {
                return Err(ChainError::PrevHash { seq });
            }
            let payload_matches = serde_json::from_str::<Value>(&row.payload)
                .is_ok_and(|payload| hash_hex(&canonical(&payload)) == row.payload_hash);
            if !payload_matches {
                return Err(ChainError::PayloadHash { seq });
            }
            let signed = row.header(author, seq);
            if !verify_domain(author, EVENT_SIG_DOMAIN, &signed, &row.signature) {
                return Err(ChainError::Signature { seq });
            }
            prev_hash = hash_hex(&canonical(&signed));
            verified += 1;
        }
        Ok(verified)
    }

    /// `author`'s events from `seq` on (inclusive), in order, as stored:
    /// [`Store::events_verify`] checks them.
    pub fn events_since(&self, author: &AgentId, seq: u64) -> anyhow::Result<Vec<Event>> {
        let Ok(from) = i64::try_from(seq) else {
            return Ok(Vec::new());
        };
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM events WHERE author = ?1 AND seq >= ?2 ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![author, from], StoredEvent::from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| row.into_event(*author))
            .collect()
    }

    /// How many events of `kind` the log holds, of any author.
    pub fn events_count(&self, kind: &str) -> anyhow::Result<u64> {
        let count: i64 =
            self.conn()
                .query_row("SELECT COUNT(*) FROM events WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })?;
        Ok(u64::try_from(count)?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    /// 2026-11-14T00:00:00Z.
    const NOW: i64 = 1_794_614_400;

    fn append_three(store: &Store, key: &AgentKey) -> Vec<Event> {
        (0..3)
            .map(|i| {
                store
                    .event_append(key, "task.accepted", &json!({ "task_id": i }), NOW)
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn each_tampered_column_is_reported() {
        let key = AgentKey::generate();
        let other_hash = hash_hex(b"other");
        let cases = [
            (
                "kind",
                json!("task.terminal"),
                ChainError::Signature { seq: 1 },
            ),
            (
                "created_at",
                json!("2026-11-13T00:00:00Z"),
                ChainError::Signature { seq: 1 },
            ),
            (
                "prev_hash",
                json!(FIRST_PREV_HASH),
                ChainError::PrevHash { seq: 1 },
            ),
            (
                "payload_hash",
                json!(other_hash),
                ChainError::PayloadHash { seq: 1 },
            ),
            (
                "payload",
                json!("not json"),
                ChainError::PayloadHash { seq: 1 },
            ),
        ];
        for (column, value, expected) in cases {
            let store = Store::open_in_memory(key.id()).unwrap();
            append_three(&store, &key);
            let changed = store
                .conn()
                .execute(
                    &format!("UPDATE events SET {column} = ?1 WHERE seq = 1"),
                    [value.as_str().unwrap()],
                )
                .unwrap();
            assert_eq!(changed, 1);
            assert_eq!(store.events_verify(&key.id()), Err(expected), "{column}");
        }

        // Another event's valid signature.
        let store = Store::open_in_memory(key.id()).unwrap();
        let events = append_three(&store, &key);
        store
            .conn()
            .execute(
                "UPDATE events SET signature = ?1 WHERE seq = 1",
                [&events[2].signature],
            )
            .unwrap();
        assert_eq!(
            store.events_verify(&key.id()),
            Err(ChainError::Signature { seq: 1 })
        );

        // An event of another author moved to the end of this chain, linked
        // correctly: only its signature gives it away.
        let store = Store::open_in_memory(key.id()).unwrap();
        let events = append_three(&store, &key);
        let last = &events[2];
        let link = hash_hex(&canonical(&header(
            &key.id(),
            2,
            &last.prev_hash,
            &last.payload_hash,
            &last.kind,
            &last.created_at,
        )));
        let impostor = AgentKey::generate();
        store
            .event_append(&impostor, "task.accepted", &json!({}), NOW)
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE events SET author = ?1, seq = 3, prev_hash = ?2 WHERE author = ?3",
                params![key.id(), link, impostor.id()],
            )
            .unwrap();
        assert_eq!(
            store.events_verify(&key.id()),
            Err(ChainError::Signature { seq: 3 })
        );
    }

    #[test]
    fn appends_through_two_connections_get_contiguous_seq() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pals.db");
        let key = Arc::new(AgentKey::generate());
        let stores = [
            Arc::new(Store::open(&path, key.id()).unwrap()),
            Arc::new(Store::open(&path, key.id()).unwrap()),
        ];
        let threads: Vec<_> = stores
            .iter()
            .cycle()
            .take(4)
            .map(|store| {
                let store = Arc::clone(store);
                let key = Arc::clone(&key);
                thread::spawn(move || {
                    for _ in 0..25 {
                        store
                            .event_append(&key, "task.accepted", &json!({}), NOW)
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(stores[0].events_verify(&key.id()).unwrap(), 100);
        assert_eq!(stores[1].events_verify(&key.id()).unwrap(), 100);
    }
}
