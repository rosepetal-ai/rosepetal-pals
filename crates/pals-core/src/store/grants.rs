//! Grants the store's owner issued, grants it holds from other agents, and
//! revocations.
//!
//! [`SignedGrant::check`] trusts that a grant was made by `issue` or
//! `from_token`, so a grant is stored as its token and every grant read back
//! is rebuilt with [`SignedGrant::from_token`], which verifies its signature.
//! The store is bound to its owner, checked on the rebuilt grant both when a
//! grant is stored and when it is read:
//!
//! - `grants_issued` takes and returns only grants the owner issued, and one
//!   covers a request only if its audience is the owner too;
//! - `grants_held` takes and returns only grants whose subject is the owner.
//!
//! The other columns only select candidate rows, and what they selected is
//! checked again on the rebuilt grant. A row edited or inserted on disk can
//! therefore hide a grant, and deleting a revocation row un-revokes one, but
//! it cannot make a grant cover more than the owner signed: a token signed by
//! any other key covers nothing. A row whose token no longer parses, names
//! another grant or does not belong to the owner is skipped with a warning.

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
    /// A `grants_issued` row rebuilt, if the owner issued it.
    fn issued_row(&self, grant_id: &str, token: &str) -> Option<SignedGrant> {
        let g = rebuild("grants_issued", grant_id, token)?;
        if g.grant.issuer != self.owner {
            warn!(grant_id, issuer = %g.grant.issuer, "skipping an issued grant of another issuer");
            return None;
        }
        Some(g)
    }

    /// A `grants_held` row rebuilt, if its subject is the owner.
    fn held_row(&self, grant_id: &str, token: &str) -> Option<SignedGrant> {
        let g = rebuild("grants_held", grant_id, token)?;
        if g.grant.subject != self.owner {
            warn!(grant_id, subject = %g.grant.subject, "skipping a held grant of another subject");
            return None;
        }
        Some(g)
    }

    /// Records a grant the owner issued; a grant of another issuer is
    /// refused.
    ///
    /// A grant id is issued once: storing the same grant again does nothing,
    /// and another grant with the same id is refused.
    pub fn grant_issued_put(&self, g: &SignedGrant) -> anyhow::Result<()> {
        let row = GrantRow::new(g)?;
        let grant = &row.grant.grant;
        ensure!(
            grant.issuer == self.owner,
            "refusing to store grant {} as issued: its issuer {} is not this agent {}",
            grant.grant_id,
            grant.issuer,
            self.owner
        );
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

    /// Records a grant another agent issued to the owner, replacing an
    /// earlier copy of the same grant (same issuer and grant id); a grant
    /// whose subject is not the owner is refused.
    pub fn grant_held_put(&self, g: &SignedGrant) -> anyhow::Result<()> {
        let row = GrantRow::new(g)?;
        let grant = &row.grant.grant;
        ensure!(
            grant.subject == self.owner,
            "refusing to store grant {} as held: its subject {} is not this agent {}",
            grant.grant_id,
            grant.subject,
            self.owner
        );
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

    /// The grant the owner issued with that id, revoked or expired alike.
    pub fn grant_issued(&self, grant_id: &str) -> anyhow::Result<Option<SignedGrant>> {
        let token: Option<String> = self
            .conn()
            .query_row(
                "SELECT token FROM grants_issued WHERE grant_id = ?1",
                [grant_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(token.and_then(|token| self.issued_row(grant_id, &token)))
    }

    /// Every grant the owner issued, revoked or expired alike, newest
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
            .filter_map(|(grant_id, token)| self.issued_row(grant_id, token))
            .collect())
    }

    /// The grants other agents issued to the owner for use against
    /// `audience`, unexpired at `now_unix`, newest `issued_at` first.
    pub fn grants_held_for(
        &self,
        audience: &AgentId,
        now_unix: i64,
    ) -> anyhow::Result<Vec<SignedGrant>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT grant_id, token FROM grants_held
             WHERE audience = ?1 AND subject = ?2 AND expires_at + ?3 > ?4
             ORDER BY issued_at DESC, grant_id, issuer",
        )?;
        let rows = stmt
            .query_map(params![audience, self.owner, CLOCK_SKEW_S, now_unix], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .iter()
            .filter_map(|(grant_id, token)| self.held_row(grant_id, token))
            .filter(|g| g.grant.audience == *audience && !expired(g, now_unix))
            .collect())
    }

    /// A grant the owner issued to `subject`, for use against the owner,
    /// that allows `op` on `resource` at `now_unix` and is not revoked, as
    /// [`SignedGrant::check`] judges it; the newest `issued_at` first when
    /// several do.
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
            let g = self.issued_row(grant_id, token)?;
            let check = GrantCheck {
                now_unix,
                audience: &self.owner,
                subject,
                accepted_issuers: &[self.owner],
                operation: op,
                resource,
                revoked: *revoked,
            };
            g.check(&check).is_ok().then_some(g)
        }))
    }

    /// Revokes a grant the owner issued; `false` when it issued no grant with
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

    /// A grant of `issuer` that lets `subject` fetch `blob:01` from
    /// `audience`.
    fn blob_grant_for(
        issuer: &AgentKey,
        grant_id: &str,
        subject: AgentId,
        audience: AgentId,
    ) -> SignedGrant {
        let grant = Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: grant_id.to_owned(),
            issuer: issuer.id(),
            subject,
            audience,
            project: "demo".to_owned(),
            resources: vec!["blob:01".to_owned()],
            operations: vec![Operation::BlobFetch],
            issued_at: format_timestamp(NOW - 3600).unwrap(),
            expires_at: format_timestamp(NOW + 3600).unwrap(),
        };
        SignedGrant::issue(issuer, grant).unwrap()
    }

    /// A grant of `issuer` that lets `subject` fetch `blob:01` from the
    /// issuer.
    fn blob_grant(issuer: &AgentKey, grant_id: &str, subject: AgentId) -> SignedGrant {
        blob_grant_for(issuer, grant_id, subject, issuer.id())
    }

    fn covering(store: &Store, subject: &AgentId) -> Option<SignedGrant> {
        store
            .grant_issued_covering(subject, Operation::BlobFetch, "blob:01", NOW)
            .unwrap()
    }

    #[test]
    fn a_grant_whose_signature_does_not_verify_is_not_stored() {
        let me = AgentKey::generate();
        let store = Store::open_in_memory(me.id()).unwrap();
        let peer = AgentKey::generate();

        let mut issued = blob_grant(&me, "g-1", AgentKey::generate().id());
        issued.grant.project = "other".to_owned();
        let err = store.grant_issued_put(&issued).unwrap_err();
        assert!(format!("{err:#}").contains("bad signature"), "{err:#}");

        let mut held = blob_grant(&peer, "g-2", me.id());
        held.grant.project = "other".to_owned();
        let err = store.grant_held_put(&held).unwrap_err();
        assert!(format!("{err:#}").contains("bad signature"), "{err:#}");

        assert!(store.grants_issued().unwrap().is_empty());
        assert!(store.grants_held_for(&peer.id(), NOW).unwrap().is_empty());
    }

    #[test]
    fn grant_issued_put_refuses_a_foreign_issuer() {
        let me = AgentKey::generate();
        let store = Store::open_in_memory(me.id()).unwrap();
        let alice = AgentKey::generate().id();
        // Validly signed, and even for use against this agent, but issued by
        // another one.
        let foreign = blob_grant_for(&AgentKey::generate(), "g-foreign", alice, me.id());
        let err = store.grant_issued_put(&foreign).unwrap_err();
        assert!(err.to_string().contains("is not this agent"), "{err}");
        assert!(store.grants_issued().unwrap().is_empty());
        assert_eq!(covering(&store, &alice), None);
    }

    #[test]
    fn grant_held_put_refuses_a_grant_for_another_subject() {
        let me = AgentKey::generate();
        let store = Store::open_in_memory(me.id()).unwrap();
        let peer = AgentKey::generate();
        let not_mine = blob_grant(&peer, "g-other", AgentKey::generate().id());
        let err = store.grant_held_put(&not_mine).unwrap_err();
        assert!(err.to_string().contains("is not this agent"), "{err}");
        assert!(store.grants_held_for(&peer.id(), NOW).unwrap().is_empty());
    }

    #[test]
    fn a_foreign_token_in_grants_issued_never_covers() {
        let me = AgentKey::generate();
        let store = Store::open_in_memory(me.id()).unwrap();
        let attacker = AgentKey::generate();
        let alice = AgentKey::generate().id();
        // Signed by a fresh key, issued to itself and for use against itself,
        // for every blob.
        let forged = |grant_id: &str| {
            let mut grant = blob_grant(&attacker, grant_id, attacker.id()).grant;
            grant.resources = vec!["blob:*".to_owned()];
            SignedGrant::issue(&attacker, grant).unwrap().to_token()
        };

        // A row inserted around grant_issued_put.
        store
            .conn()
            .execute(
                "INSERT INTO grants_issued
                     (grant_id, issuer, subject, audience, issued_at, expires_at, token)
                 VALUES ('g-forged', ?1, ?1, ?1, ?2, ?3, ?4)",
                params![attacker.id(), NOW - 3600, NOW + 3600, forged("g-forged")],
            )
            .unwrap();
        assert_eq!(covering(&store, &attacker.id()), None);
        assert_eq!(store.grant_issued("g-forged").unwrap(), None);
        assert!(store.grants_issued().unwrap().is_empty());

        // The token of a grant this agent issued, replaced.
        store
            .grant_issued_put(&blob_grant(&me, "g-alice", alice))
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE grants_issued SET subject = ?1, token = ?2 WHERE grant_id = 'g-alice'",
                params![attacker.id(), forged("g-alice")],
            )
            .unwrap();
        assert_eq!(covering(&store, &attacker.id()), None);
        assert_eq!(store.grant_issued("g-alice").unwrap(), None);
    }

    #[test]
    fn an_issued_grant_for_another_audience_never_covers() {
        let me = AgentKey::generate();
        let store = Store::open_in_memory(me.id()).unwrap();
        let alice = AgentKey::generate().id();
        let elsewhere = AgentKey::generate().id();
        let g = blob_grant_for(&me, "g-elsewhere", alice, elsewhere);
        store.grant_issued_put(&g).unwrap();
        assert_eq!(store.grant_issued("g-elsewhere").unwrap(), Some(g));
        assert_eq!(covering(&store, &alice), None);

        // The same grant for use against this agent covers.
        let here = blob_grant(&me, "g-here", alice);
        store.grant_issued_put(&here).unwrap();
        assert_eq!(covering(&store, &alice), Some(here));
    }

    #[test]
    fn an_issued_grant_id_is_never_reused() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
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
        let me = AgentKey::generate().id();
        let store = Store::open_in_memory(me).unwrap();
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
        let me = AgentKey::generate();
        let store = Store::open_in_memory(me.id()).unwrap();
        let alice = AgentKey::generate().id();
        let stranger = AgentKey::generate().id();
        let sql = |statement: &str| store.conn().execute(statement, []).unwrap();

        store
            .grant_issued_put(&blob_grant(&me, "g-alice", alice))
            .unwrap();
        sql(&format!(
            "UPDATE grants_issued SET subject = '{stranger}' WHERE grant_id = 'g-alice'"
        ));
        assert_eq!(covering(&store, &stranger), None);

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
        assert_eq!(covering(&store, &stranger), None);
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

        // A held grant's token replaced by the same grant for another subject.
        sql("UPDATE grants_held SET audience = issuer");
        assert_eq!(store.grants_held_for(&peer.id(), NOW).unwrap().len(), 1);
        sql(&format!(
            "UPDATE grants_held SET token = '{}'",
            blob_grant(&peer, "g-held", stranger).to_token()
        ));
        assert!(store.grants_held_for(&peer.id(), NOW).unwrap().is_empty());

        // A token that no longer parses.
        sql("UPDATE grants_held SET token = 'not-a-token'");
        assert!(store.grants_held_for(&peer.id(), NOW).unwrap().is_empty());
    }
}
