//! The store's schema versions, and the upgrade of an older database.
//!
//! `PRAGMA user_version` holds the version. [`upgrade`] runs inside the
//! `IMMEDIATE` transaction that opens the store, so an upgrade applies
//! whole or not at all, and two processes opening the same file never both
//! run it:
//!
//! | `user_version` | Done |
//! |-|-|
//! | 0, a new database | `schema.sql`: all of version 2 |
//! | 1, from milestone 1 | `tasks.status_at` when missing ([`tasks::add_status_at`]), then `migrate-2.sql` |
//! | 2 | nothing |
//! | any other | refused: `unsupported schema version` |
//!
//! Each step from version N - 1 to N is `migrate-N.sql`, which only adds to
//! version N - 1; `schema.sql` creates version N whole, and a migrated
//! database must end with exactly its schema
//! (`tests/store_migration.rs`). A milestone 1 database is upgraded in
//! place, keeping every row.

use anyhow::{Context, bail};
use rusqlite::Connection;

use super::tasks;

/// The schema this build writes and reads.
pub(super) const SCHEMA_VERSION: i64 = 2;
/// The whole schema, for a new database.
const SCHEMA: &str = include_str!("schema.sql");
/// From version 1 to 2.
const MIGRATE_2: &str = include_str!("migrate-2.sql");

/// Brings the database to [`SCHEMA_VERSION`]. `conn` is inside the
/// transaction of the open, which commits it; on an error nothing of the
/// upgrade remains once that transaction rolls back.
pub(super) fn upgrade(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        0 => conn.execute_batch(SCHEMA)?,
        1..SCHEMA_VERSION => {
            for from in version..SCHEMA_VERSION {
                step(conn, from).with_context(|| {
                    format!(
                        "cannot upgrade the schema from version {from} to {}",
                        from + 1
                    )
                })?;
            }
        }
        SCHEMA_VERSION => return Ok(()),
        _ => bail!(
            "unsupported schema version {version}: this build of pals reads version \
             {SCHEMA_VERSION} and upgrades version 1"
        ),
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// From version `from` to the next.
fn step(conn: &Connection, from: i64) -> anyhow::Result<()> {
    match from {
        1 => {
            // Written before `tasks.status_at` was added to version 1, while
            // it was unreleased.
            tasks::add_status_at(conn)?;
            conn.execute_batch(MIGRATE_2)?;
        }
        _ => unreachable!("no step from schema version {from}"),
    }
    Ok(())
}
