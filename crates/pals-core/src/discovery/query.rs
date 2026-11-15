//! Service queries and offers (`agentnet.service-query/1`,
//! `agentnet.service-offer/1`): what a requester asks a candidate before
//! any task, and what the candidate answers (design §7.1, §7.2).
//!
//! **Query** ([`ServiceQuery`]). A capability, the media types that go in
//! and come out, requirements, and nothing that would run work: travels as
//! its JCS (RFC 8785), at most [`MAX_QUERY_BYTES`], in one spelling (an
//! absent field is left out, never `null` nor `[]`), so that its hash
//! ([`query_hash`], BLAKE3 of those bytes) is the same on both sides.
//! [`ServiceQuery::from_jcs`] refuses anything else ([`QueryError`]), and
//! [`ServiceQuery::validate`] the rules its types do not keep: `query_id`
//! of 1 to [`MAX_QUERY_ID_CHARS`] characters `[A-Za-z0-9_-]`, a capability
//! as the inventory writes them, at most [`MAX_MEDIA_TYPES`] media types
//! per list and [`MAX_REQUIREMENT_IDS`] ids per list of requirements, a
//! `summary` of at most [`MAX_SUMMARY_BYTES`], a `project` that is a
//! project name, and `execution_authorized` `false`: a query never
//! authorizes execution. A `reply_timeout_ms` over [`MAX_REPLY_TIMEOUT_MS`]
//! is no error: it counts as that ([`ServiceQuery::reply_timeout`]).
//! Requirements this agent does not know are kept, as `other`, named by 1
//! to 255 printable ASCII characters, since an offer repeats their names:
//! mandatory ones are never met (§9.1). No inventory requirements are left
//! out: `"inventory": {}` is refused, so that it is no second spelling of
//! them.
//!
//! **Offer** ([`ServiceOffer`]). The candidate's answer, made from the
//! matching of the query against its inventory and load
//! ([`super::matching::evaluate`]) and bound to everything it depends on:
//! the requester, the query's hash, the scope its grant gave, the card of
//! that scope and the scope's view revision. A [`SignedDoc`] under
//! [`OFFER_SIG_DOMAIN`], at most [`MAX_OFFER_BYTES`] ([`sign_offer`] cuts
//! `limitations`, then `requirement_results`, then `unknown_requirements`,
//! from the end, and says [`TRUNCATED`]). Its times are whole seconds, as a
//! grant's: `expires_at` is `issued_at` and `valid_for_ms` rounded up to a
//! second. An offer reserves nothing and authorizes nothing: the task that
//! names it is checked again, grant included, when it arrives.
//!
//! **Acceptance** ([`accept_offer`]): the one check of an offer received,
//! which the requester's client runs before it keeps it.
//!
//! **Reference** ([`OfferRef`]): how the message of the task names the
//! offer it was chosen by, for the provider to check it again.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::matching::Evaluation;
use crate::benchmark::{
    MAX_OFFER_BENCHMARK_REFS, is_benchmark_id, is_measurement_kind, is_profile,
};
use crate::card_jws::CardHash;
use crate::grant::{format_timestamp, parse_timestamp};
use crate::identity::{AgentId, AgentKey};
use crate::inventory::{
    INVENTORY_EXTENSION, Scope, Section, capability_tag, is_project_name, is_service_id,
};
use crate::jcs::canonical;
use crate::signed::{Signed, SignedDoc, SignedError, check_validity};

/// The value of [`ServiceQuery::schema`].
pub const QUERY_SCHEMA: &str = "agentnet.service-query/1";
/// The value of [`ServiceOffer::schema`].
pub const OFFER_SCHEMA: &str = "agentnet.service-offer/1";
/// The prefix of the bytes an offer signature covers.
pub const OFFER_SIG_DOMAIN: &[u8] = b"agentnet.offer/1\n";
/// The largest query, its JCS (design §7.3).
pub const MAX_QUERY_BYTES: usize = 8 * 1024;
/// The largest signed offer (design §7.3).
pub const MAX_OFFER_BYTES: usize = 16 * 1024;
/// The longest a requester waits for an offer (design §7.3); a query that
/// asks for longer gets this.
pub const MAX_REPLY_TIMEOUT_MS: u32 = 3000;
/// The longest an offer lasts.
pub const MAX_OFFER_VALID_FOR_MS: u32 = 300_000;
/// The most media types in one list of a query.
pub const MAX_MEDIA_TYPES: usize = 16;
/// The most ids, or requirements, in one list of a query.
pub const MAX_REQUIREMENT_IDS: usize = 32;
/// The longest `summary` of a query, in bytes.
pub const MAX_SUMMARY_BYTES: usize = 1024;
/// The longest query or offer id, in characters `[A-Za-z0-9_-]`.
pub const MAX_QUERY_ID_CHARS: usize = 64;
/// The limitation that ends an offer [`sign_offer`] had to cut.
pub const TRUNCATED: &str = "truncated";

/// The longest media type, or language or region asked for, in bytes.
const MAX_TOKEN_BYTES: usize = 255;
/// The largest `inventory_revision`: every integer up to here is exact as a
/// JSON number in any language.
const MAX_REVISION: u64 = (1 << 53) - 1;
/// The length of an Ed25519 signature in base64url without padding.
const SIGNATURE_CHARS: usize = 86;

/// A query for a service (design §7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceQuery {
    /// [`QUERY_SCHEMA`].
    pub schema: String,
    /// The requester's id for the query, which the offer repeats.
    pub query_id: String,
    /// A common-vocabulary or namespaced capability, as the inventory
    /// writes them ([`capability_tag`]).
    pub capability: String,
    /// Each one the service must accept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_media_types: Vec<String>,
    /// Each one the service must produce.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_media_types: Vec<String>,
    pub requirements: Requirements,
    /// The minimum context of the case; never data to work on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_timeout_ms: Option<u32>,
    /// The sections of the inventory view the requester means to read; the
    /// offer does not depend on them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_inventory_sections: Vec<Section>,
    /// One of the projects the grant presented covers; it never widens the
    /// scope the grant gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Always `false`.
    pub execution_authorized: bool,
}

/// What the requester requires of the service: mandatory, every one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Requirements {
    /// A language the service works in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// `false`: the service must declare it uses no external cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_cloud_allowed: Option<bool>,
    /// The region the service must declare it processes in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<InventoryRequirements>,
    /// Requirements this agent does not know: never met.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// Requirements on the inventory (design §9): those required, and those
/// only preferred, which never make an offer incompatible.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryRequirements {
    #[serde(default, skip_serializing_if = "RequirementSet::is_empty")]
    pub required: RequirementSet,
    #[serde(default, skip_serializing_if = "RequirementSet::is_empty")]
    pub preferred: RequirementSet,
}

/// Inventory entries and resources a service needs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RequirementSet {
    /// MCP connections, observed operational.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_integration_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub information_source_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_package_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_ids: Vec<String>,
    /// `false`: no model the service works with runs at an external
    /// provider, and the service declares no external cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_external_cloud_allowed: Option<bool>,
    /// Against the RAM offered, never the RAM installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_offered_ram_mib: Option<u64>,
    /// Against the memory offered of each accelerator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_offered_vram_mib: Option<u64>,
    /// Each one among the accelerators offered, by kind or model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accelerators: Vec<String>,
    /// Requirements this agent does not know.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

impl RequirementSet {
    /// Whether it asks for nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Its lists, with their names, for the counts [`ServiceQuery::validate`]
    /// checks.
    fn lists(&self) -> [&[String]; 6] {
        [
            &self.mcp_integration_ids,
            &self.information_source_ids,
            &self.skill_package_ids,
            &self.plugin_ids,
            &self.model_ids,
            &self.accelerators,
        ]
    }
}

/// Why a query was refused. The reasons are fixed texts: they never quote
/// the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("query larger than {MAX_QUERY_BYTES} bytes")]
    TooLarge,
    #[error("{0}")]
    Invalid(&'static str),
}

impl ServiceQuery {
    /// Reads a query as it travels: at most [`MAX_QUERY_BYTES`]
    /// ([`QueryError::TooLarge`], before it is parsed), its own JCS, in one
    /// spelling, with no field this schema does not know but under
    /// `requirements`, and keeping [`ServiceQuery::validate`].
    pub fn from_jcs(bytes: &[u8]) -> Result<Self, QueryError> {
        if bytes.len() > MAX_QUERY_BYTES {
            return Err(QueryError::TooLarge);
        }
        let malformed = QueryError::Invalid("malformed query");
        let value: Value = serde_json::from_slice(bytes).map_err(|_| malformed)?;
        if canonical(&value) != bytes {
            return Err(QueryError::Invalid("a query travels as its JCS"));
        }
        let query = Self::deserialize(&value).map_err(|_| malformed)?;
        if query.to_jcs() != bytes {
            return Err(QueryError::Invalid(
                "a query has one spelling: leave out what is absent",
            ));
        }
        query.validate()?;
        Ok(query)
    }

    /// The JCS of the query: what travels, and what [`query_hash`] hashes.
    pub fn to_jcs(&self) -> Vec<u8> {
        canonical(&serde_json::to_value(self).expect("a query always serializes"))
    }

    /// [`query_hash`] of [`ServiceQuery::to_jcs`].
    pub fn hash(&self) -> String {
        query_hash(&self.to_jcs())
    }

    /// How long the requester waits: `reply_timeout_ms`, at most
    /// [`MAX_REPLY_TIMEOUT_MS`], which is also what it waits without one.
    pub fn reply_timeout(&self) -> Duration {
        let ms = self
            .reply_timeout_ms
            .map_or(MAX_REPLY_TIMEOUT_MS, |ms| ms.min(MAX_REPLY_TIMEOUT_MS));
        Duration::from_millis(ms.into())
    }

    /// The rules of a query its types do not keep, in this order: its
    /// schema, `execution_authorized` `false`, the `query_id`, the
    /// capability, the media types (at most [`MAX_MEDIA_TYPES`] per list,
    /// each `type/subtype` in printable ASCII), the `summary`, the sections
    /// asked for, the `project` (printable ASCII without spaces), and the
    /// requirements (no empty text, at most [`MAX_REQUIREMENT_IDS`] ids per
    /// list and unknown requirements per group, each unknown one named by 1
    /// to 255 printable ASCII characters, and no `inventory` that asks for
    /// nothing).
    pub fn validate(&self) -> Result<(), QueryError> {
        let invalid = |why| Err(QueryError::Invalid(why));
        if self.schema != QUERY_SCHEMA {
            return invalid("unknown query schema");
        }
        if self.execution_authorized {
            return invalid("a query never authorizes execution");
        }
        if !is_offer_id(&self.query_id) {
            return invalid("invalid query_id: 1 to 64 characters [A-Za-z0-9_-]");
        }
        if capability_tag(&self.capability).is_err() {
            return invalid("invalid capability");
        }
        for types in [&self.input_media_types, &self.output_media_types] {
            if types.len() > MAX_MEDIA_TYPES {
                return invalid("too many media types: at most 16 per list");
            }
            if !types.iter().all(|media_type| is_media_type(media_type)) {
                return invalid("invalid media type");
            }
        }
        if self
            .summary
            .as_ref()
            .is_some_and(|s| s.len() > MAX_SUMMARY_BYTES)
        {
            return invalid("summary longer than 1 KiB");
        }
        if self.requested_inventory_sections.len() > MAX_REQUIREMENT_IDS {
            return invalid("too many sections");
        }
        if !self.project.as_deref().is_none_or(is_project_name) {
            return invalid("invalid project: printable ASCII without spaces");
        }
        let requirements = &self.requirements;
        let texts = [&requirements.language, &requirements.required_region];
        if !texts
            .iter()
            .all(|text| text.as_deref().is_none_or(is_token))
        {
            return invalid("invalid language or region");
        }
        if requirements.other.len() > MAX_REQUIREMENT_IDS {
            return invalid("too many requirements");
        }
        // An offer repeats the names of the requirements it does not know.
        let unknown_name = "invalid requirement name: 1 to 255 printable ASCII characters";
        if !requirements.other.keys().all(|name| is_token(name)) {
            return invalid(unknown_name);
        }
        if requirements.inventory.as_ref().is_some_and(|inventory| {
            inventory.required.is_empty() && inventory.preferred.is_empty()
        }) {
            return invalid("inventory requirements that ask for nothing: leave them out");
        }
        let sets = requirements
            .inventory
            .iter()
            .flat_map(|inventory| [&inventory.required, &inventory.preferred]);
        for set in sets {
            if set.other.len() > MAX_REQUIREMENT_IDS {
                return invalid("too many requirements");
            }
            if !set.other.keys().all(|name| is_token(name)) {
                return invalid(unknown_name);
            }
            for list in set.lists() {
                if list.len() > MAX_REQUIREMENT_IDS {
                    return invalid("too many requirement ids: at most 32 per list");
                }
                if !list.iter().all(|id| is_token(id)) {
                    return invalid("invalid requirement id");
                }
            }
        }
        Ok(())
    }
}

/// A new query id: `q-` and 16 random hex characters.
pub fn new_query_id() -> String {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).expect("the OS random number generator failed");
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("q-{hex}")
}

/// `blake3:` and the hex of BLAKE3 over `query_jcs`, the JCS of a query:
/// what binds an offer to its query.
pub fn query_hash(query_jcs: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(query_jcs).to_hex())
}

/// A candidate's answer to a query (design §7.2), signed by it for its
/// requester.
///
/// Timestamps are in the form [`format_timestamp`] writes: `expires_at` is
/// `issued_at` and `valid_for_ms` rounded up to a whole second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceOffer {
    /// [`OFFER_SCHEMA`].
    pub schema: String,
    /// The query's id.
    pub query_id: String,
    /// The provider's id for the offer: unique among its own, not among
    /// everyone's.
    pub offer_id: String,
    /// The provider, and the offer's signer.
    pub agent_id: AgentId,
    /// The one agent the offer is for.
    pub requester: AgentId,
    /// [`query_hash`] of the query answered.
    pub query_hash: String,
    /// The service, the card skill, the offer is about; none when the
    /// requester's scope shows no service of the capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    /// The card of the requester's scope.
    pub agent_card_hash: CardHash,
    /// The requester's scope ([`Scope::key`]).
    pub scope: String,
    /// The revision of the scope's view when the offer was made: it moves
    /// whenever what the scope shows changes, an observation's times
    /// included.
    pub inventory_revision: u64,
    pub status: OfferStatus,
    /// Every mandatory requirement met.
    pub compatible: bool,
    pub conditions: Conditions,
    pub requirement_results: Vec<RequirementResult>,
    /// The mandatory requirements this agent does not know.
    pub unknown_requirements: Vec<String>,
    /// As the service declares it; absent when it does not, never zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_start_delay_ms: Option<u64>,
    /// As the service declares it; absent when it does not, never zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_duration_ms: Option<u64>,
    /// From 1 to [`MAX_OFFER_VALID_FOR_MS`].
    pub valid_for_ms: u32,
    pub issued_at: String,
    pub expires_at: String,
    pub limitations: Vec<String>,
    /// The benchmark reports of the service the requester's scope sees, at
    /// most [`MAX_OFFER_BENCHMARK_REFS`], the most recently measured first;
    /// a requester asks for each with `GetBenchmarkReport`.
    pub benchmark_refs: Vec<BenchmarkRef>,
}

/// A benchmark report an offer refers to
/// ([`crate::benchmark::BenchmarkReport::reference`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRef {
    /// 1 to 128 characters `[A-Za-z0-9._-]`.
    pub benchmark_id: String,
    /// One of the profiles of design §9.7, or a namespaced one.
    pub profile: String,
    /// `declared_spec`, `own_measurement` or `peer_observation`, as the
    /// report was signed: a declared specification is never a measurement.
    pub measurement_kind: String,
    /// As a grant timestamp.
    pub measured_at: String,
    /// As a grant timestamp.
    pub expires_at: String,
}

/// The message of the `InvalidParams` error a task gets when the offer it
/// names is not one the provider issued to its requester.
pub const OFFER_UNKNOWN: &str = "offer_unknown";
/// ... when the offer it names expired, by the provider's own clock.
pub const OFFER_EXPIRED: &str = "offer_expired";
/// ... when what the offer's scope shows changed since it was made: its
/// card, or the revision of its view.
pub const OFFER_STALE: &str = "offer_stale";

/// The offer a task was chosen by, as the task's message names it: under
/// the inventory extension's URI ([`INVENTORY_EXTENSION`]) in
/// `message.metadata`, `{"offerId", "agentCardHash", "queryId"}`, with
/// `A2A-Extensions` naming the extension. It reserves nothing and
/// authorizes nothing: the provider checks the offer again, after the
/// grant, before the task runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfferRef {
    pub offer_id: String,
    pub agent_card_hash: CardHash,
    pub query_id: String,
}

impl OfferRef {
    /// The reference to `offer`.
    pub fn of(offer: &ServiceOffer) -> Self {
        Self {
            offer_id: offer.offer_id.clone(),
            agent_card_hash: offer.agent_card_hash.clone(),
            query_id: offer.query_id.clone(),
        }
    }

    /// Its JSON, the value of the metadata entry.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("an offer reference always serializes")
    }

    /// The reference `metadata`, a message's, carries: `None` when it names
    /// no offer, `Some(None)` when its entry is not a reference (unknown
    /// fields, ids that are no offer or query ids, a hash that is none).
    pub fn in_metadata(metadata: Option<&HashMap<String, Value>>) -> Option<Option<Self>> {
        let entry = metadata?.get(INVENTORY_EXTENSION)?;
        let reference = Self::deserialize(entry)
            .ok()
            .filter(|r| is_offer_id(&r.offer_id) && is_offer_id(&r.query_id));
        Some(reference)
    }
}

/// Whether the service can be had now, and on what terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    /// Compatible, with a place to run now.
    Available,
    /// Compatible, with every place taken now.
    Busy,
    /// Not compatible only because something mandatory is unknown.
    NeedsInformation,
    /// No service of the capability in the scope, or one that fails
    /// something mandatory.
    NotOffered,
}

/// What running the service would be like, as the service declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_cloud_used: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_region: Option<String>,
    /// The grant presented does not let the requester send tasks. A task is
    /// checked against the grant it comes with whatever this says.
    pub authorization_required: bool,
}

/// How one requirement fares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementResult {
    /// `language`, `input_media_type:<type>`, `mcp_integration:<id>`…
    pub requirement: String,
    /// Required, not only preferred.
    pub mandatory: bool,
    pub result: Met,
    /// What it rests on; absent when it is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<Basis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Met {
    Met,
    NotMet,
    /// Unknown, or not shown in the requester's scope: never met.
    Unknown,
}

/// What a result rests on: the operator's declaration, or an adapter's
/// observation in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Declared,
    Observed,
}

/// What binds an offer, besides the matching's [`Evaluation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferParts {
    pub offer_id: String,
    /// The provider.
    pub agent_id: AgentId,
    pub requester: AgentId,
    pub query_id: String,
    /// [`query_hash`] of the query answered.
    pub query_hash: String,
    /// The card of the requester's scope.
    pub agent_card_hash: CardHash,
    /// The requester's scope's key.
    pub scope: String,
    pub inventory_revision: u64,
    pub valid_for_ms: u32,
    /// When the offer is issued, by the provider's clock.
    pub now_unix: i64,
}

impl ServiceOffer {
    /// The offer of `evaluation`, bound by `parts`, issued at
    /// `parts.now_unix` and expiring `valid_for_ms` rounded up to a whole
    /// second later; `None` when either time is outside the years 0000 to
    /// 9999.
    pub fn new(parts: OfferParts, evaluation: Evaluation) -> Option<Self> {
        let lifetime = i64::from(parts.valid_for_ms.div_ceil(1000));
        let issued_at = format_timestamp(parts.now_unix)?;
        let expires_at = format_timestamp(parts.now_unix.checked_add(lifetime)?)?;
        Some(Self {
            schema: OFFER_SCHEMA.to_owned(),
            query_id: parts.query_id,
            offer_id: parts.offer_id,
            agent_id: parts.agent_id,
            requester: parts.requester,
            query_hash: parts.query_hash,
            skill_id: evaluation.skill_id,
            agent_card_hash: parts.agent_card_hash,
            scope: parts.scope,
            inventory_revision: parts.inventory_revision,
            status: evaluation.status,
            compatible: evaluation.compatible,
            conditions: evaluation.conditions,
            requirement_results: evaluation.requirement_results,
            unknown_requirements: evaluation.unknown_requirements,
            estimated_start_delay_ms: evaluation.estimated_start_delay_ms,
            estimated_duration_ms: evaluation.estimated_duration_ms,
            valid_for_ms: parts.valid_for_ms,
            issued_at,
            expires_at,
            limitations: evaluation.limitations,
            benchmark_refs: Vec::new(),
        })
    }
}

impl ServiceOffer {
    /// Whether the offer is expired at `now_unix`, by the clock of whoever
    /// judges it and without skew: whether it still serves to ask with,
    /// for its requester, or to run a task with, for its provider. An
    /// `expires_at` that does not read is expired.
    pub fn expired(&self, now_unix: i64) -> bool {
        parse_timestamp(&self.expires_at).is_none_or(|at| now_unix >= at)
    }
}

impl SignedDoc for ServiceOffer {
    const SCHEMA: &'static str = OFFER_SCHEMA;
    const DOMAIN: &'static [u8] = OFFER_SIG_DOMAIN;
    const WRAPPER: &'static str = "offer";
    const MAX_BYTES: usize = MAX_OFFER_BYTES;
    const SIGNER_POINTER: &'static str = "/agent_id";

    fn signer(&self) -> &AgentId {
        &self.agent_id
    }

    /// Ids of 1 to [`MAX_QUERY_ID_CHARS`] characters `[A-Za-z0-9_-]`; a
    /// query hash; a scope key; a revision from 1 to 2^53 - 1; a
    /// `valid_for_ms` from 1 to [`MAX_OFFER_VALID_FOR_MS`], which
    /// `expires_at` follows `issued_at` by, rounded up to a second;
    /// `compatible` exactly when it is `available` or `busy`; a safe skill
    /// id; timestamps in the grant form; at most
    /// [`MAX_OFFER_BENCHMARK_REFS`] references to reports, each with a
    /// benchmark id, a known or namespaced profile and a kind of
    /// measurement.
    fn validate(&self) -> Result<(), SignedError> {
        let (Some(issued_at), Some(expires_at)) = (
            parse_timestamp(&self.issued_at),
            parse_timestamp(&self.expires_at),
        ) else {
            return Err(SignedError::Malformed);
        };
        let compatible = matches!(self.status, OfferStatus::Available | OfferStatus::Busy);
        let valid = is_offer_id(&self.offer_id)
            && is_offer_id(&self.query_id)
            && is_query_hash(&self.query_hash)
            && Scope::from_key(&self.scope).is_some()
            && (1..=MAX_REVISION).contains(&self.inventory_revision)
            && (1..=MAX_OFFER_VALID_FOR_MS).contains(&self.valid_for_ms)
            && expires_at - issued_at == i64::from(self.valid_for_ms.div_ceil(1000))
            && self.compatible == compatible
            && self.skill_id.as_deref().is_none_or(is_service_id)
            && self.benchmark_refs.len() <= MAX_OFFER_BENCHMARK_REFS
            && self.benchmark_refs.iter().all(|reference| {
                is_benchmark_id(&reference.benchmark_id)
                    && is_profile(&reference.profile)
                    && is_measurement_kind(&reference.measurement_kind)
                    && parse_timestamp(&reference.measured_at).is_some()
                    && parse_timestamp(&reference.expires_at).is_some()
            });
        if valid {
            Ok(())
        } else {
            Err(SignedError::Malformed)
        }
    }
}

/// Signs `offer` with `key`, cut to fit [`MAX_OFFER_BYTES`] when it does
/// not: as few entries as it takes go from the end of `limitations` (the
/// service's own, which its view also shows), then of
/// `requirement_results` (the preferred ones are last), then of
/// `unknown_requirements`, and [`TRUNCATED`] ends the limitations.
///
/// [`SignedError::TooLarge`] when even that does not fit; whatever else
/// [`Signed::issue`] refuses.
pub fn sign_offer(
    key: &AgentKey,
    offer: ServiceOffer,
) -> Result<Signed<ServiceOffer>, SignedError> {
    if signed_len(&offer) <= MAX_OFFER_BYTES {
        return Signed::issue(key, offer);
    }
    let removable = offer.limitations.len()
        + offer.requirement_results.len()
        + offer.unknown_requirements.len();
    if signed_len(&trimmed(&offer, removable)) > MAX_OFFER_BYTES {
        return Err(SignedError::TooLarge);
    }
    // The size only shrinks as more go: the fewest that fit, by halves.
    // `too_few` never fits (none removed does not), `enough` always does.
    let (mut too_few, mut enough) = (0, removable);
    while enough - too_few > 1 {
        let mid = too_few + (enough - too_few) / 2;
        if signed_len(&trimmed(&offer, mid)) <= MAX_OFFER_BYTES {
            enough = mid;
        } else {
            too_few = mid;
        }
    }
    Signed::issue(key, trimmed(&offer, enough))
}

/// `offer` without its last `count` entries, taken from `limitations`,
/// then `requirement_results`, then `unknown_requirements`, and with
/// [`TRUNCATED`] as its last limitation.
fn trimmed(offer: &ServiceOffer, count: usize) -> ServiceOffer {
    let mut offer = offer.clone();
    let mut left = count;
    // What a list of `len` keeps once the cut takes what it can from it.
    let mut keep = |len: usize| {
        let cut = left.min(len);
        left -= cut;
        len - cut
    };
    let limitations = keep(offer.limitations.len());
    let results = keep(offer.requirement_results.len());
    let unknown = keep(offer.unknown_requirements.len());
    offer.limitations.truncate(limitations);
    offer.requirement_results.truncate(results);
    offer.unknown_requirements.truncate(unknown);
    offer.limitations.push(TRUNCATED.to_owned());
    offer
}

/// The bytes of `offer` signed: every signature has the same length.
fn signed_len(offer: &ServiceOffer) -> usize {
    Signed {
        doc: offer.clone(),
        signature: "A".repeat(SIGNATURE_CHARS),
    }
    .to_json()
    .len()
}

/// Why a received offer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OfferError {
    /// Larger than [`MAX_OFFER_BYTES`], refused unread.
    #[error("offer larger than {MAX_OFFER_BYTES} bytes")]
    TooLarge,
    /// Not a valid signed offer, or not yet valid.
    #[error("invalid offer: {0}")]
    Signed(SignedError),
    /// Signed by, and naming, another agent than the one asked.
    #[error("offer from another agent than the one asked")]
    WrongProvider,
    #[error("offer for another requester")]
    WrongRequester,
    /// For another query, or another query id.
    #[error("offer for another query")]
    QueryMismatch,
    /// Expired beyond [`crate::grant::CLOCK_SKEW_S`].
    #[error("offer expired")]
    Expired,
}

/// The offer `bytes`, received from `provider` for `requester`'s `query`
/// at `now_unix`, once it passes, in this order, the first failure winning:
///
/// 1. at most [`MAX_OFFER_BYTES`], before it is read
///    ([`OfferError::TooLarge`]);
/// 2. a valid signed offer ([`Signed::from_json`]: its own signature over
///    the offer as received, under [`OFFER_SIG_DOMAIN`], one spelling,
///    [`SignedDoc::validate`]), [`OfferError::Signed`];
/// 3. signed by `provider` ([`OfferError::WrongProvider`]);
/// 4. for `requester` ([`OfferError::WrongRequester`]);
/// 5. for `query`: its `query_id` and [`query_hash`]
///    ([`OfferError::QueryMismatch`]);
/// 6. in force at `now_unix`, with [`crate::grant::CLOCK_SKEW_S`] of
///    tolerance at both ends, only to refuse ([`OfferError::Expired`],
///    or [`SignedError::NotYetValid`]).
///
/// Whether an offer kept still serves to ask with is another question,
/// which the requester answers by its own clock, without skew.
pub fn accept_offer(
    bytes: &[u8],
    provider: &AgentId,
    requester: &AgentId,
    query: &ServiceQuery,
    now_unix: i64,
) -> Result<Signed<ServiceOffer>, OfferError> {
    if bytes.len() > MAX_OFFER_BYTES {
        return Err(OfferError::TooLarge);
    }
    let offer = Signed::<ServiceOffer>::from_json(bytes).map_err(|err| match err {
        SignedError::TooLarge => OfferError::TooLarge,
        other => OfferError::Signed(other),
    })?;
    let doc = &offer.doc;
    if doc.agent_id != *provider {
        return Err(OfferError::WrongProvider);
    }
    if doc.requester != *requester {
        return Err(OfferError::WrongRequester);
    }
    if doc.query_id != query.query_id || doc.query_hash != query.hash() {
        return Err(OfferError::QueryMismatch);
    }
    check_validity(&doc.issued_at, &doc.expires_at, now_unix).map_err(|err| match err {
        SignedError::Expired => OfferError::Expired,
        other => OfferError::Signed(other),
    })?;
    Ok(offer)
}

/// A query or offer id: 1 to [`MAX_QUERY_ID_CHARS`] characters
/// `[A-Za-z0-9_-]`.
fn is_offer_id(id: &str) -> bool {
    (1..=MAX_QUERY_ID_CHARS).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `blake3:` and 64 lowercase hex characters.
fn is_query_hash(hash: &str) -> bool {
    hash.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// `type/subtype`, with parameters if any, in printable ASCII, at most
/// [`MAX_TOKEN_BYTES`].
fn is_media_type(media_type: &str) -> bool {
    let essence = media_type.split(';').next().unwrap_or_default().trim();
    is_token(media_type)
        && essence
            .split_once('/')
            .is_some_and(|(kind, subtype)| !kind.is_empty() && !subtype.is_empty())
}

/// 1 to [`MAX_TOKEN_BYTES`] bytes of printable ASCII, spaces included.
fn is_token(text: &str) -> bool {
    (1..=MAX_TOKEN_BYTES).contains(&text.len()) && text.bytes().all(|b| (b' '..=b'~').contains(&b))
}
