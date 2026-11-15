//! The inventory adapter: what the operator declares stays a declaration,
//! and an MCP connection with an adapter is checked with a real `tools/list`
//! over Streamable HTTP or a child's stdio, when the node starts, by timer
//! or with `pals inventory refresh`, never to answer a query. Only offered
//! tools are published; the adapter's configuration, its credentials and its
//! raw errors never reach a view, an offer or the store; a server that is
//! not there, hangs or lists without end fails within its time, and the
//! node keeps serving.

mod common;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{
    CountingExecutor, IDEMPOTENCY_EXTENSION, TestNode, TestResponse, grant, key_in, now_unix,
    spawn_node, spawn_node_in_configured,
};
use futures::FutureExt;
use pals::config::{Config, INVENTORY_FILE, WorkerCfg};
use pals::discovery::Reply;
use pals::inventory::McpAdapter;
use pals::inventory::mcp_adapter::MAX_MCP_PAGES;
use pals::node::NodeOptions;
use pals_core::discovery::query::{
    InventoryRequirements, Met, OfferRef, OfferStatus, RequirementSet, Requirements, ServiceOffer,
    ServiceQuery,
};
use pals_core::discovery::selection::SearchRequest;
use pals_core::discovery::wire::{GetInventory, Request, request, response};
use pals_core::grant::Operation::{TaskCancel, TaskRead, TaskSend};
use pals_core::grant::{GRANT_SCHEMA, Grant, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::scope::MAX_PROJECT_NAME_BYTES;
use pals_core::inventory::view::{ViewEntry, ViewPage, verify_page};
use pals_core::inventory::{
    AdapterMethod, DETAIL_TIMEOUT, DETAIL_TOO_MANY_TOOLS, DETAIL_UNREACHABLE, Entry,
    INVENTORY_EXTENSION, Inventory, McpConnection, Observation, ObservationSource,
    OperationalStatus,
};
use pals_core::store::Store;
use serde_json::{Value, json};

/// The stdio MCP server the tests start, with `python3`.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/mcp_stdio_server.py"
);
/// The MCP connection the tests configure.
const MCP: &str = "example:repo-reader";
/// The capability of the service that works with it.
const CAPABILITY: &str = "code.rust.review";
/// How long a call may take: far more than any answer needs, even on a
/// loaded host; only a broken test waits it out.
const CALL: Duration = Duration::from_secs(30);
/// How long a state a test waits for may take to come: only a broken test
/// waits it out.
const SETTLE: Duration = Duration::from_secs(60);
/// How long an adapter that must reach its answer may take: Python's start
/// and every round trip on a loaded host, far more than any needs. Only a
/// broken test waits it out.
const PATIENT: Duration = Duration::from_secs(60);
/// Every task operation.
const TASK_OPS: [pals_core::grant::Operation; 3] = [TaskSend, TaskRead, TaskCancel];
/// `INVALID_PARAMS` of JSON-RPC.
const INVALID_PARAMS: i64 = -32602;

/// `items` as a TOML array of strings.
fn toml_list(items: &[impl AsRef<str>]) -> String {
    let items: Vec<&str> = items.iter().map(AsRef::as_ref).collect();
    serde_json::to_string(&items).unwrap()
}

/// An MCP connection `id`, shown as `visibility` says, offering `offered`,
/// checked through `adapter`, the TOML of its `[mcp.adapter]`.
fn mcp_entry(id: &str, offered: &[&str], visibility: &str, adapter: &str) -> String {
    format!(
        "\n[[mcp]]\nid = \"{id}\"\ndescription = \"Reads the project repository\"\n\
         provenance = \"operator\"\nvisibility = {visibility}\noffered_tools = {}\n{adapter}",
        toml_list(offered)
    )
}

/// The `[mcp.adapter]` of the fixture, started with `args`, passed the
/// variables `env` names.
fn stdio_adapter(args: &[&str], env: &[&str]) -> String {
    let mut all = vec![FIXTURE];
    all.extend_from_slice(args);
    format!(
        "[mcp.adapter]\ntransport = \"stdio\"\ncommand = \"python3\"\nargs = {}\n\
         env_passthrough = {}\n",
        toml_list(&all),
        toml_list(env)
    )
}

/// The `[mcp.adapter]` of a server at `url`.
fn http_adapter(url: &str) -> String {
    format!("[mcp.adapter]\ntransport = \"http\"\nurl = \"{url}\"\n")
}

/// The public review service, done with [`MCP`].
const SERVICE: &str = r#"
[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["code.rust.review"]
input_modes = ["text/plain"]
output_modes = ["text/plain"]
components = [{ id = "example:repo-reader", role = "required" }]
visibility = "public"
"#;

/// The only MCP connection of `inventory`, as an adapter reads it.
fn entry_of(inventory: &str) -> Entry<McpConnection> {
    Inventory::from_toml(inventory)
        .expect("a valid inventory")
        .mcp
        .remove(0)
}

/// A node whose inventory is `inventory`, whose work `executor` does, with
/// `opts` and its configuration changed by `edit`.
async fn provider_with(
    inventory: &str,
    executor: impl a2a_server::AgentExecutor,
    opts: NodeOptions,
    edit: impl FnOnce(&mut Config),
) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    spawn_node_in_configured(dir, executor, opts, edit).await
}

/// The options of a node whose adapter gets [`PATIENT`] for each
/// connection.
fn patient() -> NodeOptions {
    NodeOptions {
        mcp_adapter_timeout: PATIENT,
        ..NodeOptions::default()
    }
}

/// [`provider_with`] [`patient`] options and the default configuration.
async fn provider(inventory: &str) -> TestNode {
    provider_with(
        inventory,
        CountingExecutor::new(Duration::ZERO),
        patient(),
        |_| {},
    )
    .await
}

/// `GetInventory` of `sections` (all when empty), presenting `grant`.
fn get_inventory(sections: &[&str], grant: Option<&SignedGrant>) -> Request {
    Request {
        version: 0,
        max_response_bytes: 0,
        grant_token: grant.map(SignedGrant::to_token).unwrap_or_default(),
        op: Some(request::Op::GetInventory(GetInventory {
            sections: sections.iter().map(|s| (*s).to_owned()).collect(),
            cursor: String::new(),
            page_entries: 0,
        })),
    }
}

/// The first page of `sections` of `to`'s view `from` gets, presenting
/// `grant`: verified as `to`'s page for `from`, and as it travelled.
async fn page(
    from: &TestNode,
    to: &TestNode,
    sections: &[&str],
    grant: Option<&SignedGrant>,
) -> (ViewPage, String) {
    let reply = from
        .node
        .discovery()
        .call(
            &to.id(),
            to.node.addr(),
            get_inventory(sections, grant),
            CALL,
        )
        .await
        .expect("a page of the inventory");
    page_of(&reply, &to.id(), &from.id())
}

/// The page `reply` carries, verified as `agent`'s for `audience`, and as
/// it travelled.
fn page_of(reply: &Reply, agent: &AgentId, audience: &AgentId) -> (ViewPage, String) {
    let Some(response::Result::InventoryPage(bytes)) = &reply.response.result else {
        panic!("an inventory page, not {:?}", reply.response.result);
    };
    let page = verify_page(bytes, agent, Some(audience), now_unix()).expect("a valid page");
    (page, String::from_utf8(bytes.clone()).unwrap())
}

/// The entry `id` of `page`.
fn entry<'a>(page: &'a ViewPage, id: &str) -> &'a ViewEntry {
    page.entries
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("{id} in {:?}", page.entries))
}

/// The names of the tools a view's MCP entry publishes.
fn tool_names(entry: &ViewEntry) -> Vec<String> {
    entry.body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The observation a view's entry carries.
fn operational(entry: &ViewEntry) -> &Observation {
    entry
        .dimensions
        .operational
        .as_ref()
        .unwrap_or_else(|| panic!("an observation of {}", entry.id))
}

/// The review query, needing [`MCP`] when `with_mcp`, as `id`.
fn review_query(id: &str, with_mcp: bool) -> ServiceQuery {
    let mut search = SearchRequest {
        capability: CAPABILITY.to_owned(),
        input_media_types: vec!["text/plain".to_owned()],
        output_media_types: vec!["text/plain".to_owned()],
        ..SearchRequest::default()
    };
    if with_mcp {
        search.requirements = Requirements {
            inventory: Some(InventoryRequirements {
                required: RequirementSet {
                    mcp_integration_ids: vec![MCP.to_owned()],
                    ..RequirementSet::default()
                },
                preferred: RequirementSet::default(),
            }),
            ..Requirements::default()
        };
    }
    search.query(id, 3000, true)
}

/// `from`'s offer from `to` for `query`, presenting `grant`, and its JSON
/// as signed.
async fn offer(
    from: &TestNode,
    to: &TestNode,
    query: &ServiceQuery,
    grant: Option<&SignedGrant>,
) -> (ServiceOffer, String) {
    let (offer, _) = from
        .node
        .discovery()
        .query(&to.id(), to.node.addr(), query, grant, CALL)
        .await
        .expect("an offer");
    let json = String::from_utf8(offer.to_json()).unwrap();
    (offer.doc, json)
}

/// The lines of `path`, none when it does not exist.
fn lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_owned).collect(),
        Err(_) => Vec::new(),
    }
}

/// How many `tools/list` requests the fixture logged in `path`.
fn list_requests(path: &Path) -> usize {
    lines(path)
        .iter()
        .filter(|line| *line == "tools/list")
        .count()
}

/// A port of 127.0.0.1 where nothing listens.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Runs `pals --home <home> <args>` off the async threads; its exit code,
/// stdout and stderr.
async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .arg("--home")
        .arg(home)
        .args(args);
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .unwrap();
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Points the configuration in `home` at `node`'s gateway, so that the CLI
/// finds it running.
fn configure_cli(home: &Path, gateway_url: &str) {
    let listen = gateway_url.replace("http://", "");
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn http_mcp_server_tools_are_observed_and_only_offered_ones_published() {
    // Another node's MCP server, at its gateway's /mcp: eight tools.
    let server = spawn_node(WorkerCfg::Echo).await;
    let url = format!("{}/mcp", server.gateway_url());
    let inventory = mcp_entry(MCP, &["pals_whoami"], "\"public\"", &http_adapter(&url));
    let b = provider(&inventory).await;
    let report = b.node.inventory().startup_refresh_done().await;
    assert_eq!((report.observed, report.ok), (1, 1), "{report:?}");
    assert!(report.failed.is_empty(), "{report:?}");

    let a = spawn_node(WorkerCfg::Echo).await;
    let (view, json) = page(&a, &b, &["mcp"], None).await;
    let connection = entry(&view, MCP);
    let observed = operational(connection);
    assert_eq!(observed.status, OperationalStatus::Ok);
    assert_eq!(
        observed.source,
        ObservationSource::Adapter {
            method: AdapterMethod::McpToolsList
        }
    );
    assert_eq!(observed.detail, None);
    assert_eq!(tool_names(connection), [format!("{MCP}/pals_whoami")]);
    let description = connection.body["tools"][0]["description"].as_str();
    assert!(description.is_some_and(|d| !d.is_empty()), "{json}");
    // The server's other tools are listed, never published.
    for tool in [
        "pals_contacts",
        "pals_ask",
        "pals_task",
        "pals_cancel",
        "pals_fetch",
    ] {
        assert!(!json.contains(tool), "{tool} in {json}");
    }
    // The observation is kept, with the tool it found.
    let kept = b.node.store.observations(now_unix()).unwrap();
    assert_eq!(kept[MCP].status, OperationalStatus::Ok);
    let names: Vec<&str> = kept[MCP].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["pals_whoami"]);
}

#[tokio::test]
async fn stdio_mcp_server_is_listed_through_a_child_process() {
    let out = tempfile::tempdir().unwrap();
    let requests = out.path().join("requests");
    let requests_arg = requests.to_str().unwrap();
    let adapter = stdio_adapter(
        &[
            "--tools",
            "read_file,delete_file,list_dir",
            "--page-size",
            "1",
            "--requests",
            requests_arg,
        ],
        &[],
    );
    let inventory = mcp_entry(MCP, &["read_file", "list_dir"], "\"public\"", &adapter);
    let b = provider(&inventory).await;
    let report = b.node.inventory().startup_refresh_done().await;
    assert_eq!((report.observed, report.ok), (1, 1), "{report:?}");

    let a = spawn_node(WorkerCfg::Echo).await;
    let (view, json) = page(&a, &b, &["mcp"], None).await;
    let connection = entry(&view, MCP);
    assert_eq!(operational(connection).status, OperationalStatus::Ok);
    assert_eq!(
        tool_names(connection),
        [format!("{MCP}/read_file"), format!("{MCP}/list_dir")]
    );
    assert_eq!(
        connection.body["tools"][0]["description"],
        "The read_file tool of the fixture"
    );
    assert!(!json.contains("delete_file"), "{json}");
    // Page by page, one tool each, and never a call.
    let methods = lines(&requests);
    assert_eq!(methods[0], "initialize", "{methods:?}");
    assert_eq!(list_requests(&requests), 3, "{methods:?}");
    assert!(!methods.iter().any(|m| m == "tools/call"), "{methods:?}");
}

#[tokio::test]
async fn offered_tool_missing_makes_the_connection_not_operational() {
    // Scenario §14: the connection is configured, and its server does not
    // serve every tool offered.
    let adapter = stdio_adapter(&["--tools", "read_file"], &[]);
    let inventory = format!(
        "{SERVICE}{}",
        mcp_entry(MCP, &["read_file", "list_dir"], "\"public\"", &adapter)
    );
    let b = provider(&inventory).await;
    let report = b.node.inventory().startup_refresh_done().await;
    assert_eq!(report.ok, 0, "{report:?}");
    assert_eq!(
        report.failed,
        [(MCP.to_owned(), "missing tools: list_dir".to_owned())]
    );

    let a = spawn_node(WorkerCfg::Echo).await;
    let (view, _) = page(&a, &b, &["mcp"], None).await;
    let connection = entry(&view, MCP);
    let observed = operational(connection);
    assert_eq!(observed.status, OperationalStatus::Failed);
    assert_eq!(observed.detail.as_deref(), Some("missing tools: list_dir"));
    // What it serves is published; what it does not, never.
    assert_eq!(tool_names(connection), [format!("{MCP}/read_file")]);

    // A query that needs the connection is not served.
    let (offer, _) = offer(&a, &b, &review_query("q-1", true), None).await;
    assert_eq!(offer.status, OfferStatus::NotOffered);
    let needed = offer
        .requirement_results
        .iter()
        .find(|r| r.requirement == format!("mcp_integration:{MCP}"))
        .expect("the connection's requirement");
    assert_eq!(needed.result, Met::NotMet);
}

#[tokio::test]
async fn unreachable_or_hanging_server_fails_and_the_node_keeps_serving() {
    // Nothing listens: refused at once, whatever time the adapter has.
    let closed = format!("http://127.0.0.1:{}/mcp", closed_port());
    let unreachable = entry_of(&mcp_entry(
        MCP,
        &["read_file"],
        "\"public\"",
        &http_adapter(&closed),
    ));
    let observed = McpAdapter::new(PATIENT).observe(&unreachable).await;
    assert_eq!(observed.status, OperationalStatus::Failed);
    assert_eq!(observed.detail.as_deref(), Some(DETAIL_UNREACHABLE));
    assert!(observed.tools.is_empty());

    // An input, never a measure: the adapter gives up after 300 ms on a
    // server that never answers.
    let adapter = McpAdapter::new(Duration::from_millis(300));
    let hanging = entry_of(&mcp_entry(
        MCP,
        &["read_file"],
        "\"public\"",
        &stdio_adapter(&["--hang"], &[]),
    ));
    let observed = adapter.observe(&hanging).await;
    assert_eq!(observed.status, OperationalStatus::Failed);
    assert_eq!(observed.detail.as_deref(), Some(DETAIL_TIMEOUT));

    // A node whose server hangs for as long as the test lasts serves
    // meanwhile.
    let out = tempfile::tempdir().unwrap();
    let ready = out.path().join("ready");
    let inventory = mcp_entry(
        MCP,
        &["read_file"],
        "\"public\"",
        &stdio_adapter(&["--hang", "--ready", ready.to_str().unwrap()], &[]),
    );
    let b = provider_with(
        &inventory,
        CountingExecutor::new(Duration::ZERO),
        NodeOptions {
            mcp_adapter_timeout: Duration::from_secs(3600),
            ..NodeOptions::default()
        },
        |_| {},
    )
    .await;
    let deadline = Instant::now() + SETTLE;
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "the hanging server never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let a = spawn_node(WorkerCfg::Echo).await;
    let (view, _) = page(&a, &b, &["mcp"], None).await;
    // Not observed yet: shown as declared, without an observation.
    assert_eq!(entry(&view, MCP).dimensions.operational, None);
    assert!(
        b.node
            .inventory()
            .startup_refresh_done()
            .now_or_never()
            .is_none(),
        "the refresh is still waiting for the server"
    );

    // And a node whose adapter gives up publishes the failures.
    let inventory = format!(
        "{}{}",
        mcp_entry(MCP, &["read_file"], "\"public\"", &http_adapter(&closed)),
        mcp_entry(
            "example:hanging",
            &["read_file"],
            "\"public\"",
            &stdio_adapter(&["--hang"], &[]),
        ),
    );
    // Long enough for the refusal to come first however loaded the host,
    // and still an end for the server that never answers.
    let c = provider_with(
        &inventory,
        CountingExecutor::new(Duration::ZERO),
        NodeOptions {
            mcp_adapter_timeout: Duration::from_secs(5),
            ..NodeOptions::default()
        },
        |_| {},
    )
    .await;
    let report = c.node.inventory().startup_refresh_done().await;
    assert_eq!(report.observed, 2, "{report:?}");
    let mut failed = report.failed.clone();
    failed.sort();
    assert_eq!(
        failed,
        [
            ("example:hanging".to_owned(), DETAIL_TIMEOUT.to_owned()),
            (MCP.to_owned(), DETAIL_UNREACHABLE.to_owned()),
        ]
    );
    let (view, _) = page(&a, &c, &["mcp"], None).await;
    assert_eq!(
        operational(entry(&view, MCP)).detail.as_deref(),
        Some(DETAIL_UNREACHABLE)
    );
    assert_eq!(
        operational(entry(&view, "example:hanging"))
            .detail
            .as_deref(),
        Some(DETAIL_TIMEOUT)
    );
}

#[tokio::test]
async fn huge_tool_list_is_capped() {
    let adapter = McpAdapter::new(PATIENT);
    let out = tempfile::tempdir().unwrap();

    // 100 tools a page, without end: past 256 tools it stops.
    let requests = out.path().join("hundreds");
    let endless = entry_of(&mcp_entry(
        MCP,
        &["read_file"],
        "\"public\"",
        &stdio_adapter(
            &[
                "--tools",
                "read_file",
                "--page-size",
                "100",
                "--endless",
                "--requests",
                requests.to_str().unwrap(),
            ],
            &[],
        ),
    ));
    let observed = adapter.observe(&endless).await;
    assert_eq!(observed.status, OperationalStatus::Failed);
    assert_eq!(observed.detail.as_deref(), Some(DETAIL_TOO_MANY_TOOLS));
    assert!(observed.tools.is_empty());
    let pages = list_requests(&requests);
    assert!((1..=3).contains(&pages), "{pages} page requests");

    // One tool a page, without end: past 16 pages it stops.
    let requests = out.path().join("ones");
    let endless = entry_of(&mcp_entry(
        MCP,
        &["read_file"],
        "\"public\"",
        &stdio_adapter(
            &[
                "--page-size",
                "1",
                "--endless",
                "--requests",
                requests.to_str().unwrap(),
            ],
            &[],
        ),
    ));
    let observed = adapter.observe(&endless).await;
    assert_eq!(observed.detail.as_deref(), Some(DETAIL_TOO_MANY_TOOLS));
    assert!(observed.tools.is_empty());
    let pages = list_requests(&requests);
    assert!(
        (1..=MAX_MCP_PAGES).contains(&pages),
        "{pages} page requests"
    );
    assert_eq!(adapter.list_calls(), 3 + MAX_MCP_PAGES as u64);
}

#[tokio::test]
async fn discovery_never_calls_the_adapter() {
    let out = tempfile::tempdir().unwrap();
    let requests = out.path().join("requests");
    let adapter = stdio_adapter(&["--requests", requests.to_str().unwrap()], &[]);
    let inventory = format!(
        "{SERVICE}{}",
        mcp_entry(MCP, &["read_file", "list_dir"], "\"public\"", &adapter)
    );
    let b = provider(&inventory).await;
    let report = b.node.inventory().startup_refresh_done().await;
    assert_eq!(report.ok, 1, "{report:?}");
    let base = b.node.inventory().adapter().list_calls();
    let logged = lines(&requests);
    assert!(base > 0);

    let a = spawn_node(WorkerCfg::Echo).await;
    for n in 0..20 {
        let (offer, _) = offer(&a, &b, &review_query(&format!("q-{n}"), true), None).await;
        assert_eq!(offer.status, OfferStatus::Available);
        page(&a, &b, &[], None).await;
    }
    assert_eq!(b.node.inventory().adapter().list_calls(), base);
    assert_eq!(lines(&requests), logged);
}

/// A `pals daemon` process and what it wrote to stderr so far.
struct Daemon {
    child: Child,
    log: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    /// Starts `pals daemon` on `home` with `env` added to its environment,
    /// and waits until it says it serves: its agent id and gateway URL.
    async fn start(home: &Path, env: &[(&str, &str)]) -> (Self, AgentId, String) {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("pals"));
        cmd.env_remove("PALS_HOME")
            .env("RUST_LOG", "pals=debug")
            .arg("--home")
            .arg(home)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (name, value) in env {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().expect("start pals daemon");
        let stderr = child.stderr.take().unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        let (serving, started) = std::sync::mpsc::channel();
        let reader = {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if let Some(rest) = line.strip_prefix("pals daemon: agent ") {
                        let _ = serving.send(rest.to_owned());
                    }
                    let mut log = log.lock().unwrap();
                    log.push_str(&line);
                    log.push('\n');
                }
            })
        };
        let line = tokio::task::spawn_blocking(move || started.recv_timeout(SETTLE))
            .await
            .unwrap()
            .unwrap_or_else(|_| panic!("the daemon did not start: {}", log.lock().unwrap()));
        // `<id> serving, gateway at <url>`
        let (id, rest) = line.split_once(' ').unwrap();
        let url = rest.rsplit(' ').next().unwrap().to_owned();
        let daemon = Self {
            child,
            log,
            reader: Some(reader),
        };
        (daemon, id.parse().unwrap(), url)
    }

    /// Stops it with SIGTERM, as an operator does, and returns its stderr.
    async fn stop(mut self) -> String {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: kill(2) only sends a signal to our own unreaped child.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let mut child = self.child;
        let status = tokio::task::spawn_blocking(move || child.wait().unwrap())
            .await
            .unwrap();
        assert!(status.success(), "the daemon exited with {status}");
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        self.log.lock().unwrap().clone()
    }
}

/// Every regular file under `dir`.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files
}

#[tokio::test]
async fn credentials_never_reach_the_inventory() {
    const VARIABLE: &str = "PALS_TEST_MCP_CREDENTIAL";
    let secret = format!("pals-secret-{}", AgentKey::generate().id());
    let home = tempfile::tempdir().unwrap();
    // The fixture's own files, outside the agent's directory.
    let out = tempfile::tempdir().unwrap();
    let (env_file, value_file) = (out.path().join("env"), out.path().join("value"));
    std::fs::write(
        home.path().join("config.toml"),
        "[gateway]\nlisten = \"127.0.0.1:0\"\n\n[net]\nrelay = false\n",
    )
    .unwrap();
    let adapter = stdio_adapter(
        &[
            "--env-file",
            env_file.to_str().unwrap(),
            "--value-of",
            VARIABLE,
            "--value-file",
            value_file.to_str().unwrap(),
        ],
        &[VARIABLE],
    );
    let inventory = format!(
        "{SERVICE}{}",
        mcp_entry(MCP, &["read_file", "list_dir"], "\"public\"", &adapter)
    );
    std::fs::write(home.path().join(INVENTORY_FILE), inventory).unwrap();

    let (daemon, b_id, gateway_url) = Daemon::start(home.path(), &[(VARIABLE, &secret)]).await;
    configure_cli(home.path(), &gateway_url);
    // Checked now, through the CLI: when it answers, the observation is in.
    let (code, refreshed, stderr) = pals(home.path(), &["inventory", "refresh"]).await;
    assert_eq!(code, 0, "{refreshed}{stderr}");
    assert!(refreshed.contains("ok 1"), "{refreshed}");
    // The child got the variable, by name and value.
    assert_eq!(std::fs::read_to_string(&value_file).unwrap(), secret);
    assert!(lines(&env_file).iter().any(|name| name == VARIABLE));

    let (code, ticket, stderr) = pals(home.path(), &["id", "--ticket"]).await;
    assert_eq!(code, 0, "{stderr}");
    let addr = ticket
        .trim()
        .parse::<iroh_tickets::endpoint::EndpointTicket>()
        .unwrap()
        .endpoint_addr()
        .clone();
    let a = spawn_node(WorkerCfg::Echo).await;
    let reply = a
        .node
        .discovery()
        .call(&b_id, addr.clone(), get_inventory(&[], None), CALL)
        .await
        .unwrap();
    let (view, json) = page_of(&reply, &b_id, &a.id());
    assert_eq!(
        operational(entry(&view, MCP)).status,
        OperationalStatus::Ok,
        "{json}"
    );
    assert!(!json.contains(&secret));
    assert!(!json.contains(VARIABLE), "{json}");
    let (offer, _) = a
        .node
        .discovery()
        .query(&b_id, addr, &review_query("q-1", true), None, CALL)
        .await
        .unwrap();
    assert!(
        !String::from_utf8(offer.to_json())
            .unwrap()
            .contains(&secret)
    );
    let (code, shown, _) = pals(home.path(), &["inventory", "show"]).await;
    assert_eq!(code, 0);
    assert!(!shown.contains(&secret));

    let log = daemon.stop().await;
    assert!(!log.contains(&secret), "the daemon logged the credential");
    for file in files_under(home.path()) {
        let bytes = std::fs::read(&file).unwrap();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "{} holds the credential",
            file.display()
        );
    }
}

#[tokio::test]
async fn adapter_url_and_command_never_appear_in_a_view() {
    let port = closed_port();
    let url = format!("http://127.0.0.1:{port}/private-mcp-path");
    let command = "/nonexistent/pals-private-command";
    let inventory = format!(
        "{SERVICE}{}{}",
        mcp_entry(MCP, &["read_file"], "\"public\"", &http_adapter(&url)),
        mcp_entry(
            "example:local-tool",
            &["read_file"],
            "\"public\"",
            &format!(
                "[mcp.adapter]\ntransport = \"stdio\"\ncommand = \"{command}\"\n\
                 args = [\"--private-arg\"]\n"
            ),
        ),
    );
    let b = provider(&inventory).await;
    let report = b.node.inventory().startup_refresh_done().await;
    let mut failed = report.failed.clone();
    failed.sort();
    assert_eq!(
        failed,
        [
            (
                "example:local-tool".to_owned(),
                DETAIL_UNREACHABLE.to_owned()
            ),
            (MCP.to_owned(), DETAIL_UNREACHABLE.to_owned()),
        ]
    );

    let a = spawn_node(WorkerCfg::Echo).await;
    let (_, view) = page(&a, &b, &[], None).await;
    let (_, offer) = offer(&a, &b, &review_query("q-1", true), None).await;
    let shown = b
        .node
        .store
        .observations(now_unix())
        .unwrap()
        .values()
        .map(|o| serde_json::to_string(o).unwrap())
        .collect::<String>();
    for text in [&view, &offer, &shown] {
        for private in [
            "private-mcp-path",
            &format!("127.0.0.1:{port}"),
            command,
            "pals-private-command",
            "--private-arg",
        ] {
            assert!(!text.contains(private), "{private} in {text}");
        }
    }
}

/// Whether the process `pid` still runs: neither gone nor a zombie.
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // `<pid> (<comm>) <state> ...`: the state follows the last `)`.
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => false,
    }
}

#[tokio::test]
async fn child_environment_is_clean_and_its_group_is_killed() {
    let out = tempfile::tempdir().unwrap();
    let env_file = out.path().join("env");
    let entry = entry_of(&mcp_entry(
        MCP,
        &["read_file"],
        "\"public\"",
        &stdio_adapter(
            &["--env-file", env_file.to_str().unwrap()],
            &["PALS_TEST_PASSED_VARIABLE"],
        ),
    ));
    let observed = McpAdapter::new(PATIENT).observe(&entry).await;
    assert_eq!(observed.status, OperationalStatus::Ok, "{observed:?}");

    let written = lines(&env_file);
    let pid_of = |key: &str| -> u32 {
        written
            .iter()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} in {written:?}"))
            .trim()
            .parse()
            .unwrap()
    };
    let (child, grandchild) = (pid_of("pid "), pid_of("grandchild "));
    let names: Vec<&String> = written.iter().filter(|line| !line.contains(' ')).collect();
    for name in &names {
        assert!(
            ["PATH", "HOME", "LANG", "PALS_TEST_PASSED_VARIABLE"].contains(&name.as_str()),
            "{name} passed to the child: {names:?}"
        );
    }
    assert!(names.iter().any(|name| *name == "PATH"), "{names:?}");

    // Its whole group is gone with the observation.
    let deadline = Instant::now() + SETTLE;
    while alive(child) || alive(grandchild) {
        assert!(
            Instant::now() < deadline,
            "the child or its own child outlived the observation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn manual_declaration_stays_declared() {
    let inventory = r#"
[[mcp]]
id = "example:repo-reader"
description = "Reads the project repository"
provenance = "operator"
visibility = "public"
server = "@modelcontextprotocol/server-filesystem"
tools = [{ name = "read_file", description = "Reads a file" }]
offered_tools = ["read_file"]
"#;
    // An observation a run with an adapter left behind counts for nothing.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    let key = AgentKey::load_or_create(&dir.path().join("agent.key")).unwrap();
    {
        let store = Store::open(&dir.path().join("pals.db"), key.id()).unwrap();
        let now = now_unix();
        let left = Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status: OperationalStatus::Ok,
            observed_at: format_timestamp(now).unwrap(),
            expires_at: format_timestamp(now + 1800).unwrap(),
            detail: None,
            tools: Vec::new(),
        };
        store.observation_put(MCP, &left).unwrap();
    }
    let b = spawn_node_in_configured(
        dir,
        CountingExecutor::new(Duration::ZERO),
        NodeOptions::default(),
        |_| {},
    )
    .await;
    assert_eq!(key_in(b.dir.path()).id(), key.id());
    let report = b.node.inventory().startup_refresh_done().await;
    assert_eq!(report.observed, 0, "{report:?}");
    assert!(b.node.inventory().snapshot().observations.is_empty());

    let a = spawn_node(WorkerCfg::Echo).await;
    let (view, _) = page(&a, &b, &["mcp"], None).await;
    let connection = entry(&view, MCP);
    assert_eq!(connection.dimensions.operational, None);
    assert_eq!(connection.provenance, "operator");
    assert_eq!(tool_names(connection), [format!("{MCP}/read_file")]);
    assert_eq!(connection.body["tools"][0]["description"], "Reads a file");
}

#[tokio::test]
async fn refresh_cli_runs_the_adapters_now() {
    let out = tempfile::tempdir().unwrap();
    let tools = out.path().join("tools");
    std::fs::write(&tools, "read_file").unwrap();
    let adapter = stdio_adapter(&["--tools-file", tools.to_str().unwrap()], &[]);
    let inventory = mcp_entry(MCP, &["read_file", "list_dir"], "\"public\"", &adapter);
    let b = provider(&inventory).await;
    let started = b.node.inventory().startup_refresh_done().await;
    assert_eq!(
        started.failed,
        [(MCP.to_owned(), "missing tools: list_dir".to_owned())]
    );
    let calls = b.node.inventory().adapter().list_calls();

    // The server serves both now; the CLI checks again at once.
    std::fs::write(&tools, "read_file,list_dir").unwrap();
    configure_cli(b.dir.path(), &b.gateway_url());
    let (code, stdout, stderr) = pals(b.dir.path(), &["inventory", "refresh"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("observed 1, ok 1, failed 0"), "{stdout}");
    assert!(b.node.inventory().adapter().list_calls() > calls);
    let snapshot = b.node.inventory().snapshot();
    assert!(snapshot.generation > started.generation);
    assert_eq!(snapshot.observations[MCP].status, OperationalStatus::Ok);
    let kept = b.node.store.observations(now_unix()).unwrap();
    assert_eq!(kept[MCP].status, OperationalStatus::Ok);
}

/// `params` of a `SendMessage` with `message_id` that names `offer`.
fn naming(message_id: &str, offer: &OfferRef) -> Value {
    json!({"message": {
        "messageId": message_id,
        "role": "ROLE_USER",
        "parts": [{"text": "review it"}],
        "metadata": { INVENTORY_EXTENSION: offer.to_value() },
    }})
}

/// The JSON-RPC error `res` carries: its code and message.
fn rpc_error(res: &TestResponse) -> (i64, String) {
    assert_eq!(res.status, 200, "{}", res.json);
    let error = &res.json["error"];
    assert!(error.is_object(), "an error, not {}", res.json);
    (
        error["code"].as_i64().unwrap(),
        error["message"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn an_observation_change_turns_held_offers_stale() {
    let out = tempfile::tempdir().unwrap();
    let tools = out.path().join("tools");
    std::fs::write(&tools, "read_file,list_dir").unwrap();
    let adapter = stdio_adapter(&["--tools-file", tools.to_str().unwrap()], &[]);
    let inventory = format!(
        "{SERVICE}{}",
        mcp_entry(MCP, &["read_file", "list_dir"], "\"public\"", &adapter)
    );
    let exec = CountingExecutor::new(Duration::ZERO);
    // Offers that outlast any loaded host's delays.
    let b = provider_with(&inventory, exec.clone(), patient(), |cfg| {
        cfg.discovery.offer_valid_for_ms = 300_000;
    })
    .await;
    assert_eq!(b.node.inventory().startup_refresh_done().await.ok, 1);
    let a = spawn_node(WorkerCfg::Echo).await;
    let send = grant(&b, &a, &TASK_OPS, 3600);
    a.hold(&send);
    let (held, _) = offer(&a, &b, &review_query("q-1", false), Some(&send)).await;
    assert_eq!(held.status, OfferStatus::Available);

    // The server stops listing an offered tool, and b checks again.
    std::fs::write(&tools, "read_file").unwrap();
    let report = b.node.inventory().refresh().await.unwrap();
    assert_eq!(
        report.failed,
        [(MCP.to_owned(), "missing tools: list_dir".to_owned())]
    );

    let extensions = format!("{IDEMPOTENCY_EXTENSION}, {INVENTORY_EXTENSION}");
    let res = a
        .rpc_with(
            &b,
            Some(&send),
            "SendMessage",
            naming("m-1", &OfferRef::of(&held)),
            &[("a2a-extensions", extensions.as_str())],
        )
        .await;
    assert_eq!(rpc_error(&res), (INVALID_PARAMS, "offer_stale".to_owned()));
    assert_eq!(exec.runs(), 0);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

/// A public model, which shows after the MCP connections.
const PUBLIC_MODEL: &str = r#"
[[models]]
id = "example:coder"
description = "The coding model"
provenance = "operator"
visibility = "public"
provider = "Example"
model_id = "example-coder"
revision = "2026-10-01"
role = "primary"
execution = { kind = "local" }
"#;

#[tokio::test]
async fn a_grantee_of_any_project_reads_a_fitted_public_entry_page_by_page() {
    // A public connection with 40 tools whose texts are of 1 KiB: far more
    // than a page, so the adapter's observation is cut to fit; with no
    // `[projects]` in the configuration.
    let tools: Vec<String> = (0..40).map(|n| format!("tool_{n:02}")).collect();
    let offered: Vec<&str> = tools.iter().map(String::as_str).collect();
    let listed = tools.join(",");
    let adapter = stdio_adapter(&["--tools", &listed, "--describe", "1024"], &[]);
    let inventory = format!(
        "{SERVICE}{}{PUBLIC_MODEL}",
        mcp_entry(MCP, &offered, "\"public\"", &adapter)
    );
    let b = provider(&inventory).await;
    assert_eq!(b.node.inventory().startup_refresh_done().await.ok, 1);
    let a = spawn_node(WorkerCfg::Echo).await;
    // A grant to send tasks in a project with the longest name, one that
    // JSON writes with two characters a byte, which no configuration names.
    let project = "\\".repeat(MAX_PROJECT_NAME_BYTES);
    let now = now_unix();
    let grant = SignedGrant::issue(
        &key_in(b.dir.path()),
        Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: "g-long-project".to_owned(),
            issuer: b.id(),
            subject: a.id(),
            audience: b.id(),
            project: project.clone(),
            resources: vec!["skill:ask".to_owned()],
            operations: TASK_OPS.to_vec(),
            issued_at: format_timestamp(now).unwrap(),
            expires_at: format_timestamp(now + 3600).unwrap(),
        },
    )
    .unwrap();

    // Page by page, to the end: never `TooLarge`.
    let mut cursor = String::new();
    let mut ids = Vec::new();
    for _ in 0..8 {
        let request = Request {
            version: 0,
            max_response_bytes: 0,
            grant_token: grant.to_token(),
            op: Some(request::Op::GetInventory(GetInventory {
                sections: Vec::new(),
                cursor: cursor.clone(),
                page_entries: 0,
            })),
        };
        let reply = a
            .node
            .discovery()
            .call(&b.id(), b.node.addr(), request, CALL)
            .await
            .expect("a reply");
        let (page, _) = page_of(&reply, &b.id(), &a.id());
        assert_eq!(page.scope, format!("task:{project}"));
        if let Some(entry) = page.entries.iter().find(|entry| entry.id == MCP) {
            // Its 40 tools, their texts cut to one cap.
            let published: Vec<String> = tools.iter().map(|tool| format!("{MCP}/{tool}")).collect();
            assert_eq!(tool_names(entry), published);
            let description = entry.body["tools"][0]["description"].as_str().unwrap();
            assert!(description.len() < 1024, "{}", description.len());
        }
        ids.extend(page.entries.iter().map(|entry| entry.id.clone()));
        match page.next_cursor {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(ids, ["repo-review", MCP, "example:coder"]);
}
