//! The inventory this agent serves, with what its adapters observed.
//!
//! [`InventoryState::load`] reads the file of `[inventory] file`
//! ([`pals_core::inventory::Inventory::load`]) when the node starts, and
//! that is the only time it is read: milestone 2 has no live reload, so
//! changing a model or a service means restarting the daemon. A missing file
//! is an empty inventory; one that cannot be read, breaks a rule of
//! `agentnet.inventory/1` or configures an adapter it may not
//! ([`mcp_adapter::check_adapters`]) stops the start with an error that
//! names the file.
//!
//! Readers take an [`InventorySnapshot`] ([`InventoryState::snapshot`]): the
//! inventory and the observations of its entries at one moment, which never
//! changes under them. New observations replace the old ones as a whole
//! ([`InventoryState::replace_observations`]) in a new snapshot, one
//! `generation` later. An observation is kept only for an entry of the
//! inventory, only when a view may carry it ([`Observation::check`]), and
//! only when every tool it names, listed or missing, is one the entry
//! offers; any other is left out and logged.
//!
//! **Adapters.** The MCP connections with an `adapter` are observed by the
//! [`McpAdapter`] ([`InventoryState::refresh`]): in the background when the
//! node starts, without delaying anything it serves
//! ([`InventoryState::startup_refresh_done`] waits for that first run), then
//! every `[inventory] refresh_interval_s`, and when the operator asks
//! (`pals inventory refresh`, `POST /inventory/refresh` of the gateway);
//! one run at a time. Never to answer a query: discovery reads the
//! snapshot only. A run observes every such connection at once, each within
//! the adapter's time, cuts what each observation shows to fit a page of
//! every scope that shows it ([`fit_observation`]), replaces their
//! observations in a new snapshot (those of other entries stay as they
//! are), and keeps them in the store ([`Store::observation_put`]), each
//! valid for twice the refresh interval. When the node starts, the
//! observations kept that are still valid stand until the first run
//! replaces them, fitted again, but only those of connections with an
//! adapter: a manual declaration is never an observation. The scopes an
//! observation is fitted to are the public one, those of the projects the
//! configuration and the entry's visibility name, and, since a grant may
//! name any project, the widest a grant's project may give
//! (`Scope::widest`). A change of what a scope shows
//! moves its view's revision, and the offers made at the old one are
//! refused as `offer_stale` (`OfferCheck`).

pub mod mcp_adapter;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use anyhow::Context;
use pals_core::clock::{Clock, SystemClock};
use pals_core::inventory::view::fit_observation;
use pals_core::inventory::{
    DETAIL_MISSING_TOOLS, Entry, Inventory, InventoryError, InventorySnapshot, McpConnection,
    Observation, OperationalStatus, Scope, Visibility,
};
use pals_core::store::Store;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};

pub use self::mcp_adapter::McpAdapter;
use self::mcp_adapter::{MCP_ADAPTER_TIMEOUT, check_adapters};
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
    store: Arc<Store>,
    /// Observes the MCP connections, dating its observations by the node's
    /// clock.
    adapter: McpAdapter,
    /// Between two runs of the adapters.
    refresh_interval: Duration,
    /// The projects of the configuration, whose scopes an observation must
    /// fit a page of.
    projects: Vec<String>,
    /// Held by the run of the adapters in progress.
    refreshing: Mutex<()>,
    /// The report of the first run, once it is over.
    startup: watch::Sender<Option<RefreshReport>>,
}

/// What a run of the adapters observed, as the snapshot and the store hold
/// it: the snapshot's generation after it, how many connections it
/// observed, how many it found operational, and, for each of the others,
/// its id and the fixed code of its observation's `detail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub generation: u64,
    pub observed: usize,
    pub ok: usize,
    /// `(entry id, code)`, in the inventory's order.
    pub failed: Vec<(String, String)>,
}

impl InventoryState {
    /// [`InventoryState::load_with`] an adapter of [`MCP_ADAPTER_TIMEOUT`]
    /// and the system's clock.
    pub fn load(cfg: &Config, store: Arc<Store>) -> anyhow::Result<Self> {
        Self::load_with(
            cfg,
            store,
            McpAdapter::new(MCP_ADAPTER_TIMEOUT),
            Arc::new(SystemClock),
        )
    }

    /// Reads the inventory at `cfg.inventory.file`, at generation 0, with
    /// the observations `store` keeps of its connections with an adapter
    /// that are valid at `clock`'s time, observed from now on by `adapter`
    /// with `clock`. Empty when the file does not exist; an error that
    /// names the file when it cannot be read or is invalid.
    pub fn load_with(
        cfg: &Config,
        store: Arc<Store>,
        adapter: McpAdapter,
        clock: Arc<dyn Clock>,
    ) -> anyhow::Result<Self> {
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
        check_adapters(&inventory)
            .with_context(|| format!("cannot load the inventory {}", path.display()))?;
        let kept = store
            .observations(clock.now_unix())
            .context("cannot read the observations kept")?;
        let projects: Vec<String> = cfg.projects.keys().cloned().collect();
        // Fitted again: the scopes to fit may have changed since it was kept.
        let kept = kept
            .into_iter()
            .filter_map(|(id, observation)| {
                let entry = inventory
                    .mcp
                    .iter()
                    .find(|entry| entry.id == id && entry.body.adapter.is_some())?;
                let fitted = fit_observation(entry, observation, &scopes_to_fit(entry, &projects));
                Some((id, fitted))
            })
            .collect();
        let observations = sanitized(&inventory, kept);
        let refresh_s = cfg.inventory.refresh_interval_s;
        let validity_s = i64::try_from(refresh_s.saturating_mul(2)).unwrap_or(i64::MAX);
        let adapter = adapter.with_validity(validity_s).with_clock(clock);
        let snapshot = InventorySnapshot {
            inventory,
            observations,
            generation: 0,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                snapshot: RwLock::new(Arc::new(snapshot)),
                store,
                adapter,
                refresh_interval: Duration::from_secs(refresh_s.max(1)),
                projects,
                refreshing: Mutex::new(()),
                startup: watch::Sender::new(None),
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

    /// The adapter that observes the MCP connections.
    pub fn adapter(&self) -> &McpAdapter {
        &self.inner.adapter
    }

    /// Replaces every observation with `observations`, by entry id, in a new
    /// snapshot one generation later. An observation of an id that is no
    /// entry of the inventory, that a view must not carry
    /// ([`Observation::check`]: a `detail` other than a fixed code, say) or
    /// that names a tool its entry does not offer is left out and logged.
    pub fn replace_observations(&self, observations: BTreeMap<String, Observation>) {
        let mut snapshot = self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let observations = sanitized(&snapshot.inventory, observations);
        *snapshot = Arc::new(InventorySnapshot {
            inventory: snapshot.inventory.clone(),
            observations,
            generation: snapshot.generation + 1,
        });
    }

    /// Runs the adapters now, as the [module](self) says, after any run in
    /// progress: the report of what they observed, once the new snapshot
    /// holds it. An error when the store cannot keep the observations,
    /// which the snapshot holds all the same.
    pub async fn refresh(&self) -> anyhow::Result<RefreshReport> {
        let (report, kept) = self.run_adapters().await;
        kept.map(|()| report)
    }

    /// The report of the run of the adapters the node makes when it starts,
    /// once it is over. Only for the state of a running node, which makes
    /// that run: otherwise it never comes.
    pub async fn startup_refresh_done(&self) -> RefreshReport {
        let mut reports = self.inner.startup.subscribe();
        let report = reports
            .wait_for(Option::is_some)
            .await
            .expect("the state holds the sender");
        report.clone().expect("waited for a report")
    }

    /// The node's runs of the adapters: one now, whose report
    /// [`InventoryState::startup_refresh_done`] gives, then one every
    /// refresh interval, for as long as it runs. A run whose observations
    /// the store cannot keep is logged; its snapshot stands.
    pub(crate) async fn keep_refreshed(self) {
        let (report, kept) = self.run_adapters().await;
        if let Err(err) = kept {
            tracing::warn!(error = ?err, "cannot keep the observations of the adapters");
        }
        self.inner.startup.send_replace(Some(report));
        let interval = self.inner.refresh_interval;
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            if let Err(err) = self.refresh().await {
                tracing::warn!(error = ?err, "cannot keep the observations of the adapters");
            }
        }
    }

    /// One run of the adapters, after any in progress: its report, and
    /// whether the store kept its observations.
    async fn run_adapters(&self) -> (RefreshReport, anyhow::Result<()>) {
        let _one_at_a_time = self.inner.refreshing.lock().await;
        let snapshot = self.snapshot();
        let entries: Vec<&Entry<McpConnection>> = snapshot
            .inventory
            .mcp
            .iter()
            .filter(|entry| entry.body.adapter.is_some())
            .collect();
        if entries.is_empty() {
            let report = RefreshReport {
                generation: snapshot.generation,
                observed: 0,
                ok: 0,
                failed: Vec::new(),
            };
            return (report, Ok(()));
        }
        let adapter = &self.inner.adapter;
        let observed: Vec<(String, Observation)> =
            futures::future::join_all(entries.iter().map(|entry| async {
                let observation = adapter.observe(entry).await;
                let scopes = scopes_to_fit(entry, &self.inner.projects);
                (
                    entry.id.clone(),
                    fit_observation(entry, observation, &scopes),
                )
            }))
            .await;

        // What the snapshot holds now, and what the store keeps and the
        // report says: never anything else.
        let (generation, kept) = self.merge_observations(observed);
        let mut report = RefreshReport {
            generation,
            observed: kept.len(),
            ok: 0,
            failed: Vec::new(),
        };
        for (id, observation) in &kept {
            match (observation.status, &observation.detail) {
                (OperationalStatus::Ok, _) => report.ok += 1,
                (_, detail) => report.failed.push((
                    id.clone(),
                    detail.clone().unwrap_or_else(|| "unknown".to_owned()),
                )),
            }
        }
        let store = Arc::clone(&self.inner.store);
        let stored = tokio::task::spawn_blocking(move || {
            kept.iter()
                .try_for_each(|(id, observation)| store.observation_put(id, observation))
        })
        .await
        .context("keeping the observations did not finish")
        .and_then(|stored| stored);
        (report, stored)
    }

    /// Replaces the observations of the entries of `observed` in a new
    /// snapshot one generation later, keeping those of every other entry
    /// ([`sanitized`] all the same): its generation, and those of
    /// `observed` it holds, in their order.
    fn merge_observations(
        &self,
        observed: Vec<(String, Observation)>,
    ) -> (u64, Vec<(String, Observation)>) {
        let mut snapshot = self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let ids: Vec<String> = observed.iter().map(|(id, _)| id.clone()).collect();
        let mut observations = snapshot.observations.clone();
        observations.extend(observed);
        let observations = sanitized(&snapshot.inventory, observations);
        let kept = ids
            .into_iter()
            .filter_map(|id| {
                let observation = observations.get(&id)?.clone();
                Some((id, observation))
            })
            .collect();
        let generation = snapshot.generation + 1;
        *snapshot = Arc::new(InventorySnapshot {
            inventory: snapshot.inventory.clone(),
            observations,
            generation,
        });
        (generation, kept)
    }
}

/// The observations of `observations` a snapshot of `inventory` may hold:
/// of an entry of it, that a view may carry, naming, listed or missing, no
/// tool the entry does not offer. The others are logged, without their
/// content.
fn sanitized(
    inventory: &Inventory,
    mut observations: BTreeMap<String, Observation>,
) -> BTreeMap<String, Observation> {
    observations.retain(|id, observation| {
        let Some(offered) = offered_tools(inventory, id) else {
            tracing::warn!("observation of {id} left out: no such entry in the inventory");
            return false;
        };
        if let Err(err) = observation.check() {
            tracing::warn!("observation of {id} left out: {err}");
            return false;
        }
        let is_offered = |name: &str| offered.iter().any(|tool| tool == name);
        let listed_offered = observation.tools.iter().all(|tool| is_offered(&tool.name));
        let missing_offered = observation
            .detail
            .as_deref()
            .and_then(|detail| detail.strip_prefix(DETAIL_MISSING_TOOLS))
            .is_none_or(|names| names.split(", ").all(is_offered));
        if !(listed_offered && missing_offered) {
            tracing::warn!("observation of {id} left out: it names a tool not offered");
            return false;
        }
        true
    });
    observations
}

/// The tools the entry `id` of `inventory` offers, none for an entry of a
/// block other than `mcp`; `None` when it has no entry `id`.
fn offered_tools<'a>(inventory: &'a Inventory, id: &str) -> Option<&'a [String]> {
    fn has<T>(block: &[Entry<T>], id: &str) -> bool {
        block.iter().any(|entry| entry.id == id)
    }
    if let Some(entry) = inventory.mcp.iter().find(|entry| entry.id == id) {
        return Some(&entry.body.offered_tools);
    }
    let other = has(&inventory.skills, id)
        || has(&inventory.plugins, id)
        || has(inventory.runtime.as_slice(), id)
        || has(&inventory.models, id)
        || has(&inventory.information, id)
        || has(inventory.hardware.as_slice(), id)
        || has(inventory.network.as_slice(), id);
    other.then_some(&[])
}

/// The scopes `entry` must fit a page of, among those it shows in: the
/// public one; the project and task scopes of each project of the
/// configuration (`projects`) and of its visibility; and the widest of any
/// other project a grant may name ([`Scope::widest`]), for an entry shown to
/// every scope, whose grantees' projects nothing here lists.
fn scopes_to_fit(entry: &Entry<McpConnection>, projects: &[String]) -> Vec<Scope> {
    let mut named: BTreeSet<&str> = projects.iter().map(String::as_str).collect();
    if let Visibility::Project(own) | Visibility::Task(own) = &entry.visibility {
        named.extend(own.iter().map(String::as_str));
    }
    std::iter::once(Scope::Public)
        .chain(named.into_iter().flat_map(|project| {
            [
                Scope::Project(project.to_owned()),
                Scope::Task(project.to_owned()),
            ]
        }))
        .chain(Scope::widest())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;

    use pals_core::clock::ManualClock;
    use pals_core::grant::format_timestamp;
    use pals_core::identity::AgentKey;
    use pals_core::inventory::{
        AdapterMethod, DETAIL_UNREACHABLE, Observation, ObservationSource, ObservedTool,
        OperationalStatus, missing_tools_detail,
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
                method: AdapterMethod::McpToolsList,
            },
            status: OperationalStatus::Failed,
            observed_at: "2026-11-14T00:00:00Z".to_owned(),
            expires_at: "2026-11-14T00:30:00Z".to_owned(),
            detail: Some(detail.to_owned()),
            tools: Vec::new(),
        }
    }

    fn with_tool(mut observation: Observation, name: &str) -> Observation {
        observation.tools.push(ObservedTool {
            name: name.to_owned(),
            description: None,
            annotations: None,
        });
        observation
    }

    const MCP_ENTRY: &str = "[[mcp]]\nid = \"example:repo\"\ndescription = \"Reads the repo\"\n\
                             provenance = \"operator\"\noffered_tools = [\"read_file\"]\n";
    const OTHER_ENTRY: &str = "[[mcp]]\nid = \"example:other\"\ndescription = \"Reads more\"\n\
                               provenance = \"operator\"\noffered_tools = [\"read_file\"]\n\
                               [[plugins]]\nid = \"example:plugin\"\ndescription = \"A plugin\"\n\
                               provenance = \"operator\"\n";
    const ADAPTED_ENTRY: &str = "[[mcp]]\nid = \"example:adapted\"\n\
                                 description = \"Reads through an adapter\"\n\
                                 provenance = \"operator\"\noffered_tools = [\"read_file\"]\n\
                                 [mcp.adapter]\ntransport = \"stdio\"\ncommand = \"npx\"\n";

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

        // An adapter it may not configure, too.
        fs::write(
            dir.path().join(INVENTORY_FILE),
            format!(
                "{MCP_ENTRY}[mcp.adapter]\ntransport = \"http\"\n\
                 url = \"http://10.0.0.7:3000/mcp\"\n"
            ),
        )
        .unwrap();
        let err = InventoryState::load(&cfg, store()).err().expect("refused");
        let err = format!("{err:#}");
        assert!(err.contains(INVENTORY_FILE), "{err}");
        assert!(err.contains("adapter.url"), "{err}");
    }

    #[test]
    fn replacing_observations_moves_the_generation_and_keeps_only_fixed_details() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(INVENTORY_FILE),
            format!("{MCP_ENTRY}{OTHER_ENTRY}"),
        )
        .unwrap();
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

    #[test]
    fn an_observation_names_only_entries_and_the_tools_they_offer() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(INVENTORY_FILE),
            format!("{MCP_ENTRY}{OTHER_ENTRY}"),
        )
        .unwrap();
        let state = InventoryState::load(&config_in(dir.path()), store()).unwrap();
        let ok = Observation {
            status: OperationalStatus::Ok,
            detail: None,
            ..failed(DETAIL_UNREACHABLE)
        };
        state.replace_observations(BTreeMap::from([
            // Not an entry: a service id, and an id of nothing.
            ("repo-review".to_owned(), ok.clone()),
            ("example:nothing".to_owned(), ok.clone()),
            // A tool listed or missing that the entry does not offer.
            (
                "example:repo".to_owned(),
                with_tool(ok.clone(), "delete_file"),
            ),
            (
                "example:other".to_owned(),
                failed(&missing_tools_detail(&["read_file", "delete_file"])),
            ),
            // A plugin offers no tools.
            (
                "example:plugin".to_owned(),
                with_tool(ok.clone(), "read_file"),
            ),
        ]));
        assert!(state.snapshot().observations.is_empty());

        state.replace_observations(BTreeMap::from([
            (
                "example:repo".to_owned(),
                with_tool(ok.clone(), "read_file"),
            ),
            ("example:plugin".to_owned(), ok),
        ]));
        let kept = &state.snapshot().observations;
        assert_eq!(
            kept.keys().collect::<Vec<_>>(),
            ["example:plugin", "example:repo"]
        );
    }

    #[test]
    fn only_kept_observations_of_adapted_connections_stand_at_load() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(INVENTORY_FILE),
            format!("{MCP_ENTRY}{ADAPTED_ENTRY}"),
        )
        .unwrap();
        let store = store();
        let now = 2_000_000_000;
        let ok = |at: i64| Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status: OperationalStatus::Ok,
            observed_at: format_timestamp(at).unwrap(),
            expires_at: format_timestamp(at + 1800).unwrap(),
            detail: None,
            tools: Vec::new(),
        };
        store.observation_put("example:repo", &ok(now)).unwrap();
        store.observation_put("example:adapted", &ok(now)).unwrap();
        store.observation_put("example:gone", &ok(now)).unwrap();
        let clock = Arc::new(ManualClock::new(now + 60));
        let state = InventoryState::load_with(
            &config_in(dir.path()),
            Arc::clone(&store),
            McpAdapter::new(MCP_ADAPTER_TIMEOUT),
            clock.clone(),
        )
        .unwrap();
        let snapshot = state.snapshot();
        assert_eq!(
            snapshot.observations.keys().collect::<Vec<_>>(),
            ["example:adapted"]
        );
        assert_eq!(snapshot.generation, 0);

        // One that expired does not stand.
        clock.set(now + 1800);
        let state = InventoryState::load_with(
            &config_in(dir.path()),
            store,
            McpAdapter::new(MCP_ADAPTER_TIMEOUT),
            clock,
        )
        .unwrap();
        assert!(state.snapshot().observations.is_empty());
    }

    #[test]
    fn a_merge_returns_only_what_the_snapshot_holds() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(INVENTORY_FILE),
            format!("{MCP_ENTRY}{OTHER_ENTRY}"),
        )
        .unwrap();
        let state = InventoryState::load(&config_in(dir.path()), store()).unwrap();
        // Kept by an earlier run, and untouched by this one.
        state.replace_observations(BTreeMap::from([(
            "example:other".to_owned(),
            failed(DETAIL_UNREACHABLE),
        )]));
        let ok = Observation {
            status: OperationalStatus::Ok,
            detail: None,
            ..failed(DETAIL_UNREACHABLE)
        };
        let (generation, kept) = state.merge_observations(vec![
            (
                "example:repo".to_owned(),
                with_tool(ok.clone(), "read_file"),
            ),
            // An id of nothing, and a tool not offered: left out of both.
            ("example:nothing".to_owned(), ok.clone()),
            ("example:plugin".to_owned(), with_tool(ok, "read_file")),
        ]);
        let snapshot = state.snapshot();
        assert_eq!(generation, snapshot.generation);
        let kept_ids: Vec<&str> = kept.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(kept_ids, ["example:repo"]);
        assert_eq!(kept[0].1, snapshot.observations["example:repo"]);
        assert_eq!(
            snapshot.observations.keys().collect::<Vec<_>>(),
            ["example:other", "example:repo"]
        );
    }

    #[test]
    fn an_entry_fits_the_scopes_it_shows_in() {
        let inventory = Inventory::from_toml(&ADAPTED_ENTRY.replace(
            "offered_tools",
            "visibility = { task = [\"demo\"] }\noffered_tools",
        ))
        .unwrap();
        let scopes = scopes_to_fit(&inventory.mcp[0], &["ops".to_owned()]);
        let widest = "\\".repeat(pals_core::inventory::scope::MAX_PROJECT_NAME_BYTES);
        let keys: Vec<String> = scopes.iter().map(Scope::key).collect();
        assert_eq!(
            keys,
            [
                "public".to_owned(),
                "project:demo".to_owned(),
                "task:demo".to_owned(),
                "project:ops".to_owned(),
                "task:ops".to_owned(),
                format!("project:{widest}"),
                format!("task:{widest}"),
            ]
        );
    }

    #[test]
    fn a_kept_observation_is_fitted_again_at_load() {
        let dir = tempfile::tempdir().unwrap();
        // A public connection with an adapter, offering 40 tools.
        let tools: Vec<String> = (0..40).map(|n| format!("tool_{n:02}")).collect();
        let entry = ADAPTED_ENTRY
            .replace(
                "offered_tools = [\"read_file\"]",
                &format!("offered_tools = {}", serde_json::to_string(&tools).unwrap()),
            )
            .replace("provenance", "visibility = \"public\"\nprovenance");
        fs::write(dir.path().join(INVENTORY_FILE), &entry).unwrap();
        let store = store();
        let now = 2_000_000_000;
        // Kept whole, as a build that fitted only the public scope, or none,
        // kept it: 40 tools with texts of 1 KiB, far more than a page.
        let text = "x".repeat(pals_core::inventory::MAX_OBSERVED_DESCRIPTION_BYTES);
        let mut kept = Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status: OperationalStatus::Ok,
            observed_at: format_timestamp(now).unwrap(),
            expires_at: format_timestamp(now + 1800).unwrap(),
            detail: None,
            tools: Vec::new(),
        };
        for name in &tools {
            kept.tools.push(pals_core::inventory::ObservedTool {
                name: name.clone(),
                description: Some(text.clone()),
                annotations: Some(serde_json::json!({"title": text})),
            });
        }
        store.observation_put("example:adapted", &kept).unwrap();
        let state = InventoryState::load_with(
            &config_in(dir.path()),
            store,
            McpAdapter::new(MCP_ADAPTER_TIMEOUT),
            Arc::new(ManualClock::new(now + 60)),
        )
        .unwrap();
        let loaded = &state.snapshot().observations["example:adapted"];
        assert_eq!(loaded.status, OperationalStatus::Ok);
        assert_eq!(loaded.tools.len(), 40);
        let cap = loaded.tools[0].description.as_ref().unwrap().len();
        assert!(0 < cap && cap < text.len(), "cut to {cap}");
        // As a run of the adapter fits it.
        let inventory = Inventory::from_toml(&entry).unwrap();
        let mcp = &inventory.mcp[0];
        assert_eq!(
            *loaded,
            fit_observation(mcp, kept, &scopes_to_fit(mcp, &[]))
        );
    }
}
