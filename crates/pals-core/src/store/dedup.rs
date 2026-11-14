//! Idempotency of the A2A messages peers send: one row per
//! `(peer, messageId)`, so that a message runs once however many times it
//! arrives.
//!
//! A row is created by the first request with its key ([`Store::dedup_begin`],
//! [`DedupBegin::New`]) and bound to that request's outcome
//! ([`Store::dedup_bind`]): the task it created or continued, bound when its
//! execution starts, and the message the agent answered with when it
//! answered with no task. Until it is bound, the first request is still on
//! its way to an execution. The daemon decides what a repeated request gets
//! from the row; this module only keeps it.

use anyhow::ensure;
use rusqlite::{TransactionBehavior, params};

use super::Store;
use crate::identity::AgentId;

/// How long a row is kept, in seconds: a message retried later than this
/// after its first attempt runs again.
pub const DEDUP_RETENTION_S: i64 = 7 * 24 * 3600;

/// How long a repeated request waits, in seconds, for the first request with
/// its key to be bound to an outcome.
pub const DEDUP_INFLIGHT_WAIT_S: u64 = 5;

/// What the first request with a key left: the hash of its content, and its
/// outcome once bound, a task id or a response's JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupRow {
    pub request_hash: String,
    pub task_id: Option<String>,
    pub response_json: Option<String>,
}

/// What [`Store::dedup_begin`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupBegin {
    /// No row had the key: this request created it, and runs.
    New,
    /// The row of an earlier request with the key.
    Existing(DedupRow),
}

impl Store {
    /// Creates the row of `peer`'s `message_id`, with `request_hash` and
    /// created at `now_unix`, or returns the row already there, which is
    /// left as it was. One transaction does both, so of any number of
    /// requests with the same key, exactly one gets [`DedupBegin::New`].
    pub fn dedup_begin(
        &self,
        peer: &AgentId,
        message_id: &str,
        request_hash: &str,
        now_unix: i64,
    ) -> anyhow::Result<DedupBegin> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT INTO dedup (peer, message_id, request_hash, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (peer, message_id) DO NOTHING",
            params![peer, message_id, request_hash, now_unix],
        )?;
        let begin = if inserted == 1 {
            DedupBegin::New
        } else {
            DedupBegin::Existing(tx.query_row(
                "SELECT request_hash, task_id, response_json FROM dedup
                 WHERE peer = ?1 AND message_id = ?2",
                params![peer, message_id],
                |r| {
                    Ok(DedupRow {
                        request_hash: r.get(0)?,
                        task_id: r.get(1)?,
                        response_json: r.get(2)?,
                    })
                },
            )?)
        };
        tx.commit()?;
        Ok(begin)
    }

    /// Binds the row of `peer`'s `message_id` to the outcome of its first
    /// request: `task_id`, the task it created or continued, and
    /// `response_json`, the response when the agent answered with no task.
    ///
    /// Each of the two is written once: binding one again to the same value
    /// changes nothing, and binding one already set to another value fails,
    /// as does binding a row that does not exist.
    pub fn dedup_bind(
        &self,
        peer: &AgentId,
        message_id: &str,
        task_id: Option<&str>,
        response_json: Option<&str>,
    ) -> anyhow::Result<()> {
        ensure!(
            task_id.is_some() || response_json.is_some(),
            "a dedup row is bound to a task or to a response"
        );
        let changed = self.conn().execute(
            "UPDATE dedup
             SET task_id = COALESCE(task_id, ?3), response_json = COALESCE(response_json, ?4)
             WHERE peer = ?1 AND message_id = ?2
               AND (?3 IS NULL OR task_id IS NULL OR task_id = ?3)
               AND (?4 IS NULL OR response_json IS NULL OR response_json = ?4)",
            params![peer, message_id, task_id, response_json],
        )?;
        ensure!(
            changed == 1,
            "the dedup row for message {message_id} of {peer} is missing or bound to another \
             outcome"
        );
        Ok(())
    }

    /// Deletes the row of `peer`'s `message_id` if it is still unbound, so
    /// that a request that failed before any execution started does not hold
    /// its key: the next request with it runs. Returns whether a row was
    /// deleted; a bound row is never deleted here.
    pub fn dedup_release(&self, peer: &AgentId, message_id: &str) -> anyhow::Result<bool> {
        let deleted = self.conn().execute(
            "DELETE FROM dedup
             WHERE peer = ?1 AND message_id = ?2
               AND task_id IS NULL AND response_json IS NULL",
            params![peer, message_id],
        )?;
        Ok(deleted == 1)
    }

    /// Deletes every unbound row, and returns how many.
    ///
    /// For the daemon to call when it starts, before it serves any request:
    /// a key is bound before its execution does any work, so an unbound row
    /// then belongs to a request that never ran, cut off by a crash or a
    /// restart. Left there, it would hold its key with no outcome to come.
    pub fn dedup_clear_unbound(&self) -> anyhow::Result<u64> {
        let deleted = self.conn().execute(
            "DELETE FROM dedup WHERE task_id IS NULL AND response_json IS NULL",
            [],
        )?;
        Ok(u64::try_from(deleted)?)
    }

    /// Deletes the rows created before `older_than_unix`, bound or not, and
    /// returns how many.
    pub fn dedup_purge(&self, older_than_unix: i64) -> anyhow::Result<u64> {
        let deleted = self
            .conn()
            .execute("DELETE FROM dedup WHERE created_at < ?1", [older_than_unix])?;
        Ok(u64::try_from(deleted)?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use rusqlite::OptionalExtension;

    use super::*;
    use crate::identity::AgentKey;

    impl Store {
        /// The row of `peer`'s `message_id`, read without creating it.
        fn dedup_row(&self, peer: &AgentId, message_id: &str) -> Option<DedupRow> {
            self.conn()
                .query_row(
                    "SELECT request_hash, task_id, response_json FROM dedup
                     WHERE peer = ?1 AND message_id = ?2",
                    params![peer, message_id],
                    |r| {
                        Ok(DedupRow {
                            request_hash: r.get(0)?,
                            task_id: r.get(1)?,
                            response_json: r.get(2)?,
                        })
                    },
                )
                .optional()
                .unwrap()
        }
    }

    fn row(task_id: Option<&str>, response_json: Option<&str>) -> DedupRow {
        DedupRow {
            request_hash: "h1".to_owned(),
            task_id: task_id.map(str::to_owned),
            response_json: response_json.map(str::to_owned),
        }
    }

    #[test]
    fn a_key_begins_once_and_each_outcome_is_written_once() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let peer = AgentKey::generate().id();

        assert_eq!(
            store.dedup_begin(&peer, "m-1", "h1", 10).unwrap(),
            DedupBegin::New
        );
        // Another hash does not replace the row.
        assert_eq!(
            store.dedup_begin(&peer, "m-1", "h2", 11).unwrap(),
            DedupBegin::Existing(row(None, None))
        );

        store.dedup_bind(&peer, "m-1", Some("t-1"), None).unwrap();
        assert_eq!(
            store.dedup_begin(&peer, "m-1", "h1", 12).unwrap(),
            DedupBegin::Existing(row(Some("t-1"), None))
        );
        // The same task again changes nothing; another task is refused.
        store.dedup_bind(&peer, "m-1", Some("t-1"), None).unwrap();
        assert!(store.dedup_bind(&peer, "m-1", Some("t-2"), None).is_err());
        assert_eq!(store.dedup_row(&peer, "m-1"), Some(row(Some("t-1"), None)));

        // The response is added beside the task, once.
        store.dedup_bind(&peer, "m-1", None, Some("{}")).unwrap();
        store
            .dedup_bind(&peer, "m-1", Some("t-1"), Some("{}"))
            .unwrap();
        assert!(store.dedup_bind(&peer, "m-1", None, Some("[]")).is_err());
        assert!(
            store
                .dedup_bind(&peer, "m-1", Some("t-2"), Some("{}"))
                .is_err()
        );
        assert_eq!(
            store.dedup_row(&peer, "m-1"),
            Some(row(Some("t-1"), Some("{}")))
        );

        // A response with no task.
        store.dedup_begin(&peer, "m-2", "h1", 10).unwrap();
        assert!(store.dedup_bind(&peer, "m-2", None, None).is_err());
        store
            .dedup_bind(&peer, "m-2", None, Some(r#"{"a":1}"#))
            .unwrap();
        assert_eq!(
            store.dedup_row(&peer, "m-2"),
            Some(row(None, Some(r#"{"a":1}"#)))
        );

        // No row, nothing to bind.
        assert!(store.dedup_bind(&peer, "m-3", Some("t-3"), None).is_err());
    }

    #[test]
    fn only_an_unbound_row_is_released() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let peer = AgentKey::generate().id();
        store.dedup_begin(&peer, "m-1", "h1", 10).unwrap();
        store.dedup_begin(&peer, "m-2", "h1", 10).unwrap();
        store.dedup_bind(&peer, "m-2", Some("t-2"), None).unwrap();

        assert!(store.dedup_release(&peer, "m-1").unwrap());
        assert!(!store.dedup_release(&peer, "m-1").unwrap());
        assert!(!store.dedup_release(&peer, "m-2").unwrap());
        assert_eq!(store.dedup_row(&peer, "m-1"), None);
        assert_eq!(store.dedup_row(&peer, "m-2"), Some(row(Some("t-2"), None)));
        assert_eq!(
            store.dedup_begin(&peer, "m-1", "h1", 11).unwrap(),
            DedupBegin::New
        );
    }

    #[test]
    fn clearing_unbound_rows_keeps_every_bound_one() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let peer = AgentKey::generate().id();
        for message_id in ["m-free", "m-free-2", "m-task", "m-answer", "m-both"] {
            store.dedup_begin(&peer, message_id, "h1", 10).unwrap();
        }
        store.dedup_bind(&peer, "m-task", Some("t"), None).unwrap();
        store
            .dedup_bind(&peer, "m-answer", None, Some("{}"))
            .unwrap();
        store
            .dedup_bind(&peer, "m-both", Some("t2"), Some("{}"))
            .unwrap();

        assert_eq!(store.dedup_clear_unbound().unwrap(), 2);
        assert_eq!(store.dedup_clear_unbound().unwrap(), 0);
        assert_eq!(store.dedup_row(&peer, "m-free"), None);
        assert_eq!(store.dedup_row(&peer, "m-free-2"), None);
        assert_eq!(store.dedup_row(&peer, "m-task"), Some(row(Some("t"), None)));
        assert_eq!(
            store.dedup_row(&peer, "m-answer"),
            Some(row(None, Some("{}")))
        );
        assert_eq!(
            store.dedup_row(&peer, "m-both"),
            Some(row(Some("t2"), Some("{}")))
        );
    }

    #[test]
    fn concurrent_begins_on_two_connections_create_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pals.db");
        let owner = AgentKey::generate().id();
        let peer = AgentKey::generate().id();
        let stores: Vec<Arc<Store>> = (0..2)
            .map(|_| Arc::new(Store::open(&path, owner).unwrap()))
            .collect();
        for round in 0..20 {
            let message_id = format!("m-{round}");
            let barrier = Arc::new(Barrier::new(stores.len()));
            let threads: Vec<_> = stores
                .iter()
                .map(|store| {
                    let (store, barrier, message_id) =
                        (Arc::clone(store), Arc::clone(&barrier), message_id.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        store.dedup_begin(&peer, &message_id, "h1", 10).unwrap()
                    })
                })
                .collect();
            let new = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|begin| *begin == DedupBegin::New)
                .count();
            assert_eq!(new, 1, "round {round}");
        }
    }
}
