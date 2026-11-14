//! Benchmark reports (`agentnet.benchmark-report/1`): what was measured,
//! how, when and by whom (design §9.7).
//!
//! A report is a [`SignedDoc`] under [`BENCHMARK_SIG_DOMAIN`], signed by
//! whoever measured: the agent named at `/provenance/measured_by`, whose key
//! [`Signed::from_json`] verifies it with, so a report signed by anybody
//! else is refused. It is at most [`MAX_REPORT_BYTES`] and
//! [`MAX_RESULTS`] results; bulky raw results travel only as `evidence`, a
//! hash and a reference to a blob, which nothing fetches when a peer asks.
//! A signature says who issued a report and that it has not changed, never
//! that the hardware exists or that the run happened as it says.
//!
//! **What it measures.** Its [`Subject`]: a service of the agent (its id),
//! a model or a hardware profile (its inventory entry id), or a pair of
//! nodes. **How.** A profile: one of the six that design §9.7 proposes
//! ([`PROFILES`]), by name, or one of its author's namespace
//! (`example:bench`); its version, runner and configuration; the
//! environment it ran in. **Kind** ([`MeasurementKind`]): a declared
//! specification, the measurer's own measurement or a peer's observation,
//! kept as signed wherever the report goes: a declared specification is
//! never a measurement. A measurement has at least one sample.
//!
//! **Who sees it.** Its visibility is not in the report: whoever imports it
//! cannot change what another signed. The store keeps it beside the report
//! ([`crate::store::Store::benchmark_put`]), private unless the operator
//! says otherwise.
//!
//! **Comparability** ([`comparable`]): a report is usable for what a
//! requester wants only when it is in force, of the profile and version
//! asked, of the same configuration (compared as JCS) and of the route and
//! pair of nodes asked, if any. Nothing measures: a search only reads
//! reports that exist (§9.7), and a subject without any shows as
//! `no_measurement`, never as zero ([`crate::inventory::view::render_benchmarks`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::discovery::query::BenchmarkRef;
use crate::grant::parse_timestamp;
use crate::identity::AgentId;
use crate::inventory::{MAX_STRING_BYTES, is_namespaced_id, is_service_id};
use crate::jcs::canonical;
use crate::signed::{Signed, SignedDoc, SignedError};

/// The value of [`BenchmarkReport::schema`].
pub const BENCHMARK_SCHEMA: &str = "agentnet.benchmark-report/1";
/// The prefix of the bytes a report signature covers.
pub const BENCHMARK_SIG_DOMAIN: &[u8] = b"agentnet.benchmark/1\n";
/// The largest signed report, in bytes.
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
/// The most results in a report.
pub const MAX_RESULTS: usize = 64;
/// The longest benchmark id, in characters `[A-Za-z0-9._-]`.
pub const MAX_BENCHMARK_ID_CHARS: usize = 128;
/// The most references to reports a service offer carries.
pub const MAX_OFFER_BENCHMARK_REFS: usize = 16;
/// The profiles design §9.7 proposes, accepted by name.
pub const PROFILES: [&str; 6] = [
    "compute.inference",
    "compute.llm",
    "storage.io",
    "network.rtt",
    "network.transfer",
    "service.task",
];

/// The deepest a report's configuration nests, in objects and arrays.
const MAX_CONFIG_DEPTH: usize = 16;
/// The longest profile version, metric, unit or statistic, in bytes.
const MAX_NAME_BYTES: usize = 64;
/// The longest hash or blob reference, in bytes.
const MAX_HASH_BYTES: usize = 256;

/// What a report says of its results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    /// A specification declared, not measured.
    DeclaredSpec,
    /// Measured by the measurer, of itself.
    OwnMeasurement,
    /// Measured by a peer, of another.
    PeerObservation,
}

impl MeasurementKind {
    /// Every kind.
    pub const ALL: [MeasurementKind; 3] = [
        MeasurementKind::DeclaredSpec,
        MeasurementKind::OwnMeasurement,
        MeasurementKind::PeerObservation,
    ];

    /// Its name: `declared_spec`, `own_measurement` or `peer_observation`.
    pub fn as_str(self) -> &'static str {
        match self {
            MeasurementKind::DeclaredSpec => "declared_spec",
            MeasurementKind::OwnMeasurement => "own_measurement",
            MeasurementKind::PeerObservation => "peer_observation",
        }
    }
}

/// Whether `name` is the name of a [`MeasurementKind`].
pub(crate) fn is_measurement_kind(name: &str) -> bool {
    MeasurementKind::ALL
        .iter()
        .any(|kind| kind.as_str() == name)
}

/// What a report measures.
///
/// In JSON, one key: `{"service": "ocr-metal-v1"}`, `{"model":
/// "example:ocr-model"}`, `{"hardware": "example:host"}` or `{"node_pair":
/// {"a": "<agent id>", "b": "<agent id>"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Subject {
    /// A service, by its id (`[a-z0-9][a-z0-9-]{0,63}`).
    Service(String),
    /// A model, by its inventory entry id (`<namespace>:<name>`).
    Model(String),
    /// A hardware profile, by its inventory entry id.
    Hardware(String),
    /// Two distinct nodes, as `a` and `b`.
    NodePair { a: AgentId, b: AgentId },
}

impl Subject {
    /// Its name: `service:<id>`, `model:<id>`, `hardware:<id>` or
    /// `node_pair:<a>:<b>`; never a benchmark id, which has no `:`.
    pub fn key(&self) -> String {
        match self {
            Subject::Service(id) => format!("service:{id}"),
            Subject::Model(id) => format!("model:{id}"),
            Subject::Hardware(id) => format!("hardware:{id}"),
            Subject::NodePair { a, b } => format!("node_pair:{a}:{b}"),
        }
    }

    /// Whether a view that shows `subjects` may name this one: it is among
    /// them, or a pair of nodes, which no inventory hides.
    pub fn shown_among(&self, subjects: &[Subject]) -> bool {
        matches!(self, Subject::NodePair { .. }) || subjects.contains(self)
    }

    /// A service id, an entry id, or two distinct nodes.
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Subject::Service(id) => is_service_id(id),
            Subject::Model(id) | Subject::Hardware(id) => is_namespaced_id(id),
            Subject::NodePair { a, b } => a != b,
        }
    }
}

/// The way a transfer or a round trip went between the two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Direct,
    Relay,
}

/// A benchmark report (design §9.7).
///
/// Timestamps are in the form [`crate::grant::format_timestamp`] writes;
/// `expires_at` comes after `measured_at`. An absent field is left out,
/// never `null`: a report has one spelling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkReport {
    /// [`BENCHMARK_SCHEMA`].
    pub schema: String,
    /// 1 to [`MAX_BENCHMARK_ID_CHARS`] characters `[A-Za-z0-9._-]`, unique
    /// among the reports an agent keeps.
    pub benchmark_id: String,
    pub subject: Subject,
    pub measurement_kind: MeasurementKind,
    pub procedure: Procedure,
    pub environment: Environment,
    /// When it was measured, or declared.
    pub measured_at: String,
    /// How long the measurement took.
    pub duration_ms: u64,
    /// At least 1 but in a declared specification.
    pub samples: u32,
    /// When it stops counting as current.
    pub expires_at: String,
    /// 1 to [`MAX_RESULTS`].
    pub results: Vec<MetricResult>,
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

/// How it was measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Procedure {
    /// One of [`PROFILES`], or `<namespace>:<name>` of its author.
    pub profile: String,
    /// 1 to 64 characters `[A-Za-z0-9._-]`.
    pub profile_version: String,
    /// The tool that ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// The profile's parameters, a JSON object: two reports compare only
    /// with the same configuration, as JCS.
    pub config: Value,
    /// A hash of the inputs, or a reference to them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_hash: Option<String>,
}

/// What it ran on, as far as the measurer knows; what is unknown is left
/// out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The hardware profile, as a reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_ref: Option<String>,
    /// The load it ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_state: Option<String>,
    /// Between nodes: direct or through a relay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,
    /// The other node, when it ran against one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<AgentId>,
}

/// One result: a metric, its unit, the statistic and its value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricResult {
    /// `latency`, `tokens_per_s`…
    pub metric: String,
    /// `ms`, `tokens/s`, `MiB/s`…
    pub unit: String,
    /// `p50`, `p95`, `mean`…
    pub statistic: String,
    /// A finite number.
    pub value: f64,
    /// The runs that failed, when counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<u64>,
    /// When it can be estimated: finite, and not negative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<f64>,
}

/// Who measured, and how they observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The measurer, and the report's signer.
    pub measured_by: AgentId,
    pub method: String,
}

/// Where the raw results are: their hash, and a reference to the blob
/// that holds them. Nothing fetches it when a peer asks for the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

impl BenchmarkReport {
    /// What a service offer says of it.
    pub fn reference(&self) -> BenchmarkRef {
        BenchmarkRef {
            benchmark_id: self.benchmark_id.clone(),
            profile: self.procedure.profile.clone(),
            measurement_kind: self.measurement_kind.as_str().to_owned(),
            measured_at: self.measured_at.clone(),
            expires_at: self.expires_at.clone(),
        }
    }

    /// The two nodes it was measured between: its subject's, when a pair of
    /// nodes, else its measurer and the peer of its environment, if any.
    pub fn node_pair(&self) -> Option<(AgentId, AgentId)> {
        match &self.subject {
            Subject::NodePair { a, b } => Some((*a, *b)),
            _ => self
                .environment
                .peer
                .map(|peer| (self.provenance.measured_by, peer)),
        }
    }
}

impl SignedDoc for BenchmarkReport {
    const SCHEMA: &'static str = BENCHMARK_SCHEMA;
    const DOMAIN: &'static [u8] = BENCHMARK_SIG_DOMAIN;
    const WRAPPER: &'static str = "report";
    const MAX_BYTES: usize = MAX_REPORT_BYTES;
    const SIGNER_POINTER: &'static str = "/provenance/measured_by";

    fn signer(&self) -> &AgentId {
        &self.provenance.measured_by
    }

    /// A benchmark id; a valid subject ([`Subject`]); a known or namespaced
    /// profile, a version of 1 to 64 `[A-Za-z0-9._-]`, an object of
    /// configuration nested at most 16 deep; texts of 1 to
    /// [`MAX_STRING_BYTES`] bytes, hashes and blob references of 1 to 256
    /// printable ASCII characters without spaces; timestamps with
    /// `measured_at` before `expires_at`; at least one sample in a
    /// measurement; 1 to [`MAX_RESULTS`] results, each with a metric, a
    /// unit and a statistic of 1 to 64 printable ASCII characters and a
    /// finite value, and a finite uncertainty that is not negative.
    fn validate(&self) -> Result<(), SignedError> {
        let (Some(measured_at), Some(expires_at)) = (
            parse_timestamp(&self.measured_at),
            parse_timestamp(&self.expires_at),
        ) else {
            return Err(SignedError::Malformed);
        };
        let procedure = &self.procedure;
        let environment = &self.environment;
        let texts = [
            &procedure.runner,
            &environment.model_revision,
            &environment.runtime,
            &environment.hardware_ref,
            &environment.load,
            &environment.warmup,
            &environment.cache_state,
        ];
        let valid = is_benchmark_id(&self.benchmark_id)
            && self.subject.is_valid()
            && is_profile(&procedure.profile)
            && is_profile_version(&procedure.profile_version)
            && procedure.config.is_object()
            && depth(&procedure.config) <= MAX_CONFIG_DEPTH
            && procedure.inputs_hash.as_deref().is_none_or(is_hash)
            && texts.iter().all(|text| text.as_deref().is_none_or(is_text))
            && measured_at < expires_at
            && (self.samples > 0 || self.measurement_kind == MeasurementKind::DeclaredSpec)
            && (1..=MAX_RESULTS).contains(&self.results.len())
            && self.results.iter().all(MetricResult::is_valid)
            && is_text(&self.provenance.method)
            && self.evidence.as_ref().is_none_or(|evidence| {
                is_hash(&evidence.hash) && evidence.blob.as_deref().is_none_or(is_hash)
            });
        if valid {
            Ok(())
        } else {
            Err(SignedError::Malformed)
        }
    }
}

impl MetricResult {
    fn is_valid(&self) -> bool {
        is_name(&self.metric)
            && is_name(&self.unit)
            && is_name(&self.statistic)
            && self.value.is_finite()
            && self
                .uncertainty
                .is_none_or(|uncertainty| uncertainty.is_finite() && uncertainty >= 0.0)
    }
}

/// What a requester wants a report for: a profile and version, a
/// configuration, and a route or a pair of nodes when it matters.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub profile: String,
    pub profile_version: String,
    /// Compared with the report's as JCS.
    pub config: Value,
    pub route: Option<Route>,
    /// In either order.
    pub peer_pair: Option<(AgentId, AgentId)>,
}

/// Whether a report can stand for what is wanted, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparability {
    Usable,
    /// Past its `expires_at`.
    Expired,
    /// Of another profile, or another version of it.
    OtherProfile,
    /// Of another configuration.
    OtherConfig,
    /// Of another route, or between other nodes, or of a route or nodes it
    /// does not say.
    OtherRoute,
}

/// Whether `report` can stand for what `wanted` describes at `now_unix`, by
/// the requester's own clock and without skew: the first reason it cannot,
/// in the order of [`Comparability`], or [`Comparability::Usable`].
///
/// A report is never compared across profiles, versions or configurations
/// (design §9.7): tokens per second of one model say nothing of another,
/// and an isolated inference is not a whole task.
pub fn comparable(report: &BenchmarkReport, wanted: &Wanted, now_unix: i64) -> Comparability {
    let expired = parse_timestamp(&report.expires_at).is_none_or(|at| now_unix >= at);
    let procedure = &report.procedure;
    if expired {
        Comparability::Expired
    } else if procedure.profile != wanted.profile
        || procedure.profile_version != wanted.profile_version
    {
        Comparability::OtherProfile
    } else if canonical(&procedure.config) != canonical(&wanted.config) {
        Comparability::OtherConfig
    } else if wanted
        .route
        .is_some_and(|route| report.environment.route != Some(route))
        || wanted.peer_pair.is_some_and(|wanted| {
            !report
                .node_pair()
                .is_some_and(|pair| same_pair(pair, wanted))
        })
    {
        Comparability::OtherRoute
    } else {
        Comparability::Usable
    }
}

/// The references a service offer carries for the service `service_id`:
/// those of `reports` about it, the most recently measured first, at most
/// [`MAX_OFFER_BENCHMARK_REFS`].
pub fn references_for(reports: &[Signed<BenchmarkReport>], service_id: &str) -> Vec<BenchmarkRef> {
    let mut about: Vec<&BenchmarkReport> = reports
        .iter()
        .map(|report| &report.doc)
        .filter(|report| matches!(&report.subject, Subject::Service(id) if id == service_id))
        .collect();
    about.sort_by(|x, y| {
        let at = |report: &BenchmarkReport| parse_timestamp(&report.measured_at);
        at(y)
            .cmp(&at(x))
            .then_with(|| x.benchmark_id.cmp(&y.benchmark_id))
    });
    about
        .into_iter()
        .take(MAX_OFFER_BENCHMARK_REFS)
        .map(BenchmarkReport::reference)
        .collect()
}

/// 1 to [`MAX_BENCHMARK_ID_CHARS`] characters `[A-Za-z0-9._-]`.
pub fn is_benchmark_id(id: &str) -> bool {
    (1..=MAX_BENCHMARK_ID_CHARS).contains(&id.len()) && id.bytes().all(is_id_byte)
}

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

/// One of [`PROFILES`], or a namespaced id.
pub(crate) fn is_profile(profile: &str) -> bool {
    PROFILES.contains(&profile) || is_namespaced_id(profile)
}

/// 1 to [`MAX_NAME_BYTES`] characters `[A-Za-z0-9._-]`.
pub(crate) fn is_profile_version(version: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&version.len()) && version.bytes().all(is_id_byte)
}

/// 1 to [`MAX_NAME_BYTES`] bytes of printable ASCII, spaces included.
fn is_name(name: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&name.len()) && name.bytes().all(|b| (b' '..=b'~').contains(&b))
}

/// 1 to [`MAX_HASH_BYTES`] bytes of printable ASCII without spaces.
fn is_hash(hash: &str) -> bool {
    (1..=MAX_HASH_BYTES).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_graphic())
}

/// 1 to [`MAX_STRING_BYTES`] bytes.
fn is_text(text: &str) -> bool {
    (1..=MAX_STRING_BYTES).contains(&text.len())
}

/// How deep `value` nests: 0 for a scalar, 1 for an object or array of
/// scalars.
fn depth(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// The same two nodes, in either order.
fn same_pair(x: (AgentId, AgentId), y: (AgentId, AgentId)) -> bool {
    x == y || x == (y.1, y.0)
}
