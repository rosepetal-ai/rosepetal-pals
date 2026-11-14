//! The `pals` command line.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use pals::config::{self, CONFIG_FILE, Config, INIT_TEMPLATE};
use pals_core::identity::AgentKey;

#[derive(Parser)]
#[command(
    name = "pals",
    version,
    about = "PALS: AI agents that work with each other over A2A"
)]
struct Cli {
    /// PALS home directory, which holds config.toml [default: $PALS_HOME, else ~/.pals]
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create config.toml and agent.key, keeping any that already exist
    Init,
    /// Print this agent's id
    Id,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(config::default_home);
    let home =
        std::path::absolute(&home).with_context(|| format!("cannot resolve {}", home.display()))?;
    match cli.command {
        Command::Init => init(&home),
        Command::Id => id(&home),
    }
}

fn init(home: &Path) -> anyhow::Result<()> {
    create_private_dir(home)?;
    let config_path = home.join(CONFIG_FILE);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
    {
        Ok(mut file) => {
            file.write_all(INIT_TEMPLATE.as_bytes())
                .with_context(|| format!("cannot write {}", config_path.display()))?;
            println!("created {}", config_path.display());
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            println!("kept the existing {}", config_path.display());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("cannot create {}", config_path.display()));
        }
    }

    let cfg = Config::load(&config_path)?;
    create_private_dir(&cfg.data_dir)?;
    let key_path = cfg.key_path();
    let existed = key_path.exists();
    let key = AgentKey::load_or_create(&key_path)
        .with_context(|| format!("cannot load or create {}", key_path.display()))?;
    let verb = if existed {
        "kept the existing"
    } else {
        "created"
    };
    println!("{verb} {}", key_path.display());
    println!("agent id: {}", key.id());
    Ok(())
}

fn id(home: &Path) -> anyhow::Result<()> {
    let config_path = home.join(CONFIG_FILE);
    if !config_path.exists() {
        bail!("{} does not exist; run `pals init`", config_path.display());
    }
    let cfg = Config::load(&config_path)?;
    let key_path = cfg.key_path();
    if !key_path.exists() {
        bail!("{} does not exist; run `pals init`", key_path.display());
    }
    let key = AgentKey::load_or_create(&key_path)
        .with_context(|| format!("cannot load {}", key_path.display()))?;
    println!("{}", key.id());
    Ok(())
}

/// Creates `dir` and its missing parents, readable only by the owner.
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))
}
