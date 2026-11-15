//! Verifies the signed, hash-chained event log of a PALS store with
//! [`Store::events_verify`], as the hito 2 exit test does on a data
//! directory a milestone 1 daemon wrote and a milestone 2 daemon then
//! upgraded (`docs/runbooks/hito-2-consulta-de-servicios.md`, case 8).
//!
//! ```text
//! cargo run --release --locked -p pals-core --example events_verify -- <pals.db> <agent id>
//! ```
//!
//! `<agent id>` is the store's owner, the one `pals id` prints, the author
//! of every event of its log: its whole chain is checked from `seq` 0
//! (gaps, previous hashes, payload hashes and signatures). Prints
//! `events_verify: <n> events of <agent id> verified` and exits 0, or names
//! the first failure and exits 1. Opening the store upgrades an older
//! schema, as the daemon does: run it on a data directory the milestone 2
//! daemon has already opened.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use pals_core::identity::AgentId;
use pals_core::store::Store;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            println!("events_verify: FAILED: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, owner] = args.as_slice() else {
        bail!("usage: events_verify <pals.db> <agent id>");
    };
    let path = PathBuf::from(path);
    if !path.is_file() {
        bail!("no database at {}", path.display());
    }
    let owner: AgentId = owner
        .parse()
        .map_err(|err| anyhow::anyhow!("not an agent id: {err}"))?;
    let store = Store::open(&path, owner)?;
    let verified = store
        .events_verify(&owner)
        .with_context(|| format!("the chain of {owner}"))?;
    if verified == 0 {
        bail!("the log holds no events of {owner}");
    }
    println!("events_verify: {verified} events of {owner} verified");
    Ok(())
}
