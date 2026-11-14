//! The `pals` command line.
//!
//! Only `pals daemon` takes the data directory's lock; every other command
//! opens the store directly, which a running daemon shares.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow, bail, ensure};
use clap::{Parser, Subcommand};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use iroh_tickets::endpoint::EndpointTicket;
use pals::config::{self, CONFIG_FILE, Config, INIT_TEMPLATE, is_header_safe};
use pals::net::agent_id_of;
use pals::node::{Node, STORE_FILE};
use pals_core::grant::{
    GRANT_SCHEMA, Grant, GrantError, MAX_TOKEN_BYTES, Operation, SignedGrant, format_timestamp,
    parse_timestamp,
};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::{Contact, Store};
use serde_json::{Value, json};

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
    Id {
        /// Print instead the ticket peers save this agent with: its id and the
        /// addresses the running daemon is reachable at
        #[arg(long)]
        ticket: bool,
    },
    /// Run the agent: serve peers over Iroh and the local A2A gateway, until
    /// SIGINT or SIGTERM
    Daemon,
    /// The peers this agent knows by name
    #[command(subcommand)]
    Contact(ContactCommand),
    /// Grants this agent issued to peers, and grants peers issued to it
    #[command(subcommand)]
    Grant(GrantCommand),
}

#[derive(Subcommand)]
enum ContactCommand {
    /// Save a peer, from the ticket that `pals id --ticket` prints on the peer
    Add {
        ticket: String,
        /// The name to reach the peer by
        #[arg(long)]
        name: String,
    },
    /// List the saved peers
    Ls,
}

#[derive(Subcommand)]
enum GrantCommand {
    /// Let a contact use this agent, and print the grant's token for the
    /// contact to import
    Issue {
        /// The contact, by name or agent id
        #[arg(long, value_name = "CONTACT")]
        to: String,
        /// The project the grant is for
        #[arg(long)]
        project: String,
        /// The operations allowed, comma separated: task.send, task.read,
        /// task.cancel, blob.fetch
        #[arg(long, default_value = "task.send,task.read,task.cancel")]
        ops: String,
        /// The resources the operations apply to, comma separated
        #[arg(long, default_value = "skill:ask")]
        resources: String,
        /// How long the grant is valid: a number and s, m, h or d
        #[arg(long, default_value = "24h")]
        ttl: String,
    },
    /// Keep a grant another agent issued to this one
    Import { token: String },
    /// Revoke a grant this agent issued
    Revoke { grant_id: String },
    /// List the grants this agent issued and those it holds
    Ls {
        /// Only the grants this agent issued
        #[arg(long, conflicts_with = "held")]
        issued: bool,
        /// Only the grants this agent holds
        #[arg(long)]
        held: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(config::default_home);
    let home =
        std::path::absolute(&home).with_context(|| format!("cannot resolve {}", home.display()))?;
    match cli.command {
        Command::Init => init(&home),
        Command::Id { ticket: false } => id(&home),
        Command::Id { ticket: true } => id_ticket(&home),
        Command::Daemon => daemon(&home),
        Command::Contact(ContactCommand::Add { ticket, name }) => contact_add(&home, &ticket, name),
        Command::Contact(ContactCommand::Ls) => contact_ls(&home),
        Command::Grant(GrantCommand::Issue {
            to,
            project,
            ops,
            resources,
            ttl,
        }) => grant_issue(&home, &to, project, &ops, &resources, &ttl),
        Command::Grant(GrantCommand::Import { token }) => grant_import(&home, &token),
        Command::Grant(GrantCommand::Revoke { grant_id }) => grant_revoke(&home, &grant_id),
        Command::Grant(GrantCommand::Ls { issued, held }) => grant_ls(&home, !held, !issued),
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
    let (_, key) = load_agent(home)?;
    println!("{}", key.id());
    Ok(())
}

/// Prints the ticket of the daemon running this agent, asked through its
/// gateway: the addresses it is reachable at exist only while it runs.
fn id_ticket(home: &Path) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let listen = cfg.gateway.listen;
    let health = runtime(false)?
        .block_on(gateway_health(listen))
        .with_context(|| {
            format!(
                "no pals daemon answers at http://{listen}: start it with `pals daemon`, whose \
                 ticket carries the addresses it is reachable at"
            )
        })?;
    let running = health["agent_id"].as_str().unwrap_or_default();
    ensure!(
        running == key.id().to_string(),
        "the pals daemon at http://{listen} runs agent {running}, not this agent {}",
        key.id()
    );
    let ticket = health["ticket"]
        .as_str()
        .ok_or_else(|| anyhow!("the pals daemon at http://{listen} reported no ticket"))?;
    println!("{ticket}");
    Ok(())
}

/// `GET /healthz` of the gateway listening on `listen`.
async fn gateway_health(listen: SocketAddr) -> anyhow::Result<Value> {
    const TIMEOUT: Duration = Duration::from_secs(5);
    let exchange = async {
        let stream = tokio::net::TcpStream::connect(listen).await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(connection);
        let request = http::Request::get("/healthz")
            .header(http::header::HOST, listen.to_string())
            .body(Empty::<Bytes>::new())?;
        let response = sender.send_request(request).await?;
        ensure!(
            response.status().is_success(),
            "/healthz answered {}",
            response.status()
        );
        let body = Limited::new(response.into_body(), 64 * 1024)
            .collect()
            .await
            .map_err(|err| anyhow!("cannot read /healthz: {err}"))?
            .to_bytes();
        Ok(serde_json::from_slice(&body)?)
    };
    tokio::time::timeout(TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("/healthz did not answer within {TIMEOUT:?}"))?
}

fn daemon(home: &Path) -> anyhow::Result<()> {
    let cfg = load_config(home)?;
    runtime(true)?.block_on(async move {
        // Before anything binds, so that a signal never kills a half-started
        // daemon.
        let stop = stop_signal()?;
        let node = Node::spawn(cfg).await?;
        eprintln!(
            "pals daemon: agent {} serving, gateway at {}",
            node.id,
            node.gateway_url()
        );
        let signal = stop.await;
        eprintln!("pals daemon: {signal}, shutting down");
        node.shutdown().await
    })
}

/// A future that resolves, with the signal's name, on SIGINT or SIGTERM.
fn stop_signal() -> anyhow::Result<impl Future<Output = &'static str>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).context("cannot handle SIGINT")?;
        let mut terminate = signal(SignalKind::terminate()).context("cannot handle SIGTERM")?;
        Ok(async move {
            let interrupted = Box::pin(interrupt.recv());
            let terminated = Box::pin(terminate.recv());
            match futures::future::select(interrupted, terminated).await {
                futures::future::Either::Left(_) => "SIGINT",
                futures::future::Either::Right(_) => "SIGTERM",
            }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            if let Err(err) = tokio::signal::ctrl_c().await {
                eprintln!("pals daemon: cannot wait for Ctrl-C: {err}");
            }
            "Ctrl-C"
        })
    }
}

fn contact_add(home: &Path, ticket: &str, name: String) -> anyhow::Result<()> {
    let ticket: EndpointTicket = ticket.trim().parse().map_err(|err| {
        anyhow!("not a valid ticket ({err}): `pals id --ticket` prints one on the peer")
    })?;
    ensure!(!name.is_empty(), "a contact name cannot be empty");
    ensure!(
        name.parse::<AgentId>().is_err(),
        "a contact name cannot be an agent id: peers are reachable by their id already"
    );
    let agent_id = agent_id_of(&ticket.endpoint_addr().id);
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    store.contact_put(&Contact {
        name: name.clone(),
        agent_id,
        ticket: Some(ticket.to_string()),
        added_at: now_unix(),
    })?;
    println!("saved contact {name}: {agent_id}");
    Ok(())
}

fn contact_ls(home: &Path) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    for contact in open_store(&cfg, &key)?.contacts()? {
        println!("{}\t{}", contact.name, contact.agent_id);
    }
    Ok(())
}

fn grant_issue(
    home: &Path,
    to: &str,
    project: String,
    ops: &str,
    resources: &str,
    ttl: &str,
) -> anyhow::Result<()> {
    ensure!(
        is_header_safe(&project),
        "invalid --project {project:?}: a project name is printable ASCII without spaces, \
         because it travels in an HTTP header"
    );
    let operations = parse_operations(ops)?;
    let resources = parse_list(resources);
    ensure!(!resources.is_empty(), "--resources lists no resource");
    let ttl_s = parse_ttl(ttl)?;

    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    let subject = match to.parse::<AgentId>() {
        Ok(id) => id,
        Err(_) => {
            store
                .contact_by_name(to)?
                .ok_or_else(|| {
                    anyhow!(
                        "unknown contact {to}: save it first with `pals contact add <ticket> \
                         --name {to}`"
                    )
                })?
                .agent_id
        }
    };
    let issued_at = now_unix();
    let expires_at = issued_at
        .checked_add(ttl_s)
        .filter(|at| format_timestamp(*at).is_some())
        .ok_or_else(|| anyhow!("--ttl {ttl} reaches past the year 9999"))?;
    ensure!(expires_at > issued_at, "--ttl {ttl} must be positive");
    let grant_id = new_grant_id();
    ensure!(
        is_header_safe(&grant_id),
        "the generated grant id {grant_id:?} is not header safe"
    );
    let grant = Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: grant_id.clone(),
        issuer: key.id(),
        subject,
        audience: key.id(),
        project,
        resources,
        operations,
        issued_at: format_timestamp(issued_at).expect("checked above"),
        expires_at: format_timestamp(expires_at).expect("checked above"),
    };
    let signed = SignedGrant::issue(&key, grant).map_err(issue_failure)?;
    store.grant_issued_put(&signed)?;
    store.event_append(
        &key,
        "grant.issued",
        &json!({"grant_id": grant_id, "subject": subject.to_string()}),
        issued_at,
    )?;
    let g = &signed.grant;
    eprintln!(
        "issued grant {grant_id} to {to}: project {}, {} on {}, until {}",
        g.project,
        operation_names(&g.operations),
        g.resources.join(","),
        g.expires_at
    );
    println!("{}", signed.to_token());
    Ok(())
}

fn grant_import(home: &Path, token: &str) -> anyhow::Result<()> {
    let g = SignedGrant::from_token(token.trim()).map_err(|err| match err {
        GrantError::BadSignature => {
            anyhow!("not a valid grant: its signature does not verify")
        }
        GrantError::TooLarge => anyhow!("not a valid grant: larger than {MAX_TOKEN_BYTES} bytes"),
        GrantError::UnknownSchema => anyhow!("not a valid grant: unknown schema"),
        _ => anyhow!("not a valid grant token"),
    })?;
    let (cfg, key) = load_agent(home)?;
    ensure!(
        g.grant.subject == key.id(),
        "this grant's subject is {}, not this agent {}: only its subject can use it",
        g.grant.subject,
        key.id()
    );
    open_store(&cfg, &key)?.grant_held_put(&g)?;
    println!(
        "imported grant {} from {}: project {}, until {}",
        g.grant.grant_id, g.grant.issuer, g.grant.project, g.grant.expires_at
    );
    Ok(())
}

fn grant_revoke(home: &Path, grant_id: &str) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    ensure!(
        store.grant_issued(grant_id)?.is_some(),
        "this agent issued no grant {grant_id}"
    );
    if store.is_revoked(grant_id)? {
        println!("grant {grant_id} was already revoked");
        return Ok(());
    }
    let now = now_unix();
    store.revoke(grant_id, now)?;
    store.event_append(&key, "grant.revoked", &json!({"grant_id": grant_id}), now)?;
    println!("revoked grant {grant_id}");
    Ok(())
}

fn grant_ls(home: &Path, issued: bool, held: bool) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    let names: HashMap<AgentId, String> = store
        .contacts()?
        .into_iter()
        .map(|contact| (contact.agent_id, contact.name))
        .collect();
    let peer = |id: &AgentId| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    let now = now_unix();
    if issued {
        println!("issued:");
        for g in store.grants_issued()? {
            let status = if store.is_revoked(&g.grant.grant_id)? {
                "revoked"
            } else {
                validity(&g.grant, now)
            };
            println!(
                "  {}  to {}",
                describe(&g.grant, status),
                peer(&g.grant.subject)
            );
        }
    }
    if held {
        println!("held:");
        for g in store.grants_held()? {
            println!(
                "  {}  from {}",
                describe(&g.grant, validity(&g.grant, now)),
                peer(&g.grant.issuer)
            );
        }
    }
    Ok(())
}

/// One line about `g`: its id, `status`, project, operations, resources and
/// expiry.
fn describe(g: &Grant, status: &str) -> String {
    format!(
        "{}  {status}  project {}  {} on {}  until {}",
        g.grant_id,
        g.project,
        operation_names(&g.operations),
        g.resources.join(","),
        g.expires_at
    )
}

/// `expired` once `g` has expired at `now`, else `active`.
fn validity(g: &Grant, now: i64) -> &'static str {
    match parse_timestamp(&g.expires_at) {
        Some(at) if now < at => "active",
        _ => "expired",
    }
}

fn operation_names(operations: &[Operation]) -> String {
    operations
        .iter()
        .map(|op| serde_json::to_value(op).expect("an operation serializes"))
        .map(|name| name.as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

/// The operations of a comma-separated `--ops` list, by their grant names.
fn parse_operations(list: &str) -> anyhow::Result<Vec<Operation>> {
    let mut operations = Vec::new();
    for name in parse_list(list) {
        let op: Operation = serde_json::from_value(Value::String(name.clone())).map_err(|_| {
            anyhow!(
                "unknown operation {name} in --ops: use task.send, task.read, task.cancel or \
                 blob.fetch"
            )
        })?;
        if !operations.contains(&op) {
            operations.push(op);
        }
    }
    ensure!(!operations.is_empty(), "--ops lists no operation");
    Ok(operations)
}

/// The non-empty, distinct items of a comma-separated list, trimmed.
fn parse_list(list: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for item in list
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        if !items.iter().any(|known| known == item) {
            items.push(item.to_owned());
        }
    }
    items
}

/// The seconds of a `--ttl` such as `90s`, `30m`, `24h` or `7d`.
fn parse_ttl(text: &str) -> anyhow::Result<i64> {
    let invalid = || anyhow!("invalid --ttl {text:?}: use a number and s, m, h or d, such as 24h");
    let unit = text.chars().last().ok_or_else(invalid)?;
    let number = &text[..text.len() - unit.len_utf8()];
    let seconds_per_unit = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(invalid()),
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let ttl = number
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(seconds_per_unit))
        .ok_or_else(|| anyhow!("--ttl {text} is too long"))?;
    ensure!(ttl > 0, "--ttl {text} must be positive");
    Ok(ttl)
}

/// `g-` and 32 random lowercase hex digits.
fn new_grant_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random number generator failed");
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("g-{hex}")
}

/// A plain account of why signing a grant failed. `issue` reports in the
/// verifier's terms what a verifier would refuse; here the grant is ours.
fn issue_failure(err: GrantError) -> anyhow::Error {
    match err {
        GrantError::TooLarge => anyhow!(
            "the grant would be larger than the {MAX_TOKEN_BYTES} bytes a token may have: list \
             fewer or shorter resources"
        ),
        GrantError::IssuerNotAccepted => anyhow!("the grant's issuer is not this agent's key"),
        GrantError::Malformed => anyhow!("the grant's validity instants are out of range"),
        other => anyhow!("cannot sign the grant ({other})"),
    }
}

/// The configuration in `home`, which `pals init` creates.
fn load_config(home: &Path) -> anyhow::Result<Config> {
    let config_path = home.join(CONFIG_FILE);
    if !config_path.exists() {
        bail!("{} does not exist; run `pals init`", config_path.display());
    }
    Config::load(&config_path)
}

/// The configuration and the key of the agent in `home`, which `pals init`
/// creates.
fn load_agent(home: &Path) -> anyhow::Result<(Config, AgentKey)> {
    let cfg = load_config(home)?;
    let key_path = cfg.key_path();
    if !key_path.exists() {
        bail!("{} does not exist; run `pals init`", key_path.display());
    }
    let key = AgentKey::load_or_create(&key_path)
        .with_context(|| format!("cannot load {}", key_path.display()))?;
    Ok((cfg, key))
}

/// The agent's store, opened without the daemon's lock: a running daemon
/// shares it.
fn open_store(cfg: &Config, key: &AgentKey) -> anyhow::Result<Store> {
    Store::open(&cfg.data_dir.join(STORE_FILE), key.id())
}

/// A Tokio runtime: on several threads for the daemon, else on this one.
fn runtime(multi_thread: bool) -> anyhow::Result<tokio::runtime::Runtime> {
    let mut builder = if multi_thread {
        tokio::runtime::Builder::new_multi_thread()
    } else {
        tokio::runtime::Builder::new_current_thread()
    };
    builder
        .enable_all()
        .build()
        .context("cannot start the async runtime")
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_takes_a_positive_number_and_a_unit() {
        assert_eq!(parse_ttl("90s").unwrap(), 90);
        assert_eq!(parse_ttl("30m").unwrap(), 1800);
        assert_eq!(parse_ttl("24h").unwrap(), 86_400);
        assert_eq!(parse_ttl("7d").unwrap(), 604_800);
        for bad in [
            "", "h", "24", "24x", "-1h", "+1h", "1.5h", "1 h", "24hh", "ñ",
        ] {
            assert!(parse_ttl(bad).is_err(), "{bad:?}");
        }
        assert!(format!("{:#}", parse_ttl("0h").unwrap_err()).contains("positive"));
        assert!(parse_ttl("99999999999999999999d").is_err());
    }

    #[test]
    fn operations_use_the_grant_names() {
        assert_eq!(
            parse_operations("task.send, task.read,task.send,,blob.fetch").unwrap(),
            [
                Operation::TaskSend,
                Operation::TaskRead,
                Operation::BlobFetch
            ]
        );
        assert!(parse_operations("").is_err());
        assert!(parse_operations(" , ").is_err());
        let err = format!("{:#}", parse_operations("task.send,TaskSend").unwrap_err());
        assert!(err.contains("TaskSend"), "{err}");
        assert_eq!(
            operation_names(&[Operation::TaskCancel, Operation::BlobFetch]),
            "task.cancel,blob.fetch"
        );
    }

    #[test]
    fn grant_ids_are_random_and_header_safe() {
        let id = new_grant_id();
        assert!(is_header_safe(&id), "{id}");
        assert_eq!(id.len(), 34);
        assert_ne!(new_grant_id(), id);
    }
}
