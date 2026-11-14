//! Matching a service query on local metadata only (design §7.3, §8):
//! the inventory as the requester's scope shows it, its observations, and
//! the worker's load as its counters say. Nothing runs: no model, no tool,
//! no adapter, no peer. [`evaluate`] is pure.
//!
//! **Service.** The services the scope shows that have the capability
//! asked for. With none, the answer is [`OfferStatus::NotOffered`] and
//! nothing else: the same whether a service the scope does not show has the
//! capability or no service has it (§9.8, §10). With several, the first
//! compatible one, else the first that only lacks information, else the
//! first, in the inventory's order.
//!
//! **Requirements**, each a [`RequirementResult`] named after what it asks:
//!
//! | Requirement | Met when |
//! |-|-|
//! | `input_media_type:<t>`, `output_media_type:<t>` | the service accepts, or produces, `<t>` (`*/*` and `type/*` cover it); unknown when it declares no modes |
//! | `language`, `required_region` | the service declares it (ASCII case aside); unknown when it declares none |
//! | `external_cloud_allowed` (`false`) | the service declares it uses no external cloud; unknown when it declares nothing |
//! | `mcp_integration:<id>` | the scope shows the connection and an observation in force says it is operational (`observed`); one in force that says it failed is not met; shown but not observed in force, or not shown, exists or not, is unknown |
//! | `information_source:<id>`, `skill_package:<id>`, `plugin:<id>`, `model:<id>` | the scope shows the entry (`declared`, or `observed` with an operational observation in force); with a failed one in force, not met; not shown, unknown |
//! | `execution_external_cloud_allowed` (`false`) | no model among the service's components the scope shows runs at an external provider, and the service declares no external cloud |
//! | `min_offered_ram_mib`, `min_offered_vram_mib`, `accelerator:<a>` | against what the hardware offers (`offered`), never what is installed (§8, §9.6); unknown when the scope does not show it or it offers nothing of that |
//! | any other key | never: unknown, and named in `unknown_requirements` when mandatory (`<key>` under `requirements`, `inventory.<key>` under `inventory.required`); ignored when only preferred |
//!
//! An observation is in force until its `expires_at` and
//! [`CLOCK_SKEW_S`] past it, by this agent's own clock. Everything under
//! `requirements` is mandatory but `inventory.preferred`, which never makes
//! an offer incompatible.
//!
//! **Result.** Compatible when every mandatory requirement is met:
//! [`OfferStatus::Available`] while the worker runs fewer tasks than it
//! takes at once, [`OfferStatus::Busy`] otherwise. Not compatible only for
//! want of information: [`OfferStatus::NeedsInformation`]. A mandatory
//! requirement not met: [`OfferStatus::NotOffered`], with the service and
//! why. The times are the service's declarations, absent when it has none,
//! never zero.

use super::Load;
use super::query::{
    Basis, Conditions, Met, OfferStatus, RequirementResult, RequirementSet, ServiceQuery,
};
use crate::grant::{CLOCK_SKEW_S, parse_timestamp};
use crate::inventory::{
    Entry, InventorySnapshot, ModelExecution, Observation, OfferedResources, OperationalStatus,
    Scope, Service,
};

/// An offer without identity, times, binding or signature: what the
/// matching decides, which [`super::query::ServiceOffer::new`] completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub skill_id: Option<String>,
    pub status: OfferStatus,
    pub compatible: bool,
    pub conditions: Conditions,
    pub requirement_results: Vec<RequirementResult>,
    pub unknown_requirements: Vec<String>,
    pub estimated_start_delay_ms: Option<u64>,
    pub estimated_duration_ms: Option<u64>,
    pub limitations: Vec<String>,
}

/// How `query` fares against `snapshot` as `scope` shows it, with the
/// worker's `load`, at `now_unix` by this agent's clock (see the module).
pub fn evaluate(
    query: &ServiceQuery,
    snapshot: &InventorySnapshot,
    scope: &Scope,
    load: Load,
    now_unix: i64,
) -> Evaluation {
    // Only a task scope's grant lets the requester send tasks.
    let authorization_required = !matches!(scope, Scope::Task(_));
    let matcher = Matcher {
        query,
        snapshot,
        scope,
        now_unix,
    };
    snapshot
        .inventory
        .services
        .iter()
        .filter(|service| {
            service.visibility.visible_in(scope) && service.capabilities.contains(&query.capability)
        })
        .map(|service| matcher.service(service, load, authorization_required))
        // The first of the best.
        .min_by_key(rank)
        .unwrap_or_else(|| not_offered(authorization_required))
}

/// The answer when the scope shows no service of the capability: the same
/// whatever the scope does not show.
fn not_offered(authorization_required: bool) -> Evaluation {
    Evaluation {
        skill_id: None,
        status: OfferStatus::NotOffered,
        compatible: false,
        conditions: Conditions {
            external_cloud_used: None,
            processing_region: None,
            authorization_required,
        },
        requirement_results: Vec::new(),
        unknown_requirements: Vec::new(),
        estimated_start_delay_ms: None,
        estimated_duration_ms: None,
        limitations: Vec::new(),
    }
}

/// Compatible first, then lacking information only, then the rest.
fn rank(evaluation: &Evaluation) -> u8 {
    match (evaluation.compatible, evaluation.status) {
        (true, _) => 0,
        (false, OfferStatus::NeedsInformation) => 1,
        (false, _) => 2,
    }
}

/// What a query is matched against.
struct Matcher<'a> {
    query: &'a ServiceQuery,
    snapshot: &'a InventorySnapshot,
    scope: &'a Scope,
    now_unix: i64,
}

impl Matcher<'_> {
    /// How `service` fares.
    fn service(&self, service: &Service, load: Load, authorization_required: bool) -> Evaluation {
        let query = self.query;
        let requirements = &query.requirements;
        let mut results = Vec::new();
        let mut unknown = Vec::new();
        for wanted in &query.input_media_types {
            results.push(media("input_media_type", &service.input_modes, wanted));
        }
        for wanted in &query.output_media_types {
            results.push(media("output_media_type", &service.output_modes, wanted));
        }
        if let Some(language) = &requirements.language {
            let declared = (!service.languages.is_empty()).then(|| {
                service
                    .languages
                    .iter()
                    .any(|spoken| spoken.eq_ignore_ascii_case(language))
            });
            results.push(declared_as("language", true, declared));
        }
        if requirements.external_cloud_allowed == Some(false) {
            let declared = service.external_cloud_used.map(|used| !used);
            results.push(declared_as("external_cloud_allowed", true, declared));
        }
        if let Some(region) = &requirements.required_region {
            let declared = service
                .processing_region
                .as_ref()
                .map(|processed| processed.eq_ignore_ascii_case(region));
            results.push(declared_as("required_region", true, declared));
        }
        for key in requirements.other.keys() {
            results.push(unknown_result(key.clone(), true));
            unknown.push(key.clone());
        }
        if let Some(inventory) = &requirements.inventory {
            self.set(service, &inventory.required, true, &mut results);
            for key in inventory.required.other.keys() {
                let name = format!("inventory.{key}");
                results.push(unknown_result(name.clone(), true));
                unknown.push(name);
            }
            // What is only preferred and unknown is ignored.
            self.set(service, &inventory.preferred, false, &mut results);
        }

        let mut mandatory = results.iter().filter(|result| result.mandatory);
        let compatible = mandatory.clone().all(|result| result.result == Met::Met);
        let status = if compatible {
            if load.running < load.max_concurrent {
                OfferStatus::Available
            } else {
                OfferStatus::Busy
            }
        } else if mandatory.all(|result| result.result != Met::NotMet) {
            OfferStatus::NeedsInformation
        } else {
            OfferStatus::NotOffered
        };
        Evaluation {
            skill_id: Some(service.id.clone()),
            status,
            compatible,
            conditions: Conditions {
                external_cloud_used: service.external_cloud_used,
                processing_region: service.processing_region.clone(),
                authorization_required,
            },
            requirement_results: results,
            unknown_requirements: unknown,
            estimated_start_delay_ms: service.estimated_start_delay_ms,
            estimated_duration_ms: service.estimated_duration_ms,
            limitations: service.limitations.clone(),
        }
    }

    /// The results of the known requirements of `set`, mandatory or not;
    /// its unknown ones are the caller's.
    fn set(
        &self,
        service: &Service,
        set: &RequirementSet,
        mandatory: bool,
        results: &mut Vec<RequirementResult>,
    ) {
        let inventory = &self.snapshot.inventory;
        for id in &set.mcp_integration_ids {
            results.push(self.integration(id, mandatory));
        }
        for id in &set.information_source_ids {
            let shown = self.shown(&inventory.information, id).is_some();
            results.push(self.entry("information_source", id, shown, mandatory));
        }
        for id in &set.skill_package_ids {
            let shown = self.shown(&inventory.skills, id).is_some();
            results.push(self.entry("skill_package", id, shown, mandatory));
        }
        for id in &set.plugin_ids {
            let shown = self.shown(&inventory.plugins, id).is_some();
            results.push(self.entry("plugin", id, shown, mandatory));
        }
        for id in &set.model_ids {
            let shown = self.shown(&inventory.models, id).is_some();
            results.push(self.entry("model", id, shown, mandatory));
        }
        if set.execution_external_cloud_allowed == Some(false) {
            results.push(self.execution_in_house(service, mandatory));
        }
        if let Some(min) = set.min_offered_ram_mib {
            let offered = self.offered().map(|offered| offered.ram_mib);
            results.push(at_least("min_offered_ram_mib", mandatory, offered, min));
        }
        if let Some(min) = set.min_offered_vram_mib {
            let offered = self.offered().map(|offered| offered.vram_mib_each);
            results.push(at_least("min_offered_vram_mib", mandatory, offered, min));
        }
        for wanted in &set.accelerators {
            let offered = self
                .offered()
                .map(|offered| &offered.accelerators)
                .filter(|accelerators| !accelerators.is_empty());
            let declared = offered.map(|accelerators| {
                accelerators
                    .iter()
                    .any(|accelerator| accelerator.eq_ignore_ascii_case(wanted))
            });
            let name = format!("accelerator:{wanted}");
            results.push(declared_as(&name, mandatory, declared));
        }
    }

    /// An MCP connection: only an observation in force shows it
    /// operational, or failed.
    fn integration(&self, id: &str, mandatory: bool) -> RequirementResult {
        let name = format!("mcp_integration:{id}");
        if self.shown(&self.snapshot.inventory.mcp, id).is_none() {
            return unknown_result(name, mandatory);
        }
        match self.in_force(id).map(|observation| observation.status) {
            Some(OperationalStatus::Ok) => result(name, mandatory, Met::Met, Basis::Observed),
            Some(OperationalStatus::Failed) => {
                result(name, mandatory, Met::NotMet, Basis::Observed)
            }
            Some(OperationalStatus::Unknown) | None => unknown_result(name, mandatory),
        }
    }

    /// An entry of another block: shown, it is met as declared, unless an
    /// observation in force says otherwise.
    fn entry(&self, kind: &str, id: &str, shown: bool, mandatory: bool) -> RequirementResult {
        let name = format!("{kind}:{id}");
        if !shown {
            return unknown_result(name, mandatory);
        }
        match self.in_force(id).map(|observation| observation.status) {
            Some(OperationalStatus::Ok) => result(name, mandatory, Met::Met, Basis::Observed),
            Some(OperationalStatus::Failed) => {
                result(name, mandatory, Met::NotMet, Basis::Observed)
            }
            Some(OperationalStatus::Unknown) | None => {
                result(name, mandatory, Met::Met, Basis::Declared)
            }
        }
    }

    /// No model of `service`'s the scope shows runs at an external
    /// provider, and the service declares no external cloud.
    fn execution_in_house(&self, service: &Service, mandatory: bool) -> RequirementResult {
        let models = &self.snapshot.inventory.models;
        let external = service
            .components
            .iter()
            .filter_map(|link| self.shown(models, &link.id))
            .any(|model| matches!(model.body.execution, Some(ModelExecution::External { .. })));
        let declared = if external {
            Some(false)
        } else {
            service.external_cloud_used.map(|used| !used)
        };
        declared_as("execution_external_cloud_allowed", mandatory, declared)
    }

    /// The entry `id` of `block`, if the scope shows it.
    fn shown<'b, T>(&self, block: &'b [Entry<T>], id: &str) -> Option<&'b Entry<T>> {
        block
            .iter()
            .find(|entry| entry.id == id && entry.visibility.visible_in(self.scope))
    }

    /// What the hardware the scope shows offers.
    fn offered(&self) -> Option<&OfferedResources> {
        let hardware = self.snapshot.inventory.hardware.as_ref()?;
        if !hardware.visibility.visible_in(self.scope) {
            return None;
        }
        hardware.body.offered.as_ref()
    }

    /// The observation of `id` in force now, by this agent's clock: until
    /// its `expires_at` and [`CLOCK_SKEW_S`] past it; never one a view
    /// would not carry.
    fn in_force(&self, id: &str) -> Option<&Observation> {
        self.snapshot.observations.get(id).filter(|observation| {
            observation.check().is_ok()
                && parse_timestamp(&observation.expires_at)
                    .is_some_and(|expires_at| self.now_unix < expires_at + CLOCK_SKEW_S)
        })
    }
}

/// Whether `declared`, the modes of a service, cover `wanted`: the same
/// type, `type/*` of its type, or `*/*`; parameters and ASCII case aside.
/// Unknown when the service declares none.
fn media(kind: &str, declared: &[String], wanted: &str) -> RequirementResult {
    let name = format!("{kind}:{wanted}");
    if declared.is_empty() {
        return unknown_result(name, true);
    }
    let wanted = essence(wanted);
    let wanted_type = wanted.split_once('/').map(|(kind, _)| kind);
    let covered = declared.iter().map(|mode| essence(mode)).any(|mode| {
        mode == "*/*"
            || mode == wanted
            || mode
                .strip_suffix("/*")
                .is_some_and(|kind| Some(kind) == wanted_type)
    });
    let met = if covered { Met::Met } else { Met::NotMet };
    result(name, true, met, Basis::Declared)
}

/// A media type without its parameters, in lowercase.
fn essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// At least `min` of what is `offered`, when it says.
fn at_least(
    name: &str,
    mandatory: bool,
    offered: Option<Option<u64>>,
    min: u64,
) -> RequirementResult {
    declared_as(
        name,
        mandatory,
        offered.flatten().map(|offered| offered >= min),
    )
}

/// Met or not as declared; unknown without a declaration.
fn declared_as(name: &str, mandatory: bool, declared: Option<bool>) -> RequirementResult {
    match declared {
        Some(true) => result(name.to_owned(), mandatory, Met::Met, Basis::Declared),
        Some(false) => result(name.to_owned(), mandatory, Met::NotMet, Basis::Declared),
        None => unknown_result(name.to_owned(), mandatory),
    }
}

fn result(requirement: String, mandatory: bool, met: Met, basis: Basis) -> RequirementResult {
    RequirementResult {
        requirement,
        mandatory,
        result: met,
        basis: Some(basis),
    }
}

fn unknown_result(requirement: String, mandatory: bool) -> RequirementResult {
    RequirementResult {
        requirement,
        mandatory,
        result: Met::Unknown,
        basis: None,
    }
}
