//! The inventory adapter of MCP connections ([`McpAdapter`]): it observes
//! whether a connection the inventory configures is operational by asking
//! its server what tools it serves, and never calls one.
//!
//! **What it does.** For an entry with an `adapter`, within the adapter's
//! time ([`MCP_ADAPTER_TIMEOUT`] unless [`McpAdapter::new`] says otherwise):
//! MCP `initialize`, then `tools/list` page by page, never `tools/call`. It
//! stops once the server has listed more than [`MAX_MCP_TOOLS`] tools, or
//! after [`MAX_MCP_PAGES`] pages with yet another to come; it never gathers
//! every page before counting. Of what it lists, it keeps only the tools of
//! `offered_tools`, each once, with its description and the `title` of its
//! annotations cut to [`MAX_OBSERVED_DESCRIPTION_BYTES`] on a character
//! boundary, and its annotations as the server declares them (§9.3:
//! declarations, never a security policy): names and texts from a server
//! are untrusted, and nothing else of them is kept.
//!
//! **What it observes** ([`Observation`], by [`AdapterMethod::McpToolsList`],
//! valid for twice the refresh interval):
//!
//! | Outcome | Status and `detail` |
//! |-|-|
//! | every offered tool listed | `ok` |
//! | some offered tool not listed | `failed`, `missing tools: <those>`: the connection is configured but does not give what is offered |
//! | no connection: nothing listens, the command does not start, the server hangs up | `failed`, [`DETAIL_UNREACHABLE`] |
//! | the time ran out | `failed`, [`DETAIL_TIMEOUT`] |
//! | an answer that is not MCP | `failed`, [`DETAIL_PROTOCOL_ERROR`]; over stdio, a line that is not JSON or is longer than [`MAX_MCP_LINE_BYTES`] is skipped (rmcp's transport does so), and the server's silence ends as [`DETAIL_TIMEOUT`]: a fixed code all the same, within the adapter's time |
//! | more than [`MAX_MCP_TOOLS`] tools or [`MAX_MCP_PAGES`] pages | `failed`, [`DETAIL_TOO_MANY_TOOLS`] |
//!
//! `detail` is only ever one of those fixed codes. The raw error, which may
//! name the URL or the command, goes to the daemon's log, as its `Debug`
//! form, and nowhere else: never to a view, an offer, an observation or the
//! CLI.
//!
//! **Transports** ([`McpTransport`]):
//!
//! - `http`: Streamable HTTP at `url`, with a client that never goes
//!   through a proxy and never follows a redirect; an event of a stream at
//!   most [`MAX_MCP_LINE_BYTES`]. The URL is `https://`, or `http://` to
//!   `127.0.0.1`, `[::1]` or `localhost`, without credentials
//!   ([`check_adapters`]).
//! - `stdio`: the command, started with `tokio::process::Command` as the
//!   leader of a process group of its own, as the `exec` worker adapter
//!   starts its commands, with MCP on its stdin and stdout and a message at
//!   most [`MAX_MCP_LINE_BYTES`] (`AsyncRwTransport`, whose limit can be
//!   set, unlike `TokioChildProcess`'s 16 MiB); stderr discarded; an
//!   environment with nothing but [`PASSED_ENV`] and the variables
//!   `env_passthrough` names, taken from the daemon's own, whose values are
//!   never kept, logged or published. When the observation ends, however it
//!   ends, the whole group is killed and the command reaped (`GroupLeader`,
//!   the `exec` adapter's own). A descendant that leaves the group
//!   (`setsid()`, `setpgid()`) escapes the kill, as with the `exec`
//!   adapter; and so does the whole group if the daemon is killed with
//!   SIGKILL in the middle of an observation, since nothing sets
//!   `PR_SET_PDEATHSIG`, the same limit as the `exec` adapter's.
//!
//! An inventory configures at most [`MAX_MCP_CONNECTIONS`] adapters
//! ([`check_adapters`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pals_core::clock::{Clock, SystemClock};
use pals_core::grant::format_timestamp;
use pals_core::inventory::{
    AdapterMethod, DETAIL_PROTOCOL_ERROR, DETAIL_TIMEOUT, DETAIL_TOO_MANY_TOOLS,
    DETAIL_UNREACHABLE, Entry, Inventory, MAX_OBSERVED_DESCRIPTION_BYTES, McpConnection,
    McpTransport, Observation, ObservationSource, ObservedTool, OperationalStatus,
    missing_tools_detail,
};
use reqwest::Url;
use rmcp::model::{ClientConfig, PaginatedRequestParams, Tool};
use rmcp::service::{ClientInitializeError, RunningService};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpError,
};
use rmcp::transport::{DynamicTransportError, StreamableHttpClientTransport};
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;
use tracing::warn;

/// How long one observation may take, connecting included.
pub const MCP_ADAPTER_TIMEOUT: Duration = Duration::from_secs(10);
/// The most tools a server may list; one more, and the observation is
/// [`DETAIL_TOO_MANY_TOOLS`].
pub const MAX_MCP_TOOLS: usize = 256;
/// The most MCP connections with an adapter in an inventory.
pub const MAX_MCP_CONNECTIONS: usize = 16;
/// The most pages of `tools/list` read; a next one after them, and the
/// observation is [`DETAIL_TOO_MANY_TOOLS`].
pub const MAX_MCP_PAGES: usize = 16;
/// The longest message of a server, a line of stdio or an event of a
/// stream; a longer one is dropped.
pub const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;
/// The variables a `stdio` server takes from the daemon's environment
/// besides those its adapter names.
pub const PASSED_ENV: [&str; 3] = ["PATH", "HOME", "LANG"];

/// How long an observation lasts unless [`McpAdapter::with_validity`] says
/// otherwise: twice the default refresh interval.
const DEFAULT_VALIDITY_S: i64 = 1800;
/// The hosts an `http://` adapter may reach.
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "[::1]", "localhost"];

/// Refuses the adapters an inventory may not configure: more than
/// [`MAX_MCP_CONNECTIONS`]; an `http` URL that is not `https://`, or
/// `http://` to a loopback name, or that carries credentials; an
/// `env_passthrough` entry that is not the name of an environment variable
/// (`[A-Za-z_][A-Za-z0-9_]*`). The error names the entry, never the URL.
pub fn check_adapters(inventory: &Inventory) -> anyhow::Result<()> {
    let configured = inventory
        .mcp
        .iter()
        .filter(|entry| entry.body.adapter.is_some())
        .count();
    anyhow::ensure!(
        configured <= MAX_MCP_CONNECTIONS,
        "invalid inventory: {configured} MCP connections with an adapter, at most \
         {MAX_MCP_CONNECTIONS}"
    );
    for (i, entry) in inventory.mcp.iter().enumerate() {
        let at = format!("mcp[{i}] ({})", entry.id);
        match entry
            .body
            .adapter
            .as_ref()
            .map(|adapter| &adapter.transport)
        {
            Some(McpTransport::Http { url }) => anyhow::ensure!(
                is_allowed_url(url),
                "invalid inventory: {at}.adapter.url must be https://<host>, or http:// to \
                 127.0.0.1, [::1] or localhost, without credentials"
            ),
            Some(McpTransport::Stdio {
                env_passthrough, ..
            }) => {
                if let Some((k, _)) = env_passthrough
                    .iter()
                    .enumerate()
                    .find(|(_, name)| !is_env_var_name(name))
                {
                    anyhow::bail!(
                        "invalid inventory: {at}.adapter.env_passthrough[{k}] is not the name \
                         of an environment variable: letters, digits and '_', not starting \
                         with a digit"
                    );
                }
            }
            None => {}
        }
    }
    Ok(())
}

/// Whether an `http` adapter may reach `url`: `https://` to a host, or
/// `http://` to a loopback name; no credentials either way. Read with the
/// parser the client connects with.
fn is_allowed_url(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let host = url.host_str();
    url.username().is_empty()
        && url.password().is_none()
        && match url.scheme() {
            "https" => host.is_some_and(|host| !host.is_empty()),
            "http" => host.is_some_and(|host| LOOPBACK_HOSTS.contains(&host)),
            _ => false,
        }
}

/// Whether `name` is a portable environment variable name.
fn is_env_var_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Observes MCP connections, as the [module](self) says. `Send + Sync`.
pub struct McpAdapter {
    timeout: Duration,
    validity_s: i64,
    clock: Arc<dyn Clock>,
    /// For the `http` transport, built when it is first needed: no proxy,
    /// no redirect.
    http: OnceLock<reqwest::Client>,
    /// The `tools/list` requests sent, every page counted.
    list_calls: AtomicU64,
}

/// Why a connection was not observed operational, before it lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Unreachable,
    Timeout,
    Protocol,
    TooManyTools,
}

impl Failure {
    /// The fixed code an observation carries.
    fn code(self) -> &'static str {
        match self {
            Failure::Unreachable => DETAIL_UNREACHABLE,
            Failure::Timeout => DETAIL_TIMEOUT,
            Failure::Protocol => DETAIL_PROTOCOL_ERROR,
            Failure::TooManyTools => DETAIL_TOO_MANY_TOOLS,
        }
    }
}

impl McpAdapter {
    /// An adapter whose observations take at most `timeout` each
    /// ([`MCP_ADAPTER_TIMEOUT`] for the daemon's), at the system's clock,
    /// lasting 30 minutes.
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            validity_s: DEFAULT_VALIDITY_S,
            clock: Arc::new(SystemClock),
            http: OnceLock::new(),
            list_calls: AtomicU64::new(0),
        }
    }

    /// The client of the `http` transport, built the first time it is
    /// needed; `None`, logged, when it cannot be built, which fails the
    /// observation as [`DETAIL_UNREACHABLE`] and lets the next one try
    /// again.
    fn http(&self, id: &str) -> Option<reqwest::Client> {
        if let Some(http) = self.http.get() {
            return Some(http.clone());
        }
        crate::install_crypto_provider();
        let built = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(self.timeout)
            .build();
        match built {
            // Another observation may have built one meanwhile: either does.
            Ok(http) => Some(self.http.get_or_init(|| http).clone()),
            Err(err) => {
                warn!(entry = id, error = ?err, "MCP connection not observed: no HTTP client");
                None
            }
        }
    }

    /// Observations that last `validity_s` seconds from when they are made.
    pub fn with_validity(mut self, validity_s: i64) -> Self {
        self.validity_s = validity_s;
        self
    }

    /// Observations dated by `clock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The `tools/list` requests this adapter sent, every page counted.
    pub fn list_calls(&self) -> u64 {
        self.list_calls.load(Ordering::SeqCst)
    }

    /// What `entry`'s server serves now, as the [module](self) says: with
    /// `initialize` and `tools/list` page by page, never `tools/call`. An
    /// entry without an adapter is not observed: `unknown`, without detail.
    pub async fn observe(&self, entry: &Entry<McpConnection>) -> Observation {
        let offered = &entry.body.offered_tools;
        let listed = match entry
            .body
            .adapter
            .as_ref()
            .map(|adapter| &adapter.transport)
        {
            Some(McpTransport::Http { url }) => self.list_http(&entry.id, url, offered).await,
            Some(McpTransport::Stdio {
                command,
                args,
                env_passthrough,
            }) => {
                self.list_stdio(&entry.id, command, args, env_passthrough, offered)
                    .await
            }
            None => return self.observation(OperationalStatus::Unknown, None, Vec::new()),
        };
        match listed {
            Ok(tools) => {
                let missing: Vec<&String> = offered
                    .iter()
                    .filter(|name| !tools.iter().any(|tool| tool.name == **name))
                    .collect();
                if missing.is_empty() {
                    self.observation(OperationalStatus::Ok, None, tools)
                } else {
                    let detail = missing_tools_detail(&missing);
                    self.observation(OperationalStatus::Failed, Some(detail), tools)
                }
            }
            Err(failure) => self.observation(
                OperationalStatus::Failed,
                Some(failure.code().to_owned()),
                Vec::new(),
            ),
        }
    }

    /// An observation made now.
    fn observation(
        &self,
        status: OperationalStatus,
        detail: Option<String>,
        tools: Vec<ObservedTool>,
    ) -> Observation {
        let now = self.clock.now_unix();
        let timestamp =
            |at: i64| format_timestamp(at).unwrap_or_else(|| "9999-12-31T23:59:59Z".to_owned());
        Observation {
            source: ObservationSource::Adapter {
                method: AdapterMethod::McpToolsList,
            },
            status,
            observed_at: timestamp(now),
            expires_at: timestamp(now.saturating_add(self.validity_s)),
            detail,
            tools,
        }
    }

    /// The offered tools the Streamable HTTP server at `url` lists.
    async fn list_http(
        &self,
        id: &str,
        url: &str,
        offered: &[String],
    ) -> Result<Vec<ObservedTool>, Failure> {
        let config = StreamableHttpClientTransportConfig::with_uri(url)
            .max_sse_event_size(MAX_MCP_LINE_BYTES)
            .reinit_on_expired_session(false);
        // Before the time starts: building the client is not the server's.
        let http = self.http(id).ok_or(Failure::Unreachable)?;
        let transport = StreamableHttpClientTransport::with_client(http, config);
        let listed = tokio::time::timeout(self.timeout, self.list(id, transport, offered)).await;
        listed.unwrap_or_else(|_| {
            warn!(entry = id, "MCP connection not observed: timeout");
            Err(Failure::Timeout)
        })
    }

    /// The offered tools the server `command` started with `args` lists on
    /// its stdio, as the [module](self) says.
    #[cfg(unix)]
    async fn list_stdio(
        &self,
        id: &str,
        command: &str,
        args: &[String],
        env_passthrough: &[String],
        offered: &[String],
    ) -> Result<Vec<ObservedTool>, Failure> {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;

        use rmcp::transport::async_rw::AsyncRwTransport;

        use crate::process_group::GroupLeader;

        let mut cmd = std::process::Command::new(command);
        cmd.args(args).env_clear();
        // `check_adapters` refused other names already.
        for name in PASSED_ENV
            .iter()
            .copied()
            .chain(env_passthrough.iter().map(String::as_str))
            .filter(|name| is_env_var_name(name))
        {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.kill_on_drop(true);
        let child = cmd.spawn().map_err(|err| {
            warn!(entry = id, error = ?err, "MCP connection not observed: unreachable");
            Failure::Unreachable
        })?;
        let mut group = GroupLeader::new(child, "MCP server");
        let (Some(stdin), Some(stdout)) = (group.child().stdin.take(), group.child().stdout.take())
        else {
            group.kill().await;
            warn!(
                entry = id,
                "MCP connection not observed: the server has no pipes"
            );
            return Err(Failure::Unreachable);
        };
        let transport =
            AsyncRwTransport::new_client(stdout, stdin).with_max_line_length(MAX_MCP_LINE_BYTES);
        let listed = tokio::time::timeout(self.timeout, self.list(id, transport, offered)).await;
        // However it ended, and before it is reaped: nothing it started
        // outlives the observation.
        group.kill().await;
        listed.unwrap_or_else(|_| {
            warn!(entry = id, "MCP connection not observed: timeout");
            Err(Failure::Timeout)
        })
    }

    #[cfg(not(unix))]
    async fn list_stdio(
        &self,
        id: &str,
        _command: &str,
        _args: &[String],
        _env_passthrough: &[String],
        _offered: &[String],
    ) -> Result<Vec<ObservedTool>, Failure> {
        warn!(
            entry = id,
            "MCP connection not observed: stdio servers need a Unix system"
        );
        Err(Failure::Unreachable)
    }

    /// `initialize` over `transport`, then the offered tools of `tools/list`
    /// page by page, within the limits; the session closed after.
    async fn list<T, E, A>(
        &self,
        id: &str,
        transport: T,
        offered: &[String],
    ) -> Result<Vec<ObservedTool>, Failure>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let client = ClientConfig::default()
            .serve(transport)
            .await
            .map_err(|err| {
                let failure = initialize_failure(&err);
                warn!(entry = id, error = ?err, "MCP connection not observed: {}", failure.code());
                failure
            })?;
        let listed = self.pages(id, &client, offered).await;
        if let Err(err) = client.cancel().await {
            tracing::debug!(entry = id, error = ?err, "the MCP session did not close cleanly");
        }
        listed
    }

    /// The offered tools `client`'s server lists, page by page.
    async fn pages(
        &self,
        id: &str,
        client: &RunningService<RoleClient, ClientConfig>,
        offered: &[String],
    ) -> Result<Vec<ObservedTool>, Failure> {
        let mut kept: Vec<ObservedTool> = Vec::new();
        let mut listed = 0usize;
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_MCP_PAGES {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            let params = cursor
                .take()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let page = client.list_tools(params).await.map_err(|err| {
                warn!(entry = id, error = ?err, "MCP connection not observed: protocol error");
                Failure::Protocol
            })?;
            listed = listed.saturating_add(page.tools.len());
            if listed > MAX_MCP_TOOLS {
                warn!(
                    entry = id,
                    "MCP connection not observed: more than {MAX_MCP_TOOLS} tools"
                );
                return Err(Failure::TooManyTools);
            }
            for tool in page.tools {
                if offered.iter().any(|name| *name == tool.name)
                    && !kept.iter().any(|seen| seen.name == tool.name)
                {
                    kept.push(observed_tool(tool));
                }
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(kept),
            }
        }
        warn!(
            entry = id,
            "MCP connection not observed: more than {MAX_MCP_PAGES} pages"
        );
        Err(Failure::TooManyTools)
    }
}

/// The failure an `initialize` that did not succeed is: no connection, or
/// one that ended before the server answered, is [`Failure::Unreachable`];
/// an answer that is not a valid one, [`Failure::Protocol`].
fn initialize_failure(err: &ClientInitializeError) -> Failure {
    match err {
        ClientInitializeError::ConnectionClosed(_) => Failure::Unreachable,
        ClientInitializeError::TransportError { error, .. } => transport_failure(error),
        _ => Failure::Protocol,
    }
}

/// The failure a transport's error is: for Streamable HTTP, a request that
/// got no response (nothing listens, TLS fails, the connection drops) is
/// [`Failure::Unreachable`], and a response that is not MCP (a status
/// other than success, another content type, JSON that is not JSON-RPC) is
/// [`Failure::Protocol`]; for stdio, whose transport fails only on its
/// pipes, [`Failure::Unreachable`].
fn transport_failure(error: &DynamicTransportError) -> Failure {
    match error
        .error
        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
    {
        None | Some(StreamableHttpError::Client(_) | StreamableHttpError::Io(_)) => {
            Failure::Unreachable
        }
        Some(_) => Failure::Protocol,
    }
}

/// What is kept of an offered tool the server listed: its name, its
/// description and its annotations as the server declares them, its texts
/// cut to [`MAX_OBSERVED_DESCRIPTION_BYTES`] ([`ObservedTool::cut_texts`]).
fn observed_tool(tool: Tool) -> ObservedTool {
    let annotations = tool
        .annotations
        .and_then(|annotations| serde_json::to_value(annotations).ok())
        .filter(Value::is_object);
    let mut kept = ObservedTool {
        name: tool.name.into_owned(),
        description: tool.description.map(|text| text.into_owned()),
        annotations,
    };
    kept.cut_texts(MAX_OBSERVED_DESCRIPTION_BYTES);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(adapters: &str) -> Inventory {
        Inventory::from_toml(adapters).unwrap()
    }

    fn http(url: &str) -> String {
        format!(
            "[[mcp]]\nid = \"example:repo\"\ndescription = \"Reads the repo\"\n\
             provenance = \"operator\"\noffered_tools = [\"read_file\"]\n\
             [mcp.adapter]\ntransport = \"http\"\nurl = \"{url}\"\n"
        )
    }

    fn stdio(env: &str) -> String {
        format!(
            "[[mcp]]\nid = \"example:repo\"\ndescription = \"Reads the repo\"\n\
             provenance = \"operator\"\noffered_tools = [\"read_file\"]\n\
             [mcp.adapter]\ntransport = \"stdio\"\ncommand = \"npx\"\n\
             env_passthrough = [\"{env}\"]\n"
        )
    }

    #[test]
    fn an_http_adapter_reaches_https_or_loopback_http_only() {
        for url in [
            "https://mcp.example.org/mcp",
            "https://mcp.example.org:8443/",
            "http://127.0.0.1:3000/mcp",
            "http://localhost:3000/mcp",
            "http://[::1]:3000/mcp",
            "HTTP://LOCALHOST:3000/mcp",
        ] {
            check_adapters(&inventory(&http(url))).unwrap_or_else(|err| panic!("{url}: {err}"));
        }
        for url in [
            "http://10.0.0.7:3000/mcp",
            "http://example.org/mcp",
            "http://localhost.example.org/mcp",
            "http://127.0.0.2:3000/mcp",
            "ftp://127.0.0.1/mcp",
            "file:///srv/mcp",
            "https://user:secret@mcp.example.org/mcp",
            "http://token@127.0.0.1:3000/mcp",
            "not a url",
        ] {
            let err = check_adapters(&inventory(&http(url))).expect_err(url);
            let err = format!("{err:#}");
            assert!(err.contains("mcp[0] (example:repo).adapter.url"), "{err}");
            assert!(!err.contains("secret"), "{err}");
        }
    }

    #[test]
    fn env_passthrough_names_only_environment_variables() {
        for name in ["GITHUB_TOKEN", "_X", "a1"] {
            check_adapters(&inventory(&stdio(name))).unwrap_or_else(|err| panic!("{name}: {err}"));
        }
        for name in ["1ABC", "A-B", "A=B", "A B", "PATH=/tmp"] {
            let err = check_adapters(&inventory(&stdio(name))).expect_err(name);
            assert!(
                format!("{err:#}").contains("env_passthrough[0]"),
                "{name}: {err:#}"
            );
        }
    }

    #[test]
    fn at_most_16_connections_have_an_adapter() {
        let entries = |count: usize| -> String {
            (0..count)
                .map(|n| {
                    format!(
                        "[[mcp]]\nid = \"example:repo-{n}\"\ndescription = \"Reads\"\n\
                         provenance = \"operator\"\n[mcp.adapter]\ntransport = \"stdio\"\n\
                         command = \"npx\"\n\n"
                    )
                })
                .collect()
        };
        check_adapters(&inventory(&entries(MAX_MCP_CONNECTIONS))).unwrap();
        let err = check_adapters(&inventory(&entries(MAX_MCP_CONNECTIONS + 1))).unwrap_err();
        assert!(format!("{err:#}").contains("at most 16"), "{err:#}");
        // Declarations without an adapter do not count.
        let declared: String = (0..40)
            .map(|n| {
                format!(
                    "[[mcp]]\nid = \"example:declared-{n}\"\ndescription = \"Reads\"\n\
                     provenance = \"operator\"\n\n"
                )
            })
            .collect();
        check_adapters(&inventory(&format!("{}{declared}", entries(16)))).unwrap();
    }

    /// An MCP connection to `url`, offering `read_file`.
    fn http_entry(url: &str) -> Entry<McpConnection> {
        inventory(&http(url)).mcp.remove(0)
    }

    #[tokio::test]
    async fn an_http_answer_that_is_not_mcp_is_a_protocol_error() {
        let app = axum::Router::new()
            .route(
                "/html",
                axum::routing::any(|| async { ([("content-type", "text/html")], "<p>hi</p>") }),
            )
            .route(
                "/json",
                axum::routing::any(|| async {
                    ([("content-type", "application/json")], r#"{"hello":1}"#)
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let adapter = McpAdapter::new(MCP_ADAPTER_TIMEOUT);
        for path in ["missing", "html", "json"] {
            let observed = adapter
                .observe(&http_entry(&format!("http://{addr}/{path}")))
                .await;
            assert_eq!(observed.status, OperationalStatus::Failed, "{path}");
            assert_eq!(
                observed.detail.as_deref(),
                Some(DETAIL_PROTOCOL_ERROR),
                "{path}"
            );
            assert!(observed.tools.is_empty());
        }
        server.abort();
        assert_eq!(
            adapter.list_calls(),
            0,
            "nothing is listed before initialize"
        );
    }

    #[test]
    fn a_tool_keeps_its_name_and_its_texts_cut_on_a_character_boundary() {
        let long = "ñ".repeat(MAX_OBSERVED_DESCRIPTION_BYTES);
        let tool: Tool = serde_json::from_value(serde_json::json!({
            "name": "read_file",
            "description": long,
            "inputSchema": {"type": "object"},
            "annotations": {"title": long, "readOnlyHint": true},
            "_meta": {"example": "dropped"},
        }))
        .unwrap();
        let kept = observed_tool(tool);
        assert_eq!(kept.name, "read_file");
        let description = kept.description.unwrap();
        assert_eq!(description.len(), MAX_OBSERVED_DESCRIPTION_BYTES);
        assert!(description.chars().all(|c| c == 'ñ'));
        let annotations = kept.annotations.unwrap();
        assert_eq!(annotations["title"], description);
        assert_eq!(annotations["readOnlyHint"], true);
        assert_eq!(annotations.as_object().unwrap().len(), 2);

        let bare: Tool = serde_json::from_value(serde_json::json!({
            "name": "list_dir",
            "inputSchema": {"type": "object"},
            "annotations": {},
        }))
        .unwrap();
        let kept = observed_tool(bare);
        assert_eq!((kept.description, kept.annotations), (None, None));
    }
}
