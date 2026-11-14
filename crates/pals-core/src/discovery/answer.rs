//! Answering a request of `agentnet-discovery/0`, with nothing but what a
//! [`DiscoveryContext`] holds: no worker, no adapter, no socket.
//!
//! In order, the first refusal winning:
//!
//! 1. **Version.** Another than [`DISCOVERY_VERSION`] is
//!    `UNSUPPORTED_VERSION`, with the versions supported.
//! 2. **Grant.** When the request presents one, it is accepted as the HTTP
//!    layer accepts it ([`accept_grant`] with [`Need::AnyOperation`]): for
//!    this agent, to the peer of the connection, from an accepted issuer,
//!    not revoked, in force. One that allows no task operation is no error:
//!    it only gives the public scope. One that is refused is
//!    `UNAUTHENTICATED`, with the reason ([`GrantError::reason`]); so is one
//!    whose project is not a project name, which no signed page can name.
//! 3. **Scope.** The grant's ([`Scope::for_grant`]); without a grant, the
//!    public one, or an empty public view when the agent shows none: a view
//!    of revision 1 whose pages are never cached, so that it and the public
//!    view a grant gives never stand for each other.
//! 4. **Operation.** `GetInventory` is a signed page of the scope's view;
//!    `GetBenchmarkReport` and `ServiceQuery` are `UNSUPPORTED_OPERATION`
//!    in this version of the daemon; none is `BAD_REQUEST`.
//!
//! Every response carries the agent's limits and its signed records in
//! force. An error message is a fixed text of at most
//! [`super::MAX_ERROR_MESSAGE_BYTES`]: it never quotes the request, and it
//! never says whether something the requester may not see exists. A
//! response larger than the smaller of the requester's
//! `max_response_bytes` and [`MAX_RESPONSE_FRAME_BYTES`] is replaced by a
//! `TOO_LARGE` error, which is sent whatever its size.

use serde::Deserialize;
use serde::de::IntoDeserializer;
use serde::de::value::{Error as NotASection, StrDeserializer};

use super::wire::{self, ErrorCode, request, response};
use super::{DISCOVERY_VERSION, DiscoveryContext, MAX_RESPONSE_FRAME_BYTES, PUBLIC_CARD};
use crate::grant::{AcceptError, Acceptance, GrantError, Need, accept_grant};
use crate::identity::AgentId;
use crate::inventory::view::{
    INVENTORY_SECTIONS, PageRequest, VIEW_TTL_S, ViewError, content_hash, page, render, sign_page,
};
use crate::inventory::{Scope, Section};
use prost::Message;

/// How long a signed page is served again from the cache: half its life, so
/// that a page served always has half its life left.
const PAGE_CACHE_S: i64 = VIEW_TTL_S / 2;
/// The most section names a request lists, repeats included.
const MAX_SECTION_NAMES: usize = 32;
/// The revision of the empty view a requester without a grant gets when the
/// agent shows no public view: its content never changes.
const EMPTY_VIEW_REVISION: u64 = 1;

/// The response to `req`, made by `peer`, the agent its connection
/// authenticated, at `now_unix`.
pub fn answer(
    ctx: &DiscoveryContext,
    peer: &AgentId,
    req: wire::Request,
    now_unix: i64,
) -> wire::Response {
    let limit = match usize::try_from(req.max_response_bytes) {
        Ok(asked) if asked > 0 => asked.min(MAX_RESPONSE_FRAME_BYTES),
        _ => MAX_RESPONSE_FRAME_BYTES,
    };
    let result = match outcome(ctx, peer, req, now_unix) {
        Ok(result) => result,
        Err(error) => response::Result::Error(error),
    };
    let response = envelope(ctx, result);
    if response.encoded_len() <= limit {
        response
    } else {
        error_response(
            ctx,
            wire::Error::new(
                ErrorCode::TooLarge,
                "response larger than max_response_bytes",
            ),
        )
    }
}

/// A response that refuses with `error`, with the agent's limits and
/// records, as every response has them.
pub fn error_response(ctx: &DiscoveryContext, error: wire::Error) -> wire::Response {
    envelope(ctx, response::Result::Error(error))
}

fn envelope(ctx: &DiscoveryContext, result: response::Result) -> wire::Response {
    let (contact_record, announce_record) = (ctx.records)();
    wire::Response {
        version: DISCOVERY_VERSION,
        limits: Some(super::server_limits()),
        contact_record,
        announce_record,
        result: Some(result),
    }
}

/// What `req` gets, or why not.
fn outcome(
    ctx: &DiscoveryContext,
    peer: &AgentId,
    req: wire::Request,
    now_unix: i64,
) -> Result<response::Result, wire::Error> {
    if req.version != DISCOVERY_VERSION {
        let mut error = wire::Error::new(ErrorCode::UnsupportedVersion, "unsupported version");
        error.supported_versions = vec![DISCOVERY_VERSION];
        return Err(error);
    }
    let viewer = viewer(ctx, peer, &req.grant_token, now_unix)?;
    match req.op {
        Some(request::Op::GetInventory(get)) => {
            get_inventory(ctx, peer, &viewer, get, now_unix).map(response::Result::InventoryPage)
        }
        Some(request::Op::GetBenchmarkReport(_) | request::Op::ServiceQuery(_)) => Err(
            wire::Error::new(ErrorCode::UnsupportedOperation, "unsupported operation"),
        ),
        None => Err(bad_request("no operation")),
    }
}

/// Who asks, as the agent sees it: the scope its grant gives, and whether
/// its view is the empty one of a requester without a grant when the agent
/// shows no public view.
struct Viewer {
    scope: Scope,
    empty: bool,
}

fn viewer(
    ctx: &DiscoveryContext,
    peer: &AgentId,
    token: &str,
    now_unix: i64,
) -> Result<Viewer, wire::Error> {
    if token.is_empty() {
        return Ok(Viewer {
            scope: Scope::Public,
            empty: !ctx.public,
        });
    }
    let audience = ctx.key.id();
    let acceptance = Acceptance {
        audience: &audience,
        subject: peer,
        accepted_issuers: &ctx.accepted_issuers,
        need: Need::AnyOperation,
        now_unix,
    };
    let grant = accept_grant(&ctx.store, token, &acceptance).map_err(|err| match err {
        AcceptError::Grant(err) => unauthenticated(err),
        AcceptError::Store => internal(),
    })?;
    let scope = Scope::for_grant(Some(&grant.grant));
    // A project no page can name: the grant is no use here.
    if scope != Scope::Public && Scope::from_key(&scope.key()).is_none() {
        return Err(unauthenticated(GrantError::Malformed));
    }
    Ok(Viewer {
        scope,
        empty: false,
    })
}

/// The signed page `get` asks for of `viewer`'s view.
fn get_inventory(
    ctx: &DiscoveryContext,
    peer: &AgentId,
    viewer: &Viewer,
    get: wire::GetInventory,
    now_unix: i64,
) -> Result<Vec<u8>, wire::Error> {
    let sections = sections(&get.sections)?;
    let scope = &viewer.scope;
    // The empty view never changes, and shares the public scope's key with
    // the public view of a grant that gives no more: it keeps a revision of
    // its own and stays out of the cache, so that neither ever stands for
    // the other.
    let (all, revision) = if viewer.empty {
        (Vec::new(), EMPTY_VIEW_REVISION)
    } else {
        // The revision follows every section the scope shows, whatever is
        // asked.
        let all = render(&(ctx.inventory)(), scope, &INVENTORY_SECTIONS);
        let revision = ctx
            .store
            .view_revision(&scope.key(), &content_hash(&all), now_unix)
            .map_err(|err| {
                tracing::warn!(error = %format!("{err:#}"), "cannot read a view's revision");
                internal()
            })?;
        (all, revision)
    };
    let req = PageRequest {
        scope,
        audience: (*scope != Scope::Public).then_some(peer),
        sections: &sections,
        cursor: (!get.cursor.is_empty()).then_some(get.cursor.as_str()),
        page_entries: (get.page_entries > 0).then_some(get.page_entries),
        revision,
    };
    if let Some(page) = ctx.views.get(&req, now_unix).filter(|_| !viewer.empty) {
        return Ok(page);
    }
    // An entry renders the same whichever sections are asked for.
    let entries: Vec<_> = all
        .into_iter()
        .filter(|entry| sections.contains(&entry.section))
        .collect();
    let slice = page(&entries, &req, &ctx.cursor_key).map_err(view_refusal)?;
    let card_hash = ctx
        .card_hashes
        .get(&scope.key())
        .or_else(|| ctx.card_hashes.get(PUBLIC_CARD))
        .ok_or_else(|| {
            tracing::error!("the discovery context holds no public card hash");
            internal()
        })?;
    let signed = sign_page(&ctx.key, card_hash, &req, slice, now_unix).map_err(view_refusal)?;
    let bytes = signed.to_json();
    if !viewer.empty {
        ctx.views.put(&req, bytes.clone(), now_unix + PAGE_CACHE_S);
    }
    Ok(bytes)
}

/// The inventory sections `names` lists, in order and without repeats; all
/// when it lists none.
fn sections(names: &[String]) -> Result<Vec<Section>, wire::Error> {
    if names.is_empty() {
        return Ok(INVENTORY_SECTIONS.to_vec());
    }
    if names.len() > MAX_SECTION_NAMES {
        return Err(bad_request("too many sections"));
    }
    let mut sections = names
        .iter()
        .map(|name| {
            let name: StrDeserializer<'_, NotASection> = name.as_str().into_deserializer();
            Section::deserialize(name)
                .ok()
                .filter(|section: &Section| INVENTORY_SECTIONS.contains(section))
                .ok_or_else(|| bad_request("unknown section"))
        })
        .collect::<Result<Vec<Section>, wire::Error>>()?;
    sections.sort_unstable();
    sections.dedup();
    Ok(sections)
}

/// The refusal of a page that cannot be made.
fn view_refusal(err: ViewError) -> wire::Error {
    match err {
        ViewError::InvalidCursor => wire::Error::new(ErrorCode::InvalidCursor, "invalid cursor"),
        ViewError::TooLarge => {
            wire::Error::new(ErrorCode::TooLarge, "an entry does not fit in a page")
        }
        other => {
            tracing::warn!(error = %other, "cannot sign a page of a view");
            internal()
        }
    }
}

fn bad_request(message: &str) -> wire::Error {
    wire::Error::new(ErrorCode::BadRequest, message)
}

fn unauthenticated(err: GrantError) -> wire::Error {
    wire::Error::new(ErrorCode::Unauthenticated, err.reason())
}

fn internal() -> wire::Error {
    wire::Error::new(ErrorCode::Internal, "internal error")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::discovery::{Load, PUBLIC_CARD};
    use crate::grant::{GRANT_SCHEMA, Grant, Operation, SignedGrant, format_timestamp};
    use crate::identity::AgentKey;
    use crate::inventory::view::{CursorKey, ViewCache, ViewPage, verify_page};
    use crate::inventory::{Inventory, InventorySnapshot};
    use crate::store::Store;

    /// 2026-11-14T00:00:00Z.
    const NOW: i64 = 1_794_614_400;

    /// The context of an agent with one public plugin that shows no public
    /// view to requesters without a grant.
    fn hidden(key: &Arc<AgentKey>) -> DiscoveryContext {
        let inventory = Inventory::from_toml(
            "[[plugins]]\nid = \"example:public-tool\"\ndescription = \"Public\"\n\
             provenance = \"operator\"\nvisibility = \"public\"\n",
        )
        .unwrap();
        let snapshot = Arc::new(InventorySnapshot {
            inventory,
            observations: BTreeMap::new(),
            generation: 0,
        });
        let hash = format!("blake3:{}", "0".repeat(64)).parse().unwrap();
        DiscoveryContext {
            key: Arc::clone(key),
            store: Arc::new(Store::open_in_memory(key.id()).unwrap()),
            inventory: Arc::new(move || Arc::clone(&snapshot)),
            card_hashes: BTreeMap::from([(PUBLIC_CARD.to_owned(), hash)]),
            records: Arc::new(|| (b"contact".to_vec(), b"announce".to_vec())),
            accepted_issuers: vec![key.id()],
            public: false,
            cursor_key: CursorKey::random(),
            views: ViewCache::new(),
            load: Arc::new(Load::unbounded()),
        }
    }

    fn get(token: &str) -> wire::Request {
        wire::Request {
            version: DISCOVERY_VERSION,
            max_response_bytes: 0,
            grant_token: token.to_owned(),
            op: Some(request::Op::GetInventory(wire::GetInventory::default())),
        }
    }

    fn page_of(ctx: &DiscoveryContext, peer: &AgentId, req: wire::Request) -> ViewPage {
        let response = answer(ctx, peer, req, NOW);
        let Some(response::Result::InventoryPage(page)) = response.result else {
            panic!("a page, not {:?}", response.result);
        };
        verify_page(&page, &ctx.key.id(), Some(peer), NOW).unwrap()
    }

    #[test]
    fn a_hidden_public_view_never_stands_for_the_public_view_a_grant_gives() {
        let key = Arc::new(AgentKey::generate());
        let ctx = hidden(&key);
        let peer = AgentKey::generate().id();
        // A grant to fetch blobs: valid, but no task operation, so the
        // public scope.
        let blobs = SignedGrant::issue(
            &key,
            Grant {
                schema: GRANT_SCHEMA.to_owned(),
                grant_id: "g-blobs".to_owned(),
                issuer: key.id(),
                subject: peer,
                audience: key.id(),
                project: "demo".to_owned(),
                resources: vec!["skill:ask".to_owned()],
                operations: vec![Operation::BlobFetch],
                issued_at: format_timestamp(NOW).unwrap(),
                expires_at: format_timestamp(NOW + 3600).unwrap(),
            },
        )
        .unwrap()
        .to_token();

        for _ in 0..2 {
            // Without a grant: the public scope, showing nothing.
            let empty = page_of(&ctx, &peer, get(""));
            assert_eq!((empty.scope.as_str(), empty.seq), ("public", 1));
            assert!(empty.entries.is_empty());
            // With the grant: the public view, whose revision does not move
            // for the empty one's sake, and which the empty one never gets.
            let shown = page_of(&ctx, &peer, get(&blobs));
            assert_eq!((shown.scope.as_str(), shown.seq), ("public", 1));
            assert_eq!(shown.entries.len(), 1);
        }
        assert_eq!(ctx.views.len(), 1, "only the public view's page is kept");
    }

    #[test]
    fn a_grant_whose_project_is_no_project_name_is_unauthenticated() {
        let key = Arc::new(AgentKey::generate());
        let ctx = hidden(&key);
        let peer = AgentKey::generate().id();
        let grant_for = |project: &str| {
            SignedGrant::issue(
                &key,
                Grant {
                    schema: GRANT_SCHEMA.to_owned(),
                    grant_id: format!("g-{}", project.len()),
                    issuer: key.id(),
                    subject: peer,
                    audience: key.id(),
                    project: project.to_owned(),
                    resources: vec!["skill:ask".to_owned()],
                    operations: vec![Operation::TaskSend],
                    issued_at: format_timestamp(NOW).unwrap(),
                    expires_at: format_timestamp(NOW + 3600).unwrap(),
                },
            )
            .unwrap()
            .to_token()
        };
        // A valid grant, whose project no signed page or offer could name.
        for project in ["two words", "diseño", ""] {
            let response = answer(&ctx, &peer, get(&grant_for(project)), NOW);
            let Some(response::Result::Error(error)) = response.result else {
                panic!("{project:?}: an error, not {:?}", response.result);
            };
            assert_eq!(error.code(), ErrorCode::Unauthenticated, "{project:?}");
            assert_eq!(error.message, "malformed", "{project:?}");
        }
        // A project name is served.
        let page = page_of(&ctx, &peer, get(&grant_for("demo")));
        assert_eq!(page.scope, "task:demo");
    }

    #[test]
    fn every_response_carries_the_records_in_force_and_the_limits() {
        let key = Arc::new(AgentKey::generate());
        let ctx = hidden(&key);
        let peer = AgentKey::generate().id();
        let mut unknown = get("");
        unknown.version = 9;
        for req in [
            get(""),
            get("not a token"),
            unknown,
            wire::Request::default(),
        ] {
            let response = answer(&ctx, &peer, req, NOW);
            assert_eq!(response.version, DISCOVERY_VERSION);
            assert_eq!(response.limits, Some(crate::discovery::server_limits()));
            assert_eq!(response.contact_record, b"contact");
            assert_eq!(response.announce_record, b"announce");
        }
        // A response larger than the requester takes is refused as such.
        let mut small = get("");
        small.max_response_bytes = 64;
        let response = answer(&ctx, &peer, small, NOW);
        let Some(response::Result::Error(error)) = response.result else {
            panic!("an error, not {:?}", response.result);
        };
        assert_eq!(error.code(), ErrorCode::TooLarge);
    }
}
