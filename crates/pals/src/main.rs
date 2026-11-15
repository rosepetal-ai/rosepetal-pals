//! The `pals` command line.
//!
//! Only `pals daemon` takes the data directory's lock; every other command,
//! `pals benchmark import|ls` and `pals contact add-a2a` included, opens the
//! store directly, which a running daemon shares; `pals contact add-a2a`
//! reads the plain agent's card itself. `pals ask`, `pals task
//! resume|cancel`, `pals card show <peer>`, `pals inventory get`, `pals
//! query` and `pals discover` reach peers through the running daemon's
//! gateway; `pals ask --offer` reads the offer it names from the store
//! first, and sends nothing when it expired.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
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
use pals::discovery::cards::PUBLIC_CONTEXT;
use pals::gateway::plain::{self, fetch_contact_card};
use pals::gateway::{CARD_HASH_HEADER, held_task_grant, query_for_grant};
use pals::inventory::{InventoryState, RefreshReport};
use pals::net::agent_id_of;
use pals::node::{Node, STORE_FILE};
use pals::outbox::{
    AskError, AskOutcome, AskRequest, OfferLookupError, Outbox, PLAIN_NOT_RESENT,
    received_offer_ref,
};
use pals_core::benchmark::{BenchmarkReport, MAX_REPORT_BYTES};
use pals_core::card_jws::{CardSigner, verify_card};
use pals_core::discovery::query::{
    MAX_QUERY_BYTES, MAX_REPLY_TIMEOUT_MS, QUERY_SCHEMA, Requirements, ServiceQuery, accept_offer,
    new_query_id,
};
use pals_core::discovery::selection::{Coverage, SearchOutcome, SearchRequest, Stage};
use pals_core::grant::{
    GRANT_SCHEMA, Grant, GrantError, MAX_TOKEN_BYTES, Operation, SignedGrant, format_timestamp,
    parse_timestamp,
};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::view::{INVENTORY_SECTIONS, render, verify_page};
use pals_core::inventory::{Scope, Section, Visibility};
use pals_core::signed::{Signed, SignedError};
use pals_core::store::{Contact, KeptBenchmark, PLAIN_PEER_PREFIX, Store};
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
        /// The offer the peer made this agent to name in the question, by
        /// its id, as `pals discover` printed it; one expired by this
        /// agent's clock sends nothing and exits 2
        #[arg(long, value_name = "OFFER_ID")]
        offer: Option<String>,
    },
    /// Search the contacts for a provider of a capability, through the
    /// running daemon's gateway: verify their cards, ask the PALS peers for
    /// offers over agentnet-discovery/0, within the limits of [discovery],
    /// and print the ranking, with the id and expiry of each offer, the
    /// candidates discarded and why, the coverage and the metrics.
    /// Searching runs nothing on any peer. With
    /// --ask, the question goes to the best candidate that takes it, with
    /// its offer, and the command exits as `pals ask` does
    Discover {
        /// The capability: one of the common vocabulary, as vision.ocr, or
        /// of its provider's namespace, as example:ocr
        #[arg(long)]
        capability: String,
        /// A media type the provider must accept; repeat for more
        #[arg(long = "input", value_name = "MIME")]
        inputs: Vec<String>,
        /// A media type the provider must produce; repeat for more
        #[arg(long = "output", value_name = "MIME")]
        outputs: Vec<String>,
        /// A JSON file with the requirements: language,
        /// external_cloud_allowed, required_region and inventory, with its
        /// required and preferred ids and resources
        #[arg(long, value_name = "JSON")]
        requirements_file: Option<PathBuf>,
        /// The project the task is for: only the contacts that gave this
        /// agent a grant for it get the inventory ids of the requirements
        #[arg(long)]
        project: Option<String>,
        /// A contact to search among, by name or agent id; repeat for more
        /// [default: every contact]
        #[arg(long = "peer", value_name = "PEER")]
        peers: Vec<String>,
        /// Ask the best candidate that takes it this question
        #[arg(long, value_name = "QUESTION")]
        ask: Option<String>,
        /// With --ask, how long to wait for the answer: a number and s, m,
        /// h or d
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
    /// Benchmark reports this agent holds, its own or imported, each as its
    /// measurer signed it. Peers read them over agentnet-discovery/0, as
    /// their visibility lets them; reading one never measures anything
    #[command(subcommand)]
    Benchmark(BenchmarkCommand),
    /// Ask a peer whether it can serve a case, over agentnet-discovery/0
    /// through the running daemon's gateway, which keeps its offer: print
    /// the offer as the peer signed it for this agent, then `verified:
    /// <agent id> <offer id> <status>`. Asking runs nothing on the peer, and
    /// an offer authorizes nothing
    Query {
        /// The peer, by contact name or agent id
        peer: String,
        /// The capability: one of the common vocabulary, as vision.ocr, or
        /// of its provider's namespace, as example:ocr
        #[arg(long)]
        capability: String,
        /// A media type the peer must accept; repeat for more
        #[arg(long = "input", value_name = "MIME")]
        inputs: Vec<String>,
        /// A media type the peer must produce; repeat for more
        #[arg(long = "output", value_name = "MIME")]
        outputs: Vec<String>,
        /// A JSON file with the requirements: language,
        /// external_cloud_allowed, required_region and inventory, with its
        /// required and preferred ids and resources
        #[arg(long, value_name = "JSON")]
        requirements_file: Option<PathBuf>,
        /// The project to be answered for, among those of the grant this
        /// agent holds for the peer; another is answered as public
        #[arg(long)]
        project: Option<String>,
    },
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
    /// Print a peer's inventory, as this agent's grant lets it see it, read
    /// over agentnet-discovery/0 through the running daemon's gateway: each
    /// page as the peer signed it, then `verified: <agent id> <scope> seq
    /// <revision>`
    Get {
        /// The peer, by contact name or agent id
        peer: String,
        /// The sections, comma separated: services, skills, plugins, mcp,
        /// runtime, models, information, hardware, network [default: all]
        #[arg(long, value_delimiter = ',')]
        sections: Vec<String>,
        /// Every page, following each page's cursor, instead of the first
        #[arg(long)]
        all: bool,
    },
    /// Check now, through the running daemon, the MCP connections the
    /// inventory configures an adapter for: initialize and tools/list on
    /// each, never a tool call. Print `generation <n>`, `observed <n>, ok
    /// <n>, failed <n>`, then `failed: <id> <code>` for each one not
    /// operational. The daemon also checks them when it starts and every
    /// [inventory] refresh_interval_s
    Refresh,
}

#[derive(Subcommand)]
enum BenchmarkCommand {
    /// Keep a signed benchmark report (agentnet.benchmark-report/1) read from
    /// a JSON file, as its measurer signed it: it must verify with the key
    /// of its provenance.measured_by, whoever that is. Importing the same
    /// report again only changes who sees it; another report under an id
    /// kept already is refused
    Import {
        /// The signed report: {"report": {...}, "signature": "..."}
        file: PathBuf,
        /// Who sees it: public, project:<project>, task:<project> or
        /// private. Kept beside the report, never in it
        #[arg(long, default_value = "private")]
        visibility: String,
    },
    /// List the reports kept, by id: id, kind, profile and version, subject,
    /// measurer, measured_at, expires_at and visibility, tab separated. A
    /// row that does not read, which no peer is shown, is listed as its id,
    /// `unreadable` and why
    Ls,
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
    /// Save a plain A2A agent, one with no PALS daemon, which this agent
    /// asks and never answers: its card is read and checked now, and at
    /// every use. Only interfaces of the URL's origin are ever called; no
    /// proxy, no redirect, no files
    #[command(name = "add-a2a")]
    AddA2a {
        /// The agent's base URL, https://<host>[/<path>], whose card is at
        /// <url>/.well-known/agent-card.json, or its card's URL (ending in
        /// .json); http:// only to 127.0.0.1, [::1] or localhost
        url: String,
        /// The name to reach the agent by: letters, digits, '-', '_' and '.'
        #[arg(long)]
        name: String,
        /// The environment variable that holds the agent's bearer token,
        /// read when a request is sent; the token itself is never saved
        #[arg(long, value_name = "VAR")]
        bearer_env: Option<String>,
    },
    /// List the saved peers: name and agent id, or name and card URL for a
    /// plain A2A agent
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
        Command::Contact(ContactCommand::AddA2a {
            url,
            name,
            bearer_env,
        }) => contact_add_a2a(&home, &url, &name, bearer_env.as_deref()),
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
            offer,
        } => return ask(&home, peer, question, attach, &wait, offer.as_deref()),
        Command::Discover {
            capability,
            inputs,
            outputs,
            requirements_file,
            project,
            peers,
            ask,
            wait,
        } => {
            let search = Searched {
                asked: Asked {
                    capability,
                    inputs,
                    outputs,
                    requirements_file,
                    project,
                },
                peers,
            };
            return discover(&home, search, ask, &wait);
        }
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
        Command::Inventory(InventoryCommand::Get {
            peer,
            sections,
            all,
        }) => inventory_get(&home, &peer, &sections, all),
        Command::Inventory(InventoryCommand::Refresh) => inventory_refresh(&home),
        Command::Benchmark(BenchmarkCommand::Import { file, visibility }) => {
            benchmark_import(&home, &file, &visibility)
        }
        Command::Benchmark(BenchmarkCommand::Ls) => benchmark_ls(&home),
        Command::Query {
            peer,
            capability,
            inputs,
            outputs,
            requirements_file,
            project,
        } => {
            let asked = Asked {
                capability,
                inputs,
                outputs,
                requirements_file,
                project,
            };
            query(&home, &peer, asked)
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

/// The largest answer `pals card show` and `pals inventory get` read from
/// the gateway: a card or a page, re-encoded in ASCII.
const MAX_GATEWAY_ANSWER_BYTES: usize = 256 * 1024;

/// The most pages `pals inventory get --all` follows: far more than a chain
/// of cursors serves within its 1 MiB.
const MAX_PAGES: usize = 1024;

/// The agent `peer` names: an agent id, or a contact's name.
fn peer_id(store: &Store, peer: &str) -> anyhow::Result<AgentId> {
    match peer.parse::<AgentId>() {
        Ok(id) => Ok(id),
        Err(_) => Ok(store
            .contact_by_name(peer)?
            .ok_or_else(|| {
                anyhow!("unknown contact {peer}: save it first with `pals contact add`")
            })?
            .agent_id),
    }
}

/// Fails unless a daemon of the agent `id` answers at `listen`.
async fn this_daemon(listen: SocketAddr, id: &AgentId) -> anyhow::Result<()> {
    let health = gateway_health(listen).await.with_context(|| {
        format!("no pals daemon answers at http://{listen}: start it with `pals daemon`")
    })?;
    let running = health["agent_id"].as_str().unwrap_or_default();
    ensure!(
        running == id.to_string(),
        "the pals daemon at http://{listen} runs agent {running}, not this agent {id}"
    );
    Ok(())
}

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
    let peer_id = peer_id(&store, peer)?;
    let listen = cfg.gateway.listen;
    runtime(false)?.block_on(async move {
        this_daemon(listen, &key.id()).await?;
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
    let sections = inventory_sections(sections)?;
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

/// The inventory sections `names` names, in order and without repeats; all
/// of them when it names none.
fn inventory_sections(names: &[String]) -> anyhow::Result<Vec<Section>> {
    let mut sections = if names.is_empty() {
        INVENTORY_SECTIONS.to_vec()
    } else {
        names
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
    Ok(sections)
}

/// Prints the pages of `peer`'s inventory view of `sections` (all when
/// empty) the running daemon's gateway reads, verified here again as the
/// peer's pages for this agent: the first, or with `all` every page of the
/// chain of cursors. Each page is printed as the peer signed it, then
/// `verified: <agent id> <scope> seq <revision>`.
fn inventory_get(home: &Path, peer: &str, sections: &[String], all: bool) -> anyhow::Result<()> {
    let sections = inventory_sections(sections)?;
    let names: Vec<String> = sections
        .iter()
        .map(|section| match serde_json::to_value(section) {
            Ok(Value::String(name)) => name,
            other => unreachable!("a section is named by a string, not {other:?}"),
        })
        .collect();
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    let peer_id = peer_id(&store, peer)?;
    let (listen, own) = (cfg.gateway.listen, key.id());
    runtime(false)?.block_on(async move {
        this_daemon(listen, &own).await?;
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut path = format!(
                "/discovery/inventory/{peer_id}?sections={}",
                names.join(",")
            );
            if let Some(cursor) = &cursor {
                let encoded = percent_encoding::utf8_percent_encode(
                    cursor,
                    percent_encoding::NON_ALPHANUMERIC,
                );
                path.push_str(&format!("&cursor={encoded}"));
            }
            let (status, _, body) = gateway_call(listen, http::Method::GET, &path, None).await?;
            let signed = gateway_json(status, &body)?;
            // The JCS of what the gateway re-encoded is what the peer signed.
            let page = verify_page(
                &pals_core::jcs::canonical(&signed),
                &peer_id,
                Some(&own),
                now_unix(),
            )
            .map_err(|err| anyhow!("a page of {peer} does not verify: {err}"))?;
            println!("{}", serde_json::to_string_pretty(&signed)?);
            println!("verified: {peer_id} {} seq {}", page.scope, page.seq);
            match page.next_cursor {
                Some(next) if all => cursor = Some(next),
                _ => return Ok(()),
            }
        }
        bail!("{peer} keeps sending pages: stopped after {MAX_PAGES}")
    })
}

/// How long `pals inventory refresh` waits for the daemon: its adapters, at
/// once, each within its time, and the kill of a stdio server.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs the inventory adapters of the running daemon now (`POST
/// /inventory/refresh` of its gateway) and prints their report: only fixed
/// codes, which the daemon's log details.
fn inventory_refresh(home: &Path) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let (listen, own) = (cfg.gateway.listen, key.id());
    runtime(false)?.block_on(async move {
        this_daemon(listen, &own).await?;
        let (status, _, body) = gateway_call_within(
            listen,
            http::Method::POST,
            "/inventory/refresh",
            None,
            REFRESH_TIMEOUT,
            MAX_GATEWAY_ANSWER_BYTES,
        )
        .await?;
        let report: RefreshReport = serde_json::from_value(gateway_json(status, &body)?)
            .context("the gateway answered something that is not a refresh report")?;
        println!("generation {}", report.generation);
        println!(
            "observed {}, ok {}, failed {}",
            report.observed,
            report.ok,
            report.failed.len()
        );
        for (id, code) in &report.failed {
            println!("failed: {} {}", shown(id), shown(code));
        }
        Ok(())
    })
}

/// The largest file `pals benchmark import` reads: a signed report is at
/// most [`MAX_REPORT_BYTES`] once canonical, whatever whitespace its file
/// has.
const MAX_REPORT_FILE_BYTES: u64 = 1024 * 1024;

/// Keeps the signed report in `file`, seen by `visibility`, as its measurer
/// signed it. The file may spell it any way JSON allows: what is signed is
/// the JCS of the report.
fn benchmark_import(home: &Path, file: &Path, visibility: &str) -> anyhow::Result<()> {
    let visibility = parse_visibility(visibility)?;
    let mut bytes = Vec::new();
    fs::File::open(file)
        .with_context(|| format!("cannot read {}", file.display()))?
        .take(MAX_REPORT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", file.display()))?;
    ensure!(
        bytes.len() as u64 <= MAX_REPORT_FILE_BYTES,
        "{} is larger than {MAX_REPORT_FILE_BYTES} bytes: not a benchmark report",
        file.display()
    );
    let json: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not JSON", file.display()))?;
    let report =
        Signed::<BenchmarkReport>::from_json(&pals_core::jcs::canonical(&json)).map_err(|err| {
            match err {
                SignedError::BadSignature => anyhow!(
                    "the report in {} is not signed by its measurer (provenance.measured_by)",
                    file.display()
                ),
                SignedError::TooLarge => anyhow!(
                    "the report in {} is larger than {MAX_REPORT_BYTES} bytes: raw results go \
                 as evidence, a hash and a blob reference",
                    file.display()
                ),
                other => anyhow!(
                    "{} is not a benchmark report (agentnet.benchmark-report/1): {other}",
                    file.display()
                ),
            }
        })?;
    let (cfg, key) = load_agent(home)?;
    open_store(&cfg, &key)?.benchmark_put(&report, &visibility, now_unix())?;
    let doc = &report.doc;
    println!(
        "kept {}: {} of {}, measured by {}, {}",
        doc.benchmark_id,
        doc.measurement_kind.as_str(),
        doc.subject.key(),
        doc.provenance.measured_by,
        visibility_name(&visibility)
    );
    Ok(())
}

/// Lists the reports kept, one per line, those whose row does not read
/// too ([`benchmark_line`]).
fn benchmark_ls(home: &Path) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    for kept in open_store(&cfg, &key)?.benchmarks()? {
        println!("{}", benchmark_line(&kept));
    }
    Ok(())
}

/// The line `pals benchmark ls` prints for `kept`, tab separated: id, kind,
/// profile and version, subject, measurer, measured_at, expires_at and
/// visibility; or, for a row that does not read, its id, `unreadable` and
/// why, so that no peer sees it and the operator does.
fn benchmark_line(kept: &KeptBenchmark) -> String {
    match &kept.report {
        Ok((report, visibility)) => {
            let doc = &report.doc;
            format!(
                "{}\t{}\t{} {}\t{}\t{}\t{}\t{}\t{}",
                doc.benchmark_id,
                doc.measurement_kind.as_str(),
                doc.procedure.profile,
                doc.procedure.profile_version,
                doc.subject.key(),
                doc.provenance.measured_by,
                doc.measured_at,
                doc.expires_at,
                visibility_name(visibility)
            )
        }
        Err(why) => format!("{}\tunreadable\t{why}", kept.benchmark_id),
    }
}

/// The visibility `text` names: `public`, `private`, `project:<project>` or
/// `task:<project>`.
fn parse_visibility(text: &str) -> anyhow::Result<Visibility> {
    match (text, Scope::from_key(text)) {
        ("private", _) => Ok(Visibility::Private),
        (_, Some(Scope::Public)) => Ok(Visibility::Public),
        (_, Some(Scope::Project(project))) => Ok(Visibility::Project(vec![project])),
        (_, Some(Scope::Task(project))) => Ok(Visibility::Task(vec![project])),
        (_, None) => bail!(
            "unknown visibility {text:?}: public, private, project:<project> or task:<project>"
        ),
    }
}

/// `visibility` as `--visibility` names it, its projects comma separated.
fn visibility_name(visibility: &Visibility) -> String {
    match visibility {
        Visibility::Private => "private".to_owned(),
        Visibility::Public => "public".to_owned(),
        Visibility::Project(projects) => format!("project:{}", projects.join(",")),
        Visibility::Task(projects) => format!("task:{}", projects.join(",")),
    }
}

/// What `pals query` asks.
struct Asked {
    capability: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    requirements_file: Option<PathBuf>,
    project: Option<String>,
}

/// Asks `peer`, through the running daemon's gateway, for an offer for the
/// query `asked` makes, and prints it as the peer signed it once it is
/// accepted here too ([`accept_offer`]), then `verified: <agent id> <offer
/// id> <status>`. The query is checked before anything is asked.
fn query(home: &Path, peer: &str, asked: Asked) -> anyhow::Result<()> {
    let requirements = read_requirements(asked.requirements_file.as_deref())?;
    let query = ServiceQuery {
        schema: QUERY_SCHEMA.to_owned(),
        query_id: new_query_id(),
        capability: asked.capability,
        input_media_types: asked.inputs,
        output_media_types: asked.outputs,
        requirements,
        summary: None,
        reply_timeout_ms: Some(MAX_REPLY_TIMEOUT_MS),
        requested_inventory_sections: Vec::new(),
        project: asked.project,
        execution_authorized: false,
    };
    query
        .validate()
        .map_err(|err| anyhow!("invalid query: {err}"))?;
    ensure!(
        query.to_jcs().len() <= MAX_QUERY_BYTES,
        "the query is larger than {MAX_QUERY_BYTES} bytes: ask for fewer things"
    );
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    let peer_id = peer_id(&store, peer)?;
    // As the gateway sends it, so that the offer is for it.
    let grant = held_task_grant(&store, &peer_id, query.project.as_deref(), now_unix())?;
    let sent = query_for_grant(query.clone(), grant.as_ref());
    if sent != query {
        match &query.project {
            Some(project) => eprintln!(
                "pals: this agent holds no grant of {}'s for project {}: the query goes \
                 without the project and without inventory ids",
                shown(peer),
                shown(project)
            ),
            None => eprintln!(
                "pals: the query names no project, so it goes without inventory ids: they go \
                 only with a grant for the project"
            ),
        }
    }
    let query = sent;
    let (listen, own) = (cfg.gateway.listen, key.id());
    runtime(false)?.block_on(async move {
        this_daemon(listen, &own).await?;
        let path = format!("/discovery/query/{peer_id}");
        let body = serde_json::to_value(&query)?;
        let (status, _, answer) =
            gateway_call(listen, http::Method::POST, &path, Some(&body)).await?;
        let signed = gateway_json(status, &answer)?;
        // The JCS of what the gateway re-encoded is what the peer signed.
        let offer = accept_offer(
            &pals_core::jcs::canonical(&signed),
            &peer_id,
            &own,
            &query,
            now_unix(),
        )
        .map_err(|err| anyhow!("the offer of {peer} is refused: {err}"))?;
        let status = match serde_json::to_value(offer.doc.status)? {
            Value::String(status) => status,
            other => unreachable!("a status is named by a string, not {other}"),
        };
        println!("{}", serde_json::to_string_pretty(&signed)?);
        println!("verified: {peer_id} {} {status}", offer.doc.offer_id);
        Ok(())
    })
}

/// The requirements in the JSON file at `path`; none without one.
fn read_requirements(path: Option<&Path>) -> anyhow::Result<Requirements> {
    let Some(path) = path else {
        return Ok(Requirements::default());
    };
    let text = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_slice::<Requirements>(&text)
        .with_context(|| format!("invalid requirements in {}", path.display()))
}

/// What `pals discover` searches for.
struct Searched {
    asked: Asked,
    peers: Vec<String>,
}

/// The longest answer of the gateway to a search.
const MAX_SEARCH_ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// Searches the contacts for a provider through the running daemon's
/// gateway (`POST /discovery/search`) and prints the outcome; with `ask`,
/// asks the one chosen and exits as `pals ask` does.
fn discover(
    home: &Path,
    searched: Searched,
    ask: Option<String>,
    wait: &str,
) -> anyhow::Result<ExitCode> {
    let wait = parse_wait(wait)?;
    let Searched { asked, peers } = searched;
    let search = SearchRequest {
        requirements: read_requirements(asked.requirements_file.as_deref())?,
        capability: asked.capability,
        input_media_types: asked.inputs,
        output_media_types: asked.outputs,
        project: asked.project,
        peers: (!peers.is_empty()).then_some(peers),
        ..SearchRequest::default()
    };
    let (cfg, key) = load_agent(home)?;
    let (listen, own) = (cfg.gateway.listen, key.id());
    runtime(false)?.block_on(async move {
        this_daemon(listen, &own).await?;
        let mut body = json!({ "search": search });
        // A search takes at most a minute; an ask, its wait too.
        let mut timeout = Duration::from_secs(120);
        if let Some(question) = &ask {
            body["ask"] = json!(question);
            body["wait_s"] = json!(wait.as_secs());
            timeout += wait;
        }
        let (status, _, answer) = gateway_call_within(
            listen,
            http::Method::POST,
            "/discovery/search",
            Some(&body),
            timeout,
            MAX_SEARCH_ANSWER_BYTES,
        )
        .await?;
        let answer = gateway_json(status, &answer)?;
        if ask.is_none() {
            print_search(&serde_json::from_value(answer)?);
            return Ok(ExitCode::SUCCESS);
        }
        print_search(&serde_json::from_value(answer["search"].clone())?);
        if let Some(chosen) = answer.get("chosen") {
            let peer = shown(chosen["candidate"]["peer"].as_str().unwrap_or_default());
            print_tried(&chosen["tried"]);
            println!("asked: {peer}");
            let outcome: AskOutcome = serde_json::from_value(chosen["outcome"].clone())?;
            return report(&peer, wait, Ok(outcome));
        }
        print_tried(&answer["tried"]);
        let error = shown(
            answer["error"]
                .as_str()
                .unwrap_or("no candidate took the task"),
        );
        if let Some(message_id) = answer["kept"].as_str() {
            eprintln!(
                "pals: {error}; the question is kept: `pals task resume {}` tries again",
                shown(message_id)
            );
            return Ok(ExitCode::from(EXIT_NO_OUTCOME));
        }
        eprintln!("pals: {error}");
        Ok(ExitCode::from(EXIT_NOT_COMPLETED))
    })
}

/// Prints a search's ranking, its discards with why, its coverage and its
/// metrics, one line each; a candidate that made an offer ends its line
/// with `, offer <offer id> until <expires_at>`, the id `pals ask --offer`
/// takes. What a peer named is printed as printable ASCII.
fn print_search(outcome: &SearchOutcome) {
    println!("ranked:");
    for (n, candidate) in outcome.ranked.iter().enumerate() {
        let status = candidate.offer.as_ref().map_or_else(
            || "card only".to_owned(),
            |offer| match serde_json::to_value(offer.status) {
                Ok(Value::String(status)) => status,
                _ => "unknown".to_owned(),
            },
        );
        let time = candidate
            .offer
            .as_ref()
            .and_then(|offer| {
                offer
                    .estimated_start_delay_ms?
                    .checked_add(offer.estimated_duration_ms?)
            })
            .map_or_else(|| "unknown".to_owned(), |ms| format!("{ms} ms"));
        let benchmark = candidate.benchmark.as_ref().map_or_else(
            || "none".to_owned(),
            |(kind, value)| format!("{} {value}", shown(kind)),
        );
        let who = candidate
            .agent_id
            .map_or_else(|| "plain A2A agent".to_owned(), |id| format!("agent {id}"));
        let grant = match (candidate.agent_id, candidate.grant_held) {
            (None, _) => "no grant needed",
            (Some(_), true) => "grant held",
            (Some(_), false) => "no grant held",
        };
        let offer = candidate.offer.as_ref().map_or_else(String::new, |offer| {
            format!(
                ", offer {} until {}",
                shown(&offer.offer_id),
                shown(&offer.expires_at)
            )
        });
        println!(
            "  {}. {} ({who}): {status}, skill {}, time {time}, preferred met {}, benchmark \
             {benchmark}, completed {} failed {}, {grant}, card {}{offer}",
            n + 1,
            shown(&candidate.peer),
            shown(candidate.skill_id.as_deref().unwrap_or("-")),
            candidate.preferred_met,
            candidate.experience.0,
            candidate.experience.1,
            shown(&candidate.card_signature.to_string()),
        );
    }
    println!("discarded:");
    for discarded in &outcome.discarded {
        let stage = match discarded.stage {
            Stage::Card => "card",
            Stage::Offer => "offer",
        };
        println!(
            "  {} ({stage}): {}",
            shown(&discarded.peer),
            shown(&discarded.reason)
        );
    }
    match &outcome.coverage {
        Coverage::Complete => println!("coverage: complete"),
        Coverage::Partial { reasons } => {
            let reasons: Vec<String> = reasons.iter().map(|r| shown(r)).collect();
            println!("coverage: partial ({})", reasons.join(", "));
        }
    }
    let metrics = &outcome.metrics;
    let first = metrics
        .time_to_first_valid_ms
        .map_or_else(|| "none".to_owned(), |ms| format!("{ms} ms"));
    println!(
        "metrics: contacted {}, messages {}, bytes sent {}, bytes received {}, first valid \
         {first}, matching {} us, elapsed {} ms",
        metrics.contacted,
        metrics.messages,
        metrics.bytes_sent,
        metrics.bytes_received,
        metrics.matching_us,
        metrics.elapsed_ms
    );
}

/// Prints the candidates asked before the one chosen, and why they did not
/// take the question: `tried`, a list of `[peer, why]`.
fn print_tried(tried: &Value) {
    for pair in tried.as_array().into_iter().flatten() {
        let (peer, why) = (
            pair[0].as_str().unwrap_or_default(),
            pair[1].as_str().unwrap_or_default(),
        );
        println!("tried: {}: {}", shown(peer), shown(why));
    }
}

/// `text`, which may come from a peer, as printable ASCII, whole: a line
/// of output bounds nothing.
fn shown(text: &str) -> String {
    pals::printable_ascii(text, usize::MAX)
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
/// [`MAX_GATEWAY_ANSWER_BYTES`].
async fn gateway_call(
    listen: SocketAddr,
    method: http::Method,
    path: &str,
    body: Option<&Value>,
) -> anyhow::Result<(http::StatusCode, http::HeaderMap, Bytes)> {
    const TIMEOUT: Duration = Duration::from_secs(60);
    gateway_call_within(
        listen,
        method,
        path,
        body,
        TIMEOUT,
        MAX_GATEWAY_ANSWER_BYTES,
    )
    .await
}

/// [`gateway_call`], within `timeout`, its answer read up to `max_bytes`.
async fn gateway_call_within(
    listen: SocketAddr,
    method: http::Method,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
    max_bytes: usize,
) -> anyhow::Result<(http::StatusCode, http::HeaderMap, Bytes)> {
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
        let body = Limited::new(body, max_bytes)
            .collect()
            .await
            .map_err(|err| anyhow!("cannot read the gateway's answer: {err}"))?
            .to_bytes();
        Ok((parts.status, parts.headers, body))
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| anyhow!("the gateway did not answer within {timeout:?}"))?
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
    offer: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let wait = parse_wait(wait)?;
    // Looked up by (the peer's agent id, its id): before anything is sent,
    // and nothing is when it expired, by this agent's clock.
    let offer = match offer {
        Some(offer_id) => {
            let (cfg, key) = load_agent(home)?;
            let store = open_store(&cfg, &key)?;
            match received_offer_ref(&store, &peer, offer_id, now_unix()) {
                Ok(reference) => Some(reference),
                Err(OfferLookupError::Expired) => {
                    eprintln!("pals: {}", OfferLookupError::Expired);
                    return Ok(ExitCode::from(EXIT_NOT_COMPLETED));
                }
                Err(OfferLookupError::Unknown) => {
                    bail!("no offer {offer_id} from {peer}: run pals discover again")
                }
                Err(err) => return Err(err.into()),
            }
        }
        None => None,
    };
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
        offer,
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
    let names: HashMap<String, String> = store
        .contacts()?
        .into_iter()
        .map(|contact| (contact.agent_id.to_string(), contact.name))
        .collect();
    for row in store.outbox_all()? {
        let peer = match row.peer.strip_prefix(PLAIN_PEER_PREFIX) {
            Some(plain_name) => plain_name.to_owned(),
            None => names.get(&row.peer).cloned().unwrap_or(row.peer),
        };
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
        AskError::NotResent { message_id } => {
            eprintln!(
                "pals: {PLAIN_NOT_RESENT}; the question is kept: `pals task resume \
                 {message_id}` sends it again"
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

fn contact_add_a2a(
    home: &Path,
    url: &str,
    name: &str,
    bearer_env: Option<&str>,
) -> anyhow::Result<()> {
    let contact = plain::new_contact(name, url, bearer_env, now_unix())?;
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    if store.contact_by_name(name)?.is_some() {
        bail!("{name} is the name of a PALS contact already: one name, one contact");
    }
    let card = runtime(false)?
        .block_on(fetch_contact_card(
            &plain::http_client(),
            &contact,
            cfg.gateway.listen.port(),
        ))
        .with_context(|| format!("cannot use the card at {}", contact.card_url))?;
    store.a2a_contact_put(&contact)?;
    let peer = format!("{PLAIN_PEER_PREFIX}{name}");
    store.peer_card_put(
        &peer,
        PUBLIC_CONTEXT,
        &card.json,
        &card.hash,
        None,
        now_unix(),
    )?;
    println!(
        "saved contact {name}: {} ({}), card signature {}",
        contact.origin, card.card.name, card.signature
    );
    if card.card.capabilities.streaming != Some(true) {
        println!(
            "note: its card does not declare streaming, so pals ask, pals_ask and pals \
             discover cannot ask it in milestone 2; a stock A2A client can still call it \
             through the gateway, at /peers/{name}/"
        );
    }
    Ok(())
}

fn contact_ls(home: &Path) -> anyhow::Result<()> {
    let (cfg, key) = load_agent(home)?;
    let store = open_store(&cfg, &key)?;
    for contact in store.contacts()? {
        println!("{}\t{}", contact.name, contact.agent_id);
    }
    for contact in store.a2a_contacts()? {
        println!("{}\t{}", contact.name, contact.card_url);
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
    fn visibilities_take_the_names_of_scopes() {
        assert_eq!(parse_visibility("private").unwrap(), Visibility::Private);
        assert_eq!(parse_visibility("public").unwrap(), Visibility::Public);
        let demo = || vec!["demo".to_owned()];
        assert_eq!(
            parse_visibility("project:demo").unwrap(),
            Visibility::Project(demo())
        );
        assert_eq!(
            parse_visibility("task:demo").unwrap(),
            Visibility::Task(demo())
        );
        for text in ["", "friends", "project:", "project:two words", "PUBLIC"] {
            assert!(parse_visibility(text).is_err(), "{text:?}");
        }
        for text in ["private", "public", "project:demo", "task:demo"] {
            assert_eq!(visibility_name(&parse_visibility(text).unwrap()), text);
        }
    }

    #[test]
    fn an_unreadable_benchmark_row_is_listed_by_its_id() {
        let kept = KeptBenchmark {
            benchmark_id: "r-1".to_owned(),
            report: Err("its report does not verify (bad signature)".to_owned()),
        };
        assert_eq!(
            benchmark_line(&kept),
            "r-1\tunreadable\tits report does not verify (bad signature)"
        );
    }

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
