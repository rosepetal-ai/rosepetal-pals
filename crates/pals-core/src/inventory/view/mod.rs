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
//! - **Benchmarks** ([`render_benchmarks`]): a reference to each benchmark
//!   report the scope sees, about what it shows ([`benchmark_subjects`]) or
//!   about a pair of nodes, with its kind of measurement as signed; and, for
//!   each service, model or hardware the scope shows without a report,
//!   `{"subject": …, "status": "no_measurement"}`, never a zero (§9.7). A
//!   page has them only when asked for: the inventory's sections are
//!   [`INVENTORY_SECTIONS`].
//! - **Revision**: a scope's view has a sequence, the `seq` of its pages,
//!   that moves only when what the scope shows changes
//!   ([`crate::store::Store::view_revision`] over [`content_hash`] of every
//!   section, benchmarks included). Hidden entries count in nothing a page
//!   carries: a public page has no totals, counts, hashes or references of
//!   what it leaves out.
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

mod cache;
mod page;
mod render;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use self::render::{NO_MEASUREMENT_PROVENANCE, NoMeasurementBody, ReportBody};
use super::{
    CARD_SKILL_ID, ComponentLink, MAX_STRING_BYTES, McpTool, Observation, Scope, Section,
    is_namespaced_id, is_service_id, is_tool_name,
};
use crate::benchmark::{is_benchmark_id, is_profile, is_profile_version};
use crate::card_jws::CardHash;
use crate::grant::parse_timestamp;
use crate::identity::AgentId;
use crate::signed::{Signed, SignedDoc, SignedError, check_validity};

pub use cache::ViewCache;
pub use page::{CursorKey, PageRequest, PageSlice, page, sign_page};
pub use render::{benchmark_subjects, content_hash, render, render_benchmarks};

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
/// The sections [`render`] shows, in the order it shows them, which a
/// request that names none gets. A scope's revision is the
/// [`content_hash`] of its view of all of them and of its benchmarks
/// ([`render_benchmarks`]), which come last.
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
///   sections;
/// - **body**: an object, never with `visibility`, `adapter` or
///   `offered_tools`; a service's without `id` or `description`, with
///   `provenance` `operator` and neither `version` nor `expires_at`, and
///   only links to entry ids; an MCP connection's tools named
///   `<its id>/<tool>`;
/// - **benchmarks**: as [`shows_as_a_benchmark`] says.
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
        Section::Benchmarks => shows_as_a_benchmark(entry),
        _ => is_namespaced_id(&entry.id),
    };
    shown && text && never && by_section
}

/// Whether `entry` of the `benchmarks` section is one a view makes
/// ([`render_benchmarks`]): never authorized nor observed, without
/// `version`, and either a reference to a report (a benchmark id, its
/// measurer as provenance, the report's `expires_at`, and a body of what it
/// measured, its kind, a known or namespaced profile, its version and when)
/// or a subject without
/// reports (named by [`crate::benchmark::Subject::key`], without
/// `expires_at`, with a body of the subject and `"status":
/// "no_measurement"` only).
fn shows_as_a_benchmark(entry: &ViewEntry) -> bool {
    let plain = !entry.dimensions.authorized
        && entry.dimensions.operational.is_none()
        && entry.version.is_none();
    let reference = || {
        ReportBody::deserialize(&entry.body).is_ok_and(|body| {
            is_benchmark_id(&entry.id)
                && entry.provenance.parse::<AgentId>().is_ok()
                && entry.expires_at.is_some()
                && body.subject.is_valid()
                && is_profile(&body.profile)
                && is_profile_version(&body.profile_version)
                && parse_timestamp(&body.measured_at).is_some()
        })
    };
    let unmeasured = || {
        NoMeasurementBody::deserialize(&entry.body).is_ok_and(|body| {
            body.subject.is_valid()
                && entry.id == body.subject.key()
                && entry.provenance == NO_MEASUREMENT_PROVENANCE
                && entry.expires_at.is_none()
        })
    };
    plain && (reference() || unmeasured())
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

fn to_value<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).expect("inventory values always serialize")
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
