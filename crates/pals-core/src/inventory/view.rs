//! Views of the inventory (`agentnet.inventory-view/1`): what a requester
//! sees of it, in signed pages (design §9.8, §10).
//!
//! The full inventory is never published. A requester sees the view of its
//! [`Scope`], which comes from the grant it presented and from nothing else
//! it sends ([`Scope::for_grant`]):
//!
//! - **What shows** ([`render`]): the entries and services whose
//!   [`super::Visibility`] is visible in the scope, never a private one
//!   (installed but not offered). Each carries its five dimensions apart
//!   ([`Dimensions`]): installed and offered, since it shows; operational,
//!   the observation the snapshot holds of it; authorized, only in a task
//!   scope. Availability is never the inventory's: a service offer says it.
//!   The body is the block's own fields, with the entry's `evidence` and
//!   `extensions`, never its `visibility` (which would name the other
//!   projects that see it) nor an MCP connection's `adapter`. A service
//!   keeps only its links to components the scope shows, and an alternative
//!   left alone in its group shows as optional: a view never reveals that a
//!   hidden component exists. An MCP connection publishes only
//!   the tools it offers, and, when an adapter checks it, only those the
//!   adapter observed, named `<connection id>/<tool>` (§9.3).
//! - **Revision**: a scope's view has a sequence, the `seq` of its pages,
//!   that moves only when what the scope shows changes
//!   ([`crate::store::Store::view_revision`] over [`content_hash`] of every
//!   section). Hidden entries count in nothing a page carries: a public page
//!   has no totals, counts, hashes or references of what it leaves out.
//! - **Pages** ([`page`]): a page ends at the first limit reached: its signed
//!   JSON at [`MAX_PAGE_BYTES`], [`MAX_PAGE_ENTRIES`] entries (or fewer, if
//!   asked), or [`MAX_QUERY_TOTAL_BYTES`] summed over the pages of one chain
//!   of cursors; [`ViewPage::truncated_by`] says which. The last page of a
//!   chain cut at the total says [`PageLimit::QueryTotal`] and has no cursor.
//! - **Cursors**: `base64url(JCS({scope, audience, sections, revision,
//!   offset, bytes_so_far}))` `.` `base64url(BLAKE3-keyed(key, payload))`,
//!   under a [`CursorKey`] drawn anew on every start: a cursor serves only
//!   the requester, scope, sections and revision it was made for, and dies
//!   with the run. Readable by its holder, never writable.
//! - **Signature**: a [`ViewPage`] is a [`SignedDoc`] under
//!   [`VIEW_SIG_DOMAIN`], at most [`MAX_PAGE_BYTES`], living at most
//!   [`VIEW_TTL_S`], made by [`sign_page`] from what [`page`] cut. A private
//!   page names its requester (`audience`), a public one nobody
//!   ([`verify_page`]). A page read from a peer must keep every rule of
//!   `inventory-view.schema.json` ([`ViewPage`]'s `validate`): an entry shows
//!   only what a view can show.
//! - **Cache** ([`ViewCache`]): pages kept by everything that makes them
//!   differ, the requester included, so a private page is never served to
//!   another requester.
//!
//! Errors never say whether something hidden exists: a cursor that does not
//! serve the request is [`ViewError::InvalidCursor`], whatever the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    CARD_SKILL_ID, ComponentLink, ComponentRole, DETAIL_MISSING_TOOLS, Entry, InventorySnapshot,
    MAX_STRING_BYTES, McpConnection, McpTool, Observation, Scope, Section, Service,
    is_namespaced_id, is_service_id, is_tool_name,
};
use crate::card_jws::CardHash;
use crate::grant::{format_timestamp, parse_timestamp};
use crate::identity::{AgentId, AgentKey};
use crate::jcs::canonical;
use crate::signed::{Signed, SignedDoc, SignedError, check_validity};

/// The value of [`ViewPage::schema`].
pub const VIEW_SCHEMA: &str = "agentnet.inventory-view/1";
/// The prefix of the bytes a page signature covers.
pub const VIEW_SIG_DOMAIN: &[u8] = b"agentnet.inventory-view/1\n";
/// How long a page is valid after it is issued, in seconds.
pub const VIEW_TTL_S: i64 = 300;
/// The largest signed page, in bytes.
pub const MAX_PAGE_BYTES: usize = 64 * 1024;
/// The most entries in a page.
pub const MAX_PAGE_ENTRIES: usize = 100;
/// The most bytes of pages one chain of cursors serves.
pub const MAX_QUERY_TOTAL_BYTES: usize = 1024 * 1024;
/// The longest cursor read; a longer one is invalid, unread.
pub const MAX_CURSOR_BYTES: usize = 16 * 1024;
/// The most pages a [`ViewCache`] keeps.
pub const MAX_CACHED_PAGES: usize = 256;
/// The sections [`render`] shows, in the order it shows them. A scope's
/// revision is the [`content_hash`] of its view of all of them.
pub const INVENTORY_SECTIONS: [Section; 9] = [
    Section::Services,
    Section::Skills,
    Section::Plugins,
    Section::Mcp,
    Section::Runtime,
    Section::Models,
    Section::Information,
    Section::Hardware,
    Section::Network,
];

/// [`ViewEntry::provenance`] of a service: the operator declares it.
const OPERATOR: &str = "operator";
/// The largest `seq`: every integer up to here is exact as a JSON number in
/// any language. The first is 1.
const MAX_SEQ: u64 = (1 << 53) - 1;
/// The length of an Ed25519 signature in base64url without padding.
const SIGNATURE_CHARS: usize = 86;

/// An entry or a service, as a scope shows it.
///
/// For a service, `id` is the service id, `provenance` is `operator` and
/// there is no `version` nor `expires_at`. `body` is a JSON object: the
/// fields of the entry's block, with its `evidence` and `extensions` when it
/// has them, or the fields of the service but its `id`, `description` and
/// `visibility`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewEntry {
    pub section: Section,
    pub id: String,
    pub description: String,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub dimensions: Dimensions,
    pub body: Value,
    /// When the declaration stops holding, as a grant timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Four of the five dimensions of design §9.3; the fifth, *available now*,
/// is a service offer's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dimensions {
    /// In the inventory: always `true`.
    pub installed: bool,
    /// What an adapter observed, with its own `observed_at` and
    /// `expires_at`, without its tool list (the body names the tools
    /// published); `None` when nothing was observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operational: Option<Observation>,
    /// Shown in the scope: always `true`.
    pub offered: bool,
    /// The requester's grant allows tasks with it: `true` only in a task
    /// scope. Checked again when a task runs; a view is never a grant.
    pub authorized: bool,
}

/// A signed page of a view.
///
/// Timestamps are in the form [`crate::grant::format_timestamp`] writes;
/// `expires_at` comes after `issued_at`, by at most [`VIEW_TTL_S`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewPage {
    /// [`VIEW_SCHEMA`].
    pub schema: String,
    /// The agent whose inventory this is, and the page's signer.
    pub agent_id: AgentId,
    /// The scope's key ([`Scope::key`]).
    pub scope: String,
    /// The requester, in every scope but `public`, which has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<AgentId>,
    /// The scope's revision, from 1 to 2^53 - 1.
    pub seq: u64,
    pub issued_at: String,
    pub expires_at: String,
    /// The card that describes the agent in this scope.
    pub agent_card_hash: CardHash,
    /// The sections asked for, in order and without repeats.
    pub sections: Vec<Section>,
    /// At most [`MAX_PAGE_ENTRIES`], each of one of `sections`.
    pub entries: Vec<ViewEntry>,
    /// Where the next page starts; present exactly when the page ended at
    /// [`PageLimit::PageBytes`] or [`PageLimit::PageEntries`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The limit that ended the page before the view's end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_by: Option<PageLimit>,
}

/// The limit that ended a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageLimit {
    /// [`MAX_PAGE_BYTES`]: the next page has more.
    PageBytes,
    /// The entries asked for, at most [`MAX_PAGE_ENTRIES`]: the next page
    /// has more.
    PageEntries,
    /// [`MAX_QUERY_TOTAL_BYTES`] over the chain of cursors: no more pages.
    QueryTotal,
}

/// Why a page or a cursor was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ViewError {
    /// Not a cursor this run made for this request.
    #[error("invalid cursor")]
    InvalidCursor,
    /// A page larger than [`MAX_PAGE_BYTES`], or an entry that does not fit
    /// in a page on its own.
    #[error("too large")]
    TooLarge,
    /// Outside its validity window, beyond [`crate::grant::CLOCK_SKEW_S`] at
    /// either end.
    #[error("expired")]
    Expired,
    /// Not signed by the agent whose page it is.
    #[error("bad signature")]
    BadSignature,
    #[error("malformed")]
    Malformed,
    /// A private page for another requester.
    #[error("wrong audience")]
    WrongAudience,
}

impl SignedDoc for ViewPage {
    const SCHEMA: &'static str = VIEW_SCHEMA;
    const DOMAIN: &'static [u8] = VIEW_SIG_DOMAIN;
    const WRAPPER: &'static str = "view";
    const MAX_BYTES: usize = MAX_PAGE_BYTES;
    const SIGNER_POINTER: &'static str = "/agent_id";

    fn signer(&self) -> &AgentId {
        &self.agent_id
    }

    /// A scope key; an `audience` exactly when the scope is not `public`;
    /// `seq` from 1 to 2^53 - 1; a lifetime of at most [`VIEW_TTL_S`];
    /// sections in order without repeats, and every entry of one of them,
    /// shown as a view shows it ([`shows_as_a_view_does`]); at most
    /// [`MAX_PAGE_ENTRIES`] entries; a cursor of at most
    /// [`MAX_CURSOR_BYTES`], present exactly when a page limit ended the
    /// page.
    fn validate(&self) -> Result<(), SignedError> {
        let scope = Scope::from_key(&self.scope).ok_or(SignedError::Malformed)?;
        let (Some(issued_at), Some(expires_at)) = (
            parse_timestamp(&self.issued_at),
            parse_timestamp(&self.expires_at),
        ) else {
            return Err(SignedError::Malformed);
        };
        let has_cursor = matches!(
            self.truncated_by,
            Some(PageLimit::PageBytes | PageLimit::PageEntries)
        );
        let valid = (scope == Scope::Public) == self.audience.is_none()
            && (1..=MAX_SEQ).contains(&self.seq)
            && issued_at < expires_at
            && expires_at - issued_at <= VIEW_TTL_S
            && self.sections.windows(2).all(|pair| pair[0] < pair[1])
            && self.entries.len() <= MAX_PAGE_ENTRIES
            && self.entries.iter().all(|entry| {
                self.sections.contains(&entry.section) && shows_as_a_view_does(entry, &scope)
            })
            && self.next_cursor.is_some() == has_cursor
            && self
                .next_cursor
                .as_ref()
                .is_none_or(|cursor| !cursor.is_empty() && cursor.len() <= MAX_CURSOR_BYTES);
        if valid {
            Ok(())
        } else {
            Err(SignedError::Malformed)
        }
    }
}

/// Whether `entry` of a page of `scope` keeps the rules of
/// `inventory-view.schema.json`, which a peer's page is held to:
///
/// - **dimensions**: `installed` and `offered`, since it shows; `authorized`
///   only in a task scope; an `operational` observation that a view may
///   carry ([`Observation::check`]), without its tool list;
/// - **text**: `description`, `provenance` and `version` of 1 to
///   [`MAX_STRING_BYTES`] bytes, `expires_at` a grant timestamp;
/// - **ids**: a service id (`[a-z0-9][a-z0-9-]{0,63}`, never `ask`) in
///   `services`, an entry id (`<namespace>:<name>`) in the inventory's other
///   sections, any non-empty id in `benchmarks`;
/// - **body**: an object, never with `visibility`, `adapter` or
///   `offered_tools`; a service's without `id` or `description`, with
///   `provenance` `operator` and neither `version` nor `expires_at`, and
///   only links to entry ids; an MCP connection's tools named
///   `<its id>/<tool>`.
fn shows_as_a_view_does(entry: &ViewEntry, scope: &Scope) -> bool {
    let dimensions = &entry.dimensions;
    let shown = dimensions.installed
        && dimensions.offered
        && (!dimensions.authorized || matches!(scope, Scope::Task(_)))
        && dimensions
            .operational
            .as_ref()
            .is_none_or(|observed| observed.tools.is_empty() && observed.check().is_ok());
    let text = is_text(&entry.description)
        && is_text(&entry.provenance)
        && entry.version.as_deref().is_none_or(is_text)
        && entry
            .expires_at
            .as_deref()
            .is_none_or(|at| parse_timestamp(at).is_some());
    let Value::Object(body) = &entry.body else {
        return false;
    };
    let never = ["visibility", "adapter", "offered_tools"]
        .iter()
        .all(|key| !body.contains_key(*key));
    let by_section = match entry.section {
        Section::Services => {
            is_service_id(&entry.id)
                && entry.id != CARD_SKILL_ID
                && entry.provenance == OPERATOR
                && entry.version.is_none()
                && entry.expires_at.is_none()
                && !body.contains_key("id")
                && !body.contains_key("description")
                && links_only_entries(body)
        }
        Section::Mcp => is_namespaced_id(&entry.id) && tools_of_its_own(&entry.id, body),
        Section::Benchmarks => !entry.id.is_empty(),
        _ => is_namespaced_id(&entry.id),
    };
    shown && text && never && by_section
}

/// A string a view carries: 1 to [`MAX_STRING_BYTES`] bytes.
fn is_text(text: &str) -> bool {
    (1..=MAX_STRING_BYTES).contains(&text.len())
}

/// Whether a service's `components`, if any, are links to entry ids.
fn links_only_entries(body: &Map<String, Value>) -> bool {
    body.get("components").is_none_or(|components| {
        Vec::<ComponentLink>::deserialize(components).is_ok_and(|links| {
            links
                .iter()
                .all(|link| is_namespaced_id(&link.id) && link.group.as_deref().is_none_or(is_text))
        })
    })
}

/// Whether an MCP connection's `tools`, if any, are its own, named
/// `<id>/<tool>`, with a text for description and an object for
/// annotations.
fn tools_of_its_own(id: &str, body: &Map<String, Value>) -> bool {
    body.get("tools").is_none_or(|tools| {
        Vec::<McpTool>::deserialize(tools).is_ok_and(|tools| {
            tools.iter().all(|tool| {
                tool.name
                    .strip_prefix(id)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .is_some_and(is_tool_name)
                    && tool.description.as_deref().is_none_or(is_text)
                    && tool.annotations.as_ref().is_none_or(Value::is_object)
            })
        })
    })
}

/// What `scope` shows of `inv` in `sections`: in the order of
/// [`INVENTORY_SECTIONS`], and in each, in the inventory's order. Only the
/// inventory's sections: [`Section::Benchmarks`] shows nothing here.
///
/// An entry renders the same whichever sections are asked for: which links
/// and tools it shows depends only on the scope and the snapshot.
pub fn render(inv: &InventorySnapshot, scope: &Scope, sections: &[Section]) -> Vec<ViewEntry> {
    let renderer = Renderer::new(inv, scope);
    let inventory = &inv.inventory;
    let mut entries = Vec::new();
    for section in INVENTORY_SECTIONS
        .into_iter()
        .filter(|section| sections.contains(section))
    {
        match section {
            Section::Services => entries.extend(
                inventory
                    .services
                    .iter()
                    .filter(|service| service.visibility.visible_in(scope))
                    .map(|service| renderer.service(service)),
            ),
            Section::Skills => renderer.block(section, &inventory.skills, &mut entries),
            Section::Plugins => renderer.block(section, &inventory.plugins, &mut entries),
            Section::Mcp => entries.extend(
                inventory
                    .mcp
                    .iter()
                    .filter(|entry| entry.visibility.visible_in(scope))
                    .map(|entry| renderer.mcp(entry)),
            ),
            Section::Runtime => {
                renderer.block(section, inventory.runtime.as_slice(), &mut entries);
            }
            Section::Models => renderer.block(section, &inventory.models, &mut entries),
            Section::Information => {
                renderer.block(section, &inventory.information, &mut entries);
            }
            Section::Hardware => {
                renderer.block(section, inventory.hardware.as_slice(), &mut entries);
            }
            Section::Network => {
                renderer.block(section, inventory.network.as_slice(), &mut entries);
            }
            Section::Benchmarks => {}
        }
    }
    entries
}

/// What a scope sees, while [`render`] builds its entries.
struct Renderer<'a> {
    scope: &'a Scope,
    observations: &'a BTreeMap<String, Observation>,
    /// The ids of the entries the scope shows, in every block.
    visible: BTreeSet<&'a str>,
    authorized: bool,
}

impl<'a> Renderer<'a> {
    fn new(inv: &'a InventorySnapshot, scope: &'a Scope) -> Self {
        let inventory = &inv.inventory;
        let mut visible = BTreeSet::new();
        add_visible(&mut visible, &inventory.skills, scope);
        add_visible(&mut visible, &inventory.plugins, scope);
        add_visible(&mut visible, &inventory.mcp, scope);
        add_visible(&mut visible, inventory.runtime.as_slice(), scope);
        add_visible(&mut visible, &inventory.models, scope);
        add_visible(&mut visible, &inventory.information, scope);
        add_visible(&mut visible, inventory.hardware.as_slice(), scope);
        add_visible(&mut visible, inventory.network.as_slice(), scope);
        Self {
            scope,
            observations: &inv.observations,
            visible,
            authorized: matches!(scope, Scope::Task(_)),
        }
    }

    fn dimensions(&self, operational: Option<Observation>) -> Dimensions {
        Dimensions {
            installed: true,
            operational,
            offered: true,
            authorized: self.authorized,
        }
    }

    /// A service, with only its links to components the scope shows. An
    /// alternative that is the only one of its group the scope shows (the
    /// links without a group being one group) shows as optional, in no
    /// group: it reads the same whether hidden alternatives stand in for it
    /// or none do.
    fn service(&self, service: &Service) -> ViewEntry {
        let shown: Vec<&ComponentLink> = service
            .components
            .iter()
            .filter(|link| self.visible.contains(link.id.as_str()))
            .collect();
        let mut alternatives = BTreeMap::<Option<&str>, usize>::new();
        for link in shown
            .iter()
            .filter(|link| link.role == ComponentRole::Alternative)
        {
            *alternatives.entry(link.group.as_deref()).or_default() += 1;
        }
        let components = shown
            .into_iter()
            .map(|link| match link.role {
                ComponentRole::Alternative if alternatives[&link.group.as_deref()] == 1 => {
                    ComponentLink {
                        id: link.id.clone(),
                        role: ComponentRole::Optional,
                        group: None,
                    }
                }
                _ => link.clone(),
            })
            .collect();
        let mut body = object_of(&Service {
            components,
            ..service.clone()
        });
        for key in ["id", "description", "visibility"] {
            body.remove(key);
        }
        ViewEntry {
            section: Section::Services,
            id: service.id.clone(),
            description: service.description.clone(),
            provenance: OPERATOR.to_owned(),
            version: None,
            dimensions: self.dimensions(None),
            body: Value::Object(body),
            expires_at: None,
        }
    }

    /// The entries of a block the scope shows, with their block's fields.
    fn block<T: Serialize>(&self, section: Section, block: &[Entry<T>], out: &mut Vec<ViewEntry>) {
        for entry in block
            .iter()
            .filter(|entry| entry.visibility.visible_in(self.scope))
        {
            let operational = self.observation(&entry.id).map(|o| published(o, &[]));
            out.push(self.entry(section, entry, object_of(&entry.body), operational));
        }
    }

    /// An MCP connection: its offered tools, and with an adapter only those
    /// it observed, named `<id>/<tool>`, with what was observed of each and
    /// else what was declared; never its adapter, nor a tool not offered.
    fn mcp(&self, entry: &Entry<McpConnection>) -> ViewEntry {
        let mcp = &entry.body;
        let observation = self.observation(&entry.id);
        let tools: Vec<McpTool> = mcp
            .offered_tools
            .iter()
            .filter_map(|name| {
                let declared = mcp.tools.iter().find(|tool| tool.name == *name);
                let observed =
                    observation.and_then(|o| o.tools.iter().find(|tool| tool.name == *name));
                if mcp.adapter.is_some() && observed.is_none() {
                    return None;
                }
                let description = observed.and_then(|tool| tool.description.clone());
                let annotations = observed.and_then(|tool| tool.annotations.clone());
                Some(McpTool {
                    name: format!("{}/{name}", entry.id),
                    description: description
                        .or_else(|| declared.and_then(|tool| tool.description.clone())),
                    annotations: annotations
                        .or_else(|| declared.and_then(|tool| tool.annotations.clone())),
                })
            })
            .collect();
        let shown = McpConnection {
            server: mcp.server.clone(),
            server_version: mcp.server_version.clone(),
            tools,
            resources: mcp.resources.clone(),
            adapter: None,
            offered_tools: Vec::new(),
        };
        let operational = observation.map(|o| published(o, &mcp.offered_tools));
        self.entry(Section::Mcp, entry, object_of(&shown), operational)
    }

    fn entry<T>(
        &self,
        section: Section,
        entry: &Entry<T>,
        mut body: Map<String, Value>,
        operational: Option<Observation>,
    ) -> ViewEntry {
        if let Some(evidence) = &entry.evidence {
            body.insert("evidence".to_owned(), to_value(evidence));
        }
        if !entry.extensions.is_empty() {
            body.insert("extensions".to_owned(), to_value(&entry.extensions));
        }
        ViewEntry {
            section,
            id: entry.id.clone(),
            description: entry.description.clone(),
            provenance: entry.provenance.clone(),
            version: entry.version.clone(),
            dimensions: self.dimensions(operational),
            body: Value::Object(body),
            expires_at: entry.expires_at.clone(),
        }
    }

    /// The observation of `id`, unless it is one a view must not carry
    /// ([`Observation::check`]).
    fn observation(&self, id: &str) -> Option<&'a Observation> {
        self.observations
            .get(id)
            .filter(|observation| observation.check().is_ok())
    }
}

/// Adds to `visible` the ids of the entries of `block` that `scope` shows.
fn add_visible<'a, T>(visible: &mut BTreeSet<&'a str>, block: &'a [Entry<T>], scope: &Scope) {
    visible.extend(
        block
            .iter()
            .filter(|entry| entry.visibility.visible_in(scope))
            .map(|entry| entry.id.as_str()),
    );
}

/// `observation` as a view carries it: without its tool list, and without a
/// `missing tools` detail that names a tool not among `offered`.
fn published(observation: &Observation, offered: &[String]) -> Observation {
    let names_only_offered = |detail: &String| match detail.strip_prefix(DETAIL_MISSING_TOOLS) {
        Some(names) => names
            .split(", ")
            .all(|name| offered.iter().any(|o| o == name)),
        None => true,
    };
    Observation {
        detail: observation.detail.clone().filter(names_only_offered),
        tools: Vec::new(),
        ..observation.clone()
    }
}

fn to_value<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).expect("inventory values always serialize")
}

/// The JSON object `value` serializes to.
fn object_of<T: Serialize>(value: &T) -> Map<String, Value> {
    match to_value(value) {
        Value::Object(map) => map,
        other => unreachable!("an inventory block serializes to an object, not {other}"),
    }
}

/// `blake3:` and the hex of BLAKE3 over the JCS of `entries`: what a scope's
/// revision follows. Kept by the agent, never published.
pub fn content_hash(entries: &[ViewEntry]) -> String {
    format!(
        "blake3:{}",
        blake3::hash(&canonical(&to_value(entries))).to_hex()
    )
}

/// The key this run's cursors are authenticated with. Never stored: a new
/// one on every start, so the cursors of a run die with it.
pub struct CursorKey([u8; 32]);

impl CursorKey {
    /// A new key from the operating system's random number generator.
    ///
    /// # Panics
    ///
    /// If the operating system cannot provide random bytes.
    pub fn random() -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("the OS random number generator failed");
        Self(key)
    }

    fn mac(&self, payload: &[u8]) -> blake3::Hash {
        blake3::keyed_hash(&self.0, payload)
    }
}

impl fmt::Debug for CursorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CursorKey(..)")
    }
}

/// What a cursor carries: what it serves, and where its chain is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audience: Option<AgentId>,
    sections: Vec<Section>,
    revision: u64,
    /// The first entry of the next page.
    offset: u64,
    /// The bytes of the chain's pages so far.
    bytes_so_far: u64,
}

impl Cursor {
    fn encode(&self, key: &CursorKey) -> String {
        let payload = canonical(&to_value(self));
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(key.mac(&payload).as_bytes())
        )
    }

    /// The cursor in `text`, if `key` made it.
    fn decode(text: &str, key: &CursorKey) -> Option<Self> {
        if text.len() > MAX_CURSOR_BYTES {
            return None;
        }
        let (payload, mac) = text.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let mac: [u8; 32] = URL_SAFE_NO_PAD.decode(mac).ok()?.try_into().ok()?;
        // `Hash` compares in constant time.
        if key.mac(&payload) != blake3::Hash::from(mac) {
            return None;
        }
        serde_json::from_slice(&payload).ok()
    }
}

/// A request for a page of a view.
#[derive(Debug, Clone, Copy)]
pub struct PageRequest<'a> {
    /// The requester's scope, from its grant.
    pub scope: &'a Scope,
    /// The requester, `None` in the public scope.
    pub audience: Option<&'a AgentId>,
    /// The sections asked for, in order and without repeats, as the page
    /// carries them.
    pub sections: &'a [Section],
    /// The previous page's `next_cursor`; `None` for the first page.
    pub cursor: Option<&'a str>,
    /// The most entries the requester wants: at most [`MAX_PAGE_ENTRIES`],
    /// which `None` and `Some(0)` also mean.
    pub page_entries: Option<u32>,
    /// The scope's revision, which the page carries as `seq`.
    pub revision: u64,
}

/// The entries of a page, its `next_cursor` and its `truncated_by`.
pub type PageSlice = (Vec<ViewEntry>, Option<String>, Option<PageLimit>);

/// The page of `entries`, the view `req` asks for, that starts at its
/// cursor: the entries, the cursor of the next page and the limit that ended
/// it ([`PageLimit`]).
///
/// The page is sized for a [`ViewPage`] of `req` (its scope, audience,
/// sections and revision as `seq`), signed: with the entries, it is at most
/// [`MAX_PAGE_BYTES`], and the bytes of the chain's pages are at most
/// [`MAX_QUERY_TOTAL_BYTES`]. When the next page could not hold even its
/// first entry within that total, this one is the last, with
/// [`PageLimit::QueryTotal`].
///
/// [`ViewError::InvalidCursor`] when the cursor is not one `key` made for
/// this scope, audience, sections and revision, whatever is wrong with it;
/// [`ViewError::TooLarge`] when an entry does not fit in a page on its own.
pub fn page(
    entries: &[ViewEntry],
    req: &PageRequest<'_>,
    key: &CursorKey,
) -> Result<PageSlice, ViewError> {
    let binding = Cursor {
        scope: req.scope.key(),
        audience: req.audience.copied(),
        sections: normalized(req.sections),
        revision: req.revision,
        offset: 0,
        bytes_so_far: 0,
    };
    let (offset, bytes_so_far) = match req.cursor {
        None => (0, 0),
        Some(text) => {
            let cursor = Cursor::decode(text, key).ok_or(ViewError::InvalidCursor)?;
            let offset = usize::try_from(cursor.offset)
                .ok()
                .filter(|&offset| offset <= entries.len());
            let bytes_so_far = usize::try_from(cursor.bytes_so_far)
                .ok()
                .filter(|&bytes| bytes <= MAX_QUERY_TOTAL_BYTES);
            let serves = cursor
                == Cursor {
                    offset: cursor.offset,
                    bytes_so_far: cursor.bytes_so_far,
                    ..binding.clone()
                };
            match (serves, offset, bytes_so_far) {
                (true, Some(offset), Some(bytes_so_far)) => (offset, bytes_so_far),
                _ => return Err(ViewError::InvalidCursor),
            }
        }
    };

    let max_entries = page_entries(req.page_entries);
    let overhead = page_overhead(req, &binding, entries.len(), key);
    let entry_bytes = |entry: &ViewEntry| canonical(&to_value(entry)).len();
    let mut size = overhead;
    let mut end = offset;
    let mut limit = None;
    while end < entries.len() {
        if end - offset == max_entries {
            limit = Some(PageLimit::PageEntries);
            break;
        }
        // A comma before every entry but the first.
        let bytes = entry_bytes(&entries[end]) + usize::from(end > offset);
        if end == offset && overhead + bytes > MAX_PAGE_BYTES {
            return Err(ViewError::TooLarge);
        }
        if bytes_so_far + size + bytes > MAX_QUERY_TOTAL_BYTES {
            limit = Some(PageLimit::QueryTotal);
            break;
        }
        if size + bytes > MAX_PAGE_BYTES {
            limit = Some(PageLimit::PageBytes);
            break;
        }
        size += bytes;
        end += 1;
    }

    let mut next_cursor = None;
    if matches!(limit, Some(PageLimit::PageBytes | PageLimit::PageEntries)) {
        let bytes_so_far = bytes_so_far + size;
        if bytes_so_far + overhead + entry_bytes(&entries[end]) > MAX_QUERY_TOTAL_BYTES {
            limit = Some(PageLimit::QueryTotal);
        } else {
            let next = Cursor {
                offset: end as u64,
                bytes_so_far: bytes_so_far as u64,
                ..binding
            };
            next_cursor = Some(next.encode(key));
        }
    }
    Ok((entries[offset..end].to_vec(), next_cursor, limit))
}

/// The page `slice` of the view `req` asks for, as `page` cut it, signed by
/// `key` at `now_unix` for [`VIEW_TTL_S`], naming the card of `card_hash`:
/// the page `page` sized, with `req`'s scope and audience, its revision as
/// `seq` and its sections in order without repeats.
///
/// [`ViewError::TooLarge`] when the signed page would exceed
/// [`MAX_PAGE_BYTES`], which a slice from [`page`] never does;
/// [`ViewError::Malformed`] when it would not be a valid page: a time
/// outside the years 0000 to 9999, or a scope whose project is not a
/// project name.
pub fn sign_page(
    key: &AgentKey,
    card_hash: &CardHash,
    req: &PageRequest<'_>,
    slice: PageSlice,
    now_unix: i64,
) -> Result<Signed<ViewPage>, ViewError> {
    let (entries, next_cursor, truncated_by) = slice;
    // Twenty characters each, as `page` sized them.
    let issued_at = format_timestamp(now_unix).ok_or(ViewError::Malformed)?;
    let expires_at = now_unix
        .checked_add(VIEW_TTL_S)
        .and_then(format_timestamp)
        .ok_or(ViewError::Malformed)?;
    let doc = ViewPage {
        schema: VIEW_SCHEMA.to_owned(),
        agent_id: key.id(),
        scope: req.scope.key(),
        audience: req.audience.copied(),
        seq: req.revision,
        issued_at,
        expires_at,
        agent_card_hash: card_hash.clone(),
        sections: normalized(req.sections),
        entries,
        next_cursor,
        truncated_by,
    };
    Signed::issue(key, doc).map_err(|err| match err {
        SignedError::TooLarge => ViewError::TooLarge,
        _ => ViewError::Malformed,
    })
}

/// The entries a page of `asked` holds at most.
fn page_entries(asked: Option<u32>) -> usize {
    match asked.and_then(|asked| usize::try_from(asked).ok()) {
        Some(asked) if asked > 0 => asked.min(MAX_PAGE_ENTRIES),
        _ => MAX_PAGE_ENTRIES,
    }
}

/// `sections` in order, without repeats.
fn normalized(sections: &[Section]) -> Vec<Section> {
    let mut sections = sections.to_vec();
    sections.sort_unstable();
    sections.dedup();
    sections
}

/// The bytes of a signed page of `req` without entries, at least: every
/// field at its longest, a cursor with the largest numbers its chain of
/// `entries` can reach and the longest limit. The entries add their JCS and
/// a comma between each two. [`sign_page`] builds the page with these
/// fields, and no other.
fn page_overhead(
    req: &PageRequest<'_>,
    binding: &Cursor,
    entries: usize,
    key: &CursorKey,
) -> usize {
    /// Any agent id: every one has 64 characters.
    static ANY_AGENT: LazyLock<AgentId> = LazyLock::new(|| AgentKey::from_bytes(&[0; 32]).id());
    /// Every timestamp has 20 characters, and every card hash 71.
    const ANY_TIMESTAMP: &str = "2026-11-14T00:00:00Z";
    const ANY_CARD_HASH: &str =
        "blake3:0000000000000000000000000000000000000000000000000000000000000000";

    let longest_cursor = Cursor {
        offset: entries as u64,
        bytes_so_far: MAX_QUERY_TOTAL_BYTES as u64,
        ..binding.clone()
    };
    let skeleton = ViewPage {
        schema: VIEW_SCHEMA.to_owned(),
        agent_id: *ANY_AGENT,
        scope: binding.scope.clone(),
        audience: req.audience.copied(),
        seq: req.revision,
        issued_at: ANY_TIMESTAMP.to_owned(),
        expires_at: ANY_TIMESTAMP.to_owned(),
        agent_card_hash: ANY_CARD_HASH.parse().expect("a card hash"),
        sections: binding.sections.clone(),
        entries: Vec::new(),
        next_cursor: Some(longest_cursor.encode(key)),
        truncated_by: Some(PageLimit::PageEntries),
    };
    Signed {
        doc: skeleton,
        signature: "A".repeat(SIGNATURE_CHARS),
    }
    .to_json()
    .len()
}

/// Reads a signed page of `agent`'s inventory for `audience` at `now_unix`.
///
/// [`ViewError::TooLarge`] over [`MAX_PAGE_BYTES`], before reading it;
/// then, as [`Signed::from_json`] checks it, [`ViewError::BadSignature`]
/// when its signature does not verify and [`ViewError::Malformed`] when it
/// is not a valid page ([`SignedDoc::validate`]);
/// [`ViewError::BadSignature`] as well when a valid page is another agent's;
/// [`ViewError::WrongAudience`] when it names a requester other than
/// `audience` (a page without one is public, and serves anybody); last,
/// [`ViewError::Expired`] outside its validity window, with
/// [`crate::grant::CLOCK_SKEW_S`] of tolerance at both ends.
pub fn verify_page(
    bytes: &[u8],
    agent: &AgentId,
    audience: Option<&AgentId>,
    now_unix: i64,
) -> Result<ViewPage, ViewError> {
    let page = Signed::<ViewPage>::from_json(bytes)
        .map_err(|err| match err {
            SignedError::TooLarge => ViewError::TooLarge,
            SignedError::BadSignature => ViewError::BadSignature,
            _ => ViewError::Malformed,
        })?
        .doc;
    if page.agent_id != *agent {
        return Err(ViewError::BadSignature);
    }
    match (&page.audience, audience) {
        // `validate` holds a page without audience to the public scope.
        (None, _) if page.scope == Scope::Public.key() => {}
        (Some(named), Some(audience)) if named == audience => {}
        _ => return Err(ViewError::WrongAudience),
    }
    check_validity(&page.issued_at, &page.expires_at, now_unix).map_err(|err| match err {
        SignedError::Malformed => ViewError::Malformed,
        _ => ViewError::Expired,
    })?;
    Ok(page)
}

/// Signed pages this agent made, kept for the requests that would make them
/// again: by scope, requester, sections, cursor, page size and revision, so
/// a page made for one requester never serves another. At most
/// [`MAX_CACHED_PAGES`]: one more drops the one that goes out soonest.
/// `Send + Sync`.
pub struct ViewCache {
    pages: Mutex<BTreeMap<CacheKey, Cached>>,
}

/// Everything that makes two pages differ.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    scope: String,
    audience: Option<AgentId>,
    sections: Vec<Section>,
    cursor: Option<String>,
    page_entries: usize,
    revision: u64,
}

impl CacheKey {
    fn of(req: &PageRequest<'_>) -> Self {
        Self {
            scope: req.scope.key(),
            audience: req.audience.copied(),
            sections: normalized(req.sections),
            cursor: req.cursor.map(str::to_owned),
            page_entries: page_entries(req.page_entries),
            revision: req.revision,
        }
    }
}

struct Cached {
    page: Vec<u8>,
    until: i64,
}

impl ViewCache {
    pub fn new() -> Self {
        Self {
            pages: Mutex::new(BTreeMap::new()),
        }
    }

    /// The page kept for `req`, while `now_unix` is before the time it was
    /// kept until.
    pub fn get(&self, req: &PageRequest<'_>, now_unix: i64) -> Option<Vec<u8>> {
        let key = CacheKey::of(req);
        let mut pages = self.pages();
        match pages.get(&key) {
            Some(cached) if now_unix < cached.until => Some(cached.page.clone()),
            Some(_) => {
                pages.remove(&key);
                None
            }
            None => None,
        }
    }

    /// Keeps `page`, the signed page made for `req`, until `until_unix`.
    pub fn put(&self, req: &PageRequest<'_>, page: Vec<u8>, until_unix: i64) {
        let key = CacheKey::of(req);
        let mut pages = self.pages();
        if !pages.contains_key(&key) && pages.len() >= MAX_CACHED_PAGES {
            let soonest = pages
                .iter()
                .min_by_key(|(_, cached)| cached.until)
                .map(|(key, _)| key.clone());
            if let Some(soonest) = soonest {
                pages.remove(&soonest);
            }
        }
        pages.insert(
            key,
            Cached {
                page,
                until: until_unix,
            },
        );
    }

    /// The pages kept.
    pub fn len(&self) -> usize {
        self.pages().len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages().is_empty()
    }

    /// A panic while the lock was held leaves a map, maybe without the page
    /// being put: taken as is.
    fn pages(&self) -> MutexGuard<'_, BTreeMap<CacheKey, Cached>> {
        self.pages.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for ViewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ViewCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewCache")
            .field("pages", &self.len())
            .finish()
    }
}
