//! The `pals` command line.
//!
//! Only `pals daemon` takes the data directory's lock; every other command
//! opens the store directly, which a running daemon shares. `pals ask`,
//! `pals task resume|cancel` and `pals card show <peer>` reach peers through
//! the running daemon's gateway.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail, ensure};
use clap::{Parser, Subcommand};
use http_body_util::{BodyExt, Empty, Full, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use iroh_tickets::endpoint::EndpointTicket;
use pals::a2a::CardSet;
use pals::config::{self, CONFIG_FILE, Config, INIT_TEMPLATE, INVENTORY_TEMPLATE, is_header_safe};
use pals::gateway::CARD_HASH_HEADER;
use pals::inventory::InventoryState;
use pals::net::agent_id_of;
use pals::node::{Node, STORE_FILE};
use pals::outbox::{AskError, AskOutcome, AskRequest, Outbox};
use pals_core::card_jws::{CardSigner, verify_card};
use pals_core::grant::{
    GRANT_SCHEMA, Grant, GrantError, MAX_TOKEN_BYTES, Operation, SignedGrant, format_timestamp,
    parse_timestamp,
};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::view::{INVENTORY_SECTIONS, render};
use pals_core::inventory::{Scope, Section};
use pals_core::store::{Contact, Store};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

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
    /// Create config.toml, inventory.toml and agent.key, keeping any that
    /// already exist
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
    /// Ask a peer a question, through the running daemon, and wait for the
    /// answer. Exits 0 when the task completes; 2 when it fails, is
    /// rejected or canceled, or the request is refused; 3 when the peer
    /// cannot be reached or the wait ends first, and the question is kept
    /// for `pals task resume`
    Ask {
        /// The peer, by contact name or agent id
        peer: String,
        /// The question
        question: String,
        /// A file to send with the question; repeat for more
        #[arg(long, value_name = "FILE")]
        attach: Vec<PathBuf>,
        /// How long to wait for the answer: a number and s, m, h or d
        #[arg(long, default_value = "600s")]
        wait: String,
    },
    /// The questions this agent asked its peers
    #[command(subcommand)]
    Task(TaskCommand),
    /// Agent Cards: this agent's, and its peers'
    #[command(subcommand)]
    Card(CardCommand),
    /// This agent's inventory
    #[command(subcommand)]
    Inventory(InventoryCommand),
}

#[derive(Subcommand)]
enum InventoryCommand {
    /// Print what a peer of a scope sees of this agent's inventory, rendered
    /// here from the inventory file: the entries of its view in one piece,
    /// unsigned, as JSON
    Show {
        /// The scope: public (any peer), project:<project> (a grant for
        /// task.read or task.cancel on skill:ask in that project) or
        /// task:<project> (one for task.send)
        #[arg(long, default_value = "public")]
        scope: String,
        /// The sections, comma separated: services, skills, plugins, mcp,
        /// runtime, models, information, hardware, network [default: all]
        #[arg(long, value_delimiter = ',')]
        sections: Vec<String>,
    },
}

#[derive(Subcommand)]
enum CardCommand {
    /// Print a signed Agent Card, then `verified: <agent id> <card hash>`:
    /// this agent's own, as its daemon signs it from config.toml and the
    /// inventory, or a peer's, read and verified through the running
    /// daemon's gateway
    Show {
        /// The peer, by contact name or agent id; this agent when left out
        peer: Option<String>,
        /// The peer's extended card instead, as this agent's grant lets it
        /// see it: the view the gateway checked, and the hash of the card it
        /// verified
        #[arg(long, requires = "peer")]
        extended: bool,
    },
}

#[derive(Subcommand)]
enum TaskCommand {
    /// List the questions asked, oldest first: message id, state, peer,
    /// task id and attempts
    Ls,
    /// Take up a question again: send it if it never reached the peer, wait
    /// for its task if it did; never runs it twice. Exits as `pals ask` does
    Resume {
        message_id: String,
        /// How long to wait for the answer: a number and s, m, h or d
        #[arg(long, default_value = "600s")]
        wait: String,
    },
    /// Cancel the peer's task for a question
    Cancel { message_id: String },
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

/// The exit code of an ask whose task failed, was rejected or canceled, or
/// whose request was refused.
const EXIT_NOT_COMPLETED: u8 = 2;
/// The exit code of an ask that got no outcome: the peer could not be
/// reached, or the wait ended first.
const EXIT_NO_OUTCOME: u8 = 3;

fn main() -> ExitCode {
    // Before any HTTP client is built.
    pals::install_crypto_provider();
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let home = cli.home.unwrap_or_else(config::default_home);
    let home =
        std::path::absolute(&home).with_context(|| format!("cannot resolve {}", home.display()))?;
    let done = match cli.command {
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
        Command::Ask {
            peer,
            question,
            attach,
            wait,
        } => return ask(&home, peer, question, attach, &wait),
        Command::Task(TaskCommand::Ls) => task_ls(&home),
        Command::Task(TaskCommand::Resume { message_id, wait }) => {
            return task_resume(&home, &message_id, &wait);
        }
        Command::Task(TaskCommand::Cancel { message_id }) => {
            return task_cancel(&home, &message_id);
        }
        Command::Card(CardCommand::Show { peer, extended }) => {
            card_show(&home, peer.as_deref(), extended)
        }
        Command::Inventory(InventoryCommand::Show { scope, sections }) => {
            inventory_show(&home, &scope, &sections)
        }
    };
    done.map(|()| ExitCode::SUCCESS)
}

fn init(home: &Path) -> anyhow::Result<()> {
    create_private_dir(home)?;
    let config_path = home.join(CONFIG_FILE);
    write_new(&config_path, INIT_TEMPLATE)?;
    let cfg = Config::load(&config_path)?;
    write_new(&cfg.inventory.file, INVENTORY_TEMPLATE)?;

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

/// Writes `contents` to a new file at `path`, keeping one that exists.
fn write_new(path: &Path, contents: &str) -> anyhow::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(contents.as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("created {}", path.display());
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            println!("kept the existing {}", path.display());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("cannot create {}", path.display()));
        }
    }
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

/// The largest answer `pals card show` reads from the gateway.
const MAX_CARD_ANSWER_BYTES: usize = 256 * 1024;

/// Prints a signed card and the agent it was verified as signed by: this
/// agent's own when `peer` is `None`, else `peer`'s public card, or its
/// extended card's gateway view with `extended`.
fn card_show(home: &Path, peer: Option<&str>, extended: bool) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let store = Arc::new(open_store(&cfg, &key)?);
    let Some(peer) = peer else {
        let inventory = InventoryState::load(&cfg, Arc::clone(&store))?.snapshot();
        let key = Arc::new(key);
        let cards = CardSet::build(
            &cfg,
            &CardSigner::new(Arc::clone(&key)),
            &inventory.inventory,
        )?;
        let card = &cards.public().json;
        let verified = verify_card(card, &key.id())
            .map_err(|err| anyhow!("this agent's own card does not verify: {err}"))?;
        print_card(card, &key.id(), verified.hash.as_str())?;
        return Ok(());
    };
    let peer_id = match peer.parse::<AgentId>() {
        Ok(id) => id,
        Err(_) => {
            store
                .contact_by_name(peer)?
                .ok_or_else(|| {
                    anyhow!("unknown contact {peer}: save it first with `pals contact add`")
                })?
                .agent_id
        }
    };
    let listen = cfg.gateway.listen;
    runtime(false)?.block_on(async move {
        let health = gateway_health(listen).await.with_context(|| {
            format!("no pals daemon answers at http://{listen}: start it with `pals daemon`")
        })?;
        let running = health["agent_id"].as_str().unwrap_or_default();
        ensure!(
            running == key.id().to_string(),
            "the pals daemon at http://{listen} runs agent {running}, not this agent {}",
            key.id()
        );
        if extended {
            let call =
                json!({"jsonrpc": "2.0", "id": 1, "method": "GetExtendedAgentCard", "params": {}});
            let path = format!("/peers/{peer_id}/jsonrpc");
            let (status, headers, body) =
                gateway_call(listen, http::Method::POST, &path, Some(&call)).await?;
            let answer = gateway_json(status, &body)?;
            if let Some(error) = answer.get("error") {
                bail!("{peer} refused its extended card: {error}");
            }
            let card = answer
                .get("result")
                .ok_or_else(|| anyhow!("the gateway answered no card: {answer}"))?;
            let hash = headers
                .get(CARD_HASH_HEADER)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| anyhow!("the gateway did not say which card it verified"))?;
            print_card(card, &peer_id, hash)
        } else {
            let path = format!("/peers/{peer_id}/signed-agent-card.json");
            let (status, _, body) = gateway_call(listen, http::Method::GET, &path, None).await?;
            let card = gateway_json(status, &body)?;
            let verified = verify_card(&card, &peer_id)
                .map_err(|err| anyhow!("the card of {peer} does not verify: {err}"))?;
            print_card(&card, &peer_id, verified.hash.as_str())
        }
    })
}

/// Prints the entries `scope` sees of this agent's inventory in `sections`
/// (all when empty), as the daemon renders them for a peer of that scope,
/// with the observations it starts with: unsigned and in one piece, without
/// the `seq`, pages and signature that only an answer to a peer carries.
fn inventory_show(home: &Path, scope: &str, sections: &[String]) -> anyhow::Result<()> {
    let scope = Scope::from_key(scope).ok_or_else(|| {
        anyhow!("unknown scope {scope:?}: public, project:<project> or task:<project>")
    })?;
    let mut sections = if sections.is_empty() {
        INVENTORY_SECTIONS.to_vec()
    } else {
        sections
            .iter()
            .map(|name| {
                serde_json::from_value::<Section>(json!(name))
                    .ok()
                    .filter(|section| INVENTORY_SECTIONS.contains(section))
                    .ok_or_else(|| {
                        anyhow!(
                            "unknown section {name:?}: services, skills, plugins, mcp, runtime, \
                             models, information, hardware or network"
                        )
                    })
            })
            .collect::<anyhow::Result<_>>()?
    };
    sections.sort_unstable();
    sections.dedup();

    let (cfg, key) = load_agent(home)?;
    let store = Arc::new(open_store(&cfg, &key)?);
    let snapshot = InventoryState::load(&cfg, store)?.snapshot();
    let view = json!({
        "scope": scope.key(),
        "sections": sections,
        "entries": render(&snapshot, &scope, &sections),
    });
    println!("{}", serde_json::to_string_pretty(&view)?);
    Ok(())
}

/// Prints `card`, then `verified: <signer> <hash>`.
fn print_card(card: &Value, signer: &AgentId, hash: &str) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(card)?);
    println!("verified: {signer} {hash}");
    Ok(())
}

/// The JSON of a gateway answer of `status`; its error when it is not a
/// success.
fn gateway_json(status: http::StatusCode, body: &[u8]) -> anyhow::Result<Value> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    if !status.is_success() {
        let reason = json
            .as_ref()
            .and_then(|json| json.get("error"))
            .map_or_else(
                || String::from_utf8_lossy(body).into_owned(),
                Value::to_string,
            );
        bail!("the gateway answered {status}: {reason}");
    }
    json.ok_or_else(|| anyhow!("the gateway answered something that is not JSON"))
}

/// `method path` of the gateway listening on `listen`, with `body` as JSON:
/// the status, headers and body of its answer, read up to
/// [`MAX_CARD_ANSWER_BYTES`].
async fn gateway_call(
    listen: SocketAddr,
    method: http::Method,
    path: &str,
    body: Option<&Value>,
) -> anyhow::Result<(http::StatusCode, http::HeaderMap, Bytes)> {
    const TIMEOUT: Duration = Duration::from_secs(60);
    let exchange = async {
        let stream = tokio::net::TcpStream::connect(listen).await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(connection);
        let mut request = http::Request::builder()
            .method(method)
            .uri(path)
            .header(http::header::HOST, listen.to_string())
            .header("a2a-version", "1.0");
        let bytes = match body {
            Some(body) => {
                request = request.header(http::header::CONTENT_TYPE, "application/json");
                Bytes::from(serde_json::to_vec(body)?)
            }
            None => Bytes::new(),
        };
        let response = sender.send_request(request.body(Full::new(bytes))?).await?;
        let (parts, body) = response.into_parts();
        let body = Limited::new(body, MAX_CARD_ANSWER_BYTES)
            .collect()
            .await
            .map_err(|err| anyhow!("cannot read the gateway's answer: {err}"))?
            .to_bytes();
        Ok((parts.status, parts.headers, body))
    };
    tokio::time::timeout(TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("the gateway did not answer within {TIMEOUT:?}"))?
}

fn daemon(home: &Path) -> anyhow::Result<()> {
    install_log();
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
        // Listed, never sent again on their own.
        for row in node.outbox().pending()? {
            eprintln!(
                "pals daemon: question {} to {} is still pending: `pals task resume {}` takes \
                 it up again",
                row.message_id, row.peer, row.message_id
            );
        }
        let signal = stop.await;
        eprintln!("pals daemon: {signal}, shutting down");
        node.shutdown().await
    })
}

/// Logs the daemon's diagnostics, such as a blob that cannot be fetched or
/// a grant check that fails, to stderr, one compact line each: from `INFO`
/// up, unless `RUST_LOG` filters otherwise (`RUST_LOG=pals=debug`).
fn install_log() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::LevelFilter;

    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    // Only one subscriber per process; nothing else installs one.
    let _ = tracing_subscriber::fmt()
        .compact()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .try_init();
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

fn ask(
    home: &Path,
    peer: String,
    question: String,
    attachments: Vec<PathBuf>,
    wait: &str,
) -> anyhow::Result<ExitCode> {
    let wait = parse_wait(wait)?;
    let attachments = attachments
        .iter()
        .map(|path| {
            std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let request = AskRequest {
        peer: peer.clone(),
        question,
        attachments,
        wait,
    };
    with_outbox(home, |outbox| async move {
        // At once, so that an interrupted command leaves the id to resume.
        let kept = |message_id: &str| {
            eprintln!(
                "pals: message {message_id} kept; resume with: pals task resume {message_id}"
            );
        };
        let asked = outbox
            .ask_kept(request, &CancellationToken::new(), kept)
            .await;
        report(&peer, wait, asked)
    })
}

fn task_ls(home: &Path) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    let names: HashMap<AgentId, String> = store
        .contacts()?
        .into_iter()
        .map(|contact| (contact.agent_id, contact.name))
        .collect();
    for row in store.outbox_all()? {
        let peer = names
            .get(&row.peer)
            .cloned()
            .unwrap_or_else(|| row.peer.to_string());
        println!(
            "{}\t{}\t{}\t{}\t{}",
            row.message_id,
            row.state,
            peer,
            row.task_id.as_deref().unwrap_or("-"),
            row.attempts
        );
    }
    Ok(())
}

fn task_resume(home: &Path, message_id: &str, wait: &str) -> anyhow::Result<ExitCode> {
    let wait = parse_wait(wait)?;
    let message_id = message_id.to_owned();
    with_outbox(home, |outbox| async move {
        let resumed = outbox.resume(&message_id, wait).await;
        report("the peer", wait, resumed)
    })
}

fn task_cancel(home: &Path, message_id: &str) -> anyhow::Result<ExitCode> {
    let message_id = message_id.to_owned();
    with_outbox(home, |outbox| async move {
        match outbox.cancel(&message_id).await {
            Ok(outcome) => {
                println!(
                    "task {}: {}",
                    outcome.task_id.as_deref().unwrap_or("-"),
                    outcome.state
                );
                Ok(ExitCode::SUCCESS)
            }
            Err(err) => refused_or_unreachable(err),
        }
    })
}

/// Runs `call` with the outbox of the agent in `home`, which asks through
/// the gateway of its running daemon.
fn with_outbox<F, Fut>(home: &Path, call: F) -> anyhow::Result<ExitCode>
where
    F: FnOnce(Outbox) -> Fut,
    Fut: Future<Output = anyhow::Result<ExitCode>>,
{
    let (cfg, key) = load_agent(home)?;
    let listen = cfg.gateway.listen;
    let data_dir = cfg.data_dir.clone();
    let store = Arc::new(open_store(&cfg, &key)?);
    runtime(false)?.block_on(async move {
        let health = gateway_health(listen).await.with_context(|| {
            format!("no pals daemon answers at http://{listen}: start it with `pals daemon`")
        })?;
        let running = health["agent_id"].as_str().unwrap_or_default();
        ensure!(
            running == key.id().to_string(),
            "the pals daemon at http://{listen} runs agent {running}, not this agent {}",
            key.id()
        );
        call(Outbox::new(store, format!("http://{listen}")).with_data_dir(&data_dir)).await
    })
}

/// Prints how an ask of `peer` ended, and returns its exit code: the answer
/// and the files on stdout, and anything else on stderr.
fn report(
    peer: &str,
    wait: Duration,
    asked: Result<AskOutcome, AskError>,
) -> anyhow::Result<ExitCode> {
    let outcome = match asked {
        Ok(outcome) => outcome,
        Err(AskError::Unreachable { message_id }) => {
            eprintln!(
                "pals: {peer} could not be reached; the question is kept: \
                 `pals task resume {message_id}` tries again"
            );
            return Ok(ExitCode::from(EXIT_NO_OUTCOME));
        }
        Err(err) => return refused_or_unreachable(err),
    };
    let task = outcome.task_id.as_deref().unwrap_or("-");
    match outcome.state.as_str() {
        "TASK_STATE_COMPLETED" => {
            if let Some(answer) = &outcome.answer {
                println!("{answer}");
            }
            for file in &outcome.artifacts {
                println!("file {}: {}", file.name, file.url);
            }
            Ok(ExitCode::SUCCESS)
        }
        "TASK_STATE_SUBMITTED" | "TASK_STATE_WORKING" => {
            eprintln!(
                "pals: task {task} is still {} after {}s: `pals task resume {}` waits for it \
                 again",
                outcome.state,
                wait.as_secs(),
                outcome.message_id
            );
            Ok(ExitCode::from(EXIT_NO_OUTCOME))
        }
        state => {
            eprintln!(
                "pals: task {task} ended {state}: {}",
                outcome.answer.as_deref().unwrap_or("no reason given")
            );
            Ok(ExitCode::from(EXIT_NOT_COMPLETED))
        }
    }
}

/// The exit code, and the message on stderr, of an ask or a cancel that
/// failed.
fn refused_or_unreachable(err: AskError) -> anyhow::Result<ExitCode> {
    match err {
        AskError::Rejected(reason) => {
            eprintln!("pals: refused: {reason}");
            Ok(ExitCode::from(EXIT_NOT_COMPLETED))
        }
        AskError::Unreachable { message_id } => {
            eprintln!(
                "pals: the peer could not be reached: `pals task resume {message_id}` tries \
                 again"
            );
            Ok(ExitCode::from(EXIT_NO_OUTCOME))
        }
        AskError::Local(reason) => Err(anyhow!(reason)),
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
    parse_seconds("--ttl", text, "24h")
}

/// The duration of a `--wait` such as `90s`, `30m` or `1h`.
fn parse_wait(text: &str) -> anyhow::Result<Duration> {
    let seconds = parse_seconds("--wait", text, "600s")?;
    Ok(Duration::from_secs(u64::try_from(seconds)?))
}

/// The seconds of `flag`'s `text`: a positive number and s, m, h or d.
/// `example` is shown when `text` is not one.
fn parse_seconds(flag: &str, text: &str, example: &str) -> anyhow::Result<i64> {
    let invalid =
        || anyhow!("invalid {flag} {text:?}: use a number and s, m, h or d, such as {example}");
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
    let seconds = number
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(seconds_per_unit))
        .ok_or_else(|| anyhow!("{flag} {text} is too long"))?;
    ensure!(seconds > 0, "{flag} {text} must be positive");
    Ok(seconds)
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
    fn wait_takes_a_positive_number_and_a_unit() {
        assert_eq!(parse_wait("600s").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_wait("2m").unwrap(), Duration::from_secs(120));
        for bad in ["", "600", "0s", "-1s", "1.5m"] {
            let err = format!("{:#}", parse_wait(bad).unwrap_err());
            assert!(err.contains("--wait"), "{bad:?}: {err}");
        }
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
