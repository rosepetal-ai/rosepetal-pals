//! Inventory views ([`crate::inventory::view`]): the revisions of this
//! agent's own, and the pages of its peers' it read.
//!
//! **Revisions.** One sequence per scope, which moves only when what the
//! scope shows changes. A scope is its key
//! ([`crate::inventory::Scope::key`]) and its content the
//! [`crate::inventory::view::content_hash`] of its view; the store neither
//! parses nor checks either. The sequence survives restarts, so a revision
//! never goes back, and it moves on any change, back to an older content
//! included.
//!
//! **Peers' pages.** A signed page a peer sent, kept by the peer, the
//! authorization context it was asked in (`public`, or
//! `grant:<issuer>/<grant_id>` for the grant presented) and the request
//! (its sections and cursor, as the caller keys them), until it expires.
//! A page kept in one context is never read in another: what one grant let
//! this agent see never answers a request made with another grant, or with
//! none. The store neither parses nor verifies the pages.

use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::identity::AgentId;

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

    /// Keeps `page_json`, a signed page `peer` sent in `context` for the
    /// request `request_key`, until `expires_at`, in place of any kept for
    /// the same.
    pub fn inventory_cache_put(
        &self,
        peer: &AgentId,
        context: &str,
        request_key: &str,
        page_json: &[u8],
        expires_at: i64,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO inventory_cache (peer, context, request_key, page_json, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (peer, context, request_key) DO UPDATE SET
                 page_json = excluded.page_json, expires_at = excluded.expires_at",
            params![peer, context, request_key, page_json, expires_at],
        )?;
        Ok(())
    }

    /// The page kept for `peer`, `context` and `request_key`, while
    /// `now_unix` is before its expiry.
    pub fn inventory_cache(
        &self,
        peer: &AgentId,
        context: &str,
        request_key: &str,
        now_unix: i64,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT page_json FROM inventory_cache
                 WHERE peer = ?1 AND context = ?2 AND request_key = ?3 AND expires_at > ?4",
                params![peer, context, request_key, now_unix],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Deletes the pages expired at `now_unix`; how many.
    pub fn inventory_cache_purge(&self, now_unix: i64) -> anyhow::Result<u64> {
        let purged = self.conn().execute(
            "DELETE FROM inventory_cache WHERE expires_at <= ?1",
            [now_unix],
        )?;
        Ok(u64::try_from(purged)?)
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

    #[test]
    fn a_peer_page_is_kept_by_context_until_it_expires() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let (peer, other) = (AgentKey::generate().id(), AgentKey::generate().id());
        let grant = "grant:issuer/g-1";
        store
            .inventory_cache_put(&peer, grant, "k", b"private page", 100)
            .unwrap();
        store
            .inventory_cache_put(&peer, "public", "k", b"public page", 200)
            .unwrap();
        let read = |peer: &AgentId, context: &str, key: &str, now: i64| {
            store.inventory_cache(peer, context, key, now).unwrap()
        };
        assert_eq!(
            read(&peer, grant, "k", 99).as_deref(),
            Some(&b"private page"[..])
        );
        // Never in another context, for another request or of another peer.
        assert_eq!(
            read(&peer, "public", "k", 99).as_deref(),
            Some(&b"public page"[..])
        );
        assert_eq!(read(&peer, "grant:issuer/g-2", "k", 99), None);
        assert_eq!(read(&peer, grant, "other", 99), None);
        assert_eq!(read(&other, grant, "k", 99), None);
        // Until it expires.
        assert_eq!(read(&peer, grant, "k", 100), None);
        // A newer page takes its place.
        store
            .inventory_cache_put(&peer, grant, "k", b"newer page", 300)
            .unwrap();
        assert_eq!(
            read(&peer, grant, "k", 150).as_deref(),
            Some(&b"newer page"[..])
        );
        // The purge deletes what expired.
        assert_eq!(store.inventory_cache_purge(250).unwrap(), 1);
        assert_eq!(read(&peer, "public", "k", 150), None);
        assert_eq!(
            read(&peer, grant, "k", 250).as_deref(),
            Some(&b"newer page"[..])
        );
    }
}
