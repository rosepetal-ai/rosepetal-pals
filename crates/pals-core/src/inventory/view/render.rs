//! What a scope shows of the inventory ([`render`]), and the hash its
//! revision follows ([`content_hash`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value};

use super::{Dimensions, INVENTORY_SECTIONS, OPERATOR, ViewEntry, to_value};
use crate::inventory::{
    ComponentLink, ComponentRole, DETAIL_MISSING_TOOLS, Entry, InventorySnapshot, McpConnection,
    McpTool, Observation, Scope, Section, Service,
};
use crate::jcs::canonical;

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
