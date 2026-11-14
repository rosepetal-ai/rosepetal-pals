//! Signed contact and announce records ([`crate::records`]): this agent's
//! own, with the sequence numbers it hands out, and the newest of each peer,
//! against which a later one is checked for a regression.
//!
//! A record kind is a short name, `contact` or `announce`. Records are kept
//! as the bytes of their signed JSON; the store neither parses nor verifies
//! them.

use anyhow::{Context, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::Store;
use crate::grant::CLOCK_SKEW_S;
use crate::identity::AgentId;

/// What [`Store::peer_record_accept`] made of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordAccept {
    /// The first of its agent and kind, one with a higher `seq` than the one
    /// known, or any once the known one has expired: stored, in its place.
    New,
    /// The one known, byte for byte.
    Same,
    /// A lower `seq` than the one known, or the same `seq` with other
    /// bytes: not stored.
    Regression { known_seq: u64 },
}

impl Store {
    /// The next sequence number of this agent's records of `kind`: 1 the
    /// first time, then one more on every call. It is stored before it is
    /// returned, so it is never handed out again, even after a restart.
    ///
    /// Until [`Store::own_record_put`] stores the record that carries it,
    /// [`Store::own_record`] has none for `kind`.
    pub fn own_seq_next(&self, kind: &str) -> anyhow::Result<u64> {
        let seq: i64 = self.conn().query_row(
            "INSERT INTO own_records (kind, seq, record, updated_at) VALUES (?1, 1, NULL, 0)
             ON CONFLICT (kind) DO UPDATE SET seq = seq + 1, record = NULL
             RETURNING seq",
            [kind],
            |row| row.get(0),
        )?;
        from_sql(seq)
    }

    /// Stores `json`, this agent's signed record of `kind`, which carries
    /// `seq`, at `now_unix`.
    ///
    /// `seq` must be the last [`Store::own_seq_next`] handed out for `kind`:
    /// an older one would go back, a newer one was never handed out. One
    /// `seq` carries one record: once stored, only the same bytes can be
    /// stored again under it, since a peer that saw the first would take any
    /// other for a regression.
    pub fn own_record_put(
        &self,
        kind: &str,
        seq: u64,
        json: &[u8],
        now_unix: i64,
    ) -> anyhow::Result<()> {
        let updated = self.conn().execute(
            "UPDATE own_records SET record = ?3, updated_at = ?4
             WHERE kind = ?1 AND seq = ?2 AND (record IS NULL OR record = ?3)",
            params![kind, to_sql(seq)?, json, now_unix],
        )?;
        ensure!(
            updated == 1,
            "seq {seq} is not the last one handed out for own {kind} records, or already \
             carries another record"
        );
        Ok(())
    }

    /// This agent's stored record of `kind`, with its `seq`; `None` before
    /// the first, and from the moment a newer `seq` is handed out until its
    /// record is stored.
    pub fn own_record(&self, kind: &str) -> anyhow::Result<Option<(u64, Vec<u8>)>> {
        let row: Option<(i64, Vec<u8>)> = self
            .conn()
            .query_row(
                "SELECT seq, record FROM own_records WHERE kind = ?1 AND record IS NOT NULL",
                [kind],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(seq, record)| Ok((from_sql(seq)?, record)))
            .transpose()
    }

    /// Takes `json`, `agent`'s signed record of `kind`, which carries `seq`
    /// and expires at `expires_at`, received at `now_unix`, and compares it
    /// with the newest one known of that agent and kind.
    ///
    /// The caller has verified the record: its signature, that `agent`
    /// signed it, and its validity. A lower `seq`, or the same with other
    /// bytes, is a [`RecordAccept::Regression`] and is not stored; a higher
    /// one replaces the known record. A known record that expired more than
    /// [`CLOCK_SKEW_S`] ago counts as none, as it does for every receiver:
    /// an agent that lost its sequence (a new database, the same key) is
    /// accepted again once its last record has expired. No record outlives
    /// its issue by more than [`crate::records::RECORD_TTL_S`], and an agent
    /// issues each with that whole lifetime, in `seq` order, so by then every
    /// older record has expired too. One `IMMEDIATE` transaction, so
    /// concurrent calls see each other's records.
    pub fn peer_record_accept(
        &self,
        agent: &AgentId,
        kind: &str,
        seq: u64,
        json: &[u8],
        expires_at: i64,
        now_unix: i64,
    ) -> anyhow::Result<RecordAccept> {
        let sql_seq = to_sql(seq)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let known: Option<(i64, Vec<u8>, i64)> = tx
            .query_row(
                "SELECT seq, record, expires_at FROM peer_records
                 WHERE agent_id = ?1 AND kind = ?2",
                params![agent, kind],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let known = known
            .filter(|(.., known_expires_at)| {
                now_unix < known_expires_at.saturating_add(CLOCK_SKEW_S)
            })
            .map(|(known_seq, record, _)| (known_seq, record));
        let accept = match known {
            Some((known_seq, _)) if known_seq > sql_seq => RecordAccept::Regression {
                known_seq: from_sql(known_seq)?,
            },
            Some((known_seq, record)) if known_seq == sql_seq => {
                if record == json {
                    RecordAccept::Same
                } else {
                    RecordAccept::Regression { known_seq: seq }
                }
            }
            _ => RecordAccept::New,
        };
        match accept {
            RecordAccept::New => tx.execute(
                "INSERT INTO peer_records (agent_id, kind, seq, record, expires_at, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (agent_id, kind) DO UPDATE SET
                     seq = excluded.seq,
                     record = excluded.record,
                     expires_at = excluded.expires_at,
                     seen_at = excluded.seen_at",
                params![agent, kind, sql_seq, json, expires_at, now_unix],
            )?,
            RecordAccept::Same => tx.execute(
                "UPDATE peer_records SET seen_at = ?3 WHERE agent_id = ?1 AND kind = ?2",
                params![agent, kind, now_unix],
            )?,
            RecordAccept::Regression { .. } => 0,
        };
        tx.commit()?;
        Ok(accept)
    }

    /// The newest record of `kind` known of `agent`, with its `seq`, whether
    /// or not it has expired: the caller checks its validity.
    pub fn peer_record(
        &self,
        agent: &AgentId,
        kind: &str,
    ) -> anyhow::Result<Option<(u64, Vec<u8>)>> {
        let row: Option<(i64, Vec<u8>)> = self
            .conn()
            .query_row(
                "SELECT seq, record FROM peer_records WHERE agent_id = ?1 AND kind = ?2",
                params![agent, kind],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(seq, record)| Ok((from_sql(seq)?, record)))
            .transpose()
    }
}

fn to_sql(seq: u64) -> anyhow::Result<i64> {
    i64::try_from(seq).with_context(|| format!("record seq {seq} out of range"))
}

fn from_sql(seq: i64) -> anyhow::Result<u64> {
    u64::try_from(seq).with_context(|| format!("record seq {seq} out of range"))
}
