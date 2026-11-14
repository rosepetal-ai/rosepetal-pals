//! The revisions of this agent's own inventory views
//! ([`crate::inventory::view`]): one sequence per scope, which moves only
//! when what the scope shows changes.
//!
//! A scope is its key ([`crate::inventory::Scope::key`]) and its content the
//! [`crate::inventory::view::content_hash`] of its view; the store neither
//! parses nor checks either. The sequence survives restarts, so a revision
//! never goes back, and it moves on any change, back to an older content
//! included.

use rusqlite::{OptionalExtension, params};

use super::Store;

impl Store {
    /// The revision of the view of `scope_key` whose content hashes to
    /// `content_hash`, at `now_unix`: 1 the first time, the same while the
    /// content does not change, one more each time it does.
    pub fn view_revision(
        &self,
        scope_key: &str,
        content_hash: &str,
        now_unix: i64,
    ) -> anyhow::Result<u64> {
        let conn = self.conn();
        // Most requests find the content unchanged: no write then.
        let known: Option<(String, i64)> = conn
            .query_row(
                "SELECT content_hash, seq FROM view_revisions WHERE scope_key = ?1",
                [scope_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let seq = match known {
            Some((hash, seq)) if hash == content_hash => seq,
            // SQLite reads every right-hand side from the row as it was.
            _ => conn.query_row(
                "INSERT INTO view_revisions (scope_key, content_hash, seq, updated_at)
                 VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT (scope_key) DO UPDATE SET
                     seq = CASE WHEN content_hash = excluded.content_hash
                               THEN seq ELSE seq + 1 END,
                     updated_at = CASE WHEN content_hash = excluded.content_hash
                                       THEN updated_at ELSE excluded.updated_at END,
                     content_hash = excluded.content_hash
                 RETURNING seq",
                params![scope_key, content_hash, now_unix],
                |row| row.get(0),
            )?,
        };
        Ok(u64::try_from(seq)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentKey;

    #[test]
    fn a_revision_moves_only_on_change_and_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pals.db");
        let owner = AgentKey::generate().id();
        {
            let store = Store::open(&path, owner).unwrap();
            assert_eq!(store.view_revision("public", "blake3:a", 10).unwrap(), 1);
            assert_eq!(store.view_revision("public", "blake3:a", 11).unwrap(), 1);
            assert_eq!(store.view_revision("public", "blake3:b", 12).unwrap(), 2);
            // Scopes are counted apart.
            assert_eq!(store.view_revision("task:demo", "blake3:b", 12).unwrap(), 1);
        }
        let store = Store::open(&path, owner).unwrap();
        assert_eq!(store.view_revision("public", "blake3:b", 13).unwrap(), 2);
        // Back to an older content is a change too.
        assert_eq!(store.view_revision("public", "blake3:a", 14).unwrap(), 3);
        let updated_at: i64 = store
            .conn()
            .query_row(
                "SELECT updated_at FROM view_revisions WHERE scope_key = 'public'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(updated_at, 14);
    }
}
