//! The agent's local SQLite database, `<data_dir>/pals.db`.
//!
//! One connection behind a mutex, in WAL mode with `foreign_keys = ON` and a
//! 5 s busy timeout, so the daemon and CLI commands can share the file. The
//! schema version lives in `PRAGMA user_version`; a database written by a
//! newer version is refused. All calls are synchronous.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, ToSql, TransactionBehavior};

use crate::identity::AgentId;

mod contacts;
mod grants;

pub use contacts::Contact;

/// The schema this build writes and reads.
const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = include_str!("schema.sql");
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// The local store. `Send + Sync`: share it behind an `Arc`.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens the database at `path`, creating it and its schema if needed.
    ///
    /// Fails with an error containing `unsupported schema version` when the
    /// file was written by a newer schema.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open the database {}", path.display()))?;
        Self::init(conn, true).with_context(|| format!("cannot open {}", path.display()))
    }

    /// A private database in memory, for tests.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?, false)
    }

    fn init(mut conn: Connection, on_disk: bool) -> anyhow::Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let journal_mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        // An in-memory database keeps its `memory` journal.
        ensure!(
            !on_disk || journal_mode.eq_ignore_ascii_case("wal"),
            "the database cannot use WAL mode (journal_mode is {journal_mode})"
        );
        conn.pragma_update(None, "foreign_keys", true)?;

        // Immediate, so two processes opening a new file do not both create
        // the schema.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                tx.execute_batch(SCHEMA)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            SCHEMA_VERSION => {}
            _ => bail!(
                "unsupported schema version {version}: this build of pals reads version \
                 {SCHEMA_VERSION}"
            ),
        }
        tx.commit()?;
        Ok(Self {
            conn: Mutex::new(conn),
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

    #[test]
    fn connection_pragmas_are_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("pals.db")).unwrap();
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
        assert_eq!(pragma("busy_timeout"), "5000");
        assert_eq!(pragma("user_version"), "1");
    }
}
