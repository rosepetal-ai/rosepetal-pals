//! [`Outbox`]: the questions this agent asks its peers, sent through its own
//! gateway and kept in the store's outbox, so that none is lost and none
//! runs twice.
//!
//! An ask gets a new `messageId`, a UUID v4, and its whole
//! `SendMessageRequest` is written to the outbox before the first attempt
//! to send it. Every attempt goes through the gateway
//! (`<gateway>/peers/<agent id>/rest`) with the stock A2A client, on the
//! HTTP+JSON binding, whose errors keep the gateway's HTTP status, and with
//! `A2A-Version: 1.0`:
//!
//! - while the peer's task is not known, the same request, byte for byte,
//!   with `SendStreamingMessage`: the peer runs the first copy that reaches
//!   it, and attaches every other one to that run;
//! - once it is known, `GetTask`, and `SubscribeToTask` while it runs.
//!
//! An attempt fails on the way when the gateway cannot be reached or answers
//! a server error (`502` when it cannot reach the peer), when a stream
//! breaks, or when it ends before the task does. The ask then waits 1 s,
//! 2 s and 4 s before each of its [`ASK_MAX_ATTEMPTS`] retries; an attempt
//! that saw the task move starts the count again. When the retries run out,
//! the row stays pending and the ask fails with [`AskError::Unreachable`]:
//! [`Outbox::resume`] takes it up again, and nothing else does, the daemon
//! that starts included. Any other refusal, of the gateway (`4xx`) or of the
//! peer (an A2A error), is [`AskError::Rejected`], at once.
//!
//! An ask ends when its task does (`COMPLETED`, `FAILED`, `CANCELED`,
//! `REJECTED`) or waits for the requester (`INPUT_REQUIRED`,
//! `AUTH_REQUIRED`), with the task as it is then; or when its `wait` runs
//! out, with the state last seen and the row still pending.
//!
//! What a peer sends is used only once checked. A task id that is not
//! [safe](is_safe_task_id) ends the attempt as refused, and is neither kept
//! nor ever put in a URL; a task the peer answers for must be the one asked
//! about.
//!
//! An attachment is read only when it is a regular file outside the agent's
//! data directory ([`Outbox::with_data_dir`]), which holds its key and its
//! grants: the path is resolved first, symbolic links and `..` included,
//! and the file opened must be the one checked.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use a2a::{
    A2AError, CancelTaskRequest, GetTaskRequest, Message, Part, PartContent, Role,
    SVC_PARAM_EXTENSIONS, SendMessageRequest, StreamResponse, SubscribeToTaskRequest, Task,
    TaskState, error_code,
};
use a2a_client::A2AClient;
use a2a_client::middleware::CallInterceptor;
use a2a_client::rest::RestTransport;
use a2a_client::transport::ServiceParams;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use pals_core::identity::AgentId;
use pals_core::store::{OUTBOX_PENDING, OUTBOX_REFUSED, OutboxRow, Store};
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

use crate::a2a::{IDEMPOTENCY_EXTENSION, is_safe_task_id};

/// How many times an ask retries an attempt that failed on the way, after
/// waiting 1 s, 2 s and 4 s.
pub const ASK_MAX_ATTEMPTS: u32 = 3;
/// The wait before each retry.
const BACKOFF: [Duration; ASK_MAX_ATTEMPTS as usize] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
/// How long one attempt to cancel a task may take.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(30);
/// The media type of an attachment whose extension says nothing.
const OCTET_STREAM: &str = "application/octet-stream";
/// Why a peer's answer naming an unsafe or another task is refused.
const INVALID_TASK_ID: &str = "the peer returned an invalid task id";

type Events = BoxStream<'static, Result<StreamResponse, A2AError>>;
type Client = A2AClient<RestTransport>;

/// A question for a peer.
#[derive(Debug, Clone)]
pub struct AskRequest {
    /// A contact name or an agent id.
    pub peer: String,
    pub question: String,
    /// Files sent with the question.
    pub attachments: Vec<PathBuf>,
    /// How long to wait for the answer.
    pub wait: Duration,
}

/// A file a peer answered with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactRef {
    /// The name the peer gave it: untrusted, never a path.
    pub name: String,
    /// Where this agent's gateway serves it: `<gateway>/blobs/<holder>/<hash>`.
    pub url: String,
}

/// How an ask ended, or where it stands when its wait ran out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AskOutcome {
    pub message_id: String,
    /// The peer's task, once known.
    pub task_id: Option<String>,
    /// The A2A name of the task's state: `TASK_STATE_COMPLETED`; a state
    /// that is not final (`TASK_STATE_WORKING`) when the wait ran out.
    pub state: String,
    /// The text of the task's artifacts; with none, the text of its status
    /// message, the reason a task failed.
    pub answer: Option<String>,
    /// The files the task's artifacts hold.
    pub artifacts: Vec<ArtifactRef>,
    /// How many exchanges with the peer the question has taken so far.
    pub attempts: u32,
}

/// Why an ask has no outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AskError {
    /// No attempt got through: the question is kept, pending, for
    /// [`Outbox::resume`].
    #[error("the peer could not be reached; the question is kept as message {message_id}")]
    Unreachable { message_id: String },
    /// The gateway or the peer refused the request.
    #[error("refused: {0}")]
    Rejected(String),
    /// Nothing was asked, or the outbox could not be read or written.
    #[error("{0}")]
    Local(String),
}

/// The questions this agent asks its peers, through the gateway at
/// `gateway_url`, kept in `store`.
///
/// Cloning is cheap; clones share the store and the HTTP client.
#[derive(Clone)]
pub struct Outbox {
    store: Arc<Store>,
    gateway_url: String,
    http: reqwest::Client,
    /// The agent's data directory, resolved: no attachment comes from it.
    data_dir: Option<PathBuf>,
}

impl Outbox {
    /// The outbox of `store`, asking through the gateway at `gateway_url`
    /// (`http://127.0.0.1:<port>`). It never goes through a proxy.
    pub fn new(store: Arc<Store>, gateway_url: String) -> Self {
        crate::install_crypto_provider();
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("an HTTP client with no proxy builds once a crypto provider is installed");
        Self {
            store,
            gateway_url: gateway_url.trim_end_matches('/').to_owned(),
            http,
            data_dir: None,
        }
    }

    /// This outbox, refusing to send any file inside `data_dir`, the
    /// agent's data directory: its key, its store with the grants it holds,
    /// its blobs and its tasks' files. The directory is resolved now, as
    /// attachments are.
    pub fn with_data_dir(mut self, data_dir: &Path) -> Self {
        let resolved = std::fs::canonicalize(data_dir)
            .or_else(|_| std::path::absolute(data_dir))
            .unwrap_or_else(|_| data_dir.to_owned());
        self.data_dir = Some(resolved);
        self
    }

    /// The agent's data directory, resolved, as [`Outbox::with_data_dir`]
    /// set it.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// Asks `req.peer` `req.question`, with `req.attachments` uploaded to
    /// the gateway as blobs and sent as file parts, and waits up to
    /// `req.wait` for the answer.
    ///
    /// Fails with [`AskError::Local`], having sent nothing, when the peer is
    /// neither a contact nor an agent id, or an attachment cannot be read
    /// or uploaded: one that is not a regular file, or is inside the data
    /// directory, is refused.
    pub async fn ask(&self, req: AskRequest) -> Result<AskOutcome, AskError> {
        self.ask_kept(req, |_| {}).await
    }

    /// [`Outbox::ask`], calling `kept` with the question's message id as
    /// soon as the outbox keeps it, before the first attempt to send it:
    /// from then on, [`Outbox::resume`] takes it up by that id.
    pub async fn ask_kept(
        &self,
        req: AskRequest,
        kept: impl FnOnce(&str) + Send,
    ) -> Result<AskOutcome, AskError> {
        let AskRequest {
            peer,
            question,
            attachments,
            wait,
        } = req;
        let peer = self.resolve(peer).await?;
        let mut parts = vec![Part::text(question)];
        for path in &attachments {
            parts.push(self.upload(path).await?);
        }
        let message_id = new_message_id();
        let mut message = Message::new(Role::User, parts);
        message.message_id = message_id.clone();
        let request = SendMessageRequest {
            message,
            configuration: None,
            metadata: None,
            tenant: None,
        };
        let request_json = serde_json::to_string(&request)
            .map_err(|err| AskError::Local(format!("cannot encode the request: {err}")))?;
        // What every attempt sends is what the outbox keeps.
        let request = decode_request(&request_json)?;
        let now = now_unix();
        let row = OutboxRow {
            message_id: message_id.clone(),
            peer,
            request_json,
            task_id: None,
            state: OUTBOX_PENDING.to_owned(),
            attempts: 0,
            created_at: now,
            updated_at: now,
        };
        self.with_store(move |store| store.outbox_insert(&row))
            .await?;
        kept(&message_id);
        self.drive(Ask {
            message_id,
            peer,
            request,
            task_id: None,
            wait,
        })
        .await
    }

    /// Takes up the question `message_id` again, as [`Outbox::ask`] left
    /// it, and waits up to `wait` for its answer: sends it again while its
    /// task is not known, follows the task once it is. A question that has
    /// an outcome gets it again.
    pub async fn resume(&self, message_id: &str, wait: Duration) -> Result<AskOutcome, AskError> {
        let row = self.row(message_id).await?;
        check_stored_task(&row)?;
        let request = decode_request(&row.request_json)?;
        if row.state == OUTBOX_REFUSED {
            let id = row.message_id.clone();
            self.with_store(move |store| store.outbox_set_state(&id, OUTBOX_PENDING, now_unix()))
                .await?;
        }
        self.drive(Ask {
            message_id: row.message_id,
            peer: row.peer,
            request,
            task_id: row.task_id,
            wait,
        })
        .await
    }

    /// Cancels the task of the question `message_id`, retrying as an ask
    /// does (an attempt with no answer within 30 s failed on the way), and
    /// returns the task as the peer left it.
    ///
    /// [`AskError::Local`] when no task is known for it yet;
    /// [`AskError::Rejected`] when the peer refuses, as it does for a task
    /// that has ended.
    pub async fn cancel(&self, message_id: &str) -> Result<AskOutcome, AskError> {
        let row = self.row(message_id).await?;
        check_stored_task(&row)?;
        let Some(task_id) = row.task_id else {
            return Err(AskError::Local(format!(
                "no task of the peer is known for message {message_id} yet: there is nothing \
                 to cancel"
            )));
        };
        let client = self.client(&row.peer);
        let request = CancelTaskRequest {
            id: task_id,
            metadata: None,
            tenant: None,
        };
        let mut failures: u32 = 0;
        loop {
            let attempts = self.count_attempt(message_id).await?;
            let canceled = tokio::time::timeout(CANCEL_TIMEOUT, client.cancel_task(&request))
                .await
                .unwrap_or_else(|_| Err(A2AError::internal("the cancel got no answer in time")));
            match canceled {
                Ok(task) if task.id != request.id => {
                    return Err(AskError::Rejected(INVALID_TASK_ID.to_owned()));
                }
                Ok(task) => return self.finish(message_id, &task, attempts).await,
                Err(err) => match Failure::of(&err) {
                    Failure::Refused(reason) => return Err(AskError::Rejected(reason)),
                    Failure::Transport(reason) => {
                        tracing::debug!(message_id, %reason, "a cancel did not get through");
                        let Some(pause) = BACKOFF.get(failures as usize) else {
                            return Err(AskError::Unreachable {
                                message_id: message_id.to_owned(),
                            });
                        };
                        failures += 1;
                        tokio::time::sleep(*pause).await;
                    }
                },
            }
        }
    }

    /// The questions still waiting for an outcome, oldest first.
    pub fn pending(&self) -> anyhow::Result<Vec<OutboxRow>> {
        self.store.outbox_pending()
    }

    /// Attempts `ask` until it has an outcome, is refused, runs out of
    /// retries or of time.
    async fn drive(&self, mut ask: Ask) -> Result<AskOutcome, AskError> {
        let deadline = Instant::now() + ask.wait;
        let client = self.client(&ask.peer);
        let mut failures: u32 = 0;
        // The state last seen, by this call.
        let mut seen: Option<TaskState> = None;
        loop {
            let attempts = self.count_attempt(&ask.message_id).await?;
            let exchange = self.exchange(&client, &mut ask, &mut seen);
            let Ok(ended) = tokio::time::timeout_at(deadline, exchange).await else {
                return out_of_time(&ask, seen.as_ref(), attempts);
            };
            match ended {
                Ended::Task(task) => return self.finish(&ask.message_id, &task, attempts).await,
                Ended::Message(message) => {
                    let id = ask.message_id.clone();
                    let state = state_name(&TaskState::Completed);
                    self.with_store(move |store| store.outbox_set_state(&id, &state, now_unix()))
                        .await?;
                    return Ok(message_outcome(&ask.message_id, &message, attempts));
                }
                Ended::Refused(reason) => {
                    let id = ask.message_id.clone();
                    self.with_store(move |store| {
                        store.outbox_set_state(&id, OUTBOX_REFUSED, now_unix())
                    })
                    .await?;
                    return Err(AskError::Rejected(reason));
                }
                Ended::Local(err) => return Err(err),
                Ended::Broken { progressed, reason } => {
                    tracing::debug!(
                        message_id = %ask.message_id,
                        %reason,
                        "an attempt did not get through"
                    );
                    if progressed {
                        failures = 0;
                    }
                    let Some(pause) = BACKOFF.get(failures as usize) else {
                        return Err(AskError::Unreachable {
                            message_id: ask.message_id,
                        });
                    };
                    failures += 1;
                    // No retry would have time to get anywhere.
                    if Instant::now() + *pause >= deadline {
                        return out_of_time(&ask, seen.as_ref(), attempts);
                    }
                    tokio::time::sleep(*pause).await;
                }
            }
        }
    }

    /// One attempt: the request, while the task is not known; else the task,
    /// and its events while it runs.
    async fn exchange(
        &self,
        client: &Client,
        ask: &mut Ask,
        seen: &mut Option<TaskState>,
    ) -> Ended {
        let Some(task_id) = ask.task_id.clone() else {
            return match client.send_streaming_message(&ask.request).await {
                Ok(events) => self.follow(client, ask, events, seen).await,
                Err(err) => Ended::failed(&err, false),
            };
        };
        let task = match get_task(client, &task_id).await {
            Ok(task) if task.id != task_id => return Ended::Refused(INVALID_TASK_ID.to_owned()),
            Ok(task) => task,
            Err(err) => return Ended::failed(&err, false),
        };
        *seen = Some(task.status.state.clone());
        if is_final(&task.status.state) {
            return Ended::Task(task);
        }
        let subscription = SubscribeToTaskRequest {
            id: task_id.clone(),
            tenant: None,
        };
        match client.subscribe_to_task(&subscription).await {
            Ok(events) => self.follow(client, ask, events, seen).await,
            // It ended in between.
            Err(err) if err.code == error_code::UNSUPPORTED_OPERATION => {
                final_task(client, &task_id, seen).await
            }
            Err(err) => Ended::failed(&err, false),
        }
    }

    /// Follows `events` until the task is final, recording its id as soon
    /// as an event names it.
    async fn follow(
        &self,
        client: &Client,
        ask: &mut Ask,
        mut events: Events,
        seen: &mut Option<TaskState>,
    ) -> Ended {
        let mut progressed = false;
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(err) => return Ended::failed(&err, progressed),
            };
            progressed = true;
            if let StreamResponse::Message(message) = &event
                && message.task_id.is_none()
                && ask.task_id.is_none()
            {
                // The peer answered with a message and no task.
                return Ended::Message(message.clone());
            }
            if let Some(task_id) = event_task_id(&event)
                && ask.task_id.as_deref() != Some(task_id)
            {
                // Neither kept nor put in a URL.
                if !is_safe_task_id(task_id) || ask.task_id.is_some() {
                    return Ended::Refused(INVALID_TASK_ID.to_owned());
                }
                let (id, task) = (ask.message_id.clone(), task_id.to_owned());
                let recorded = self
                    .with_store(move |store| store.outbox_set_task(&id, &task, now_unix()))
                    .await;
                if let Err(err) = recorded {
                    return Ended::Local(err);
                }
                ask.task_id = Some(task_id.to_owned());
            }
            match event {
                StreamResponse::Task(task) => {
                    *seen = Some(task.status.state.clone());
                    if is_final(&task.status.state) {
                        return Ended::Task(task);
                    }
                }
                StreamResponse::StatusUpdate(update) => {
                    *seen = Some(update.status.state.clone());
                    if is_final(&update.status.state) {
                        // The whole task, artifacts included: the one
                        // checked and recorded above.
                        return final_task(client, &update.task_id, seen).await;
                    }
                }
                StreamResponse::ArtifactUpdate(_) | StreamResponse::Message(_) => {}
            }
        }
        Ended::Broken {
            progressed,
            reason: "the stream ended before the task".to_owned(),
        }
    }

    /// Records the outcome of `task`, once final, and returns it.
    async fn finish(
        &self,
        message_id: &str,
        task: &Task,
        attempts: u32,
    ) -> Result<AskOutcome, AskError> {
        if is_final(&task.status.state) {
            let (id, state) = (message_id.to_owned(), state_name(&task.status.state));
            self.with_store(move |store| store.outbox_set_state(&id, &state, now_unix()))
                .await?;
        }
        Ok(task_outcome(message_id, task, attempts))
    }

    /// The agent `peer` names: an agent id, or a contact's name.
    async fn resolve(&self, peer: String) -> Result<AgentId, AskError> {
        if let Ok(id) = peer.parse::<AgentId>() {
            return Ok(id);
        }
        let name = peer.clone();
        let contact = self
            .with_store(move |store| store.contact_by_name(&name))
            .await?;
        contact.map(|contact| contact.agent_id).ok_or_else(|| {
            AskError::Local(format!(
                "unknown peer {peer}: neither a contact nor an agent id"
            ))
        })
    }

    /// Uploads the file at `path` to the gateway as a blob, and returns the
    /// file part that sends it: its gateway URL, its name and its media
    /// type.
    async fn upload(&self, path: &Path) -> Result<Part, AskError> {
        let cannot =
            |what: String| AskError::Local(format!("cannot send {}: {what}", path.display()));
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| cannot("it names no file".to_owned()))?;
        let (path_owned, data_dir) = (path.to_owned(), self.data_dir.clone());
        let file =
            tokio::task::spawn_blocking(move || open_attachment(&path_owned, data_dir.as_deref()))
                .await
                .map_err(|err| cannot(err.to_string()))?
                .map_err(cannot)?;
        let file = tokio::fs::File::from_std(file);
        let response = self
            .http
            .post(format!("{}/blobs", self.gateway_url))
            .header(http::header::CONTENT_TYPE, OCTET_STREAM)
            .body(reqwest::Body::from(file))
            .send()
            .await
            .map_err(|err| cannot(format!("the gateway cannot be reached ({err})")))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|err| cannot(format!("no answer from the gateway ({err})")))?;
        if status != http::StatusCode::CREATED {
            return Err(cannot(format!(
                "the gateway answered {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }
        let url = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|created| created["url"].as_str().map(str::to_owned))
            .ok_or_else(|| cannot("the gateway gave no URL for it".to_owned()))?;
        Ok(Part::url(url)
            .with_filename(name)
            .with_media_type(media_type(path)))
    }

    /// The A2A client for `peer`, through the gateway. Every request it
    /// sends activates the [idempotency extension](IDEMPOTENCY_EXTENSION),
    /// which the resends of an ask rely on.
    fn client(&self, peer: &AgentId) -> Client {
        let base_url = format!("{}/peers/{peer}/rest", self.gateway_url);
        A2AClient::new(RestTransport::new(self.http.clone(), base_url))
            .with_interceptors(vec![Arc::new(Idempotent)])
    }

    /// The row of `message_id`.
    async fn row(&self, message_id: &str) -> Result<OutboxRow, AskError> {
        let id = message_id.to_owned();
        self.with_store(move |store| store.outbox_get(&id))
            .await?
            .ok_or_else(|| {
                AskError::Local(format!("no question was asked as message {message_id}"))
            })
    }

    /// Counts one more attempt for `message_id`; how many it has taken.
    async fn count_attempt(&self, message_id: &str) -> Result<u32, AskError> {
        let id = message_id.to_owned();
        self.with_store(move |store| store.outbox_attempt(&id, now_unix()))
            .await
    }

    /// Runs a store call off the async threads.
    async fn with_store<T: Send + 'static>(
        &self,
        call: impl FnOnce(&Store) -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, AskError> {
        let store = Arc::clone(&self.store);
        match tokio::task::spawn_blocking(move || call(&store)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(AskError::Local(format!("the outbox failed: {err:#}"))),
            Err(err) => Err(AskError::Local(format!("the outbox failed: {err}"))),
        }
    }
}

/// A question being attempted.
struct Ask {
    message_id: String,
    peer: AgentId,
    /// What every attempt that sends it sends.
    request: SendMessageRequest,
    task_id: Option<String>,
    wait: Duration,
}

/// How an attempt ended.
enum Ended {
    /// With the task final.
    Task(Task),
    /// With a message and no task.
    Message(Message),
    /// Refused, by the gateway or by the peer.
    Refused(String),
    /// On the way: the attempt may be retried. `progressed` when it saw the
    /// task move first.
    Broken { progressed: bool, reason: String },
    /// The outbox could not be written.
    Local(AskError),
}

impl Ended {
    fn failed(err: &A2AError, progressed: bool) -> Self {
        match Failure::of(err) {
            Failure::Transport(reason) => Self::Broken { progressed, reason },
            Failure::Refused(reason) => Self::Refused(reason),
        }
    }
}

/// Why an exchange failed.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// On the way: the gateway or the peer could not be reached, a stream
    /// broke, or a server failed. Worth retrying.
    Transport(String),
    /// The gateway or the peer refused the request.
    Refused(String),
}

impl Failure {
    /// What the A2A client's `err` was.
    ///
    /// The peer's A2A errors keep their code, and all but `InternalError`
    /// are refusals. A response with no A2A error, from the gateway or from
    /// the peer's authentication, the client reports as an `InternalError`
    /// with the message `HTTP <status>: <body>`: a `4xx` other than `408`
    /// and `429` is a refusal. Every other `InternalError` (no connection,
    /// a broken stream, a server error) is on the way.
    fn of(err: &A2AError) -> Self {
        if err.code != error_code::INTERNAL_ERROR {
            return Self::Refused(err.message.clone());
        }
        match http_failure(&err.message) {
            Some((status, reason))
                if (400..500).contains(&status) && status != 408 && status != 429 =>
            {
                Self::Refused(format!("HTTP {status}: {reason}"))
            }
            _ => Self::Transport(err.message.clone()),
        }
    }
}

/// The status and reason of an `HTTP <status> <phrase>: <body>` message: the
/// body's `error` when it is JSON with one, else the body.
fn http_failure(message: &str) -> Option<(u16, String)> {
    let rest = message.strip_prefix("HTTP ")?;
    let status = rest.get(..3)?.parse::<u16>().ok()?;
    let body = rest.split_once(": ").map_or("", |(_, body)| body);
    let reason = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.to_owned());
    Some((status, reason))
}

/// `task_id`, which an event said is final, as the peer keeps it.
async fn final_task(client: &Client, task_id: &str, seen: &mut Option<TaskState>) -> Ended {
    match get_task(client, task_id).await {
        Ok(task) if task.id != task_id => Ended::Refused(INVALID_TASK_ID.to_owned()),
        Ok(task) if is_final(&task.status.state) => Ended::Task(task),
        Ok(task) => {
            *seen = Some(task.status.state);
            Ended::Broken {
                progressed: true,
                reason: "the task is not final yet".to_owned(),
            }
        }
        Err(err) => Ended::failed(&err, true),
    }
}

/// `task_id`, without its history.
async fn get_task(client: &Client, task_id: &str) -> Result<Task, A2AError> {
    client
        .get_task(&GetTaskRequest {
            id: task_id.to_owned(),
            history_length: Some(0),
            tenant: None,
        })
        .await
}

/// What an ask whose wait ran out returns: where its task stands, or
/// [`AskError::Unreachable`] when this call saw nothing of it.
fn out_of_time(ask: &Ask, seen: Option<&TaskState>, attempts: u32) -> Result<AskOutcome, AskError> {
    match seen {
        Some(state) => Ok(AskOutcome {
            message_id: ask.message_id.clone(),
            task_id: ask.task_id.clone(),
            state: state_name(state),
            answer: None,
            artifacts: Vec::new(),
            attempts,
        }),
        None => Err(AskError::Unreachable {
            message_id: ask.message_id.clone(),
        }),
    }
}

/// Whether a task in `state` is done with, for now: ended, or waiting for
/// the requester.
fn is_final(state: &TaskState) -> bool {
    state.is_terminal() || matches!(state, TaskState::InputRequired | TaskState::AuthRequired)
}

/// The task an event names.
fn event_task_id(event: &StreamResponse) -> Option<&str> {
    match event {
        StreamResponse::Task(task) => Some(&task.id),
        StreamResponse::StatusUpdate(update) => Some(&update.task_id),
        StreamResponse::ArtifactUpdate(update) => Some(&update.task_id),
        StreamResponse::Message(message) => message.task_id.as_deref(),
    }
}

/// The A2A name of `state`: `TASK_STATE_COMPLETED`.
fn state_name(state: &TaskState) -> String {
    match serde_json::to_value(state) {
        Ok(Value::String(name)) => name,
        _ => "TASK_STATE_UNSPECIFIED".to_owned(),
    }
}

/// The outcome `task` gives the question `message_id`.
fn task_outcome(message_id: &str, task: &Task, attempts: u32) -> AskOutcome {
    let mut answer = Answer::default();
    for artifact in task.artifacts.iter().flatten() {
        answer.add(&artifact.parts, artifact.name.as_deref());
    }
    let mut text = answer.text();
    if text.is_none()
        && let Some(message) = &task.status.message
    {
        text = Answer::of(&message.parts).text();
    }
    AskOutcome {
        message_id: message_id.to_owned(),
        task_id: Some(task.id.clone()),
        state: state_name(&task.status.state),
        answer: text,
        artifacts: answer.files,
        attempts,
    }
}

/// The outcome of a question the peer answered with `message` and no task:
/// complete.
fn message_outcome(message_id: &str, message: &Message, attempts: u32) -> AskOutcome {
    let answer = Answer::of(&message.parts);
    AskOutcome {
        message_id: message_id.to_owned(),
        task_id: None,
        state: state_name(&TaskState::Completed),
        answer: answer.text(),
        artifacts: answer.files,
        attempts,
    }
}

/// The text and the files of an answer's parts.
#[derive(Default)]
struct Answer {
    texts: Vec<String>,
    files: Vec<ArtifactRef>,
}

impl Answer {
    fn of(parts: &[Part]) -> Self {
        let mut answer = Self::default();
        answer.add(parts, None);
        answer
    }

    /// Adds `parts`; a file part with no name of its own is named `name`.
    fn add(&mut self, parts: &[Part], name: Option<&str>) {
        for part in parts {
            match &part.content {
                PartContent::Text(text) => self.texts.push(text.clone()),
                PartContent::Url(url) => self.files.push(ArtifactRef {
                    name: part
                        .filename
                        .as_deref()
                        .or(name)
                        .unwrap_or("file")
                        .to_owned(),
                    url: url.clone(),
                }),
                PartContent::Raw(_) | PartContent::Data(_) => {}
            }
        }
    }

    /// The texts, one per line; none when there are none.
    fn text(&self) -> Option<String> {
        (!self.texts.is_empty()).then(|| self.texts.join("\n"))
    }
}

/// Opens the attachment at `path` to read it, once resolved (symbolic links
/// and `..` included): it must be outside `data_dir` and a regular file,
/// checked before it is opened, so that a FIFO cannot block the call, and
/// the file opened must be the one checked.
fn open_attachment(path: &Path, data_dir: Option<&Path>) -> Result<std::fs::File, String> {
    let resolved = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
    if let Some(data_dir) = data_dir
        && resolved.starts_with(data_dir)
    {
        return Err(format!(
            "it is inside this agent's data directory {}, which holds its key and its grants: \
             nothing in it is ever sent",
            data_dir.display()
        ));
    }
    let checked = std::fs::metadata(&resolved).map_err(|err| err.to_string())?;
    if !checked.is_file() {
        return Err("not a regular file".to_owned());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // Should the path have become a FIFO or a link since, opening it does
    // not wait for a writer nor follow the link.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOFOLLOW,
    );
    let file = options.open(&resolved).map_err(|err| err.to_string())?;
    let opened = file.metadata().map_err(|err| err.to_string())?;
    if !opened.is_file() || !same_file(&checked, &opened) {
        return Err("it changed while it was being opened".to_owned());
    }
    Ok(file)
}

/// Whether `a` and `b` describe the same file.
#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Whether `a` and `b` describe the same file: as far as this platform
/// tells, files of the same size.
#[cfg(not(unix))]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.len() == b.len()
}

/// Refuses a row whose task id is not [safe](is_safe_task_id), as a build
/// that did not check them may have kept it.
fn check_stored_task(row: &OutboxRow) -> Result<(), AskError> {
    match &row.task_id {
        Some(task_id) if !is_safe_task_id(task_id) => {
            Err(AskError::Rejected(INVALID_TASK_ID.to_owned()))
        }
        _ => Ok(()),
    }
}

/// The request the outbox keeps as `json`.
fn decode_request(json: &str) -> Result<SendMessageRequest, AskError> {
    serde_json::from_str(json)
        .map_err(|err| AskError::Local(format!("the outbox holds an unreadable request: {err}")))
}

/// A new random UUID v4, as a `messageId`.
fn new_message_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random number generator failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Activates the [`IDEMPOTENCY_EXTENSION`] on every request, with the
/// `A2A-Extensions` service parameter.
struct Idempotent;

#[async_trait]
impl CallInterceptor for Idempotent {
    async fn before(&self, _method: &str, params: &mut ServiceParams) -> Result<(), A2AError> {
        params.insert(
            SVC_PARAM_EXTENSIONS.to_owned(),
            vec![IDEMPOTENCY_EXTENSION.to_owned()],
        );
        Ok(())
    }
}

/// The media type an attachment is sent with, from its extension;
/// `application/octet-stream` when the extension says nothing.
fn media_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("zip") => "application/zip",
        _ => OCTET_STREAM,
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_are_random_uuid_v4() {
        let id = new_message_id();
        assert_eq!(id.len(), 36, "{id}");
        let groups: Vec<&str> = id.split('-').collect();
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(groups[2].starts_with('4'), "{id}");
        assert!(matches!(&groups[3][..1], "8" | "9" | "a" | "b"), "{id}");
        assert!(id.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit()));
        assert_eq!(id, id.to_ascii_lowercase());
        assert_ne!(new_message_id(), id);
    }

    #[test]
    fn failures_on_the_way_are_retried_and_refusals_are_not() {
        let internal = |message: &str| A2AError::internal(message);
        let transport = |err: A2AError| matches!(Failure::of(&err), Failure::Transport(_));
        // No connection, a broken stream, a server error, the gateway's
        // 502 for a peer out of reach, an overloaded peer.
        for message in [
            "HTTP request failed: error sending request",
            "SSE stream error: error decoding response body",
            "stream ended with 12 unparseable trailing byte(s)",
            "the first request with this messageId is still in progress",
            r#"HTTP 502 Bad Gateway: {"error":"peer unreachable"}"#,
            r#"HTTP 500 Internal Server Error: {"error":"internal error"}"#,
            "HTTP 429 Too Many Requests: ",
        ] {
            assert!(transport(internal(message)), "{message}");
        }
        // The gateway's and the peer's own refusals.
        assert_eq!(
            Failure::of(&internal(
                r#"HTTP 401 Unauthorized: {"error":"no grant held for abc"}"#
            )),
            Failure::Refused("HTTP 401: no grant held for abc".to_owned())
        );
        assert_eq!(
            Failure::of(&internal("HTTP 403 Forbidden: not json")),
            Failure::Refused("HTTP 403: not json".to_owned())
        );
        // An A2A error of the peer.
        assert_eq!(
            Failure::of(&A2AError::invalid_params("messageId reused")),
            Failure::Refused("messageId reused".to_owned())
        );
        assert!(!transport(A2AError::task_not_found("t-1")));
    }

    #[test]
    fn the_answer_is_the_artifacts_text_else_the_status_reason() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "t-1",
            "contextId": "c-1",
            "status": {"state": "TASK_STATE_COMPLETED"},
            "artifacts": [{
                "artifactId": "a-1",
                "name": "answer",
                "parts": [
                    {"text": "first"},
                    {"url": "http://gw/blobs/h/1", "filename": "out.txt"},
                    {"url": "http://gw/blobs/h/2"},
                    {"text": "second"}
                ]
            }]
        }))
        .unwrap();
        let outcome = task_outcome("m-1", &task, 2);
        assert_eq!(outcome.state, "TASK_STATE_COMPLETED");
        assert_eq!(outcome.task_id.as_deref(), Some("t-1"));
        assert_eq!(outcome.answer.as_deref(), Some("first\nsecond"));
        assert_eq!(
            outcome.artifacts,
            [
                ArtifactRef {
                    name: "out.txt".to_owned(),
                    url: "http://gw/blobs/h/1".to_owned()
                },
                ArtifactRef {
                    name: "answer".to_owned(),
                    url: "http://gw/blobs/h/2".to_owned()
                },
            ]
        );

        let failed: Task = serde_json::from_value(serde_json::json!({
            "id": "t-2",
            "contextId": "c-1",
            "status": {
                "state": "TASK_STATE_FAILED",
                "message": {"messageId": "m", "role": "ROLE_AGENT", "parts": [{"text": "why"}]}
            }
        }))
        .unwrap();
        let outcome = task_outcome("m-2", &failed, 1);
        assert_eq!(outcome.state, "TASK_STATE_FAILED");
        assert_eq!(outcome.answer.as_deref(), Some("why"));
        assert!(outcome.artifacts.is_empty());
    }

    #[test]
    fn attachments_get_a_media_type_from_their_extension() {
        assert_eq!(media_type(Path::new("/a/notes.TXT")), "text/plain");
        assert_eq!(media_type(Path::new("/a/data.json")), "application/json");
        assert_eq!(media_type(Path::new("/a/blob")), OCTET_STREAM);
        assert_eq!(media_type(Path::new("/a/x.unknown")), OCTET_STREAM);
    }
}
