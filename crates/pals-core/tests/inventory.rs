//! The `agentnet.inventory/1` inventory: optional blocks whose unknown values
//! stay unknown, identifiers, limits, visibility and the scopes grants give.

use pals_core::grant::{GRANT_SCHEMA, Grant, Operation};
use pals_core::identity::AgentKey;
use pals_core::inventory::{
    CAPABILITY_PREFIX, ComponentRole, Inventory, InventoryError, MAX_INVENTORY_BYTES, McpTransport,
    ModelExecution, Scope, Visibility, capability_tag,
};
use serde_json::{Value, json};

/// The message `toml` is refused with.
fn refusal(toml: &str) -> String {
    match Inventory::from_toml(toml) {
        Ok(inventory) => panic!("accepted {toml}\nas {inventory:?}"),
        Err(err) => err.to_string(),
    }
}

/// An entry of `block` with only the fields every entry needs.
fn minimal_entry(block: &str, id: &str) -> String {
    let header = match block {
        "runtime" | "hardware" | "network" => format!("[{block}]"),
        _ => format!("[[{block}]]"),
    };
    format!("{header}\nid = \"{id}\"\ndescription = \"An entry\"\nprovenance = \"operator\"\n")
}

/// A service with `fields` after its id, name and description.
fn service(id: &str, fields: &str) -> String {
    format!(
        "[[services]]\nid = \"{id}\"\nname = \"Industrial OCR\"\n\
         description = \"Reads stamped serial numbers\"\n{fields}\n"
    )
}

/// Every value in `json` that would pass for a known one when nothing is
/// known: zero, false, an empty string, list or object.
fn zero_like(json: &Value, path: &str, found: &mut Vec<String>) {
    match json {
        Value::Number(n) if n.as_f64() == Some(0.0) => found.push(path.to_owned()),
        Value::Bool(false) => found.push(path.to_owned()),
        Value::String(s) if s.is_empty() => found.push(path.to_owned()),
        Value::Array(items) if items.is_empty() => found.push(path.to_owned()),
        Value::Object(map) if map.is_empty() => found.push(path.to_owned()),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                zero_like(item, &format!("{path}/{i}"), found);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                zero_like(value, &format!("{path}/{key}"), found);
            }
        }
        _ => {}
    }
}

fn grant(operations: &[Operation], resources: &[&str]) -> Grant {
    Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: "g-1".to_owned(),
        issuer: AgentKey::generate().id(),
        subject: AgentKey::generate().id(),
        audience: AgentKey::generate().id(),
        project: "demo".to_owned(),
        resources: resources.iter().map(|&r| r.to_owned()).collect(),
        operations: operations.to_vec(),
        issued_at: "2026-11-14T00:00:00Z".to_owned(),
        expires_at: "2026-11-15T00:00:00Z".to_owned(),
    }
}

#[test]
fn every_block_is_optional() {
    assert_eq!(Inventory::from_toml("").unwrap(), Inventory::default());
    assert_eq!(
        Inventory::from_toml("# only a comment\n").unwrap(),
        Inventory::default()
    );
    assert_eq!(
        serde_json::to_value(Inventory::default()).unwrap(),
        json!({})
    );

    // Each block alone, with only what every entry needs.
    for block in [
        "skills",
        "plugins",
        "mcp",
        "runtime",
        "models",
        "information",
        "hardware",
        "network",
    ] {
        let inventory = Inventory::from_toml(&minimal_entry(block, "example:one"))
            .unwrap_or_else(|err| panic!("{block}: {err}"));
        let json = serde_json::to_value(&inventory).unwrap();
        let entry = match &json[block] {
            Value::Array(entries) => {
                assert_eq!(entries.len(), 1, "{block}: {json}");
                &entries[0]
            }
            single => single,
        };
        assert_eq!(entry["id"], "example:one", "{block}: {json}");
        assert_eq!(json.as_object().unwrap().len(), 1, "{block}: {json}");
    }
    // A service needs a capability or a tag besides its id, name and
    // description, and nothing else.
    let inventory = Inventory::from_toml(&service("ocr", "tags = [\"ocr\"]")).unwrap();
    assert_eq!(inventory.services.len(), 1);
    assert!(inventory.skills.is_empty() && inventory.runtime.is_none());
}

#[test]
fn unknown_values_stay_unknown() {
    let inventory = Inventory::from_toml(
        r#"
[[models]]
id = "example:llm"
description = "A local model"
provenance = "operator"
provider = "Example"
model_id = "example-7b"

[hardware]
id = "example:workstation"
description = "The inspection workstation"
provenance = "operator"
cpu = { arch = "x86_64", cores = 16 }
offered = { cpu_cores = 4 }
"#,
    )
    .unwrap();
    let model = &inventory.models[0].body;
    assert_eq!(model.revision, None);
    assert_eq!(model.context_tokens, None);
    assert_eq!(model.execution, None);
    let hardware = &inventory.hardware.as_ref().unwrap().body;
    assert_eq!(hardware.ram_mib, None);
    assert_eq!(hardware.offered.as_ref().unwrap().ram_mib, None);

    let json = serde_json::to_value(&inventory).unwrap();
    let model = &json["models"][0];
    for key in [
        "revision",
        "context_tokens",
        "output_tokens",
        "role",
        "execution",
    ] {
        assert!(model.get(key).is_none(), "{key} in {model}");
    }
    let hardware = &json["hardware"];
    assert!(hardware.get("ram_mib").is_none(), "{hardware}");
    assert!(hardware["cpu"].get("model").is_none(), "{hardware}");
    assert!(hardware["offered"].get("ram_mib").is_none(), "{hardware}");
    assert_eq!(hardware["cpu"]["cores"], 16);
    // Nominal figures are labelled as declared, never as measured (§9.6).
    assert_eq!(hardware["basis"], "declared");

    // Nothing the file left out reads as zero, false or empty.
    let mut found = Vec::new();
    zero_like(&json, "", &mut found);
    assert!(found.is_empty(), "{found:?} in {json}");

    // An empty string is not a value: what is unknown is left out.
    let err = refusal(
        "[[models]]\nid = \"example:llm\"\ndescription = \"A model\"\nprovenance = \"operator\"\n\
         revision = \"\"\n",
    );
    assert!(err.contains("revision") && err.contains("empty"), "{err}");
}

#[test]
fn unknown_plain_field_is_refused_and_namespaced_extension_is_kept() {
    // An unknown block, an unknown key in an entry, in a service and in a
    // nested table.
    for (toml, key) in [
        ("[[gpus]]\nid = \"example:gpu\"\n".to_owned(), "gpus"),
        (
            format!(
                "{}gpu_tier = \"gold\"\n",
                minimal_entry("hardware", "example:hw")
            ),
            "gpu_tier",
        ),
        (
            format!("{}speed_ghz = 4\n", minimal_entry("models", "example:llm")),
            "speed_ghz",
        ),
        (service("ocr", "tags = [\"ocr\"]\nprice = 3"), "price"),
        (
            format!(
                "{}cpu = {{ arch = \"x86_64\", speed_ghz = 4 }}\n",
                minimal_entry("hardware", "example:hw")
            ),
            "speed_ghz",
        ),
        (
            format!(
                "{}tools = [{{ name = \"read_file\", danger = true }}]\n",
                minimal_entry("mcp", "example:repo")
            ),
            "danger",
        ),
    ] {
        let err = refusal(&toml);
        assert!(err.contains(key), "{toml}: {err}");
    }

    // Under `extensions`, only keys with a namespace, never `agentnet`.
    for key in ["gpu_tier", "agentnet:tier", ":tier", "Example:tier"] {
        let toml = format!(
            "{}[hardware.extensions]\n\"{key}\" = \"gold\"\n",
            minimal_entry("hardware", "example:hw")
        );
        let err = refusal(&toml);
        assert!(err.contains("extension"), "{key}: {err}");
    }

    let toml = format!(
        "{}[models.extensions]\n\"example:tier\" = \"gold\"\n\"example:limits\" = {{ rpm = 60, regions = [\"eu\"] }}\n",
        minimal_entry("models", "example:llm")
    );
    let inventory = Inventory::from_toml(&toml).unwrap();
    let extensions = &inventory.models[0].extensions;
    assert_eq!(extensions["example:tier"], json!("gold"));
    assert_eq!(
        extensions["example:limits"],
        json!({"rpm": 60, "regions": ["eu"]})
    );
    let json = serde_json::to_value(&inventory).unwrap();
    assert_eq!(json["models"][0]["extensions"]["example:tier"], "gold");
    let back: Inventory = serde_json::from_value(json).unwrap();
    assert_eq!(back, inventory);
}

#[test]
fn default_visibility_is_private() {
    assert_eq!(Visibility::default(), Visibility::Private);
    let toml = format!(
        "{}{}",
        minimal_entry("mcp", "example:repo"),
        service("ocr", "tags = [\"ocr\"]")
    );
    let inventory = Inventory::from_toml(&toml).unwrap();
    assert_eq!(inventory.mcp[0].visibility, Visibility::Private);
    assert_eq!(inventory.services[0].visibility, Visibility::Private);
    let json = serde_json::to_value(&inventory).unwrap();
    assert_eq!(json["mcp"][0]["visibility"], "private");
    assert_eq!(json["services"][0]["visibility"], "private");
    for scope in [
        Scope::Public,
        Scope::Project("demo".to_owned()),
        Scope::Task("demo".to_owned()),
    ] {
        assert!(!inventory.mcp[0].visibility.visible_in(&scope), "{scope:?}");
    }
}

#[test]
fn visibility_by_scope() {
    let public = Scope::Public;
    let project_demo = Scope::Project("demo".to_owned());
    let task_demo = Scope::Task("demo".to_owned());
    let project_other = Scope::Project("other".to_owned());
    let task_other = Scope::Task("other".to_owned());
    let scopes = [
        &public,
        &project_demo,
        &task_demo,
        &project_other,
        &task_other,
    ];
    assert_eq!(public.key(), "public");
    assert_eq!(project_demo.key(), "project:demo");
    assert_eq!(task_demo.key(), "task:demo");

    let visible = |visibility: &Visibility| -> Vec<String> {
        scopes
            .iter()
            .filter(|scope| visibility.visible_in(scope))
            .map(|scope| scope.key())
            .collect()
    };
    assert_eq!(
        visible(&Visibility::Public),
        [
            "public",
            "project:demo",
            "task:demo",
            "project:other",
            "task:other"
        ]
    );
    assert_eq!(
        visible(&Visibility::Project(vec!["demo".to_owned()])),
        ["project:demo", "task:demo"]
    );
    assert_eq!(
        visible(&Visibility::Task(vec!["demo".to_owned()])),
        ["task:demo"]
    );
    assert!(visible(&Visibility::Private).is_empty());
    assert_eq!(
        visible(&Visibility::Project(vec![
            "demo".to_owned(),
            "other".to_owned()
        ])),
        ["project:demo", "task:demo", "project:other", "task:other"]
    );
}

#[test]
fn visibility_and_adapter_read_as_the_toml_examples_write_them() {
    // The operator's example, as written.
    let inventory = Inventory::from_toml(
        r#"
[[mcp]]
id = "example:repo-reader"
description = "Reads the project repository"
provenance = "operator"
visibility = { project = ["demo"] }
offered_tools = ["read_file", "list_dir"]   # the only thing published; never the rest
[mcp.adapter]
transport = "stdio"                         # or "http" with url = "http://127.0.0.1:3000/mcp" (loopback) or https
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/srv/repo"]
env_passthrough = []                        # names only
"#,
    )
    .unwrap();
    let mcp = &inventory.mcp[0];
    assert_eq!(mcp.visibility, Visibility::Project(vec!["demo".to_owned()]));
    assert_eq!(mcp.body.offered_tools, ["read_file", "list_dir"]);
    assert_eq!(
        mcp.body.adapter.as_ref().unwrap().transport,
        McpTransport::Stdio {
            command: "npx".to_owned(),
            args: vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
                "/srv/repo".to_owned()
            ],
            env_passthrough: Vec::new(),
        }
    );
    let json = serde_json::to_value(&inventory).unwrap();
    assert_eq!(json["mcp"][0]["visibility"], json!({"project": ["demo"]}));
    assert_eq!(json["mcp"][0]["adapter"]["transport"], "stdio");
    assert_eq!(json["mcp"][0]["adapter"]["command"], "npx");

    let inventory = Inventory::from_toml(&format!(
        "{}[mcp.adapter]\ntransport = \"http\"\nurl = \"http://127.0.0.1:3000/mcp\"\n",
        minimal_entry("mcp", "example:pals")
    ))
    .unwrap();
    assert_eq!(
        inventory.mcp[0].body.adapter.as_ref().unwrap().transport,
        McpTransport::Http {
            url: "http://127.0.0.1:3000/mcp".to_owned()
        }
    );

    // Every spelling of visibility.
    for (written, read) in [
        ("\"public\"", Visibility::Public),
        ("\"private\"", Visibility::Private),
        (
            "{ project = [\"demo\", \"other\"] }",
            Visibility::Project(vec!["demo".to_owned(), "other".to_owned()]),
        ),
        (
            "{ task = [\"demo\"] }",
            Visibility::Task(vec!["demo".to_owned()]),
        ),
    ] {
        let toml = service("ocr", &format!("tags = [\"ocr\"]\nvisibility = {written}"));
        let inventory =
            Inventory::from_toml(&toml).unwrap_or_else(|err| panic!("{written}: {err}"));
        assert_eq!(inventory.services[0].visibility, read, "{written}");
    }
    for written in [
        "\"protected\"",
        "{ project = [] }",
        "{ project = [\"two words\"] }",
        "{ project = [\"demo\"], task = [\"demo\"] }",
        "{ project = \"demo\" }",
    ] {
        let toml = service("ocr", &format!("tags = [\"ocr\"]\nvisibility = {written}"));
        assert!(Inventory::from_toml(&toml).is_err(), "{written}");
    }

    // An adapter takes only its own transport's keys.
    for adapter in [
        "transport = \"stdio\"\ncommand = \"npx\"\nurl = \"http://127.0.0.1:1/mcp\"",
        "transport = \"http\"\nurl = \"http://127.0.0.1:1/mcp\"\ncommand = \"npx\"",
        "transport = \"carrier-pigeon\"\ncommand = \"npx\"",
        "command = \"npx\"",
    ] {
        let toml = format!(
            "{}[mcp.adapter]\n{adapter}\n",
            minimal_entry("mcp", "example:repo")
        );
        assert!(Inventory::from_toml(&toml).is_err(), "{adapter}");
    }

    // Model execution and component roles, as the examples write them.
    let inventory = Inventory::from_toml(&format!(
        "{}execution = {{ kind = \"external\", provider = \"anthropic-api\" }}\n{}\
         components = [{{ id = \"example:llm\", role = \"alternative\", group = \"llm\" }}]\n",
        minimal_entry("models", "example:llm"),
        service("ocr", "tags = [\"ocr\"]"),
    ))
    .unwrap();
    assert_eq!(
        inventory.models[0].body.execution,
        Some(ModelExecution::External {
            provider: "anthropic-api".to_owned()
        })
    );
    let link = &inventory.services[0].components[0];
    assert_eq!(link.role, ComponentRole::Alternative);
    assert_eq!(link.group.as_deref(), Some("llm"));
    let local = format!(
        "{}execution = {{ kind = \"local\" }}\n",
        minimal_entry("models", "example:llm")
    );
    assert_eq!(
        Inventory::from_toml(&local).unwrap().models[0]
            .body
            .execution,
        Some(ModelExecution::Local)
    );
    let local_with_provider = format!(
        "{}execution = {{ kind = \"local\", provider = \"x\" }}\n",
        minimal_entry("models", "example:llm")
    );
    assert!(Inventory::from_toml(&local_with_provider).is_err());
}

#[test]
fn scope_follows_the_grant() {
    use Operation::{BlobFetch, TaskCancel, TaskRead, TaskSend};
    let demo = || "demo".to_owned();
    assert_eq!(Scope::for_grant(None), Scope::Public);
    for (operations, resources, scope) in [
        (&[TaskRead][..], &["skill:ask"][..], Scope::Project(demo())),
        (&[TaskCancel], &["skill:ask"], Scope::Project(demo())),
        (
            &[TaskRead, TaskCancel],
            &["skill:ask"],
            Scope::Project(demo()),
        ),
        (&[TaskSend], &["skill:ask"], Scope::Task(demo())),
        (&[TaskRead, TaskSend], &["skill:ask"], Scope::Task(demo())),
        (&[TaskSend], &["skill:*"], Scope::Task(demo())),
        (
            &[TaskSend, BlobFetch],
            &["blob:*", "skill:ask"],
            Scope::Task(demo()),
        ),
        (&[BlobFetch], &["blob:*"], Scope::Public),
        (&[BlobFetch], &["skill:ask"], Scope::Public),
        (&[TaskSend], &["skill:other"], Scope::Public),
        (&[TaskRead], &["blob:*"], Scope::Public),
    ] {
        let grant = grant(operations, resources);
        assert_eq!(
            Scope::for_grant(Some(&grant)),
            scope,
            "{operations:?} on {resources:?}"
        );
    }
    let mut other = grant(&[TaskSend], &["skill:ask"]);
    other.project = "other".to_owned();
    assert_eq!(Scope::for_grant(Some(&other)).key(), "task:other");
}

#[test]
fn a_declaration_cannot_say_operational() {
    for toml in [
        format!(
            "{}operational = \"ok\"\n",
            minimal_entry("mcp", "example:repo")
        ),
        format!(
            "{}operational = \"ok\"\n",
            minimal_entry("hardware", "example:hw")
        ),
        service("ocr", "tags = [\"ocr\"]\noperational = \"ok\""),
    ] {
        let err = Inventory::from_toml(&toml).expect_err("refused");
        assert!(
            matches!(err, InventoryError::DeclaredOperational(_)),
            "{toml}: {err:?}"
        );
        let text = err.to_string();
        assert!(
            text.contains("operational") && text.contains("declar"),
            "{text}"
        );
    }
}

#[test]
fn ids_need_a_namespace_and_agentnet_is_reserved() {
    let namespace_64 = "n".repeat(64);
    let name_128 = "N".repeat(128);
    for id in [
        "example:repo-reader".to_owned(),
        "project:repo-controller".to_owned(),
        "ex.co_1-2:Name/With.Slash_-9".to_owned(),
        format!("{namespace_64}:{name_128}"),
    ] {
        let toml = minimal_entry("mcp", &id);
        let inventory = Inventory::from_toml(&toml).unwrap_or_else(|err| panic!("{id}: {err}"));
        assert_eq!(inventory.mcp[0].id, id);
    }
    for id in [
        "repo-reader".to_owned(),
        ":repo".to_owned(),
        "example:".to_owned(),
        "agentnet:repo".to_owned(),
        "Example:repo".to_owned(),
        "example:two words".to_owned(),
        "example:a:b".to_owned(),
        format!("{namespace_64}n:repo"),
        format!("example:{name_128}N"),
    ] {
        for block in ["skills", "mcp", "runtime", "hardware"] {
            let err = refusal(&minimal_entry(block, &id));
            assert!(err.contains("id"), "{block} {id}: {err}");
        }
    }

    // One id names one entry, whichever its block.
    let toml = format!(
        "{}{}",
        minimal_entry("skills", "example:repo"),
        minimal_entry("mcp", "example:repo")
    );
    assert!(refusal(&toml).contains("example:repo"));

    // A service links only to entries the inventory has.
    let link = |id: &str| {
        format!(
            "{}{}",
            minimal_entry("models", "example:llm"),
            service(
                "ocr",
                &format!(
                    "tags = [\"ocr\"]\ncomponents = [{{ id = \"{id}\", role = \"required\" }}]"
                )
            )
        )
    };
    assert!(Inventory::from_toml(&link("example:llm")).is_ok());
    let err = refusal(&link("example:missing"));
    assert!(err.contains("example:missing"), "{err}");
}

#[test]
fn service_ids_are_safe_skill_ids() {
    for id in ["ocr", "ocr-metal-v1", "0", &"a".repeat(64)] {
        let toml = service(id, "tags = [\"ocr\"]");
        assert!(Inventory::from_toml(&toml).is_ok(), "{id}");
    }
    for id in [
        "vision.ocr",
        "example:ocr",
        "OCR",
        "-ocr",
        "ocr_metal",
        "",
        &"a".repeat(65),
        // The card's own skill.
        "ask",
    ] {
        let err = refusal(&service(id, "tags = [\"ocr\"]"));
        assert!(err.contains("service"), "{id}: {err}");
    }
    let twice = format!(
        "{}{}",
        service("ocr", "tags = [\"ocr\"]"),
        service("ocr", "tags = [\"ocr\"]")
    );
    assert!(refusal(&twice).contains("ocr"));
}

#[test]
fn service_without_name_description_or_tags_is_refused() {
    let without = |field: &str| {
        let toml = service("ocr", "tags = [\"ocr\"]");
        let line = toml
            .lines()
            .find(|line| line.starts_with(field))
            .unwrap()
            .to_owned();
        toml.replace(&line, &format!("{field} = \"\""))
    };
    for field in ["name", "description"] {
        let err = refusal(&without(field));
        assert!(err.contains(field), "{field}: {err}");
    }
    let err = refusal(&service("ocr", ""));
    assert!(err.contains("capabilit") && err.contains("tag"), "{err}");
    let err = refusal(&service("ocr", "capabilities = []\ntags = []"));
    assert!(err.contains("capabilit") && err.contains("tag"), "{err}");

    // Either one is enough.
    assert!(Inventory::from_toml(&service("ocr", "capabilities = [\"vision.ocr\"]")).is_ok());
    assert!(Inventory::from_toml(&service("ocr", "tags = [\"ocr\"]")).is_ok());

    // What becomes a card skill keeps no empty value (task 1's fixed point),
    // and capabilities follow the vocabulary.
    for fields in [
        "tags = [\"\"]",
        "tags = [\"ocr\"]\nexamples = [\"\"]",
        "tags = [\"ocr\"]\ninput_modes = [\"\"]",
        "tags = [\"ocr\"]\noutput_modes = [\"\"]",
        "capabilities = [\"Vision OCR\"]",
        "capabilities = [\"agentnet:capability:v1:vision.ocr\"]",
    ] {
        assert!(
            Inventory::from_toml(&service("ocr", fields)).is_err(),
            "{fields}"
        );
    }
}

#[test]
fn capability_tags_follow_the_vocabulary() {
    assert_eq!(
        capability_tag("vision.ocr").unwrap(),
        "agentnet:capability:v1:vision.ocr"
    );
    assert_eq!(
        capability_tag("code.rust.review").unwrap(),
        format!("{CAPABILITY_PREFIX}code.rust.review")
    );
    assert_eq!(capability_tag("ex:foo").unwrap(), "ex:foo");
    assert_eq!(
        capability_tag("example:Ocr/Metal").unwrap(),
        "example:Ocr/Metal"
    );
    for capability in [
        "",
        "Vision.OCR",
        "vision ocr",
        "vision/ocr",
        "agentnet:capability:v1:vision.ocr",
        "agentnet:ocr",
        "ex:",
        ":foo",
        "ex:a:b",
        &"a".repeat(129),
    ] {
        assert!(capability_tag(capability).is_err(), "{capability:?}");
    }
    assert!(capability_tag(&"a".repeat(128)).is_ok());
}

#[test]
fn own_tags_cannot_take_the_reserved_agentnet_prefix() {
    // A card skill's tags are its capability tags and its own together: an
    // own tag in the reserved namespace would pass for a capability.
    for tag in [
        "agentnet:capability:v1:vision.ocr",
        "agentnet:anything",
        "agentnet:",
        "AgentNet:capability:v1:vision.ocr",
        " agentnet:capability:v1:vision.ocr",
    ] {
        let toml = service("ocr", &format!("tags = [\"metal\", {}]", json!(tag)));
        let err = refusal(&toml);
        assert!(err.contains("services[0] (ocr).tags[1]"), "{tag:?}: {err}");
        assert!(err.contains("agentnet:"), "{tag:?}: {err}");
    }
    // The capability itself, and tags that only mention the word, are fine.
    for fields in [
        "capabilities = [\"vision.ocr\"]\ntags = [\"metal\"]",
        "tags = [\"agentnet\", \"not-agentnet:ocr\", \"example:agentnet\"]",
    ] {
        let inventory = Inventory::from_toml(&service("ocr", fields))
            .unwrap_or_else(|err| panic!("{fields}: {err}"));
        assert_eq!(inventory.services.len(), 1);
    }
}

#[test]
fn inventory_file_over_1_mib_is_refused() {
    // Comments only: the size alone decides.
    let fill = |bytes: usize| {
        let mut text = "#".repeat(bytes - 1);
        text.push('\n');
        text
    };
    assert_eq!(
        Inventory::from_toml(&fill(MAX_INVENTORY_BYTES)).unwrap(),
        Inventory::default()
    );
    let err = Inventory::from_toml(&fill(MAX_INVENTORY_BYTES + 1)).unwrap_err();
    assert!(matches!(err, InventoryError::TooLarge), "{err:?}");
    assert!(err.to_string().contains("1048576"), "{err}");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inventory.toml");
    std::fs::write(&path, fill(MAX_INVENTORY_BYTES + 1)).unwrap();
    assert!(matches!(
        Inventory::load(&path),
        Err(InventoryError::TooLarge)
    ));
    std::fs::write(&path, minimal_entry("mcp", "example:repo")).unwrap();
    assert_eq!(Inventory::load(&path).unwrap().mcp[0].id, "example:repo");
    let missing = Inventory::load(&dir.path().join("none.toml")).unwrap_err();
    assert!(
        matches!(&missing, InventoryError::Io(err) if err.kind() == std::io::ErrorKind::NotFound),
        "{missing:?}"
    );
}

#[test]
fn strings_over_4_kib_and_blocks_over_1000_entries_are_refused() {
    let entry_with_description = |description: &str| {
        format!(
            "[[mcp]]\nid = \"example:repo\"\ndescription = \"{description}\"\nprovenance = \"operator\"\n"
        )
    };
    assert!(Inventory::from_toml(&entry_with_description(&"d".repeat(4096))).is_ok());
    let err = refusal(&entry_with_description(&"d".repeat(4097)));
    assert!(err.contains("4096"), "{err}");
    // Extension values and keys count too.
    let toml = format!(
        "{}[mcp.extensions]\n\"example:note\" = [\"{}\"]\n",
        minimal_entry("mcp", "example:repo"),
        "n".repeat(4097)
    );
    assert!(refusal(&toml).contains("4096"));

    let block = |entries: usize| -> String {
        (0..entries)
            .map(|i| minimal_entry("skills", &format!("example:skill-{i}")))
            .collect()
    };
    assert_eq!(
        Inventory::from_toml(&block(1000)).unwrap().skills.len(),
        1000
    );
    let err = refusal(&block(1001));
    assert!(err.contains("skills") && err.contains("1000"), "{err}");
    let services: String = (0..1001)
        .map(|i| service(&format!("s{i}"), "tags = [\"ocr\"]"))
        .collect();
    assert!(refusal(&services).contains("1000"));
}

#[test]
fn vector_parses_and_round_trips() {
    let vector: Value =
        serde_json::from_str(include_str!("vectors/inventory-v1.json")).expect("vector is JSON");
    let toml = vector["toml"].as_str().expect("the vector has its TOML");
    let from_toml = Inventory::from_toml(toml).expect("the vector's TOML is an inventory");
    let from_json: Inventory =
        serde_json::from_value(vector["inventory"].clone()).expect("the vector's JSON form");
    assert_eq!(from_toml, from_json);
    assert_eq!(
        serde_json::to_value(&from_toml).unwrap(),
        vector["inventory"]
    );

    // The vector shows every block.
    let inventory = &vector["inventory"];
    for block in [
        "services",
        "skills",
        "plugins",
        "mcp",
        "runtime",
        "models",
        "information",
        "hardware",
        "network",
    ] {
        assert!(inventory.get(block).is_some(), "{block} missing");
    }
}
