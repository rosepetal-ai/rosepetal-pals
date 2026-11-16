//! The questions this agent asks its peers: one row per `messageId`, written
//! before the first attempt to send it, so that every attempt, however late,
//! sends the same request and none of them runs it twice.
//!
//! A row is [`OUTBOX_PENDING`] until its outcome is known: then its state is
//! the A2A name of the state its task ended in (`TASK_STATE_COMPLETED`), or
//! [`OUTBOX_REFUSED`] when the request was refused before any task.

use std::collections::HashMap;

use anyhow::ensure;
use rusqlite::{OptionalExtension, Row, params};

use super::Store;

/// The state of a row whose outcome is not known yet.
pub const OUTBOX_PENDING: &str = "pending";
/// The state of a row whose request was refused with no task.
pub const OUTBOX_REFUSED: &str = "refused";

/// A question sent, or to be sent, to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    /// The A2A `messageId`, unique among rows.
    pub message_id: String,
    /// The peer asked, as its [`PeerRef::key`](super::PeerRef::key): its
    /// agent id in hex, or `a2a:<name>` for a plain A2A contact.
    pub peer: String,
    /// The A2A `SendMessageRequest` as JSON, sent unchanged on every attempt.
    pub request_json: String,
    /// The task the peer runs it as, once known.
    pub task_id: Option<String>,
    /// [`OUTBOX_PENDING`], an A2A task state name, or [`OUTBOX_REFUSED`].
    pub state: String,
    /// How many exchanges with the peer it has taken so far.
    pub attempts: u32,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

const COLUMNS: &str =
    "message_id, peer, request_json, task_id, state, attempts, created_at, updated_at";

impl OutboxRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            message_id: row.get(0)?,
            peer: row.get(1)?,
            request_json: row.get(2)?,
            task_id: row.get(3)?,
            state: row.get(4)?,
            attempts: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

impl Store {
    /// Adds `row`; fails when a row with its message id exists.
    pub fn outbox_insert(&self, row: &OutboxRow) -> anyhow::Result<()> {
        self.conn().execute(
            &format!("INSERT INTO outbox ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            params![
                row.message_id,
                row.peer,
                row.request_json,
                row.task_id,
                row.state,
                row.attempts,
                row.created_at,
                row.updated_at
            ],
        )?;
        Ok(())
    }

    /// The row of `message_id`.
    pub fn outbox_get(&self, message_id: &str) -> anyhow::Result<Option<OutboxRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM outbox WHERE message_id = ?1"),
                [message_id],
                OutboxRow::from_row,
            )
            .optional()?)
    }

    /// Records the task the peer runs `message_id` as, once: recording the
    /// same task again changes nothing, and another task, or a row that does
    /// not exist, fails.
    pub fn outbox_set_task(
        &self,
        message_id: &str,
        task_id: &str,
        now_unix: i64,
    ) -> anyhow::Result<()> {
        let changed = self.conn().execute(
            "UPDATE outbox SET task_id = ?2, updated_at = ?3
             WHERE message_id = ?1 AND (task_id IS NULL OR task_id = ?2)",
            params![message_id, task_id, now_unix],
        )?;
        ensure!(
            changed == 1,
            "the outbox row of message {message_id} is missing or bound to another task"
        );
        Ok(())
    }

    /// Sets the state of `message_id`.
    pub fn outbox_set_state(
        &self,
        message_id: &str,
        state: &str,
        now_unix: i64,
    ) -> anyhow::Result<()> {
        let changed = self.conn().execute(
            "UPDATE outbox SET state = ?2, updated_at = ?3 WHERE message_id = ?1",
            params![message_id, state, now_unix],
        )?;
        ensure!(changed == 1, "no outbox row for message {message_id}");
        Ok(())
    }

    /// Counts one more attempt for `message_id`, and returns how many it has
    /// taken.
    pub fn outbox_attempt(&self, message_id: &str, now_unix: i64) -> anyhow::Result<u32> {
        let attempts = self
            .conn()
            .query_row(
                "UPDATE outbox SET attempts = attempts + 1, updated_at = ?2
                 WHERE message_id = ?1 RETURNING attempts",
                params![message_id, now_unix],
                |r| r.get(0),
            )
            .optional()?;
        attempts.ok_or_else(|| anyhow::anyhow!("no outbox row for message {message_id}"))
    }

    /// How the questions to each peer ended, by its
    /// [`PeerRef::key`](super::PeerRef::key): how many completed
    /// (`TASK_STATE_COMPLETED`) and how many failed (`TASK_STATE_FAILED`).
    /// A peer with neither is not in it. Local experience only, which
    /// orders candidates and grants nothing.
    pub fn outbox_outcomes_by_peer(&self) -> anyhow::Result<HashMap<String, (u32, u32)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT peer,
                    SUM(state = 'TASK_STATE_COMPLETED'),
                    SUM(state = 'TASK_STATE_FAILED')
             FROM outbox
             WHERE state IN ('TASK_STATE_COMPLETED', 'TASK_STATE_FAILED')
             GROUP BY peer",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, (row.get(1)?, row.get(2)?)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The rows still [`OUTBOX_PENDING`], oldest first.
    pub fn outbox_pending(&self) -> anyhow::Result<Vec<OutboxRow>> {
        self.outbox_where(Some(OUTBOX_PENDING))
    }

    /// Every row, oldest first.
    pub fn outbox_all(&self) -> anyhow::Result<Vec<OutboxRow>> {
        self.outbox_where(None)
    }

    fn outbox_where(&self, state: Option<&str>) -> anyhow::Result<Vec<OutboxRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM outbox WHERE ?1 IS NULL OR state = ?1
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt
            .query_map([state], OutboxRow::from_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentKey;

    fn row(message_id: &str, created_at: i64) -> OutboxRow {
        OutboxRow {
            message_id: message_id.to_owned(),
            peer: AgentKey::generate().id().to_string(),
            request_json: "{}".to_owned(),
            task_id: None,
            state: OUTBOX_PENDING.to_owned(),
            attempts: 0,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn rows_keep_their_request_and_move_to_their_outcome() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let first = row("m-1", 10);
        store.outbox_insert(&first).unwrap();
        store.outbox_insert(&row("m-2", 20)).unwrap();
        assert!(
            store.outbox_insert(&first).is_err(),
            "a message id is unique"
        );
        assert_eq!(store.outbox_get("m-1").unwrap(), Some(first.clone()));
        assert_eq!(store.outbox_get("m-3").unwrap(), None);

        assert_eq!(store.outbox_attempt("m-1", 11).unwrap(), 1);
        assert_eq!(store.outbox_attempt("m-1", 12).unwrap(), 2);
        assert!(store.outbox_attempt("m-3", 12).is_err());
        store.outbox_set_task("m-1", "t-1", 13).unwrap();
        store.outbox_set_task("m-1", "t-1", 14).unwrap();
        assert!(store.outbox_set_task("m-1", "t-2", 14).is_err());
        assert!(store.outbox_set_task("m-3", "t-1", 14).is_err());

        let ids = |rows: Vec<OutboxRow>| -> Vec<String> {
            rows.into_iter().map(|r| r.message_id).collect()
        };
        assert_eq!(ids(store.outbox_pending().unwrap()), ["m-1", "m-2"]);
        store
            .outbox_set_state("m-1", "TASK_STATE_COMPLETED", 15)
            .unwrap();
        assert!(store.outbox_set_state("m-3", OUTBOX_REFUSED, 15).is_err());
        assert_eq!(ids(store.outbox_pending().unwrap()), ["m-2"]);
        assert_eq!(ids(store.outbox_all().unwrap()), ["m-1", "m-2"]);

        let done = store.outbox_get("m-1").unwrap().unwrap();
        assert_eq!(
            done,
            OutboxRow {
                task_id: Some("t-1".to_owned()),
                state: "TASK_STATE_COMPLETED".to_owned(),
                attempts: 2,
                updated_at: 15,
                ..first
            }
        );
    }

    #[test]
    fn outcomes_are_counted_by_peer() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let bob = AgentKey::generate().id().to_string();
        let ended = [
            ("m-1", bob.as_str(), "TASK_STATE_COMPLETED"),
            ("m-2", bob.as_str(), "TASK_STATE_COMPLETED"),
            ("m-3", bob.as_str(), "TASK_STATE_FAILED"),
            ("m-4", bob.as_str(), OUTBOX_PENDING),
            ("m-5", "a2a:plain", "TASK_STATE_FAILED"),
            ("m-6", "a2a:other", "TASK_STATE_CANCELED"),
            ("m-7", "a2a:other", OUTBOX_REFUSED),
        ];
        for (n, (message_id, peer, state)) in ended.into_iter().enumerate() {
            let at = i64::try_from(n).unwrap();
            let row = OutboxRow {
                peer: peer.to_owned(),
                ..row(message_id, at)
            };
            store.outbox_insert(&row).unwrap();
            store.outbox_set_state(message_id, state, at).unwrap();
        }
        let outcomes = store.outbox_outcomes_by_peer().unwrap();
        assert_eq!(
            outcomes,
            HashMap::from([(bob, (2, 1)), ("a2a:plain".to_owned(), (0, 1))])
        );
    }
}
