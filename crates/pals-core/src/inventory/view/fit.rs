//! An MCP connection's observation, cut to fit a page
//! ([`fit_observation`]): a page holds at least one entry whole, so an
//! entry that would not fit alone in a page of a scope that shows it would
//! stop that scope's chain of cursors there, as [`ViewError::TooLarge`]
//! (`super::ViewError`), for every requester.

use std::collections::BTreeMap;

use super::page::entry_budget;
use super::render::render;
use super::to_value;
use crate::inventory::{
    DETAIL_TOO_MANY_TOOLS, Entry, Inventory, InventorySnapshot, MAX_OBSERVED_DESCRIPTION_BYTES,
    McpConnection, Observation, OperationalStatus, Scope, Section,
};
use crate::jcs::canonical;

/// `observation` of `entry`, such that the entry, as each of `scopes` that
/// shows it renders it, fits alone in a page of that scope
/// ([`entry_budget`]):
///
/// - as it is, when it fits;
/// - else with the description of every tool it keeps, and the `title` of
///   its annotations, cut to one same cap, the longest one that fits,
///   shorter than [`MAX_OBSERVED_DESCRIPTION_BYTES`] and on a character
///   boundary; a text cut to nothing is left out;
/// - else, when even without those texts it does not fit, `Failed` with
///   [`DETAIL_TOO_MANY_TOOLS`] and no tools.
///
/// What the operator declared of the entry is never cut: only what the
/// adapter observed.
pub fn fit_observation(
    entry: &Entry<McpConnection>,
    observation: Observation,
    scopes: &[Scope],
) -> Observation {
    let scopes: Vec<&Scope> = scopes
        .iter()
        .filter(|scope| entry.visibility.visible_in(scope))
        .collect();
    let fits = |observed: &Observation| {
        scopes
            .iter()
            .all(|scope| rendered_bytes(entry, observed, scope) <= entry_budget(scope))
    };
    if fits(&observation) {
        return observation;
    }
    let cut = |cap: usize| cut_texts(&observation, cap);
    if !fits(&cut(0)) {
        return Observation {
            status: OperationalStatus::Failed,
            detail: Some(DETAIL_TOO_MANY_TOOLS.to_owned()),
            tools: Vec::new(),
            ..observation
        };
    }
    // `fits(cut(fitting))` and not `fits(cut(too_long))`, since the cap
    // `MAX_OBSERVED_DESCRIPTION_BYTES` cuts nothing a view may carry.
    let (mut fitting, mut too_long) = (0, MAX_OBSERVED_DESCRIPTION_BYTES);
    while too_long - fitting > 1 {
        let cap = fitting + (too_long - fitting) / 2;
        if fits(&cut(cap)) {
            fitting = cap;
        } else {
            too_long = cap;
        }
    }
    cut(fitting)
}

/// The bytes of the JCS of `entry` with `observation`, as `scope` renders
/// it.
fn rendered_bytes(entry: &Entry<McpConnection>, observation: &Observation, scope: &Scope) -> usize {
    let snapshot = InventorySnapshot {
        inventory: Inventory {
            mcp: vec![entry.clone()],
            ..Inventory::default()
        },
        observations: BTreeMap::from([(entry.id.clone(), observation.clone())]),
        generation: 0,
    };
    render(&snapshot, scope, &[Section::Mcp])
        .first()
        .map_or(0, |rendered| canonical(&to_value(rendered)).len())
}

/// `observation` with every tool's texts cut to `cap`
/// ([`ObservedTool::cut_texts`](crate::inventory::ObservedTool::cut_texts)).
fn cut_texts(observation: &Observation, cap: usize) -> Observation {
    let mut cut = observation.clone();
    for tool in &mut cut.tools {
        tool.cut_texts(cap);
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::format_timestamp;
    use crate::inventory::view::MAX_PAGE_BYTES;
    use crate::inventory::{
        AdapterMethod, McpAdapterCfg, McpTransport, ObservationSource, ObservedTool, Visibility,
    };

    const ID: &str = "example:repo-reader";

    /// A connection offering `tools`, shown as `visibility` says, with an
    /// adapter: only the tools it observed show.
    fn connection(tools: &[String], visibility: Visibility) -> Entry<McpConnection> {
        Entry {
            id: ID.to_owned(),
            description: "Reads the project repository".to_owned(),
            provenance: "operator".to_owned(),
            visibility,
            body: McpConnection {
                offered_tools: tools.to_vec(),
                adapter: Some(McpAdapterCfg {
                    transport: McpTransport::Stdio {
                        command: "mcp-server".to_owned(),
                        args: Vec::new(),
                        env_passthrough: Vec::new(),
                    },
                }),
                ..McpConnection::default()
            },
            ..Entry::default()
        }
    }

    /// `tools` observed, each described by `description` and titled by
    /// `title`.
    fn observed(tools: &[String], description: &str, title: &str) -> Observation {
        Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status: OperationalStatus::Ok,
            observed_at: format_timestamp(1_000_000).unwrap(),
            expires_at: format_timestamp(1_001_800).unwrap(),
            detail: None,
            tools: tools
                .iter()
                .map(|name| ObservedTool {
                    name: name.clone(),
                    description: Some(description.to_owned()),
                    annotations: Some(serde_json::json!({"title": title, "readOnlyHint": true})),
                })
                .collect(),
        }
    }

    fn names(count: usize, len: usize) -> Vec<String> {
        (0..count).map(|n| format!("{n:0len$}")).collect()
    }

    fn scopes() -> Vec<Scope> {
        vec![
            Scope::Public,
            Scope::Project("demo".to_owned()),
            Scope::Task("demo".to_owned()),
        ]
    }

    #[test]
    fn an_observation_that_fits_is_kept_as_it_is() {
        let tools = names(3, 8);
        let entry = connection(&tools, Visibility::Public);
        let observation = observed(&tools, "Reads a file", "Read");
        assert_eq!(
            fit_observation(&entry, observation.clone(), &scopes()),
            observation
        );
    }

    #[test]
    fn long_texts_are_cut_to_one_cap_until_the_entry_fits() {
        // 120 tools of 1 KiB descriptions and titles, in characters of two
        // bytes: 240 KiB, far more than a page.
        let tools = names(120, 16);
        let entry = connection(&tools, Visibility::Project(vec!["demo".to_owned()]));
        let text = "é".repeat(MAX_OBSERVED_DESCRIPTION_BYTES / 2);
        let fitted = fit_observation(&entry, observed(&tools, &text, &text), &scopes());

        assert_eq!(fitted.status, OperationalStatus::Ok);
        assert_eq!(fitted.tools.len(), 120);
        let descriptions: Vec<&str> = fitted
            .tools
            .iter()
            .map(|tool| tool.description.as_deref().unwrap())
            .collect();
        let cap = descriptions[0].len();
        assert!(0 < cap && cap < text.len(), "cut to {cap}");
        assert!(descriptions.iter().all(|d| *d == descriptions[0]));
        assert!(descriptions[0].chars().all(|c| c == 'é'));
        for tool in &fitted.tools {
            assert_eq!(tool.annotations.as_ref().unwrap()["title"], descriptions[0]);
            assert_eq!(tool.annotations.as_ref().unwrap()["readOnlyHint"], true);
        }
        // It fits in every scope that shows it, and one character more
        // would not.
        for scope in &scopes()[1..] {
            assert!(rendered_bytes(&entry, &fitted, scope) <= entry_budget(scope));
        }
        let longer = cut_texts(&observed(&tools, &text, &text), cap + 2);
        let scope = &scopes()[1];
        assert!(rendered_bytes(&entry, &longer, scope) > entry_budget(scope));
        assert!(entry_budget(scope) < MAX_PAGE_BYTES);
    }

    #[test]
    fn the_widest_scopes_leave_the_least_room() {
        use crate::inventory::scope::MAX_PROJECT_NAME_BYTES;
        let [project, task] = Scope::widest().map(|scope| entry_budget(&scope));
        assert!(project <= task);
        assert!(task < entry_budget(&Scope::Public));
        let longest = |c: &str| c.repeat(MAX_PROJECT_NAME_BYTES);
        for name in [
            "demo".to_owned(),
            longest("p"),
            longest("\""),
            longest("\\"),
        ] {
            assert!(
                entry_budget(&Scope::Project(name.clone())) >= project,
                "{name}"
            );
            assert!(
                entry_budget(&Scope::Task(name.clone())) >= project,
                "{name}"
            );
        }
    }

    #[test]
    fn what_does_not_fit_even_without_texts_is_too_many_tools() {
        // 600 tools named with 128 characters: their names alone are more
        // than a page.
        let tools = names(600, 128);
        let entry = connection(&tools, Visibility::Public);
        let fitted = fit_observation(&entry, observed(&tools, "Reads", "Read"), &scopes());
        assert_eq!(fitted.status, OperationalStatus::Failed);
        assert_eq!(fitted.detail.as_deref(), Some(DETAIL_TOO_MANY_TOOLS));
        assert!(fitted.tools.is_empty());
        assert!(rendered_bytes(&entry, &fitted, &Scope::Public) <= entry_budget(&Scope::Public));
    }
}
