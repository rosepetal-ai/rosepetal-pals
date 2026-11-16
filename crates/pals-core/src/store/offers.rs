//! Service offers ([`crate::discovery::query`]): those this agent issued
//! that a task can name, of a task scope, kept to check an offer again when
//! the task that names it arrives, and those its peers sent it, kept to name
//! the one it asks with.
//!
//! An offer is kept as the bytes of its signed JSON, with the columns that
//! index it, and read back verified ([`Signed::from_json`]): a row that no
//! longer verifies is an error, never an offer. Offer ids are their
//! provider's: one issued here is unique among this agent's own, one
//! received is unique only with its provider, so received offers are keyed
//! by (provider, `offer_id`), and two providers may use the same id.
//!
//! The daemon purges both every hour: those issued once they expired more
//! than a day ago ([`Store::offers_issued_purge`]), those received once they
//! expired ([`Store::offers_received_purge`]).

use anyhow::{Context, anyhow};
use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::discovery::query::ServiceOffer;
use crate::grant::parse_timestamp;
use crate::identity::AgentId;
use crate::signed::Signed;

impl Store {
    /// Keeps `offer`, which this agent issued. An offer id is issued once:
    /// keeping another offer under an id kept already is an error.
    pub fn offer_issued_put(&self, offer: &Signed<ServiceOffer>) -> anyhow::Result<()> {
        let doc = &offer.doc;
        let (issued_at, expires_at) = times(doc)?;
        self.conn()
            .execute(
                "INSERT INTO offers_issued (offer_id, requester, query_hash, scope_key,
                     card_hash, revision, issued_at, expires_at, offer_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    doc.offer_id,
                    doc.requester,
                    doc.query_hash,
                    doc.scope,
                    doc.agent_card_hash.as_str(),
                    i64::try_from(doc.inventory_revision)?,
                    issued_at,
                    expires_at,
                    offer.to_json(),
                ],
            )
            .with_context(|| format!("cannot keep the offer {} issued", doc.offer_id))?;
        Ok(())
    }

    /// The offer this agent issued as `offer_id`, if it keeps it.
    pub fn offer_issued(&self, offer_id: &str) -> anyhow::Result<Option<Signed<ServiceOffer>>> {
        let json: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT offer_json FROM offers_issued WHERE offer_id = ?1",
                [offer_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| read(&json)).transpose()
    }

    /// How many offers this agent issued it keeps.
    pub fn offers_issued_count(&self) -> anyhow::Result<u64> {
        let count: i64 =
            self.conn()
                .query_row("SELECT COUNT(*) FROM offers_issued", [], |row| row.get(0))?;
        Ok(u64::try_from(count)?)
    }

    /// Deletes the offers issued that expired before `older_than_unix`; how
    /// many.
    pub fn offers_issued_purge(&self, older_than_unix: i64) -> anyhow::Result<u64> {
        let purged = self.conn().execute(
            "DELETE FROM offers_issued WHERE expires_at < ?1",
            [older_than_unix],
        )?;
        Ok(u64::try_from(purged)?)
    }

    /// Keeps `offer`, which its provider, `offer.agent_id`, sent this agent
    /// and this agent accepted ([`crate::discovery::query::accept_offer`]),
    /// received at `now_unix`; in place of any kept of the same provider and
    /// id.
    pub fn offer_received_put(
        &self,
        offer: &Signed<ServiceOffer>,
        now_unix: i64,
    ) -> anyhow::Result<()> {
        let doc = &offer.doc;
        let (_, expires_at) = times(doc)?;
        self.conn().execute(
            "INSERT INTO offers_received (provider, offer_id, query_id, received_at,
                 expires_at, offer_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (provider, offer_id) DO UPDATE SET
                 query_id = excluded.query_id,
                 received_at = excluded.received_at,
                 expires_at = excluded.expires_at,
                 offer_json = excluded.offer_json",
            params![
                doc.agent_id,
                doc.offer_id,
                doc.query_id,
                now_unix,
                expires_at,
                offer.to_json()
            ],
        )?;
        Ok(())
    }

    /// The offer `provider` sent as `offer_id`, if this agent keeps it,
    /// expired or not.
    pub fn offer_received(
        &self,
        provider: &AgentId,
        offer_id: &str,
    ) -> anyhow::Result<Option<Signed<ServiceOffer>>> {
        let json: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT offer_json FROM offers_received WHERE provider = ?1 AND offer_id = ?2",
                params![provider, offer_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| read(&json)).transpose()
    }

    /// Deletes the offers received that expired by `now_unix`; how many.
    pub fn offers_received_purge(&self, now_unix: i64) -> anyhow::Result<u64> {
        let purged = self.conn().execute(
            "DELETE FROM offers_received WHERE expires_at <= ?1",
            [now_unix],
        )?;
        Ok(u64::try_from(purged)?)
    }
}

/// `issued_at` and `expires_at` of `offer`, in Unix seconds.
fn times(offer: &ServiceOffer) -> anyhow::Result<(i64, i64)> {
    let time = |text: &str| {
        parse_timestamp(text).ok_or_else(|| anyhow!("an offer time that does not read: {text:?}"))
    };
    Ok((time(&offer.issued_at)?, time(&offer.expires_at)?))
}

/// A kept offer, verified again.
fn read(json: &[u8]) -> anyhow::Result<Signed<ServiceOffer>> {
    Signed::from_json(json).map_err(|err| anyhow!("a kept offer does not verify: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::matching::Evaluation;
    use crate::discovery::query::{Conditions, OFFER_SCHEMA, OfferParts, OfferStatus, sign_offer};
    use crate::identity::AgentKey;

    /// 2026-11-14T00:00:00Z.
    const NOW: i64 = 1_794_614_400;

    /// An offer of `provider`'s to `requester`, `offer_id`, valid 15 s from
    /// `now`.
    fn offer(
        provider: &AgentKey,
        requester: AgentId,
        offer_id: &str,
        now: i64,
    ) -> Signed<ServiceOffer> {
        let evaluation = Evaluation {
            skill_id: None,
            status: OfferStatus::NotOffered,
            compatible: false,
            conditions: Conditions {
                external_cloud_used: None,
                processing_region: None,
                authorization_required: true,
            },
            requirement_results: Vec::new(),
            unknown_requirements: Vec::new(),
            estimated_start_delay_ms: None,
            estimated_duration_ms: None,
            limitations: Vec::new(),
        };
        let parts = OfferParts {
            offer_id: offer_id.to_owned(),
            agent_id: provider.id(),
            requester,
            query_id: "q-1".to_owned(),
            query_hash: format!("blake3:{}", "1".repeat(64)),
            agent_card_hash: format!("blake3:{}", "0".repeat(64)).parse().unwrap(),
            scope: "public".to_owned(),
            inventory_revision: 1,
            valid_for_ms: 15_000,
            now_unix: now,
        };
        let offer = ServiceOffer::new(parts, evaluation).unwrap();
        assert_eq!(offer.schema, OFFER_SCHEMA);
        sign_offer(provider, offer).unwrap()
    }

    #[test]
    fn an_issued_offer_is_kept_once_and_purged_a_day_after_it_expires() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        let requester = AgentKey::generate().id();
        let first = offer(&key, requester, "offer-a", NOW);
        store.offer_issued_put(&first).unwrap();
        assert_eq!(store.offer_issued("offer-a").unwrap(), Some(first.clone()));
        assert_eq!(store.offer_issued("offer-b").unwrap(), None);
        // One id, one offer.
        let again = offer(&key, requester, "offer-a", NOW + 1);
        assert!(store.offer_issued_put(&again).is_err());
        assert_eq!(store.offer_issued("offer-a").unwrap(), Some(first));
        // The columns index it.
        let (scope, revision, expires_at): (String, i64, i64) = store
            .conn()
            .query_row(
                "SELECT scope_key, revision, expires_at FROM offers_issued",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (scope.as_str(), revision, expires_at),
            ("public", 1, NOW + 15)
        );

        let later = offer(&key, requester, "offer-c", NOW + 3600);
        store.offer_issued_put(&later).unwrap();
        // Expired, but not a day ago: kept.
        assert_eq!(store.offers_issued_purge(NOW + 15).unwrap(), 0);
        assert_eq!(store.offers_issued_purge(NOW + 16).unwrap(), 1);
        assert_eq!(store.offer_issued("offer-a").unwrap(), None);
        assert_eq!(store.offer_issued("offer-c").unwrap(), Some(later));
    }

    #[test]
    fn received_offers_are_kept_by_provider_and_id_until_they_expire() {
        let own = AgentKey::generate().id();
        let store = Store::open_in_memory(own).unwrap();
        let (b, c) = (AgentKey::generate(), AgentKey::generate());
        let from_b = offer(&b, own, "offer-001", NOW);
        let from_c = offer(&c, own, "offer-001", NOW + 10);
        store.offer_received_put(&from_b, NOW).unwrap();
        store.offer_received_put(&from_c, NOW + 10).unwrap();
        assert_eq!(
            store.offer_received(&b.id(), "offer-001").unwrap(),
            Some(from_b.clone())
        );
        assert_eq!(
            store.offer_received(&c.id(), "offer-001").unwrap(),
            Some(from_c.clone())
        );
        assert_eq!(store.offer_received(&own, "offer-001").unwrap(), None);
        // The same provider's same id again: the newer one.
        let newer = offer(&b, own, "offer-001", NOW + 5);
        store.offer_received_put(&newer, NOW + 5).unwrap();
        assert_eq!(
            store.offer_received(&b.id(), "offer-001").unwrap(),
            Some(newer)
        );
        // Purged once expired: b's at NOW + 20, c's at NOW + 25.
        assert_eq!(store.offers_received_purge(NOW + 19).unwrap(), 0);
        assert_eq!(store.offers_received_purge(NOW + 20).unwrap(), 1);
        assert_eq!(store.offer_received(&b.id(), "offer-001").unwrap(), None);
        assert_eq!(
            store.offer_received(&c.id(), "offer-001").unwrap(),
            Some(from_c)
        );
    }

    #[test]
    fn a_kept_offer_that_no_longer_verifies_is_an_error() {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        let kept = offer(&key, AgentKey::generate().id(), "offer-a", NOW);
        store.offer_issued_put(&kept).unwrap();
        store
            .conn()
            .execute("UPDATE offers_issued SET offer_json = x'7b7d'", [])
            .unwrap();
        assert!(store.offer_issued("offer-a").is_err());
    }
}
