//! The inventory this agent serves, with what its adapters observed.
//!
//! [`InventoryState::load`] reads the file of `[inventory] file`
//! ([`pals_core::inventory::Inventory::load`]) when the node starts, and
//! that is the only time it is read: milestone 2 has no live reload, so
//! changing a model or a service means restarting the daemon. A missing file
//! is an empty inventory; one that cannot be read or breaks a rule of
//! `agentnet.inventory/1` stops the start with an error that names the file.
//!
//! Readers take an [`InventorySnapshot`] ([`InventoryState::snapshot`]): the
//! inventory and the observations of its entries at one moment, which never
//! changes under them. New observations replace the old ones as a whole
//! ([`InventoryState::replace_observations`]) in a new snapshot, one
//! `generation` later.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, PoisonError, RwLock};

use anyhow::Context;
use pals_core::inventory::{Inventory, InventoryError, InventorySnapshot, Observation};
use pals_core::store::Store;

use crate::config::Config;

/// The node's inventory and the observations of its entries. Clones share
/// it.
#[derive(Clone)]
pub struct InventoryState {
    inner: Arc<Inner>,
}

struct Inner {
    snapshot: RwLock<Arc<InventorySnapshot>>,
    /// Where the adapters' observations are kept across restarts.
    #[expect(dead_code, reason = "no inventory adapter persists observations yet")]
    store: Arc<Store>,
}

impl InventoryState {
    /// Reads the inventory at `cfg.inventory.file`, with no observations yet
    /// and generation 0. Empty when the file does not exist; an error that
    /// names the file when it cannot be read or is invalid.
    pub fn load(cfg: &Config, store: Arc<Store>) -> anyhow::Result<Self> {
        let path = &cfg.inventory.file;
        let inventory = match Inventory::load(path) {
            Ok(inventory) => inventory,
            Err(InventoryError::Io(err)) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no inventory at {}: it is empty", path.display());
                Inventory::default()
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot load the inventory {}", path.display()));
            }
        };
        let snapshot = InventorySnapshot {
            inventory,
            observations: BTreeMap::new(),
            generation: 0,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                snapshot: RwLock::new(Arc::new(snapshot)),
                store,
            }),
        })
    }

    /// The inventory and its observations now.
    pub fn snapshot(&self) -> Arc<InventorySnapshot> {
        Arc::clone(
            &self
                .inner
                .snapshot
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Replaces every observation with `observations`, by entry id, in a new
    /// snapshot one generation later. An observation a view must not carry
    /// ([`Observation::check`]: a `detail` other than a fixed code, say) is
    /// left out and logged.
    pub fn replace_observations(&self, mut observations: BTreeMap<String, Observation>) {
        observations.retain(|id, observation| match observation.check() {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("observation of {id} left out: {err}");
                false
            }
        });
        let mut snapshot = self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        *snapshot = Arc::new(InventorySnapshot {
            inventory: snapshot.inventory.clone(),
            observations,
            generation: snapshot.generation + 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;

    use pals_core::identity::AgentKey;
    use pals_core::inventory::{
        DETAIL_UNREACHABLE, Observation, ObservationSource, OperationalStatus, missing_tools_detail,
    };
    use pals_core::store::Store;

    use super::*;
    use crate::config::{Config, INVENTORY_FILE};

    fn config_in(dir: &std::path::Path) -> Config {
        let path = dir.join(crate::config::CONFIG_FILE);
        fs::write(&path, "").unwrap();
        Config::load(&path).unwrap()
    }

    fn store() -> Arc<Store> {
        Arc::new(Store::open_in_memory(AgentKey::generate().id()).unwrap())
    }

    fn failed(detail: &str) -> Observation {
        Observation {
            source: ObservationSource::Adapter {
                method: "mcp tools/list".to_owned(),
            },
            status: OperationalStatus::Failed,
            observed_at: "2026-11-14T00:00:00Z".to_owned(),
            expires_at: "2026-11-14T00:30:00Z".to_owned(),
            detail: Some(detail.to_owned()),
            tools: Vec::new(),
        }
    }

    const MCP_ENTRY: &str = "[[mcp]]\nid = \"example:repo\"\ndescription = \"Reads the repo\"\n\
                             provenance = \"operator\"\noffered_tools = [\"read_file\"]\n";

    #[test]
    fn a_missing_file_is_an_empty_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let state = InventoryState::load(&config_in(dir.path()), store()).unwrap();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.inventory, Default::default());
        assert!(snapshot.observations.is_empty());
        assert_eq!(snapshot.generation, 0);
    }

    #[test]
    fn the_file_is_read_and_an_invalid_one_fails_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config_in(dir.path());
        fs::write(dir.path().join(INVENTORY_FILE), MCP_ENTRY).unwrap();
        let state = InventoryState::load(&cfg, store()).unwrap();
        assert_eq!(state.snapshot().inventory.mcp[0].id, "example:repo");

        fs::write(
            dir.path().join(INVENTORY_FILE),
            format!("{MCP_ENTRY}operational = \"ok\"\n"),
        )
        .unwrap();
        let err = InventoryState::load(&cfg, store()).err().expect("refused");
        let err = format!("{err:#}");
        assert!(err.contains(INVENTORY_FILE), "{err}");
        assert!(err.contains("operational"), "{err}");
    }

    #[test]
    fn replacing_observations_moves_the_generation_and_keeps_only_fixed_details() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(INVENTORY_FILE), MCP_ENTRY).unwrap();
        let state = InventoryState::load(&config_in(dir.path()), store()).unwrap();
        let before = state.snapshot();
        let clone = state.clone();

        let mut observations = BTreeMap::new();
        observations.insert("example:repo".to_owned(), failed(DETAIL_UNREACHABLE));
        clone.replace_observations(observations);
        let after = state.snapshot();
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.inventory, before.inventory);
        assert_eq!(
            after.observations["example:repo"].detail.as_deref(),
            Some(DETAIL_UNREACHABLE)
        );
        // A snapshot already taken does not change.
        assert!(before.observations.is_empty());

        // A raw error never reaches a snapshot, and so never a view.
        let mut observations = BTreeMap::new();
        observations.insert(
            "example:repo".to_owned(),
            failed("connection refused: http://10.0.0.7:3000/mcp"),
        );
        observations.insert(
            "example:other".to_owned(),
            failed(&missing_tools_detail(&["read_file"])),
        );
        state.replace_observations(observations);
        let last = state.snapshot();
        assert_eq!(last.generation, before.generation + 2);
        assert!(!last.observations.contains_key("example:repo"));
        assert_eq!(
            last.observations["example:other"].detail.as_deref(),
            Some("missing tools: read_file")
        );
    }
}
