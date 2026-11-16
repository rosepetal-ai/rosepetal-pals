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
