//! Inventory views by scope: what each scope shows, signed pages, cursors,
//! per-scope revisions, the cache keyed by authorization context, and the
//! frozen vector.

use std::collections::BTreeMap;

use pals_core::card_jws::CardHash;
use pals_core::grant::{CLOCK_SKEW_S, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::view::{
    CursorKey, Dimensions, INVENTORY_SECTIONS, MAX_CACHED_PAGES, MAX_PAGE_BYTES, MAX_PAGE_ENTRIES,
    MAX_QUERY_TOTAL_BYTES, PageLimit, PageRequest, PageSlice, VIEW_SCHEMA, VIEW_SIG_DOMAIN,
    VIEW_TTL_S, ViewCache, ViewEntry, ViewError, ViewPage, content_hash, page, render, verify_page,
};
use pals_core::inventory::{
    ComponentLink, ComponentRole, Entry, Inventory, InventorySnapshot, Observation,
    ObservationSource, ObservedTool, OperationalStatus, Plugin, Scope, Section, Visibility,
};
use pals_core::jcs;
use pals_core::signed::{Signed, SignedDoc};
use pals_core::store::Store;
use serde_json::{Map, Value, json};

/// 2026-11-14T00:00:00Z.
const NOW: i64 = 1_794_614_400;
/// The hash of the frozen card vector, `card-jws-v1.json`.
const CARD_HASH: &str = "blake3:856acc066c7304adbae9c444204c10a0c2e3d113491fca62c1a06da74fedd4fe";

/// The key whose secret is 32 bytes of `byte`.
fn fixed_key(byte: u8) -> AgentKey {
    AgentKey::from_bytes(&[byte; 32])
}

fn ts(unix_s: i64) -> String {
    format_timestamp(unix_s).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn value_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn project(name: &str) -> Scope {
    Scope::Project(name.to_owned())
}

fn task(name: &str) -> Scope {
    Scope::Task(name.to_owned())
}

/// Every scope the fixtures can be seen in.
fn every_scope() -> Vec<Scope> {
    vec![
        Scope::Public,
        project("demo"),
        task("demo"),
        project("other"),
        task("other"),
    ]
}

/// An inventory with something of every visibility:
/// - public: the `ocr` service, the `example:ocr-model` model and the
///   `example:vision-pack` plugin;
/// - project `demo`: the `review` service and the `example:demo-repo` MCP
///   connection, which declares `read_file` and `delete_file` and offers only
///   `read_file`;
/// - task `demo`: the `deploy` service and the `example:demo-db` source;
/// - project `other`: the `example:other-repo` MCP connection;
/// - private: the `internal` service and the `example:secret-plugin`
///   plugin, installed but not offered.
///
/// `ocr` links to a public, a private and a project component.
const FIXTURE: &str = r#"
[[services]]
id = "ocr"
name = "OCR"
description = "Reads stamped serial numbers"
capabilities = ["vision.ocr"]
visibility = "public"
components = [
  { id = "example:ocr-model", role = "required" },
  { id = "example:secret-plugin", role = "optional" },
  { id = "example:demo-repo", role = "optional" },
]

[[services]]
id = "review"
name = "Review"
description = "Reviews the demo repository"
capabilities = ["code.rust.review"]
visibility = { project = ["demo"] }
components = [{ id = "example:demo-repo", role = "required" }]

[[services]]
id = "deploy"
name = "Deploy"
description = "Deploys the demo"
tags = ["deploy"]
visibility = { task = ["demo"] }

[[services]]
id = "internal"
name = "Internal"
description = "Never offered"
tags = ["internal"]

[[plugins]]
id = "example:vision-pack"
description = "Vision tools"
provenance = "operator"
visibility = "public"
components = ["ocr"]

[[plugins]]
id = "example:secret-plugin"
description = "Installed, never offered"
provenance = "operator"
runtime = "claude-code"

[[mcp]]
id = "example:demo-repo"
description = "Reads the demo repository"
provenance = "operator"
visibility = { project = ["demo"] }
server = "@modelcontextprotocol/server-filesystem"
tools = [
  { name = "read_file", description = "Reads a file", annotations = { readOnlyHint = true } },
  { name = "delete_file", description = "Deletes a file" },
]
offered_tools = ["read_file"]

[[mcp]]
id = "example:other-repo"
description = "Reads the other repository"
provenance = "operator"
visibility = { project = ["other"] }

[[models]]
id = "example:ocr-model"
description = "OCR model"
provenance = "operator"
visibility = "public"
model_id = "example-ocr"

[[information]]
id = "example:demo-db"
description = "The demo database"
provenance = "operator"
visibility = { task = ["demo"] }
live = true
"#;

fn snapshot_of(inventory: Inventory) -> InventorySnapshot {
    InventorySnapshot {
        inventory,
        observations: BTreeMap::new(),
        generation: 0,
    }
}

fn fixture() -> InventorySnapshot {
    snapshot_of(Inventory::from_toml(FIXTURE).unwrap())
}

fn ids(entries: &[ViewEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

fn text_of(entries: &[ViewEntry]) -> String {
    serde_json::to_string(entries).unwrap()
}

/// A plugin entry with `description`, of `visibility`.
fn plugin(id: &str, description: &str, visibility: Visibility) -> Entry<Plugin> {
    Entry {
        id: id.to_owned(),
        description: description.to_owned(),
        provenance: "operator".to_owned(),
        visibility,
        ..Default::default()
    }
}

/// An inventory of `count` public plugins whose descriptions are `bytes`
/// long.
fn plugins(count: usize, bytes: usize) -> InventorySnapshot {
    let mut inventory = Inventory::default();
    for i in 0..count {
        let description = format!("{i:05} {}", "x".repeat(bytes.saturating_sub(6)));
        inventory.plugins.push(plugin(
            &format!("example:plugin-{i:05}"),
            &description,
            Visibility::Public,
        ));
    }
    snapshot_of(inventory)
}

fn adapter_observation(tools: &[(&str, Option<&str>)]) -> Observation {
    Observation {
        source: ObservationSource::Adapter {
            method: "mcp tools/list".to_owned(),
        },
        status: OperationalStatus::Ok,
        observed_at: ts(NOW),
        expires_at: ts(NOW + 1800),
        detail: None,
        tools: tools
            .iter()
            .map(|&(name, description)| ObservedTool {
                name: name.to_owned(),
                description: description.map(str::to_owned),
                annotations: Some(json!({"title": format!("observed {name}")})),
            })
            .collect(),
    }
}

/// The agent that serves views in these tests: its key, its store, its
/// cursor key, and the hash of the card each page names.
struct Provider {
    key: AgentKey,
    store: Store,
    cursors: CursorKey,
    card: CardHash,
}

impl Provider {
    fn new() -> Self {
        let key = AgentKey::generate();
        let store = Store::open_in_memory(key.id()).unwrap();
        Self {
            key,
            store,
            cursors: CursorKey::random(),
            card: CARD_HASH.parse().unwrap(),
        }
    }

    fn id(&self) -> AgentId {
        self.key.id()
    }

    /// The revision of `scope`'s view of `snapshot` now: of every section.
    fn revision(&self, snapshot: &InventorySnapshot, scope: &Scope, now: i64) -> u64 {
        let all = render(snapshot, scope, &INVENTORY_SECTIONS);
        self.store
            .view_revision(&scope.key(), &content_hash(&all), now)
            .unwrap()
    }

    /// The signed page of `snapshot` a requester of `scope` gets at `now`,
    /// as an answer to the discovery protocol would build it.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fields of a page request, spelled out"
    )]
    fn serve(
        &self,
        snapshot: &InventorySnapshot,
        scope: &Scope,
        audience: Option<&AgentId>,
        sections: &[Section],
        cursor: Option<&str>,
        page_entries: Option<u32>,
        now: i64,
    ) -> Result<Vec<u8>, ViewError> {
        let revision = self.revision(snapshot, scope, now);
        let entries = render(snapshot, scope, sections);
        let req = PageRequest {
            scope,
            audience,
            sections,
            cursor,
            page_entries,
            revision,
        };
        self.sign(&entries, &req, now)
    }

    /// Pages `entries` for `req` and signs the page, issued at `now`.
    fn sign(
        &self,
        entries: &[ViewEntry],
        req: &PageRequest<'_>,
        now: i64,
    ) -> Result<Vec<u8>, ViewError> {
        let (entries, next_cursor, truncated_by) = page(entries, req, &self.cursors)?;
        let doc = ViewPage {
            schema: VIEW_SCHEMA.to_owned(),
            agent_id: self.id(),
            scope: req.scope.key(),
            audience: req.audience.copied(),
            seq: req.revision,
            issued_at: ts(now),
            expires_at: ts(now + VIEW_TTL_S),
            agent_card_hash: self.card.clone(),
            sections: req.sections.to_vec(),
            entries,
            next_cursor,
            truncated_by,
        };
        Ok(Signed::issue(&self.key, doc)
            .expect("a page within its limits signs")
            .to_json())
    }
}

/// The signed page in `bytes`, parsed without checking who it is for.
fn parsed(bytes: &[u8]) -> ViewPage {
    Signed::<ViewPage>::from_json(bytes).unwrap().doc
}

#[test]
fn public_view_never_shows_private_or_project_entries() {
    let snapshot = fixture();
    let public = render(&snapshot, &Scope::Public, &INVENTORY_SECTIONS);
    // Sections in their fixed order, entries in the file's order.
    assert_eq!(
        ids(&public),
        ["ocr", "example:vision-pack", "example:ocr-model"]
    );
    for entry in &public {
        assert_eq!(
            (
                entry.dimensions.installed,
                entry.dimensions.offered,
                entry.dimensions.authorized
            ),
            (true, true, false),
            "{}",
            entry.id
        );
        assert_eq!(entry.dimensions.operational, None);
    }
    // The public service keeps only its public component.
    assert_eq!(
        public[0].body["components"],
        json!([{"id": "example:ocr-model", "role": "required"}])
    );
    // A service's body is its block's fields, without its id, description
    // or visibility.
    assert_eq!(public[0].section, Section::Services);
    assert_eq!(public[0].description, "Reads stamped serial numbers");
    assert_eq!(public[0].provenance, "operator");
    for key in ["id", "description", "visibility"] {
        assert!(public[0].body.get(key).is_none(), "{key}");
    }
    assert_eq!(public[0].body["name"], json!("OCR"));

    let text = text_of(&public);
    for hidden in [
        "review",
        "deploy",
        "internal",
        "secret",
        "demo-repo",
        "other-repo",
        "demo-db",
        "visibility",
        "demo",
    ] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }

    // Sections not asked for are left out.
    let models = render(&snapshot, &Scope::Public, &[Section::Models]);
    assert_eq!(ids(&models), ["example:ocr-model"]);
    assert!(render(&snapshot, &Scope::Public, &[]).is_empty());
}

#[test]
fn plugin_installed_but_not_offered_is_absent_from_every_view() {
    // Spec §14: a plugin installed but not offered.
    let snapshot = fixture();
    for scope in every_scope() {
        for sections in [&INVENTORY_SECTIONS[..], &[Section::Plugins]] {
            let view = render(&snapshot, &scope, sections);
            let text = text_of(&view);
            assert!(!text.contains("secret-plugin"), "{scope:?}: {text}");
            assert!(!text.contains("Installed, never offered"), "{scope:?}");
            assert!(!text.contains("internal"), "{scope:?}");
            // Whatever shows is offered there.
            assert!(view.iter().all(|entry| entry.dimensions.offered));
        }
    }
}

#[test]
fn declared_but_not_offered_tool_is_absent_from_every_view() {
    let read_file = json!({
        "name": "example:demo-repo/read_file",
        "description": "Reads a file",
        "annotations": {"readOnlyHint": true},
    });

    // Without an adapter: the offered tools as declared.
    let snapshot = fixture();
    for scope in every_scope() {
        let view = render(&snapshot, &scope, &INVENTORY_SECTIONS);
        let text = text_of(&view);
        assert!(!text.contains("delete_file"), "{scope:?}: {text}");
        assert!(!text.contains("Deletes a file"), "{scope:?}");
        assert!(!text.contains("offered_tools"), "{scope:?}");
        if let Some(mcp) = view.iter().find(|e| e.id == "example:demo-repo") {
            assert_eq!(mcp.section, Section::Mcp);
            assert_eq!(mcp.body["tools"], json!([read_file]));
            assert_eq!(
                mcp.body["server"],
                json!("@modelcontextprotocol/server-filesystem")
            );
        }
    }

    // With an adapter: an offered tool shows only once observed, with what
    // was observed; a declared tool not offered never shows, even when the
    // server lists it; the adapter never shows.
    let mut inventory = Inventory::from_toml(&FIXTURE.replace(
        "offered_tools = [\"read_file\"]\n",
        "offered_tools = [\"read_file\", \"list_dir\"]\n\
         [mcp.adapter]\ntransport = \"stdio\"\ncommand = \"/opt/secret/fs-server\"\n\
         args = [\"--root\", \"/srv/private\"]\nenv_passthrough = [\"GITHUB_TOKEN\"]\n",
    ))
    .unwrap();
    inventory.mcp[0]
        .body
        .tools
        .push(pals_core::inventory::McpTool {
            name: "list_dir".to_owned(),
            description: Some("Lists a directory, as declared".to_owned()),
            annotations: None,
        });
    let mut snapshot = snapshot_of(inventory);

    // Not observed yet: no tool.
    let view = render(&snapshot, &project("demo"), &[Section::Mcp]);
    assert_eq!(ids(&view), ["example:demo-repo"]);
    assert!(view[0].body.get("tools").is_none(), "{}", view[0].body);
    assert_eq!(view[0].dimensions.operational, None);

    // Observed: `read_file` with the observed description and annotations,
    // `list_dir` not listed by the server, `delete_file` listed but never
    // offered.
    let observation = adapter_observation(&[
        ("read_file", Some("Reads a file, as observed")),
        ("delete_file", Some("Deletes a file, as observed")),
    ]);
    snapshot
        .observations
        .insert("example:demo-repo".to_owned(), observation.clone());
    for scope in [project("demo"), task("demo")] {
        let view = render(&snapshot, &scope, &[Section::Mcp]);
        assert_eq!(
            view[0].body["tools"],
            json!([{
                "name": "example:demo-repo/read_file",
                "description": "Reads a file, as observed",
                "annotations": {"title": "observed read_file"},
            }])
        );
        // The observation, without its tool list: the body has the tools.
        let operational = view[0].dimensions.operational.clone().unwrap();
        assert_eq!(
            operational,
            Observation {
                tools: Vec::new(),
                ..observation.clone()
            }
        );
        let text = text_of(&view);
        for never in [
            "delete_file",
            "list_dir",
            "\"adapter\":",
            "/opt/secret",
            "/srv/private",
            "GITHUB_TOKEN",
            "stdio",
            "offered_tools",
        ] {
            assert!(!text.contains(never), "{never} in {text}");
        }
    }

    // An observed tool without a description takes the declared one.
    snapshot.observations.insert(
        "example:demo-repo".to_owned(),
        Observation {
            tools: vec![ObservedTool {
                name: "read_file".to_owned(),
                description: None,
                annotations: None,
            }],
            ..observation
        },
    );
    let view = render(&snapshot, &project("demo"), &[Section::Mcp]);
    assert_eq!(view[0].body["tools"], json!([read_file]));
}

#[test]
fn project_view_shows_only_that_projects_entries() {
    let snapshot = fixture();
    let demo = render(&snapshot, &project("demo"), &INVENTORY_SECTIONS);
    assert_eq!(
        ids(&demo),
        [
            "ocr",
            "review",
            "example:vision-pack",
            "example:demo-repo",
            "example:ocr-model"
        ]
    );
    assert!(demo.iter().all(|entry| !entry.dimensions.authorized));
    // Its own project's component shows; the private one never does.
    assert_eq!(
        demo[0].body["components"],
        json!([
            {"id": "example:ocr-model", "role": "required"},
            {"id": "example:demo-repo", "role": "optional"},
        ])
    );
    let text = text_of(&demo);
    for hidden in ["deploy", "demo-db", "other-repo", "secret", "internal"] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }

    let other = render(&snapshot, &project("other"), &INVENTORY_SECTIONS);
    assert_eq!(
        ids(&other),
        [
            "ocr",
            "example:vision-pack",
            "example:other-repo",
            "example:ocr-model"
        ]
    );
    assert!(!text_of(&other).contains("demo"));
}

#[test]
fn task_view_adds_task_entries_and_marks_them_authorized() {
    let snapshot = fixture();
    let task_view = render(&snapshot, &task("demo"), &INVENTORY_SECTIONS);
    assert_eq!(
        ids(&task_view),
        [
            "ocr",
            "review",
            "deploy",
            "example:vision-pack",
            "example:demo-repo",
            "example:ocr-model",
            "example:demo-db"
        ]
    );
    // Authorized only in a task scope, for everything it shows.
    assert!(task_view.iter().all(|entry| entry.dimensions.authorized));
    for scope in [Scope::Public, project("demo")] {
        assert!(
            render(&snapshot, &scope, &INVENTORY_SECTIONS)
                .iter()
                .all(|entry| !entry.dimensions.authorized)
        );
    }
    let db = task_view
        .iter()
        .find(|e| e.id == "example:demo-db")
        .unwrap();
    assert_eq!(db.section, Section::Information);
    assert_eq!(db.body, json!({"live": true}));
    // The task of another project sees none of it.
    let other = render(&snapshot, &task("other"), &INVENTORY_SECTIONS);
    assert!(!text_of(&other).contains("demo"));
}

#[test]
fn public_seq_does_not_move_when_only_private_entries_change() {
    let provider = Provider::new();
    let before = fixture();
    let public = provider.revision(&before, &Scope::Public, NOW);
    let demo = provider.revision(&before, &project("demo"), NOW);
    let other = provider.revision(&before, &project("other"), NOW);
    assert_eq!((public, demo, other), (1, 1, 1));

    // Private entries come and go, a private one changes, and so does what
    // only another project sees: neither the public view nor demo's moves.
    let mut changed = before.inventory.clone();
    changed.plugins[1].description = "Still installed, still not offered".to_owned();
    changed.plugins.push(plugin(
        "example:another-secret",
        "Private",
        Visibility::Private,
    ));
    changed.mcp[1].description = "Reads the other repository, now faster".to_owned();
    changed.services.retain(|service| service.id != "internal");
    let changed = snapshot_of(changed);
    assert_eq!(provider.revision(&changed, &Scope::Public, NOW + 10), 1);
    assert_eq!(provider.revision(&changed, &project("demo"), NOW + 10), 1);
    // The other project's own view did change.
    assert_eq!(provider.revision(&changed, &project("other"), NOW + 10), 2);
    assert_eq!(provider.revision(&changed, &project("other"), NOW + 20), 2);

    // A public change moves every scope that shows it, once.
    let mut public_change = changed.inventory.clone();
    public_change.plugins[0].description = "Vision tools, renamed".to_owned();
    let public_change = snapshot_of(public_change);
    assert_eq!(
        provider.revision(&public_change, &Scope::Public, NOW + 30),
        2
    );
    assert_eq!(
        provider.revision(&public_change, &Scope::Public, NOW + 40),
        2
    );
    assert_eq!(
        provider.revision(&public_change, &project("demo"), NOW + 30),
        2
    );
    // Back to an earlier content is a change too: the seq never goes back.
    assert_eq!(provider.revision(&changed, &Scope::Public, NOW + 50), 3);
}

#[test]
fn public_view_carries_no_count_or_hash_of_hidden_entries() {
    // Two inventories that differ only in what the public scope does not
    // show: private entries, a public service's links to hidden ones, among
    // them alternatives to a public one, and entries of a project.
    let link = |id: &str, role, group: Option<&str>| ComponentLink {
        id: id.to_owned(),
        role,
        group: group.map(str::to_owned),
    };
    let mut small = Inventory::from_toml(FIXTURE).unwrap();
    small
        .plugins
        .retain(|p| p.visibility != Visibility::Private);
    small
        .services
        .retain(|s| s.visibility == Visibility::Public);
    small.mcp.clear();
    small.information.clear();
    small.services[0].components = vec![
        link("example:ocr-model", ComponentRole::Required, None),
        link(
            "example:vision-pack",
            ComponentRole::Alternative,
            Some("vision"),
        ),
    ];
    let small = snapshot_of(small);
    let mut large = fixture();
    large.inventory.services[0].components = vec![
        link("example:ocr-model", ComponentRole::Required, None),
        link(
            "example:vision-pack",
            ComponentRole::Alternative,
            Some("vision"),
        ),
        link(
            "example:secret-plugin",
            ComponentRole::Alternative,
            Some("vision"),
        ),
        link(
            "example:demo-repo",
            ComponentRole::Alternative,
            Some("vision"),
        ),
        link("example:other-repo", ComponentRole::Optional, None),
    ];
    // The one alternative of its group the scope sees shows as optional, in
    // no group: it reads the same whether hidden ones stand in for it or not.
    // Where the scope sees more than one, they keep their group.
    for snapshot in [&small, &large] {
        let public = render(snapshot, &Scope::Public, &[Section::Services]);
        assert_eq!(
            public[0].body["components"],
            json!([
                {"id": "example:ocr-model", "role": "required"},
                {"id": "example:vision-pack", "role": "optional"},
            ])
        );
    }
    let demo = render(&large, &project("demo"), &[Section::Services]);
    assert_eq!(
        demo[0].body["components"],
        json!([
            {"id": "example:ocr-model", "role": "required"},
            {"id": "example:vision-pack", "role": "alternative", "group": "vision"},
            {"id": "example:demo-repo", "role": "alternative", "group": "vision"},
        ])
    );
    for i in 0..50 {
        large.inventory.plugins.push(plugin(
            &format!("example:hidden-{i}"),
            "Hidden",
            Visibility::Private,
        ));
    }
    large
        .observations
        .insert("example:secret-plugin".to_owned(), adapter_observation(&[]));

    let provider = Provider::new();
    let page_of = |snapshot: &InventorySnapshot, cursor: Option<&str>, now: i64| {
        provider
            .serve(
                snapshot,
                &Scope::Public,
                None,
                &INVENTORY_SECTIONS,
                cursor,
                Some(2),
                now,
            )
            .unwrap()
    };
    let without = |bytes: &[u8]| {
        let mut value = value_of(bytes);
        let view = value["view"].as_object_mut().unwrap();
        for key in ["issued_at", "expires_at"] {
            assert!(view.remove(key).is_some(), "{key}");
        }
        value.as_object_mut().unwrap().remove("signature").unwrap();
        value
    };

    let first_small = page_of(&small, None, NOW);
    let first_large = page_of(&large, None, NOW + 100);
    assert_ne!(first_small, first_large);
    assert_eq!(without(&first_small), without(&first_large));
    // The cursor carries nothing of what is hidden either: the next pages
    // are the same too.
    let cursor = parsed(&first_small).next_cursor.unwrap();
    let second_small = page_of(&small, Some(&cursor), NOW + 200);
    let second_large = page_of(&large, Some(&cursor), NOW + 300);
    assert_eq!(without(&second_small), without(&second_large));
    let second = parsed(&second_large);
    assert_eq!((second.next_cursor, second.truncated_by), (None, None));

    // Nothing hidden is named either (the signature and the cursor's MAC,
    // random text, left aside).
    for bytes in [&first_large, &second_large] {
        let mut view = without(bytes)["view"].clone();
        view.as_object_mut().unwrap().remove("next_cursor");
        let text = view.to_string();
        for hidden in ["secret", "hidden", "demo", "other", "internal"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
    }
}

#[test]
fn page_ends_at_64_kib_or_100_entries_whichever_first() {
    let provider = Provider::new();
    let all = &INVENTORY_SECTIONS[..];

    // Small entries: 100 entries first.
    let small = plugins(150, 40);
    let bytes = provider
        .serve(&small, &Scope::Public, None, all, None, None, NOW)
        .unwrap();
    let first = parsed(&bytes);
    assert_eq!(first.entries.len(), MAX_PAGE_ENTRIES);
    assert_eq!(first.truncated_by, Some(PageLimit::PageEntries));
    let rest = provider
        .serve(
            &small,
            &Scope::Public,
            None,
            all,
            first.next_cursor.as_deref(),
            None,
            NOW,
        )
        .unwrap();
    let rest = parsed(&rest);
    assert_eq!(rest.entries.len(), 50);
    assert_eq!((rest.next_cursor, rest.truncated_by), (None, None));
    assert_eq!(rest.entries[0].id, "example:plugin-00100");

    // A smaller page asked for, and more than the most a page holds.
    for (asked, got) in [
        (Some(10), 10),
        (Some(1000), MAX_PAGE_ENTRIES),
        (Some(0), 100),
    ] {
        let bytes = provider
            .serve(&small, &Scope::Public, None, all, None, asked, NOW)
            .unwrap();
        let page = parsed(&bytes);
        assert_eq!(page.entries.len(), got, "{asked:?}");
        assert_eq!(page.truncated_by, Some(PageLimit::PageEntries));
    }

    // Large entries: 64 KiB first, as full as the next entry allows.
    let large = plugins(60, 3000);
    let entry_bytes =
        jcs::canonical(&serde_json::to_value(&render(&large, &Scope::Public, all)[0]).unwrap())
            .len();
    let bytes = provider
        .serve(&large, &Scope::Public, None, all, None, None, NOW)
        .unwrap();
    let first = parsed(&bytes);
    assert!(first.entries.len() < MAX_PAGE_ENTRIES);
    assert_eq!(first.truncated_by, Some(PageLimit::PageBytes));
    assert!(bytes.len() <= MAX_PAGE_BYTES, "{}", bytes.len());
    assert!(
        bytes.len() + entry_bytes > MAX_PAGE_BYTES - 1024,
        "{} bytes, entries of {entry_bytes}",
        bytes.len()
    );
    // Whichever comes first: 5 large entries end at 5.
    let bytes = provider
        .serve(&large, &Scope::Public, None, all, None, Some(5), NOW)
        .unwrap();
    assert_eq!(parsed(&bytes).entries.len(), 5);
    assert_eq!(parsed(&bytes).truncated_by, Some(PageLimit::PageEntries));

    // The pages cover every entry once, in order.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let bytes = provider
            .serve(
                &large,
                &Scope::Public,
                None,
                all,
                cursor.as_deref(),
                None,
                NOW,
            )
            .unwrap();
        assert!(bytes.len() <= MAX_PAGE_BYTES);
        let page = parsed(&bytes);
        seen.extend(page.entries.into_iter().map(|entry| entry.id));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let expected: Vec<String> = (0..60).map(|i| format!("example:plugin-{i:05}")).collect();
    assert_eq!(seen, expected);

    // An entry that cannot fit in a page on its own is never cut.
    let mut huge = Inventory::default();
    let mut entry = plugin("example:huge", "Huge", Visibility::Public);
    entry.body.components = (0..20)
        .map(|i| format!("{i:04}{}", "c".repeat(4000)))
        .collect();
    huge.plugins.push(entry);
    assert_eq!(
        provider.serve(
            &snapshot_of(huge),
            &Scope::Public,
            None,
            all,
            None,
            None,
            NOW
        ),
        Err(ViewError::TooLarge)
    );
}

#[test]
fn cursor_chain_stops_at_1_mib() {
    let provider = Provider::new();
    let all = &INVENTORY_SECTIONS[..];
    // About 1.8 MiB of public entries.
    let snapshot = plugins(600, 3000);

    let mut total = 0;
    let mut served = 0;
    let mut cursor = None;
    let last = loop {
        let bytes = provider
            .serve(
                &snapshot,
                &Scope::Public,
                None,
                all,
                cursor.as_deref(),
                None,
                NOW,
            )
            .unwrap();
        total += bytes.len();
        let page = parsed(&bytes);
        assert!(!page.entries.is_empty());
        served += page.entries.len();
        match &page.next_cursor {
            Some(next) => {
                assert_eq!(page.truncated_by, Some(PageLimit::PageBytes));
                cursor = Some(next.clone());
            }
            None => break page,
        }
    };
    assert_eq!(last.truncated_by, Some(PageLimit::QueryTotal));
    assert!(served < 600, "{served}");
    assert!(total <= MAX_QUERY_TOTAL_BYTES, "{total}");
    // It stopped because the next page would not fit, not before.
    assert!(
        total > MAX_QUERY_TOTAL_BYTES - 2 * MAX_PAGE_BYTES,
        "{total}"
    );
}

/// The cursor of the first page `audience` gets in `scope`, a page of one
/// entry of the fixture's view there.
fn first_cursor(provider: &Provider, scope: &Scope, audience: Option<&AgentId>) -> String {
    let bytes = provider
        .serve(
            &fixture(),
            scope,
            audience,
            &INVENTORY_SECTIONS,
            None,
            Some(1),
            NOW,
        )
        .unwrap();
    parsed(&bytes).next_cursor.expect("more than one entry")
}

/// The next page for `cursor` asked by `audience` in `scope`, `revision`
/// and `sections`.
fn follow(
    provider: &Provider,
    scope: &Scope,
    audience: Option<&AgentId>,
    sections: &[Section],
    cursor: &str,
    revision: u64,
) -> Result<PageSlice, ViewError> {
    let entries = render(&fixture(), scope, sections);
    let req = PageRequest {
        scope,
        audience,
        sections,
        cursor: Some(cursor),
        page_entries: Some(1),
        revision,
    };
    page(&entries, &req, &provider.cursors)
}

#[test]
fn cursor_of_one_audience_is_invalid_for_another() {
    // Spec §14: the cursor of one requester does not serve another.
    let provider = Provider::new();
    let alice = AgentKey::generate().id();
    let bob = AgentKey::generate().id();
    let scope = task("demo");
    let cursor = first_cursor(&provider, &scope, Some(&alice));
    let all = &INVENTORY_SECTIONS[..];

    let (entries, _, _) = follow(&provider, &scope, Some(&alice), all, &cursor, 1).unwrap();
    assert_eq!(ids(&entries), ["review"]);
    assert_eq!(
        follow(&provider, &scope, Some(&bob), all, &cursor, 1),
        Err(ViewError::InvalidCursor)
    );
    assert_eq!(
        follow(&provider, &scope, None, all, &cursor, 1),
        Err(ViewError::InvalidCursor)
    );
    // A public cursor does not serve a requester named either.
    let public = first_cursor(&provider, &Scope::Public, None);
    assert!(follow(&provider, &Scope::Public, None, all, &public, 1).is_ok());
    assert_eq!(
        follow(&provider, &Scope::Public, Some(&alice), all, &public, 1),
        Err(ViewError::InvalidCursor)
    );
}

#[test]
fn cursor_of_one_scope_or_revision_is_invalid() {
    let provider = Provider::new();
    let alice = AgentKey::generate().id();
    let all = &INVENTORY_SECTIONS[..];
    let cursor = first_cursor(&provider, &project("demo"), Some(&alice));
    assert!(follow(&provider, &project("demo"), Some(&alice), all, &cursor, 1).is_ok());

    for scope in [task("demo"), project("other"), Scope::Public] {
        assert_eq!(
            follow(&provider, &scope, Some(&alice), all, &cursor, 1),
            Err(ViewError::InvalidCursor),
            "{scope:?}"
        );
    }
    // Another revision of the same scope: the content changed.
    assert_eq!(
        follow(&provider, &project("demo"), Some(&alice), all, &cursor, 2),
        Err(ViewError::InvalidCursor)
    );
    // Other sections.
    assert_eq!(
        follow(
            &provider,
            &project("demo"),
            Some(&alice),
            &[Section::Services],
            &cursor,
            1
        ),
        Err(ViewError::InvalidCursor)
    );
}

#[test]
fn tampered_cursor_is_invalid() {
    let provider = Provider::new();
    let all = &INVENTORY_SECTIONS[..];
    let cursor = first_cursor(&provider, &Scope::Public, None);
    let (payload, mac) = cursor.split_once('.').unwrap();

    // The payload names its binding, readable but not writable.
    let decoded = base64_decode(payload);
    let mut claims = value_of(&decoded);
    assert_eq!(claims["scope"], json!("public"));
    assert_eq!(claims["offset"], json!(1));
    claims["offset"] = json!(2);
    let forged = format!("{}.{mac}", base64_encode(&jcs::canonical(&claims)));

    let mut flipped_mac = mac.to_owned().into_bytes();
    flipped_mac[0] = if flipped_mac[0] == b'A' { b'B' } else { b'A' };
    let flipped_mac = format!("{payload}.{}", String::from_utf8(flipped_mac).unwrap());

    for bad in [
        forged,
        flipped_mac,
        format!("{payload}."),
        format!(".{mac}"),
        payload.to_owned(),
        format!("{payload}.{mac}.{mac}"),
        format!("{payload}={mac}"),
        format!("{cursor}A"),
        String::new(),
        "x".repeat(64 * 1024),
        "not a cursor".to_owned(),
    ] {
        assert_eq!(
            follow(&provider, &Scope::Public, None, all, &bad, 1),
            Err(ViewError::InvalidCursor),
            "{bad:.80}"
        );
    }
    assert!(follow(&provider, &Scope::Public, None, all, &cursor, 1).is_ok());
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .unwrap()
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[test]
fn cursor_from_another_run_is_invalid() {
    let provider = Provider::new();
    let cursor = first_cursor(&provider, &Scope::Public, None);
    let restarted = Provider {
        cursors: CursorKey::random(),
        ..provider
    };
    assert_eq!(
        follow(
            &restarted,
            &Scope::Public,
            None,
            &INVENTORY_SECTIONS,
            &cursor,
            1
        ),
        Err(ViewError::InvalidCursor)
    );
}

#[test]
fn signed_page_verifies_and_names_its_audience() {
    let provider = Provider::new();
    let alice = AgentKey::generate().id();
    let bob = AgentKey::generate().id();
    let snapshot = fixture();
    let all = &INVENTORY_SECTIONS[..];

    // A private page names its requester, and verifies only for it.
    let bytes = provider
        .serve(&snapshot, &task("demo"), Some(&alice), all, None, None, NOW)
        .unwrap();
    let page = verify_page(&bytes, &provider.id(), Some(&alice), NOW).unwrap();
    assert_eq!(page.audience, Some(alice));
    assert_eq!(page.scope, "task:demo");
    assert_eq!(page.agent_id, provider.id());
    assert_eq!(page.seq, 1);
    assert_eq!(page.agent_card_hash.as_str(), CARD_HASH);
    assert_eq!(page.entries, render(&snapshot, &task("demo"), all));
    for other in [Some(&bob), None] {
        assert_eq!(
            verify_page(&bytes, &provider.id(), other, NOW),
            Err(ViewError::WrongAudience)
        );
    }

    // A public page names nobody and verifies for anybody.
    let bytes = provider
        .serve(&snapshot, &Scope::Public, None, all, None, None, NOW)
        .unwrap();
    for audience in [Some(&alice), Some(&bob), None] {
        let page = verify_page(&bytes, &provider.id(), audience, NOW).unwrap();
        assert_eq!((page.audience, page.scope.as_str()), (None, "public"));
    }

    // Signed by another agent, or altered.
    let stranger = AgentKey::generate().id();
    assert_eq!(
        verify_page(&bytes, &stranger, None, NOW),
        Err(ViewError::BadSignature)
    );
    let mut altered = value_of(&bytes);
    altered["view"]["seq"] = json!(7);
    assert_eq!(
        verify_page(&jcs::canonical(&altered), &provider.id(), None, NOW),
        Err(ViewError::BadSignature)
    );

    // Signed, but without an audience in a private scope, or with one in
    // the public scope: neither issued nor accepted.
    let mut doc = parsed(&bytes);
    doc.scope = "project:demo".to_owned();
    assert!(Signed::issue(&provider.key, doc.clone()).is_err());
    let forged = sign_raw(&provider.key, &serde_json::to_value(&doc).unwrap());
    assert_eq!(
        verify_page(&forged, &provider.id(), Some(&alice), NOW),
        Err(ViewError::Malformed)
    );
    let mut doc = parsed(&bytes);
    doc.audience = Some(alice);
    assert!(Signed::issue(&provider.key, doc.clone()).is_err());

    // Too large before anything else.
    assert_eq!(
        verify_page(&vec![b' '; MAX_PAGE_BYTES + 1], &provider.id(), None, NOW),
        Err(ViewError::TooLarge)
    );
    assert_eq!(
        verify_page(b"{}", &provider.id(), None, NOW),
        Err(ViewError::Malformed)
    );
}

/// `doc` signed by `key` under the view domain, whatever it says.
fn sign_raw(key: &AgentKey, doc: &Value) -> Vec<u8> {
    let mut envelope = Map::new();
    envelope.insert(ViewPage::WRAPPER.to_owned(), doc.clone());
    envelope.insert(
        "signature".to_owned(),
        json!(jcs::sign_domain(key, VIEW_SIG_DOMAIN, doc)),
    );
    jcs::canonical(&Value::Object(envelope))
}

#[test]
fn private_page_cached_for_one_requester_is_not_served_to_another() {
    let cache = ViewCache::new();
    let alice = AgentKey::generate().id();
    let bob = AgentKey::generate().id();
    let scope = task("demo");
    let all = &INVENTORY_SECTIONS[..];
    let req = |audience, cursor, page_entries, revision| PageRequest {
        scope: &scope,
        audience,
        sections: all,
        cursor,
        page_entries,
        revision,
    };

    cache.put(
        &req(Some(&alice), None, None, 1),
        b"alice".to_vec(),
        NOW + 100,
    );
    assert_eq!(
        cache.get(&req(Some(&alice), None, None, 1), NOW),
        Some(b"alice".to_vec())
    );
    // Another requester, another revision, cursor, page size, scope or
    // sections: not this page.
    assert_eq!(cache.get(&req(Some(&bob), None, None, 1), NOW), None);
    assert_eq!(cache.get(&req(None, None, None, 1), NOW), None);
    assert_eq!(cache.get(&req(Some(&alice), None, None, 2), NOW), None);
    assert_eq!(cache.get(&req(Some(&alice), Some("c"), None, 1), NOW), None);
    assert_eq!(cache.get(&req(Some(&alice), None, Some(5), 1), NOW), None);
    let demo = project("demo");
    let other_scope = PageRequest {
        scope: &demo,
        ..req(Some(&alice), None, None, 1)
    };
    assert_eq!(cache.get(&other_scope, NOW), None);
    let other_sections = PageRequest {
        sections: &[Section::Mcp],
        ..req(Some(&alice), None, None, 1)
    };
    assert_eq!(cache.get(&other_sections, NOW), None);
    // Bob's own page is his.
    cache.put(&req(Some(&bob), None, None, 1), b"bob".to_vec(), NOW + 100);
    assert_eq!(
        cache.get(&req(Some(&bob), None, None, 1), NOW),
        Some(b"bob".to_vec())
    );
    assert_eq!(
        cache.get(&req(Some(&alice), None, None, 1), NOW),
        Some(b"alice".to_vec())
    );
    // Served until its time, not after.
    assert_eq!(
        cache.get(&req(Some(&alice), None, None, 1), NOW + 100),
        None
    );

    // At most MAX_CACHED_PAGES, the soonest to go out first.
    for i in 0..(MAX_CACHED_PAGES as u64 + 50) {
        let revision = 10 + i;
        cache.put(
            &req(Some(&alice), None, None, revision),
            revision.to_string().into_bytes(),
            NOW + 1000 + revision as i64,
        );
        assert!(cache.len() <= MAX_CACHED_PAGES);
    }
    assert_eq!(cache.len(), MAX_CACHED_PAGES);
    let newest = 10 + MAX_CACHED_PAGES as u64 + 49;
    assert!(
        cache
            .get(&req(Some(&alice), None, None, newest), NOW)
            .is_some()
    );
    assert_eq!(cache.get(&req(Some(&alice), None, None, 10), NOW), None);
}

#[test]
fn expired_page_is_refused_beyond_skew() {
    let provider = Provider::new();
    let all = &INVENTORY_SECTIONS[..];
    let bytes = provider
        .serve(&fixture(), &Scope::Public, None, all, None, None, NOW)
        .unwrap();
    let id = provider.id();
    let expires = NOW + VIEW_TTL_S;
    assert!(verify_page(&bytes, &id, None, NOW).is_ok());
    assert!(verify_page(&bytes, &id, None, expires + CLOCK_SKEW_S - 1).is_ok());
    assert_eq!(
        verify_page(&bytes, &id, None, expires + CLOCK_SKEW_S + 1),
        Err(ViewError::Expired)
    );
    // A page from a clock ahead by more than the skew is refused as well.
    assert!(verify_page(&bytes, &id, None, NOW - CLOCK_SKEW_S + 1).is_ok());
    assert_eq!(
        verify_page(&bytes, &id, None, NOW - CLOCK_SKEW_S - 1),
        Err(ViewError::Expired)
    );

    // No page lives longer than VIEW_TTL_S.
    let mut doc = parsed(&bytes);
    doc.expires_at = ts(NOW + VIEW_TTL_S + 1);
    assert!(Signed::issue(&provider.key, doc.clone()).is_err());
    let forged = sign_raw(&provider.key, &serde_json::to_value(&doc).unwrap());
    assert_eq!(
        verify_page(&forged, &id, None, NOW),
        Err(ViewError::Malformed)
    );
}

#[test]
fn optional_fields_round_trip_present_and_absent() {
    // One spelling (ruling T2-a): every optional field is left out when
    // absent, never null, and reads back to the same bytes either way.
    let provider = Provider::new();
    let alice = AgentKey::generate().id();
    let entry = |version: Option<&str>, expires_at: Option<&str>, operational| ViewEntry {
        section: Section::Plugins,
        id: "example:plugin".to_owned(),
        description: "A plugin".to_owned(),
        provenance: "operator".to_owned(),
        version: version.map(str::to_owned),
        dimensions: Dimensions {
            installed: true,
            operational,
            offered: true,
            authorized: true,
        },
        body: json!({"runtime": "claude-code"}),
        expires_at: expires_at.map(str::to_owned),
    };
    let failed = Observation {
        status: OperationalStatus::Failed,
        detail: Some("unreachable".to_owned()),
        tools: Vec::new(),
        ..adapter_observation(&[])
    };
    let ok = adapter_observation(&[]);
    let entries = vec![
        entry(None, None, None),
        entry(Some("1.0.0"), Some("2026-12-14T00:00:00Z"), Some(failed)),
        entry(None, Some("2026-12-14T00:00:00Z"), Some(ok)),
    ];
    let doc = |scope: &str, audience, next_cursor: Option<&str>, truncated_by| ViewPage {
        schema: VIEW_SCHEMA.to_owned(),
        agent_id: provider.id(),
        scope: scope.to_owned(),
        audience,
        seq: 3,
        issued_at: ts(NOW),
        expires_at: ts(NOW + VIEW_TTL_S),
        agent_card_hash: provider.card.clone(),
        sections: vec![Section::Plugins],
        entries: entries.clone(),
        next_cursor: next_cursor.map(str::to_owned),
        truncated_by,
    };
    for page in [
        doc("public", None, None, None),
        doc(
            "task:demo",
            Some(alice),
            Some("abc.def"),
            Some(PageLimit::PageBytes),
        ),
        doc(
            "project:demo",
            Some(alice),
            Some("abc.def"),
            Some(PageLimit::PageEntries),
        ),
        doc("public", None, None, Some(PageLimit::QueryTotal)),
    ] {
        let signed = Signed::issue(&provider.key, page.clone()).unwrap();
        let bytes = signed.to_json();
        let back = Signed::<ViewPage>::from_json(&bytes).unwrap();
        assert_eq!(back, signed);
        assert_eq!(back.to_json(), bytes);
        let view = &value_of(&bytes)["view"];
        for (key, present) in [
            ("audience", page.audience.is_some()),
            ("next_cursor", page.next_cursor.is_some()),
            ("truncated_by", page.truncated_by.is_some()),
        ] {
            assert_eq!(view.get(key).is_some(), present, "{key}");
        }
        let entries = view["entries"].as_array().unwrap();
        assert!(entries[0].get("version").is_none());
        assert!(entries[0].get("expires_at").is_none());
        assert!(entries[0]["dimensions"].get("operational").is_none());
        assert_eq!(entries[1]["version"], json!("1.0.0"));
        assert_eq!(
            entries[1]["dimensions"]["operational"]["detail"],
            json!("unreachable")
        );
        assert!(
            entries[2]["dimensions"]["operational"]
                .get("detail")
                .is_none()
        );
        assert!(
            entries[2]["dimensions"]["operational"]
                .get("tools")
                .is_none()
        );
        assert!(!view.to_string().contains("null"), "{view}");
    }

    // A null where a field is absent is another spelling: refused.
    let mut spelled = serde_json::to_value(doc("public", None, None, None)).unwrap();
    spelled["entries"][0]["version"] = Value::Null;
    assert_eq!(
        verify_page(
            &sign_raw(&provider.key, &spelled),
            &provider.id(),
            None,
            NOW
        ),
        Err(ViewError::Malformed)
    );
    let mut spelled = serde_json::to_value(doc("public", None, None, None)).unwrap();
    spelled["audience"] = Value::Null;
    assert_eq!(
        verify_page(
            &sign_raw(&provider.key, &spelled),
            &provider.id(),
            None,
            NOW
        ),
        Err(ViewError::Malformed)
    );
    // A cursor without the limit that ended its page, or the reverse.
    for (next_cursor, truncated_by) in [
        (Some("abc.def"), None),
        (Some("abc.def"), Some(PageLimit::QueryTotal)),
        (None, Some(PageLimit::PageBytes)),
    ] {
        let page = doc("public", None, next_cursor, truncated_by);
        assert!(Signed::issue(&provider.key, page).is_err());
    }
}

#[test]
fn view_vector_matches() {
    let read = |name: &str| -> Value {
        let path = format!("{}/tests/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
    };
    let vector = read("view-v1.json");
    let key = fixed_key(0x01);
    let audience = fixed_key(0x02).id();
    assert_eq!(vector["signing_key_hex"], json!(hex(&[0x01; 32])));
    assert_eq!(vector["agent_id"], json!(key.id().to_string()));
    assert_eq!(vector["audience_key_hex"], json!(hex(&[0x02; 32])));
    assert_eq!(vector["view"]["audience"], json!(audience.to_string()));
    assert_eq!(
        vector["canonical_hex"],
        json!(hex(&jcs::canonical(&vector["view"])))
    );
    assert_eq!(
        vector["signed"],
        json!({"view": vector["view"], "signature": vector["signature"]})
    );
    assert_eq!(
        vector["view"]["agent_card_hash"],
        read("card-jws-v1.json")["hash"]
    );

    // The inputs: the inventory vector, the observation of its MCP
    // connection, the scope and the sections.
    let inventory: Inventory =
        Inventory::from_toml(read("inventory-v1.json")["toml"].as_str().unwrap()).unwrap();
    let observations: BTreeMap<String, Observation> =
        serde_json::from_value(vector["observations"].clone()).unwrap();
    let snapshot = InventorySnapshot {
        inventory,
        observations,
        generation: 1,
    };
    let scope = Scope::Task("demo".to_owned());
    assert_eq!(vector["view"]["scope"], json!(scope.key()));
    let entries = render(&snapshot, &scope, &INVENTORY_SECTIONS);
    let page = ViewPage {
        schema: VIEW_SCHEMA.to_owned(),
        agent_id: key.id(),
        scope: scope.key(),
        audience: Some(audience),
        seq: 1,
        issued_at: "2026-11-14T00:00:00Z".to_owned(),
        expires_at: "2026-11-14T00:05:00Z".to_owned(),
        agent_card_hash: CARD_HASH.parse().unwrap(),
        sections: INVENTORY_SECTIONS.to_vec(),
        entries,
        next_cursor: None,
        truncated_by: None,
    };
    assert_eq!(serde_json::to_value(&page).unwrap(), vector["view"]);

    // What the vector shows: the adapter, the tool not offered and the
    // private model never; the private alternative's link dropped, and the
    // other alternative of its group shown as optional.
    let text = vector["view"].to_string();
    for never in [
        "\"adapter\":",
        "delete_file",
        "npx",
        "GITHUB_TOKEN",
        "offered_tools",
        "example:assistant",
        "reviewer",
        "visibility",
    ] {
        assert!(!text.contains(never), "{never}");
    }
    assert!(text.contains("example:repo-reader/read_file"));

    // Recomputed byte for byte, and the frozen bytes accepted as they are.
    let issued = Signed::issue(&key, page).unwrap();
    assert_eq!(json!(issued.signature), vector["signature"]);
    let frozen = jcs::canonical(&vector["signed"]);
    assert_eq!(issued.to_json(), frozen);
    assert_eq!(
        verify_page(&frozen, &key.id(), Some(&audience), NOW).unwrap(),
        issued.doc
    );
}
