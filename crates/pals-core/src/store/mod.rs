//! The agent's local SQLite database, `<data_dir>/pals.db`.
//!
//! One connection behind a mutex, in WAL mode with `foreign_keys = ON` and a
//! 30 s busy timeout, so the daemon and CLI commands can share the file, even
//! on a host so loaded that another connection holds a write for seconds. The
//! schema version lives in `PRAGMA user_version`: opening a database written
//! by an older version upgrades it, in one transaction, and one written by a
//! newer version is refused (`migrate.rs`). All calls are synchronous.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, ensure};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, ToSql, TransactionBehavior};

use crate::identity::AgentId;

mod cards;
mod contacts;
mod dedup;
mod grants;
mod migrate;
mod outbox;
mod records;
mod tasks;

pub use cards::MAX_CARD_CONTEXTS_PER_PEER;
pub use contacts::Contact;
pub use dedup::{DEDUP_INFLIGHT_WAIT_S, DEDUP_RETENTION_S, DedupBegin, DedupRow};
pub use outbox::{OUTBOX_PENDING, OUTBOX_REFUSED, OutboxRow};
pub use records::RecordAccept;
pub use tasks::INTERRUPTED_MESSAGE;

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// The local store of one agent, its owner. `Send + Sync`: share it behind
/// an `Arc`.
///
/// Grants are checked against the owner: issued grants must be issued by it,
/// held grants must have it as subject. The owner is not kept in the
/// database; every open names it.
pub struct Store {
    conn: Mutex<Connection>,
    owner: AgentId,
}

impl Store {
    /// Opens `owner`'s database at `path`, creating it and its schema if
    /// needed, or upgrading the schema of an older version.
    ///
    /// Fails with an error containing `unsupported schema version` when the
    /// file was written by a newer schema, and leaves the file as it was
    /// when an upgrade fails.
    pub fn open(path: &Path, owner: AgentId) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open the database {}", path.display()))?;
        Self::init(conn, true, owner).with_context(|| format!("cannot open {}", path.display()))
    }

    /// A private database of `owner` in memory, for tests.
    pub fn open_in_memory(owner: AgentId) -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?, false, owner)
    }

    /// The agent this store belongs to.
    pub fn owner(&self) -> &AgentId {
        &self.owner
    }

    fn init(mut conn: Connection, on_disk: bool, owner: AgentId) -> anyhow::Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let journal_mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        // An in-memory database keeps its `memory` journal.
        ensure!(
            !on_disk || journal_mode.eq_ignore_ascii_case("wal"),
            "the database cannot use WAL mode (journal_mode is {journal_mode})"
        );
        conn.pragma_update(None, "foreign_keys", true)?;

        // Immediate, so two processes opening a new or older file do not
        // both create or upgrade the schema.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        migrate::upgrade(&tx)?;
        tx.commit()?;
        Ok(Self {
            conn: Mutex::new(conn),
            owner,
        })
    }

    /// The connection, for one call.
    ///
    /// A panic while the lock was held leaves no partial write behind (an
    /// open transaction rolls back when dropped), so a poisoned lock is
    /// taken as is.
    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ToSql for AgentId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.to_string()))
    }
}

impl FromSql for AgentId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value.as_str()?.parse().map_err(FromSqlError::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentKey;

    #[test]
    fn connection_pragmas_are_set() {
        let dir = tempfile::tempdir().unwrap();
        let owner = AgentKey::generate().id();
        let store = Store::open(&dir.path().join("pals.db"), owner).unwrap();
        assert_eq!(store.owner(), &owner);
        let conn = store.conn();
        let pragma = |name: &str| -> String {
            conn.pragma_query_value(None, name, |row| {
                row.get::<_, rusqlite::types::Value>(0)
                    .map(|value| match value {
                        rusqlite::types::Value::Integer(n) => n.to_string(),
                        rusqlite::types::Value::Text(text) => text,
                        other => format!("{other:?}"),
                    })
            })
            .unwrap()
        };
        assert_eq!(pragma("journal_mode"), "wal");
        assert_eq!(pragma("foreign_keys"), "1");
        assert_eq!(pragma("busy_timeout"), "30000");
        assert_eq!(pragma("user_version"), "2");
    }
}
