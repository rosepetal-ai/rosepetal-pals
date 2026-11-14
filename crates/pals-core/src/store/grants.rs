//! Grants this agent issued, grants it holds from other agents, and
//! revocations.
//!
//! [`SignedGrant::check`] trusts that a grant was made by `issue` or
//! `from_token`, so a grant is stored as its token and every grant read back
//! is rebuilt with [`SignedGrant::from_token`], which verifies its signature.
//! The other columns only select candidate rows; what they selected is checked
//! again on the rebuilt grant, so a row edited on disk can hide a grant but
//! never widen one. A row whose token no longer parses, or names another
//! grant, is skipped with a warning.

use anyhow::{Context, ensure};
use rusqlite::{OptionalExtension, params};
use tracing::warn;

use super::Store;
use crate::grant::{CLOCK_SKEW_S, GrantCheck, Operation, SignedGrant, parse_timestamp};
use crate::identity::AgentId;

/// A grant ready to be stored: its token and the columns that index it, all
/// taken from the verified token.
struct GrantRow {
    grant: SignedGrant,
    token: String,
    issued_at: i64,
    expires_at: i64,
}

impl GrantRow {
    fn new(g: &SignedGrant) -> anyhow::Result<Self> {
        let token = g.to_token();
        // `SignedGrant`'s fields are public: never store what would not
        // come back.
        let grant = SignedGrant::from_token(&token)
            .with_context(|| format!("refusing to store grant {}", g.grant.grant_id))?;
        let instant = |text: &str| parse_timestamp(text).context("grant timestamp out of form");
        Ok(Self {
            issued_at: instant(&grant.grant.issued_at)?,
            expires_at: instant(&grant.grant.expires_at)?,
            grant,
            token,
        })
    }
}

/// The grant in `token`, or `None` with a warning when the token no longer
/// parses or is not the grant the row's `grant_id` names.
fn rebuild(table: &str, grant_id: &str, token: &str) -> Option<SignedGrant> {
    match SignedGrant::from_token(token) {
        Ok(g) if g.grant.grant_id == grant_id => Some(g),
        Ok(g) => {
            warn!(
                table,
                grant_id,
                token_grant_id = %g.grant.grant_id,
                "skipping a stored grant whose token names another grant"
            );
            None
        }
        Err(err) => {
            warn!(table, grant_id, %err, "skipping a stored grant whose token does not parse");
            None
        }
    }
}

/// Whether `g` has expired at `now_unix`, with the clock skew
/// [`SignedGrant::check`] allows.
fn expired(g: &SignedGrant, now_unix: i64) -> bool {
    parse_timestamp(&g.grant.expires_at).is_none_or(|at| now_unix >= at + CLOCK_SKEW_S)
}

impl Store {
    /// Records a grant this agent issued.
    ///
    /// A grant id is issued once: storing the same grant again does nothing,
    /// and another grant with the same id is refused.
    pub fn grant_issued_put(&self, g: &SignedGrant) -> anyhow::Result<()> {
        let row = GrantRow::new(g)?;
        let grant = &row.grant.grant;
        let conn = self.conn();
        let inserted = conn.execute(
            "INSERT INTO grants_issued
                 (grant_id, issuer, subject, audience, issued_at, expires_at, token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (grant_id) DO NOTHING",
            params![
                grant.grant_id,
                grant.issuer,
                grant.subject,
                grant.audience,
                row.issued_at,
                row.expires_at,
                row.token
            ],
        )?;
        if inserted == 0 {
            let stored: String = conn.query_row(
                "SELECT token FROM grants_issued WHERE grant_id = ?1",
                [&grant.grant_id],
                |r| r.get(0),
            )?;
            ensure!(
                stored == row.token,
                "grant id {} was already issued with other content",
                grant.grant_id
            );
        }
        Ok(())
    }

    /// Records a grant another agent issued to this one, replacing an earlier
    /// copy of the same grant (same issuer and grant id).
    pub fn grant_held_put(&self, g: &SignedGrant) -> anyhow::Result<()> {
        let row = GrantRow::new(g)?;
        let grant = &row.grant.grant;
        self.conn().execute(
            "INSERT INTO grants_held
                 (issuer, grant_id, subject, audience, issued_at, expires_at, token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (issuer, grant_id) DO UPDATE SET
                 subject = excluded.subject,
                 audience = excluded.audience,
                 issued_at = excluded.issued_at,
                 expires_at = excluded.expires_at,
                 token = excluded.token",
            params![
                grant.issuer,
                grant.grant_id,
                grant.subject,
                grant.audience,
                row.issued_at,
                row.expires_at,
                row.token
            ],
        )?;
        Ok(())
    }

    /// The grant this agent issued with that id, revoked or expired alike.
    pub fn grant_issued(&self, grant_id: &str) -> anyhow::Result<Option<SignedGrant>> {
        let token: Option<String> = self
            .conn()
            .query_row(
                "SELECT token FROM grants_issued WHERE grant_id = ?1",
                [grant_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(token.and_then(|token| rebuild("grants_issued", grant_id, &token)))
    }

    /// Every grant this agent issued, revoked or expired alike, newest
    /// `issued_at` first.
    pub fn grants_issued(&self) -> anyhow::Result<Vec<SignedGrant>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT grant_id, token FROM grants_issued ORDER BY issued_at DESC, grant_id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .iter()
            .filter_map(|(grant_id, token)| rebuild("grants_issued", grant_id, token))
            .collect())
    }

    /// The grants other agents issued to this one for use against
    /// `audience`, unexpired at `now_unix`, newest `issued_at` first.
    pub fn grants_held_for(
        &self,
        audience: &AgentId,
        now_unix: i64,
    ) -> anyhow::Result<Vec<SignedGrant>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT grant_id, token FROM grants_held
             WHERE audience = ?1 AND expires_at + ?2 > ?3
             ORDER BY issued_at DESC, grant_id, issuer",
        )?;
        let rows = stmt
            .query_map(params![audience, CLOCK_SKEW_S, now_unix], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .iter()
            .filter_map(|(grant_id, token)| rebuild("grants_held", grant_id, token))
            .filter(|g| g.grant.audience == *audience && !expired(g, now_unix))
            .collect())
    }

    /// A grant this agent issued to `subject` that allows `op` on `resource`
    /// at `now_unix` and is not revoked, as [`SignedGrant::check`] judges it;
    /// the newest `issued_at` first when several do.
    pub fn grant_issued_covering(
        &self,
        subject: &AgentId,
        op: Operation,
        resource: &str,
        now_unix: i64,
    ) -> anyhow::Result<Option<SignedGrant>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT g.grant_id, g.token, r.grant_id IS NOT NULL
             FROM grants_issued AS g LEFT JOIN revocations AS r USING (grant_id)
             WHERE g.subject = ?1 AND g.expires_at + ?2 > ?3
             ORDER BY g.issued_at DESC, g.grant_id",
        )?;
        let rows = stmt
            .query_map(params![subject, CLOCK_SKEW_S, now_unix], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.iter().find_map(|(grant_id, token, revoked)| {
            let g = rebuild("grants_issued", grant_id, token)?;
            // Issuer and audience are this agent's own: the grant is checked
            // for subject, revocation, validity, operation and resource.
            let check = GrantCheck {
                now_unix,
                audience: &g.grant.audience,
                subject,
                accepted_issuers: &[g.grant.issuer],
                operation: op,
                resource,
                revoked: *revoked,
            };
            g.check(&check).is_ok().then_some(g)
        }))
    }

    /// Revokes a grant this agent issued; `false` when it issued no grant with
    /// that id. Revoking twice keeps the first instant.
    pub fn revoke(&self, grant_id: &str, now_unix: i64) -> anyhow::Result<bool> {
        let conn = self.conn();
        let issued: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM grants_issued WHERE grant_id = ?1)",
            [grant_id],
            |r| r.get(0),
        )?;
        if issued {
            conn.execute(
                "INSERT INTO revocations (grant_id, revoked_at) VALUES (?1, ?2)
                 ON CONFLICT (grant_id) DO NOTHING",
                params![grant_id, now_unix],
            )?;
        }
        Ok(issued)
    }

    pub fn is_revoked(&self, grant_id: &str) -> anyhow::Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM revocations WHERE grant_id = ?1)",
            [grant_id],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::{GRANT_SCHEMA, Grant, format_timestamp};
    use crate::identity::AgentKey;

    /// 2026-11-14T12:00:00Z.
    const NOW: i64 = 1_794_657_600;

    fn blob_grant(issuer: &AgentKey, grant_id: &str, subject: AgentId) -> SignedGrant {
        let grant = Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: grant_id.to_owned(),
            issuer: issuer.id(),
            subject,
            audience: issuer.id(),
            project: "demo".to_owned(),
            resources: vec!["blob:01".to_owned()],
            operations: vec![Operation::BlobFetch],
            issued_at: format_timestamp(NOW - 3600).unwrap(),
            expires_at: format_timestamp(NOW + 3600).unwrap(),
        };
        SignedGrant::issue(issuer, grant).unwrap()
    }

    #[test]
    fn a_grant_whose_signature_does_not_verify_is_not_stored() {
        let store = Store::open_in_memory().unwrap();
        let key = AgentKey::generate();
        let mut forged = blob_grant(&key, "g-1", AgentKey::generate().id());
        forged.grant.project = "other".to_owned();
        assert!(store.grant_issued_put(&forged).is_err());
        assert!(store.grant_held_put(&forged).is_err());
        assert!(store.grants_issued().unwrap().is_empty());
        assert!(store.grants_held_for(&key.id(), NOW).unwrap().is_empty());
    }

    #[test]
    fn an_issued_grant_id_is_never_reused() {
        let store = Store::open_in_memory().unwrap();
        let key = AgentKey::generate();
        let first = blob_grant(&key, "g-1", AgentKey::generate().id());
        store.grant_issued_put(&first).unwrap();
        store.grant_issued_put(&first).unwrap();
        let other = blob_grant(&key, "g-1", AgentKey::generate().id());
        let err = store.grant_issued_put(&other).unwrap_err();
        assert!(err.to_string().contains("already issued"), "{err}");
        assert_eq!(store.grants_issued().unwrap(), vec![first]);
    }

    #[test]
    fn held_grant_ids_are_unique_per_issuer() {
        let store = Store::open_in_memory().unwrap();
        let me = AgentKey::generate().id();
        let peer = AgentKey::generate();
        let other_peer = AgentKey::generate();
        store.grant_held_put(&blob_grant(&peer, "g-1", me)).unwrap();
        store
            .grant_held_put(&blob_grant(&other_peer, "g-1", me))
            .unwrap();
        assert_eq!(store.grants_held_for(&peer.id(), NOW).unwrap().len(), 1);
        assert_eq!(
            store.grants_held_for(&other_peer.id(), NOW).unwrap().len(),
            1
        );

        // The issuer's new copy of a grant replaces the old one.
        let mut renewed = blob_grant(&peer, "g-1", me).grant;
        renewed.expires_at = format_timestamp(NOW + 7200).unwrap();
        let renewed = SignedGrant::issue(&peer, renewed).unwrap();
        store.grant_held_put(&renewed).unwrap();
        assert_eq!(
            store.grants_held_for(&peer.id(), NOW).unwrap(),
            vec![renewed]
        );
    }

    #[test]
    fn edited_rows_never_widen_a_grant() {
        let store = Store::open_in_memory().unwrap();
        let me = AgentKey::generate();
        let alice = AgentKey::generate().id();
        let stranger = AgentKey::generate().id();
        let sql = |statement: &str| store.conn().execute(statement, []).unwrap();
        let covering = |subject: &AgentId| {
            store
                .grant_issued_covering(subject, Operation::BlobFetch, "blob:01", NOW)
                .unwrap()
        };

        store
            .grant_issued_put(&blob_grant(&me, "g-alice", alice))
            .unwrap();
        sql(&format!(
            "UPDATE grants_issued SET subject = '{stranger}' WHERE grant_id = 'g-alice'"
        ));
        assert_eq!(covering(&stranger), None);

        // A valid token of another grant under this grant's id.
        let stranger_grant = blob_grant(&me, "g-stranger", stranger);
        store.grant_issued_put(&stranger_grant).unwrap();
        sql(&format!(
            "UPDATE grants_issued SET token = '{}' WHERE grant_id = 'g-alice'",
            stranger_grant.to_token()
        ));
        sql("UPDATE grants_issued SET grant_id = 'g-moved' WHERE grant_id = 'g-stranger'");
        assert_eq!(store.grant_issued("g-alice").unwrap(), None);
        assert_eq!(store.grant_issued("g-moved").unwrap(), None);
        assert_eq!(covering(&stranger), None);
        assert!(store.grants_issued().unwrap().is_empty());

        // A held grant moved to another audience.
        let peer = AgentKey::generate();
        store
            .grant_held_put(&blob_grant(&peer, "g-held", me.id()))
            .unwrap();
        sql(&format!(
            "UPDATE grants_held SET audience = '{stranger}' WHERE grant_id = 'g-held'"
        ));
        assert!(store.grants_held_for(&stranger, NOW).unwrap().is_empty());

        // A token that no longer parses.
        sql("UPDATE grants_held SET audience = issuer, token = 'not-a-token'");
        assert!(store.grants_held_for(&peer.id(), NOW).unwrap().is_empty());
    }
}
