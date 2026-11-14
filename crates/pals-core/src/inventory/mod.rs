//! The inventory (`agentnet.inventory/1`): what an agent offers and with
//! what.
//!
//! The operator writes it in `inventory.toml`, read once when the daemon
//! starts. Its `services` become the skills of the agent's Agent Card; the
//! other blocks describe what the agent works with: installed skill packages,
//! plugins, MCP connections, its runtime, models, information sources,
//! hardware and network. Every block is optional, and so is every field of
//! an entry but `id`, `description` and `provenance`: a field left out is
//! unknown and is never written, so it never reads as zero, `false` or an
//! empty value. An empty string is not a value either: what is unknown is
//! left out.
//!
//! Each entry tracks the five dimensions of the design (§9.3) apart:
//! *installed* is being in the inventory; *operational* is an
//! [`Observation`] an adapter makes, never a declaration, so the file cannot
//! say it ([`InventoryError::DeclaredOperational`]); *offered* is its
//! [`Visibility`] in a [`Scope`]; *authorized* is the requester's grant,
//! checked again when a task runs; *available now* is a service offer's,
//! never the inventory's. A peer never reads the inventory itself, only the
//! signed view of its scope ([`view`]).
//!
//! Limits: the file at most [`MAX_INVENTORY_BYTES`], strings and keys at
//! most [`MAX_STRING_BYTES`], at most [`MAX_BLOCK_ENTRIES`] entries per
//! block. Keys this schema does not know are refused, except under an
//! entry's `extensions`, whose keys need a namespace.
//!
//! Identifiers:
//! - an entry id is `<namespace>:<name>`, 1 to 64 characters `[a-z0-9._-]`
//!   in the namespace, never `agentnet`, and 1 to 128 `[A-Za-z0-9._/-]` in
//!   the name; one id names one entry across every block;
//! - a service id is a safe skill id, `[a-z0-9][a-z0-9-]{0,63}`, because
//!   it becomes the id of a card skill, which milestone 1's gateway checks
//!   as it checks a task id; never `ask`, the card's own skill;
//! - a capability without `:` belongs to the common vocabulary and travels
//!   as the skill tag [`CAPABILITY_PREFIX`]`<capability>`; one with `:`
//!   belongs to its provider's namespace and travels as it is
//!   ([`capability_tag`]);
//! - a service's own tags travel next to its capability tags, so none may
//!   start with `agentnet:`, in any case: it would pass for a capability.
//!
//! A service needs a name, a description and at least one capability or
//! tag, and none of what becomes card fields may be empty: an empty field
//! would not be a fixed point of the card's canonical forms
//! ([`crate::card_jws::normalize`]). Whether all the services fit in a card
//! ([`crate::card_jws::validate_card`]) is checked when the cards are built.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::grant::parse_timestamp;

pub mod scope;
pub mod view;

pub use scope::Scope;

/// The profile the inventory extension declares in its `params`.
pub const INVENTORY_PROFILE: &str = "agentnet.inventory/1";
/// The URI of the A2A extension that carries the inventory.
pub const INVENTORY_EXTENSION: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/extensions/inventory/v1";
/// The prefix that turns a common-vocabulary capability into a skill tag.
pub const CAPABILITY_PREFIX: &str = "agentnet:capability:v1:";
/// The largest inventory file, in bytes.
pub const MAX_INVENTORY_BYTES: usize = 1024 * 1024;
/// The longest string or key in an inventory, in bytes.
pub const MAX_STRING_BYTES: usize = 4 * 1024;
/// The most entries in one block.
pub const MAX_BLOCK_ENTRIES: usize = 1000;
/// The longest description of an observed MCP tool, in bytes.
pub const MAX_OBSERVED_DESCRIPTION_BYTES: usize = 1024;

/// [`Observation::detail`]: the adapter's time ran out.
pub const DETAIL_TIMEOUT: &str = "timeout";
/// [`Observation::detail`]: the connection could not be made.
pub const DETAIL_UNREACHABLE: &str = "unreachable";
/// [`Observation::detail`]: the server answered something invalid.
pub const DETAIL_PROTOCOL_ERROR: &str = "protocol error";
/// [`Observation::detail`]: the server lists more tools than an adapter
/// reads.
pub const DETAIL_TOO_MANY_TOOLS: &str = "too many tools";
/// [`Observation::detail`]: what precedes the offered tools the server does
/// not list, see [`missing_tools_detail`].
pub const DETAIL_MISSING_TOOLS: &str = "missing tools: ";

/// The skill id the Agent Card gives `ask`, which no service may take.
const CARD_SKILL_ID: &str = "ask";
/// The namespace no inventory id, capability or extension key may use.
const RESERVED_NAMESPACE: &str = "agentnet";

/// A block of the inventory, as a requester names it when it asks for some.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Services,
    Skills,
    Plugins,
    Mcp,
    Runtime,
    Models,
    Information,
    Hardware,
    Network,
    /// Benchmark reports, kept apart from the inventory file.
    Benchmarks,
}

/// An agent's inventory, as `inventory.toml` declares it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// What the agent offers: each one a skill of its Agent Card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<Service>,
    /// Installed skill packages: procedures of the runtime, not A2A skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Entry<SkillPackage>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<Entry<Plugin>>,
    /// MCP connections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<Entry<McpConnection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Entry<Runtime>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Entry<Model>>,
    /// Information sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub information: Vec<Entry<InformationSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<Entry<Hardware>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Entry<Network>>,
}

/// An entry of every block but `services`: what each one has, and `body`,
/// the fields of its block, at the same level.
// With `flatten`, `body` takes only its own fields, and serde refuses any key
// left over: `deny_unknown_fields` still holds (test
// `unknown_plain_field_is_refused_and_namespaced_extension_is_kept`). `body`
// must therefore be a struct without flattened fields of its own.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry<T> {
    /// `<namespace>:<name>`, unique in the inventory.
    pub id: String,
    pub description: String,
    /// Who declares it: `operator` for a declaration in the file.
    pub provenance: String,
    /// Its version or hash, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub visibility: Visibility,
    /// When the declaration stops holding, as a grant timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    #[serde(flatten)]
    pub body: T,
    /// Data outside this schema, under keys with a namespace
    /// (`example:tier`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

/// Where an entry or a service is shown: never anywhere (`Private`, the
/// default), to everyone (`Public`), or to requesters whose grant is for
/// one of the projects: any task operation (`Project`) or `task.send`
/// (`Task`) on `skill:ask`, see [`Scope::for_grant`] and
/// [`Visibility::visible_in`].
///
/// In TOML: `"private"`, `"public"`, `{ project = ["demo"] }` or
/// `{ task = ["demo"] }`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Private,
    Public,
    Project(Vec<String>),
    Task(Vec<String>),
}

/// What backs a declaration: a reference, a hash of what it refers to, or
/// both.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// A service: a result the agent offers, which becomes a skill of its Agent
/// Card when its visibility shows it there.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// The card skill's id: `[a-z0-9][a-z0-9-]{0,63}`.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Common-vocabulary (`vision.ocr`) or namespaced (`example:ocr`)
    /// capabilities; with `tags`, at least one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Media types it accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    /// Media types it produces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_cloud_used: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_start_delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_duration_ms: Option<u64>,
    /// The entries it is done with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    #[serde(default)]
    pub visibility: Visibility,
}

/// A service's link to an entry of the same inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentLink {
    pub id: String,
    pub role: ComponentRole,
    /// Names the alternatives that stand in for each other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentRole {
    Required,
    Optional,
    Alternative,
}

/// An installed skill package.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkillPackage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The kinds of task it is offered for.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offered_for: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plugin {
    /// The runtime it works with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The components it offers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

/// An MCP connection: what the operator declares of it, which tools it
/// offers, and how an adapter reaches it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConnection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    /// Tools as the operator declares them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<McpTool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    /// How an adapter reaches the server: the operator's configuration,
    /// never shown to a peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<McpAdapterCfg>,
    /// The tools offered: the only ones a view may name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offered_tools: Vec<String>,
}

/// A tool of an MCP connection, as declared. Names are 1 to 128 characters
/// `[A-Za-z0-9_.-]`, the set MCP recommends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MCP annotations: declarations, never a security policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

/// How an inventory adapter reaches an MCP server. In TOML, `transport`
/// sits next to its own keys:
///
/// ```toml
/// [mcp.adapter]
/// transport = "stdio"
/// command = "npx"
/// args = ["-y", "@modelcontextprotocol/server-filesystem", "/srv/repo"]
/// ```
// Unknown keys are refused by `McpTransport`, which sees them all: a
// `deny_unknown_fields` here would refuse the transport's own keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAdapterCfg {
    #[serde(flatten)]
    pub transport: McpTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpTransport {
    /// Streamable HTTP at `url`.
    Http { url: String },
    /// A child process that speaks MCP on its stdin and stdout.
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Names of the environment variables the child receives; never
        /// their values.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env_passthrough: Vec<String>,
    },
}

/// The agent's runtime; its version is the entry's.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runtime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The ways of working it supports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restrictions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Model {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// A fixed revision or hash of the weights.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The alias used when there is no fixed revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutable_alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<ModelRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ModelExecution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modalities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// The policy, when the model is chosen per task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_selection: Option<String>,
}

/// The agent's main model, an auxiliary one, or one offered as a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Primary,
    Auxiliary,
    Offered,
}

/// Where a model runs. In TOML, `{ kind = "local" }` or
/// `{ kind = "external", provider = "…" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ModelExecution {
    /// On this machine.
    #[serde(deserialize_with = "no_fields")]
    Local,
    /// At `provider`'s, outside this machine: its hardware is not this
    /// inventory's.
    External { provider: String },
}

/// Reads a variant without fields, refusing any key besides its tag: serde
/// ignores the keys under a unit variant of an internally tagged enum.
fn no_fields<'de, D: serde::Deserializer<'de>>(fields: D) -> Result<(), D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NoFields {}
    NoFields::deserialize(fields).map(|NoFields {}| ())
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InformationSource {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    /// Temporal or thematic coverage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<String>,
    /// When the source last changed, as a grant timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Live data (`true`) or an indexed copy (`false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approximate_volume: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<InformationOperation>,
    /// Limits on rows, fields, export or retention.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<String>,
}

/// What a requester may have done with an information source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationOperation {
    AnswerWithSources,
    Search,
    QueryRecords,
    Aggregate,
    ProcessInPlace,
    DeliverSubset,
}

/// The machine's resource profile: what is installed and what is offered.
/// Its figures are nominal, declared by the operator, never measured
/// (`basis`, always `declared`); measurements are benchmark reports.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hardware {
    pub basis: HardwareBasis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<Cpu>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_mib: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accelerators: Vec<Accelerator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<Storage>,
    /// What a task may use: requirements compare against this, never
    /// against what is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offered: Option<OfferedResources>,
}

/// How the hardware figures are known: declared, the only basis an
/// inventory has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareBasis {
    #[default]
    Declared,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cpu {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cores: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Accelerator {
    /// `gpu`, `npu`…
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mib_each: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity_gib: Option<u64>,
}

/// The resources a task may use.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OfferedResources {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_mib: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_mib_each: Option<u64>,
    /// Tasks at the same time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    /// The accelerators offered, by `kind` or `model`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accelerators: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transports: Vec<String>,
    /// An approximate region the operator allows to show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_connectivity_observed: Option<bool>,
}

/// What an adapter observed of an entry: the *operational* dimension, which
/// no declaration gives.
///
/// `detail` carries only a fixed code: [`DETAIL_TIMEOUT`],
/// [`DETAIL_UNREACHABLE`], [`DETAIL_PROTOCOL_ERROR`],
/// [`DETAIL_TOO_MANY_TOOLS`] or [`missing_tools_detail`]; never the error
/// itself, which may name a URL or a command ([`Observation::check`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub source: ObservationSource,
    pub status: OperationalStatus,
    /// When it was observed, as a grant timestamp.
    pub observed_at: String,
    /// When it stops holding, as a grant timestamp.
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The offered tools the server listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ObservedTool>,
}

/// A tool an adapter listed, with its description cut to
/// [`MAX_OBSERVED_DESCRIPTION_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// As the server declares them: declarations, never a security policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
}

/// Who made an observation: the operator (`{"kind": "declared"}`) or an
/// adapter (`{"kind": "adapter", "method": "…"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationSource {
    Declared,
    Adapter { method: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalStatus {
    Ok,
    Failed,
    Unknown,
}

/// The inventory as the node serves it at one moment, with the observations
/// of its entries by entry id. `generation` grows with every new set of
/// observations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InventorySnapshot {
    pub inventory: Inventory,
    pub observations: BTreeMap<String, Observation>,
    pub generation: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error("inventory larger than {MAX_INVENTORY_BYTES} bytes")]
    TooLarge,
    #[error("cannot read the inventory: {0}")]
    Io(#[from] io::Error),
    #[error("invalid inventory: {0}")]
    Invalid(String),
    /// The file says an entry is operational: only an adapter's check can.
    #[error(
        "invalid inventory: {0}: `operational` cannot be declared; a manual declaration is \
         never a check, and only an inventory adapter observes an entry operational"
    )]
    DeclaredOperational(String),
}

fn invalid(message: impl Into<String>) -> InventoryError {
    InventoryError::Invalid(message.into())
}

impl Inventory {
    /// Reads `inventory.toml` text: at most [`MAX_INVENTORY_BYTES`], every
    /// rule of the module checked ([`Inventory::validate`]).
    pub fn from_toml(text: &str) -> Result<Self, InventoryError> {
        if text.len() > MAX_INVENTORY_BYTES {
            return Err(InventoryError::TooLarge);
        }
        let table: toml::Table = toml::from_str(text).map_err(|err| invalid(err.to_string()))?;
        check_declarations(&table)?;
        // Read again, typed, for errors that point at the line.
        let inventory: Self = toml::from_str(text).map_err(|err| invalid(err.to_string()))?;
        inventory.validate()?;
        Ok(inventory)
    }

    /// Reads the inventory file at `path`, never more than
    /// [`MAX_INVENTORY_BYTES`] and one byte of it.
    /// [`InventoryError::Io`] when it cannot be read, a missing file
    /// included.
    pub fn load(path: &Path) -> Result<Self, InventoryError> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_INVENTORY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_INVENTORY_BYTES {
            return Err(InventoryError::TooLarge);
        }
        let text = String::from_utf8(bytes).map_err(|_| invalid("not UTF-8"))?;
        Self::from_toml(&text)
    }

    /// Checks the rules a typed inventory can break: block sizes, ids,
    /// services, visibility, timestamps, extension keys, tool names,
    /// component links, and the length and emptiness of every string.
    pub fn validate(&self) -> Result<(), InventoryError> {
        for (block, entries) in [
            ("services", self.services.len()),
            ("skills", self.skills.len()),
            ("plugins", self.plugins.len()),
            ("mcp", self.mcp.len()),
            ("models", self.models.len()),
            ("information", self.information.len()),
        ] {
            if entries > MAX_BLOCK_ENTRIES {
                return Err(invalid(format!(
                    "{block}: more than {MAX_BLOCK_ENTRIES} entries"
                )));
            }
        }

        let mut services = BTreeMap::new();
        for (i, service) in self.services.iter().enumerate() {
            let at = format!("services[{i}]");
            check_service(&at, service)?;
            if let Some(first) = services.insert(service.id.as_str(), at.clone()) {
                return Err(invalid(format!(
                    "{at}: the id {:?} is taken by {first}",
                    service.id
                )));
            }
        }

        let mut ids = BTreeMap::new();
        check_block("skills", &self.skills, false, &mut ids)?;
        check_block("plugins", &self.plugins, false, &mut ids)?;
        check_block("mcp", &self.mcp, false, &mut ids)?;
        check_block("runtime", self.runtime.as_slice(), true, &mut ids)?;
        check_block("models", &self.models, false, &mut ids)?;
        check_block("information", &self.information, false, &mut ids)?;
        check_block("hardware", self.hardware.as_slice(), true, &mut ids)?;
        check_block("network", self.network.as_slice(), true, &mut ids)?;
        for (i, mcp) in self.mcp.iter().enumerate() {
            check_mcp(&format!("mcp[{i}]"), &mcp.body)?;
        }
        for (i, info) in self.information.iter().enumerate() {
            if let Some(at) = &info.body.updated_at {
                check_timestamp(&format!("information[{i}].updated_at"), at)?;
            }
        }

        let json = serde_json::to_value(self).expect("an inventory always serializes");
        check_strings(&json, "", false)?;

        for (i, service) in self.services.iter().enumerate() {
            for link in &service.components {
                if !ids.contains_key(link.id.as_str()) {
                    return Err(invalid(format!(
                        "services[{i}] ({}): the component {:?} is not an entry of this \
                         inventory",
                        service.id, link.id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// The skill tag a capability travels as: [`CAPABILITY_PREFIX`] before a
/// common-vocabulary capability (`vision.ocr`, 1 to 128 characters
/// `[a-z0-9._-]`), and a namespaced one (`example:ocr`, as an entry id)
/// unchanged.
pub fn capability_tag(capability: &str) -> Result<String, InventoryError> {
    if capability.contains(':') {
        if is_namespaced_id(capability) {
            return Ok(capability.to_owned());
        }
    } else if is_common_capability(capability) {
        return Ok(format!("{CAPABILITY_PREFIX}{capability}"));
    }
    Err(invalid(
        "a capability is a common-vocabulary id, 1 to 128 characters [a-z0-9._-], or \
         <namespace>:<name> in a namespace other than agentnet",
    ))
}

/// [`Observation::detail`] for the offered tools a server does not list:
/// `missing tools: a, b`.
pub fn missing_tools_detail<S: AsRef<str>>(missing: &[S]) -> String {
    let names: Vec<&str> = missing.iter().map(AsRef::as_ref).collect();
    format!("{DETAIL_MISSING_TOOLS}{}", names.join(", "))
}

impl Observation {
    /// Refuses an observation a view must not carry: a `detail` other than
    /// a fixed code, timestamps not in the grant form, an observed tool with
    /// an invalid name or a description longer than
    /// [`MAX_OBSERVED_DESCRIPTION_BYTES`]. The error never quotes `detail`.
    pub fn check(&self) -> Result<(), InventoryError> {
        if let Some(detail) = &self.detail
            && !is_fixed_detail(detail)
        {
            return Err(invalid(
                "an observation's detail must be one of the fixed codes",
            ));
        }
        check_timestamp("observed_at", &self.observed_at)?;
        check_timestamp("expires_at", &self.expires_at)?;
        if let ObservationSource::Adapter { method } = &self.source
            && (method.is_empty() || method.len() > MAX_STRING_BYTES)
        {
            return Err(invalid("an adapter observation names its method"));
        }
        for tool in &self.tools {
            if !is_tool_name(&tool.name) {
                return Err(invalid("an observed tool has an invalid name"));
            }
            if tool
                .description
                .as_ref()
                .is_some_and(|d| d.len() > MAX_OBSERVED_DESCRIPTION_BYTES)
            {
                return Err(invalid(format!(
                    "an observed tool's description is longer than \
                     {MAX_OBSERVED_DESCRIPTION_BYTES} bytes"
                )));
            }
            if tool.annotations.as_ref().is_some_and(|a| !a.is_object()) {
                return Err(invalid("an observed tool's annotations are not an object"));
            }
        }
        Ok(())
    }
}

/// Whether `detail` is one of the fixed codes of [`Observation::detail`].
fn is_fixed_detail(detail: &str) -> bool {
    match detail.strip_prefix(DETAIL_MISSING_TOOLS) {
        Some(names) => names.split(", ").all(is_tool_name),
        None => matches!(
            detail,
            DETAIL_TIMEOUT | DETAIL_UNREACHABLE | DETAIL_PROTOCOL_ERROR | DETAIL_TOO_MANY_TOOLS
        ),
    }
}

/// Refuses what the typed reading cannot say clearly: an entry that
/// declares itself operational, and a TOML date-time where a timestamp
/// string goes.
fn check_declarations(table: &toml::Table) -> Result<(), InventoryError> {
    for (block, value) in table {
        let entries: Vec<(String, &toml::Table)> = match value {
            toml::Value::Array(items) => items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| Some((format!("{block}[{i}]"), item.as_table()?)))
                .collect(),
            toml::Value::Table(entry) => vec![(block.clone(), entry)],
            _ => Vec::new(),
        };
        for (at, entry) in entries {
            if entry.contains_key("operational") {
                let named = match entry.get("id").and_then(toml::Value::as_str) {
                    Some(id) => format!("{at} ({id})"),
                    None => at,
                };
                return Err(InventoryError::DeclaredOperational(named));
            }
        }
    }
    table
        .iter()
        .try_for_each(|(key, value)| check_no_datetime(value, key))
}

fn check_no_datetime(value: &toml::Value, at: &str) -> Result<(), InventoryError> {
    match value {
        toml::Value::Datetime(_) => Err(invalid(format!(
            "{at}: write a timestamp as a string, \"2026-11-14T00:00:00Z\""
        ))),
        toml::Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, item)| check_no_datetime(item, &format!("{at}[{i}]"))),
        toml::Value::Table(table) => table
            .iter()
            .try_for_each(|(key, item)| check_no_datetime(item, &join(at, key))),
        _ => Ok(()),
    }
}

/// `at.key`, or `key` at the top.
fn join(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_owned()
    } else {
        format!("{at}.{key}")
    }
}

/// Refuses a string or key longer than [`MAX_STRING_BYTES`] anywhere, and
/// an empty string outside `extensions` and an adapter's `args` (`free`):
/// what is unknown is left out.
fn check_strings(value: &Value, at: &str, free: bool) -> Result<(), InventoryError> {
    match value {
        Value::String(text) if text.len() > MAX_STRING_BYTES => Err(invalid(format!(
            "{at}: longer than {MAX_STRING_BYTES} bytes"
        ))),
        Value::String(text) if text.is_empty() && !free => Err(invalid(format!(
            "{at}: empty value; leave out what is unknown"
        ))),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, item)| check_strings(item, &format!("{at}[{i}]"), free)),
        Value::Object(map) => map.iter().try_for_each(|(key, item)| {
            let at = join(at, key);
            if key.len() > MAX_STRING_BYTES {
                return Err(invalid(format!(
                    "{at}: key longer than {MAX_STRING_BYTES} bytes"
                )));
            }
            check_strings(item, &at, free || key == "extensions" || key == "args")
        }),
        _ => Ok(()),
    }
}

fn check_service(at: &str, service: &Service) -> Result<(), InventoryError> {
    let id = &service.id;
    if id == CARD_SKILL_ID {
        return Err(invalid(format!(
            "{at}: the service id {id:?} is the Agent Card's own skill"
        )));
    }
    if !is_service_id(id) {
        return Err(invalid(format!(
            "{at}: the service id {id:?} is not a safe skill id, [a-z0-9][a-z0-9-]{{0,63}}: \
             every service becomes a skill of the Agent Card"
        )));
    }
    let at = format!("{at} ({id})");
    if service.name.is_empty() {
        return Err(invalid(format!(
            "{at}: empty name; every service becomes a skill of the Agent Card, which needs one"
        )));
    }
    if service.description.is_empty() {
        return Err(invalid(format!(
            "{at}: empty description; every service becomes a skill of the Agent Card, which \
             needs one"
        )));
    }
    if service.capabilities.is_empty() && service.tags.is_empty() {
        return Err(invalid(format!(
            "{at}: no capabilities and no tags; the Agent Card skill needs at least one tag"
        )));
    }
    for (i, capability) in service.capabilities.iter().enumerate() {
        capability_tag(capability)
            .map_err(|err| invalid(format!("{at}.capabilities[{i}]: {}", message(&err))))?;
    }
    for (i, tag) in service.tags.iter().enumerate() {
        if is_reserved_tag(tag) {
            return Err(invalid(format!(
                "{at}.tags[{i}]: the tag {tag:?} starts with {RESERVED_NAMESPACE}:, which only \
                 capability tags use: list the capability under `capabilities`"
            )));
        }
    }
    check_visibility(&format!("{at}.visibility"), &service.visibility)?;
    for (i, link) in service.components.iter().enumerate() {
        if link.group.as_deref() == Some("") {
            return Err(invalid(format!("{at}.components[{i}].group: empty value")));
        }
    }
    Ok(())
}

/// The message of an [`InventoryError::Invalid`], without its prefix.
fn message(err: &InventoryError) -> String {
    match err {
        InventoryError::Invalid(message) => message.clone(),
        other => other.to_string(),
    }
}

/// Checks what every entry of a block has, and records its id in `ids`.
fn check_block<'a, T>(
    block: &str,
    entries: &'a [Entry<T>],
    single: bool,
    ids: &mut BTreeMap<&'a str, String>,
) -> Result<(), InventoryError> {
    for (i, entry) in entries.iter().enumerate() {
        let at = if single {
            block.to_owned()
        } else {
            format!("{block}[{i}]")
        };
        if !is_namespaced_id(&entry.id) {
            return Err(invalid(format!(
                "{at}: the id {:?} is not <namespace>:<name>, with 1 to 64 characters \
                 [a-z0-9._-] in a namespace other than agentnet and 1 to 128 [A-Za-z0-9._/-] \
                 in the name",
                entry.id
            )));
        }
        if let Some(first) = ids.insert(&entry.id, at.clone()) {
            return Err(invalid(format!(
                "{at}: the id {:?} is taken by {first}",
                entry.id
            )));
        }
        let at = format!("{at} ({})", entry.id);
        check_visibility(&format!("{at}.visibility"), &entry.visibility)?;
        if let Some(expires_at) = &entry.expires_at {
            check_timestamp(&format!("{at}.expires_at"), expires_at)?;
        }
        if let Some(evidence) = &entry.evidence
            && evidence.reference.is_none()
            && evidence.hash.is_none()
        {
            return Err(invalid(format!(
                "{at}.evidence: give a reference, a hash or both"
            )));
        }
        if let Some(key) = entry.extensions.keys().find(|key| !is_namespaced_id(key)) {
            return Err(invalid(format!(
                "{at}.extensions: the extension key {key:?} needs a namespace other than \
                 agentnet, as example:{key}"
            )));
        }
    }
    Ok(())
}

fn check_mcp(at: &str, mcp: &McpConnection) -> Result<(), InventoryError> {
    let mut declared = BTreeMap::new();
    for (i, tool) in mcp.tools.iter().enumerate() {
        if !is_tool_name(&tool.name) {
            return Err(invalid(format!(
                "{at}.tools[{i}].name: a tool name is 1 to 128 characters [A-Za-z0-9_.-]"
            )));
        }
        if declared.insert(tool.name.as_str(), i).is_some() {
            return Err(invalid(format!(
                "{at}.tools[{i}]: the tool {:?} is declared twice",
                tool.name
            )));
        }
        if tool.annotations.as_ref().is_some_and(|a| !a.is_object()) {
            return Err(invalid(format!("{at}.tools[{i}].annotations: not a table")));
        }
    }
    let mut offered = BTreeMap::new();
    for (i, name) in mcp.offered_tools.iter().enumerate() {
        if !is_tool_name(name) {
            return Err(invalid(format!(
                "{at}.offered_tools[{i}]: a tool name is 1 to 128 characters [A-Za-z0-9_.-]"
            )));
        }
        if offered.insert(name.as_str(), i).is_some() {
            return Err(invalid(format!(
                "{at}.offered_tools[{i}]: {name:?} is offered twice"
            )));
        }
    }
    Ok(())
}

fn check_visibility(at: &str, visibility: &Visibility) -> Result<(), InventoryError> {
    match visibility {
        Visibility::Private | Visibility::Public => Ok(()),
        Visibility::Project(projects) | Visibility::Task(projects) => {
            if projects.is_empty() {
                return Err(invalid(format!(
                    "{at}: name at least one project, or make it private"
                )));
            }
            match projects.iter().find(|p| !is_project_name(p)) {
                Some(project) => Err(invalid(format!(
                    "{at}: the project {project:?} must be printable ASCII without spaces, as \
                     project names are"
                ))),
                None => Ok(()),
            }
        }
    }
}

fn check_timestamp(at: &str, text: &str) -> Result<(), InventoryError> {
    match parse_timestamp(text) {
        Some(_) => Ok(()),
        None => Err(invalid(format!(
            "{at}: not a timestamp in the form 2026-11-14T00:00:00Z"
        ))),
    }
}

/// `<namespace>:<name>`: 1 to 64 `[a-z0-9._-]`, not `agentnet`, then 1 to
/// 128 `[A-Za-z0-9._/-]`.
pub(crate) fn is_namespaced_id(id: &str) -> bool {
    let Some((namespace, name)) = id.split_once(':') else {
        return false;
    };
    namespace != RESERVED_NAMESPACE
        && (1..=64).contains(&namespace.len())
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
        && (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

/// Whether a service's own `tag` starts with `agentnet:`, in any case and
/// after any leading blanks: the card skill lists capability tags and own
/// tags together, and such a tag would pass for a capability
/// ([`CAPABILITY_PREFIX`]).
fn is_reserved_tag(tag: &str) -> bool {
    let tag = tag.trim_start().as_bytes();
    let prefix = RESERVED_NAMESPACE.len();
    tag.len() > prefix
        && tag[prefix] == b':'
        && tag[..prefix].eq_ignore_ascii_case(RESERVED_NAMESPACE.as_bytes())
}

/// A common-vocabulary capability: 1 to 128 `[a-z0-9._-]`.
fn is_common_capability(capability: &str) -> bool {
    (1..=128).contains(&capability.len())
        && capability
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

/// A safe skill id: `[a-z0-9][a-z0-9-]{0,63}`.
pub(crate) fn is_service_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.len() <= 64
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An MCP tool name: 1 to 128 `[A-Za-z0-9_.-]`.
fn is_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// A project name as the configuration and grants carry it: not empty,
/// printable ASCII without spaces.
pub(crate) fn is_project_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic())
}
