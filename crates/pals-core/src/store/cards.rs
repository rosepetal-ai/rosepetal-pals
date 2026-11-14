//! Verified Agent Cards of peers, by authorization context.
//!
//! `peer` is the peer's agent id in hex, or `a2a:<name>` for a plain A2A
//! contact; `context` is `public`, or `grant:<issuer>/<grant_id>` for the
//! extended card a grant let this agent see. A card is kept as received,
//! with its hash and the agent whose signature was verified (none for a
//! plain contact, whose card needs no signature). The caller has validated
//! and verified it: the store neither parses nor checks it.
//!
//! At most [`MAX_CARD_CONTEXTS_PER_PEER`] contexts are kept per peer: storing
//! one more deletes the oldest.

use anyhow::Context;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

use super::Store;
use crate::card_jws::CardHash;
use crate::identity::AgentId;

/// The most cards kept of one peer, one per context.
pub const MAX_CARD_CONTEXTS_PER_PEER: usize = 8;

impl Store {
    /// Keeps `json`, a verified card of `peer` in `context`, with its `hash`
    /// and the agent whose signature was verified, fetched at `now_unix`; it
    /// replaces the card that `peer` had in `context`. When `peer` then has
    /// more than [`MAX_CARD_CONTEXTS_PER_PEER`] cards, the oldest others go.
    pub fn peer_card_put(
        &self,
        peer: &str,
        context: &str,
        json: &Value,
        hash: &CardHash,
        signer: Option<&AgentId>,
        now_unix: i64,
    ) -> anyhow::Result<()> {
        let card_json = serde_json::to_string(json).context("a card that does not serialize")?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO peer_cards (peer, context, card_json, card_hash, signer, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (peer, context) DO UPDATE SET
                 card_json = excluded.card_json,
                 card_hash = excluded.card_hash,
                 signer = excluded.signer,
                 fetched_at = excluded.fetched_at",
            params![peer, context, card_json, hash.as_str(), signer, now_unix],
        )?;
        let kept: i64 = tx.query_row(
            "SELECT COUNT(*) FROM peer_cards WHERE peer = ?1",
            [peer],
            |row| row.get(0),
        )?;
        let excess = kept.saturating_sub(MAX_CARD_CONTEXTS_PER_PEER as i64);
        if excess > 0 {
            tx.execute(
                "DELETE FROM peer_cards WHERE peer = ?1 AND context IN (
                     SELECT context FROM peer_cards WHERE peer = ?1 AND context <> ?2
                     ORDER BY fetched_at, context LIMIT ?3)",
                params![peer, context, excess],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The card kept of `peer` in `context`, and its hash.
    pub fn peer_card(
        &self,
        peer: &str,
        context: &str,
    ) -> anyhow::Result<Option<(Value, CardHash)>> {
        let row: Option<(String, String)> = self
            .conn()
            .query_row(
                "SELECT card_json, card_hash FROM peer_cards WHERE peer = ?1 AND context = ?2",
                params![peer, context],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(json, hash)| {
            let json = serde_json::from_str(&json).context("a kept card that is not JSON")?;
            let hash = hash.parse().context("a kept card hash that is not one")?;
            Ok((json, hash))
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::card_jws::card_hash;
    use crate::identity::AgentKey;

    #[test]
    fn a_peer_keeps_its_newest_contexts_only() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let signer = AgentKey::generate().id();
        let peer = signer.to_string();
        let card = |n: i64| json!({"name": format!("card {n}")});
        for n in 0..10 {
            let context = format!("grant:{signer}/g-{n}");
            store
                .peer_card_put(
                    &peer,
                    &context,
                    &card(n),
                    &card_hash(&card(n)),
                    Some(&signer),
                    100 + n,
                )
                .unwrap();
        }
        // The two oldest went; the eight newest are kept, as stored.
        for n in 0..2 {
            let context = format!("grant:{signer}/g-{n}");
            assert_eq!(store.peer_card(&peer, &context).unwrap(), None);
        }
        for n in 2..10 {
            let context = format!("grant:{signer}/g-{n}");
            assert_eq!(
                store.peer_card(&peer, &context).unwrap(),
                Some((card(n), card_hash(&card(n))))
            );
        }

        // Storing a context again replaces its card and refreshes it.
        let context = format!("grant:{signer}/g-2");
        store
            .peer_card_put(&peer, &context, &card(20), &card_hash(&card(20)), None, 200)
            .unwrap();
        assert_eq!(
            store.peer_card(&peer, &context).unwrap(),
            Some((card(20), card_hash(&card(20))))
        );
        store
            .peer_card_put(&peer, "public", &card(21), &card_hash(&card(21)), None, 201)
            .unwrap();
        assert!(store.peer_card(&peer, &context).unwrap().is_some());
        let oldest = format!("grant:{signer}/g-3");
        assert_eq!(store.peer_card(&peer, &oldest).unwrap(), None);

        // Other peers are counted apart.
        store
            .peer_card_put(
                "a2a:plain",
                "public",
                &card(0),
                &card_hash(&card(0)),
                None,
                1,
            )
            .unwrap();
        assert!(store.peer_card("a2a:plain", "public").unwrap().is_some());
        assert!(store.peer_card(&peer, "public").unwrap().is_some());
    }
}
