//! The `pals` command line, run as a separate process.

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;

/// `pals <args>` with no `$PALS_HOME` inherited from the environment.
fn pals(args: &[&str]) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME").args(args);
    cmd
}

fn stdout_of(cmd: &mut assert_cmd::Command) -> String {
    let output = cmd.assert().success().get_output().stdout.clone();
    String::from_utf8(output).expect("utf-8 output")
}

fn is_agent_id(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn home_arg(dir: &Path) -> &str {
    dir.to_str().expect("utf-8 temp path")
}

#[test]
fn init_then_id_prints_64_hex() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    pals(&["init", "--home", home_arg(&home)])
        .assert()
        .success();
    assert!(home.join("config.toml").is_file());
    assert!(home.join("agent.key").is_file());

    let id = stdout_of(&mut pals(&["id", "--home", home_arg(&home)]));
    assert!(is_agent_id(id.trim_end()), "{id:?}");
}

#[test]
fn second_init_keeps_the_existing_key_and_config() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_arg(dir.path());
    pals(&["init", "--home", home]).assert().success();
    let first_id = stdout_of(&mut pals(&["id", "--home", home]));
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "[agent]\nname = \"edited\"\n").unwrap();

    let second_init = stdout_of(&mut pals(&["init", "--home", home]));
    assert!(second_init.contains("kept"), "{second_init}");
    assert_eq!(stdout_of(&mut pals(&["id", "--home", home])), first_id);
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "[agent]\nname = \"edited\"\n"
    );
}

#[test]
fn init_writes_a_commented_inventory_and_keeps_an_existing_one() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_arg(dir.path());
    let first_init = stdout_of(&mut pals(&["init", "--home", home]));
    let inventory_path = dir.path().join("inventory.toml");
    assert!(first_init.contains("created"), "{first_init}");
    assert!(first_init.contains("inventory.toml"), "{first_init}");

    // Only comments: the inventory is empty until the operator writes it.
    let template = std::fs::read_to_string(&inventory_path).unwrap();
    assert!(
        template
            .lines()
            .all(|line| line.is_empty() || line.starts_with('#')),
        "{template}"
    );
    assert_eq!(
        pals_core::inventory::Inventory::from_toml(&template).unwrap(),
        pals_core::inventory::Inventory::default()
    );

    std::fs::write(&inventory_path, "# edited\n").unwrap();
    let second_init = stdout_of(&mut pals(&["init", "--home", home]));
    assert!(
        second_init
            .lines()
            .any(|line| line.contains("kept") && line.contains("inventory.toml")),
        "{second_init}"
    );
    assert_eq!(
        std::fs::read_to_string(&inventory_path).unwrap(),
        "# edited\n"
    );
}

#[test]
fn pals_home_is_used_and_home_flag_overrides_it() {
    let env_dir = tempfile::tempdir().unwrap();
    let flag_dir = tempfile::tempdir().unwrap();
    pals(&["init"])
        .env("PALS_HOME", env_dir.path())
        .assert()
        .success();
    assert!(env_dir.path().join("agent.key").is_file());

    pals(&["init", "--home", home_arg(flag_dir.path())])
        .env("PALS_HOME", env_dir.path())
        .assert()
        .success();
    assert!(flag_dir.path().join("agent.key").is_file());

    let from_env = stdout_of(pals(&["id"]).env("PALS_HOME", env_dir.path()));
    let from_flag = stdout_of(
        pals(&["id", "--home", home_arg(flag_dir.path())]).env("PALS_HOME", env_dir.path()),
    );
    assert_ne!(from_env, from_flag);
}

#[test]
fn id_without_init_fails_with_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let assert = pals(&["id", "--home", home_arg(dir.path())])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("pals init"), "{stderr}");
}

#[test]
fn inventory_show_renders_what_a_scope_sees() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_arg(dir.path());
    pals(&["init", "--home", home]).assert().success();
    std::fs::write(
        dir.path().join("inventory.toml"),
        r#"
[[plugins]]
id = "example:vision-pack"
description = "Vision tools"
provenance = "operator"
visibility = "public"

[[plugins]]
id = "example:secret-plugin"
description = "Installed, never offered"
provenance = "operator"

[[mcp]]
id = "example:repo"
description = "Reads the demo repository"
provenance = "operator"
visibility = { project = ["demo"] }
tools = [{ name = "read_file" }, { name = "delete_file" }]
offered_tools = ["read_file"]
"#,
    )
    .unwrap();

    let show = |args: &[&str]| -> serde_json::Value {
        let mut all = vec!["inventory", "show", "--home", home];
        all.extend_from_slice(args);
        serde_json::from_str(&stdout_of(&mut pals(&all))).expect("JSON on stdout")
    };
    let ids = |view: &serde_json::Value| -> Vec<String> {
        view["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_owned())
            .collect()
    };

    // Public by default, every section.
    let public = show(&[]);
    assert_eq!(public["scope"], "public");
    assert_eq!(ids(&public), ["example:vision-pack"]);

    let demo = show(&["--scope", "project:demo", "--sections", "mcp,plugins"]);
    assert_eq!(demo["scope"], "project:demo");
    assert_eq!(demo["sections"], serde_json::json!(["plugins", "mcp"]));
    assert_eq!(ids(&demo), ["example:vision-pack", "example:repo"]);
    assert_eq!(
        demo["entries"][1]["body"]["tools"],
        serde_json::json!([{"name": "example:repo/read_file"}])
    );
    let text = demo.to_string();
    for never in ["secret", "delete_file", "offered_tools"] {
        assert!(!text.contains(never), "{never} in {text}");
    }

    for bad in [
        &["--scope", "everyone"][..],
        &["--scope", "project:"],
        &["--sections", "mcp,gadgets"],
    ] {
        let mut all = vec!["inventory", "show", "--home", home];
        all.extend_from_slice(bad);
        pals(&all).assert().failure();
    }
}
