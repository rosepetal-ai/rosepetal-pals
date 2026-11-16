//! What the inventory adapters last observed of each entry
//! (`inventory_observations`), kept across restarts.
//!
//! One row per entry id: the newest observation replaces the one before.
//! `detail_json` is the whole [`Observation`] as JSON; `source` (its JSON)
//! and `status` repeat what it says, and `observed_at` and `expires_at` are
//! its times in Unix seconds. An observation carries only what a view may
//! show ([`Observation::check`]): fixed codes, never a raw error, and never
//! an adapter's configuration or the values of its environment. The store
//! checks that before it keeps one, and does not know which entries the
//! inventory has.

use std::collections::BTreeMap;

use anyhow::{Context, anyhow};
use rusqlite::params;

use super::Store;
use crate::grant::parse_timestamp;
use crate::inventory::Observation;

impl Store {
    /// Keeps `o` as what was last observed of `entry_id`, in place of the
    /// observation kept before. Refused, unkept, when `o` is not one a view
    /// may carry ([`Observation::check`]).
    pub fn observation_put(&self, entry_id: &str, o: &Observation) -> anyhow::Result<()> {
        o.check()
            .map_err(|err| anyhow!("observation of {entry_id} not kept: {err}"))?;
        let observed_at = parse_timestamp(&o.observed_at).context("observed_at")?;
        let expires_at = parse_timestamp(&o.expires_at).context("expires_at")?;
        let source = serde_json::to_string(&o.source)?;
        let status = match serde_json::to_value(o.status)? {
            serde_json::Value::String(status) => status,
            other => unreachable!("a status serializes to a string, not {other}"),
        };
        let detail_json = serde_json::to_string(o)?;
        self.conn().execute(
            "INSERT INTO inventory_observations
                 (entry_id, source, status, observed_at, expires_at, detail_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (entry_id) DO UPDATE SET
                 source = excluded.source, status = excluded.status,
                 observed_at = excluded.observed_at, expires_at = excluded.expires_at,
                 detail_json = excluded.detail_json",
            params![
                entry_id,
                source,
                status,
                observed_at,
                expires_at,
                detail_json
            ],
        )?;
        Ok(())
    }

    /// The observations kept that have not expired at `now_unix`, by entry
    /// id. A row that does not read as an observation a view may carry is
    /// left out, and logged without its content: it hides only itself.
    pub fn observations(&self, now_unix: i64) -> anyhow::Result<BTreeMap<String, Observation>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT entry_id, detail_json FROM inventory_observations
             WHERE expires_at > ?1 ORDER BY entry_id",
        )?;
        let rows = statement.query_map([now_unix], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut observations = BTreeMap::new();
        for row in rows {
            let (entry_id, json) = row?;
            match serde_json::from_str::<Observation>(&json) {
                Ok(observation) if observation.check().is_ok() => {
                    observations.insert(entry_id, observation);
                }
                _ => tracing::warn!(entry_id, "a kept observation does not read; left out"),
            }
        }
        Ok(observations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::format_timestamp;
    use crate::identity::AgentKey;
    use crate::inventory::{
        AdapterMethod, DETAIL_UNREACHABLE, ObservationSource, ObservedTool, OperationalStatus,
    };

    fn observed(status: OperationalStatus, at: i64, detail: Option<&str>) -> Observation {
        Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status,
            observed_at: format_timestamp(at).unwrap(),
            expires_at: format_timestamp(at + 1800).unwrap(),
            detail: detail.map(str::to_owned),
            tools: vec![ObservedTool {
                name: "read_file".to_owned(),
                description: Some("Reads a file".to_owned()),
                annotations: Some(serde_json::json!({"readOnlyHint": true})),
            }],
        }
    }

    #[test]
    fn the_newest_observation_is_kept_until_it_expires_across_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pals.db");
        let owner = AgentKey::generate().id();
        let first = observed(OperationalStatus::Failed, 1000, Some(DETAIL_UNREACHABLE));
        let newer = observed(OperationalStatus::Ok, 2000, None);
        {
            let store = Store::open(&path, owner).unwrap();
            store.observation_put("example:repo", &first).unwrap();
            store.observation_put("example:repo", &newer).unwrap();
            store
                .observation_put("example:other", &observed(OperationalStatus::Ok, 100, None))
                .unwrap();
        }
        let store = Store::open(&path, owner).unwrap();
        let kept = store.observations(2000).unwrap();
        assert_eq!(kept.len(), 1, "the other one expired at 1900");
        assert_eq!(kept["example:repo"], newer);
        let (source, status, observed_at, expires_at): (String, String, i64, i64) = store
            .conn()
            .query_row(
                "SELECT source, status, observed_at, expires_at FROM inventory_observations
                 WHERE entry_id = 'example:repo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(source, r#"{"kind":"adapter","method":"mcp tools/list"}"#);
        assert_eq!(
            (status.as_str(), observed_at, expires_at),
            ("ok", 2000, 3800)
        );
        // Until it expires.
        assert!(store.observations(3800).unwrap().is_empty());
    }

    #[test]
    fn an_observation_a_view_must_not_carry_is_never_kept_nor_read() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let raw = observed(
            OperationalStatus::Failed,
            1000,
            Some("connection refused: http://10.0.0.7:3000/mcp"),
        );
        let err = store.observation_put("example:repo", &raw).unwrap_err();
        assert!(!format!("{err:#}").contains("10.0.0.7"), "{err:#}");
        assert!(store.observations(0).unwrap().is_empty());

        // A row written some other way hides only itself.
        store
            .observation_put("example:good", &observed(OperationalStatus::Ok, 1000, None))
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO inventory_observations
                     (entry_id, source, status, observed_at, expires_at, detail_json)
                 VALUES ('example:bad', 'x', 'ok', 1000, 9999, ?1)",
                [serde_json::to_string(&raw).unwrap()],
            )
            .unwrap();
        let kept = store.observations(1000).unwrap();
        assert_eq!(kept.keys().collect::<Vec<_>>(), ["example:good"]);
    }
}
