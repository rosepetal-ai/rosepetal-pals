//! [`Outbox`]: the questions this agent asks its peers, sent through its own
//! gateway and kept in the store's outbox, so that none is lost and none
//! runs twice.
//!
//! An ask gets a new `messageId`, a UUID v4, and its whole
//! `SendMessageRequest` is written to the outbox before the first attempt
//! to send it, with the peer it goes to as its [`PeerRef::key`]: an agent id,
//! or `a2a:<name>` for a plain A2A contact (`crate::gateway::plain`), which
//! is asked, kept and taken up again just the same. Every attempt goes
//! through the gateway with the stock A2A client and `A2A-Version: 1.0`: to
//! a PALS peer at `<gateway>/peers/<agent id>/rest`, on the HTTP+JSON
//! binding, whose errors keep the gateway's HTTP status; to a plain contact
//! at `<gateway>/peers/<name>/rest`, or, when its card has no HTTP+JSON
//! interface and has a JSON-RPC one, at `<gateway>/peers/<name>/jsonrpc`
//! with the client's JSON-RPC transport. A plain contact's card is read
//! through the gateway, which checks it (`Outbox::plain_speaks`), before
//! anything is kept: when it cannot be had, or does not declare
//! `capabilities.streaming`, which the outbox needs
//! ([`PLAIN_NEEDS_STREAMING`]), the ask fails with [`AskError::Local`] and
//! nothing is asked. Then:
//!
//! - while the peer's task is not known, the same request, byte for byte,
//!   with `SendStreamingMessage`: a PALS peer runs the first copy that
//!   reaches it, and attaches every other one to that run, as does a plain
//!   agent whose card declares the idempotency extension;
//! - once it is known, `GetTask`, and `SubscribeToTask` while it runs.
//!
//! A question asked with the offer its peer was chosen by
//! ([`AskRequest::offer`]) names it in its message, under the inventory
//! extension's URI in `metadata` ([`OfferRef`]), and every attempt
//! activates that extension too, and presents the grant this agent holds
//! for the scope the offer was made in: being in the request kept, the
//! reference goes again with every retry. The provider checks the offer again, and
//! refuses a message whose offer is unknown, expired or stale before it
//! runs anything; the grant is checked first all the same.
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
//! A plain agent whose card does not declare the idempotency extension may
//! run every copy that reaches it. So while its task is not known, the
//! question goes again on its own only when it certainly never reached the
//! agent: the gateway could not be reached, its connection never made
//! (the route's own client, which keeps no connection, sees it), or the
//! gateway answered its own `502` [`PLAIN_NOT_SENT`], that it could not
//! reach the agent and sent it nothing, which it never lets an agent's
//! answer say. On the JSON-RPC binding, whose client reads no HTTP status,
//! that refusal is known by the client's message quoting it. Any other
//! failure on the way then (every refusal of the agent's answer by the
//! gateway, an unsafe task id, a `2xx` that is not JSON, an event too
//! large or a `Content-Encoding`; a `5xx`; an `InternalError`; a stream
//! broken after the question left) leaves the row pending, and the ask
//! fails with [`AskError::NotResent`], [`PLAIN_NOT_RESENT`]: only
//! [`Outbox::resume`], asked for, sends it again. Once the task is known,
//! reading and following it are retried as for a peer. These messages are
//! the pinned client's (`HTTP request failed: …`, `HTTP <status>: <body>`,
//! `failed to parse JSON-RPC response: …`), to check again on an upgrade.
//!
//! An ask ends when its task does (`COMPLETED`, `FAILED`, `CANCELED`,
//! `REJECTED`) or waits for the requester (`INPUT_REQUIRED`,
//! `AUTH_REQUIRED`), with the task as it is then; or when its `wait` runs
//! out, with the state last seen and the row still pending. The wait counts
//! from the call, the upload of the attachments included: when it runs out
//! before the question is kept, or the caller cancels the ask then
//! ([`Outbox::ask_kept`]), nothing is kept nor sent, so that asking again
//! never runs the question twice.
//!
//! What a peer sends is used only once checked. A task id that is not
//! [safe](is_safe_task_id) ends the attempt as refused, and is neither kept
//! nor ever put in a URL; a task the peer answers for must be the one asked
//! about. The gateway checks task ids first ([`crate::gateway`]): an answer
//! that names an unsafe one never gets through it, so the attempt breaks
//! as on a cut connection, and is retried, but to a plain agent that may
//! run every copy.
//!
//! Files go to PALS peers only: an ask to a plain contact with attachments
//! is refused before anything is read or sent. An attachment is read only
//! as the path policy's reading rule allows
//! (`crate::path_policy`): resolved first, symbolic links and `..`
//! included, it must be a regular file outside the agent's data directory
//! ([`Outbox::with_data_dir`]), which holds its key and its grants, and
//! outside `/proc`, `/sys`, `/dev` and any procfs or sysfs; the file opened
//! must be the one checked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use a2a::{
    A2AError, AgentCard, CancelTaskRequest, GetTaskRequest, Message, Part, PartContent, Role,
    SVC_PARAM_EXTENSIONS, SendMessageRequest, StreamResponse, SubscribeToTaskRequest,
    TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC, Task, TaskState, error_code,
};
use a2a_client::A2AClient;
use a2a_client::jsonrpc::JsonRpcTransport;
use a2a_client::middleware::CallInterceptor;
use a2a_client::rest::RestTransport;
use a2a_client::transport::{ServiceParams, Transport};
use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use http::header::AUTHORIZATION;
use pals_core::discovery::query::OfferRef;
use pals_core::grant::SignedGrant;
use pals_core::inventory::{INVENTORY_EXTENSION, Scope};
use pals_core::store::{A2aContact, OUTBOX_PENDING, OUTBOX_REFUSED, OutboxRow, PeerRef, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::a2a::{IDEMPOTENCY_EXTENSION, is_safe_task_id};
use crate::gateway::plain::{PLAIN_NOT_SENT, path_segment};
use crate::gateway::serves_tasks;
use crate::path_policy::open_attachment;

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
pub(crate) const INVALID_TASK_ID: &str = "the peer returned an invalid task id";
/// Why a question to a plain A2A contact whose card does not declare
/// streaming is refused, before anything is kept or sent.
pub const PLAIN_NEEDS_STREAMING: &str = "the plain A2A agent's card does not declare \
    streaming, which pals ask needs in milestone 2: a stock A2A client can still call it \
    through the gateway";
/// The most bytes of a plain contact's card read through the gateway.
const MAX_PLAIN_CARD_BYTES: usize = 64 * 1024;
/// The longest reason of the gateway's quoted.
const MAX_REASON_CHARS: usize = 200;
/// Why a question to a plain A2A contact that may have reached it is not
/// sent again on its own ([`AskError::NotResent`]).
pub const PLAIN_NOT_RESENT: &str = "the plain A2A agent may have received the question, and it \
    is not sent again on its own, since the agent may run every copy that reaches it";
/// How the pinned client's message for a request it could not send begins.
const REQUEST_FAILED: &str = "HTTP request failed: ";
/// How the pinned client's JSON-RPC message for a body that is no JSON-RPC
/// response, such as the gateway's own refusal, begins.
const JSONRPC_UNREAD: &str = "failed to parse JSON-RPC response: ";

type Events = BoxStream<'static, Result<StreamResponse, A2AError>>;
type Client = A2AClient<Box<dyn Transport>>;

/// A question for a peer.
#[derive(Debug, Clone)]
pub struct AskRequest {
    /// A contact name, a plain A2A contact's name or an agent id.
    pub peer: String,
    pub question: String,
    /// Files sent with the question.
    pub attachments: Vec<PathBuf>,
    /// How long to wait for the answer.
    pub wait: Duration,
    /// The offer the peer was chosen by, which the message names
    /// ([`OfferRef`]); none for a question asked without one, which goes as
    /// in milestone 1.
    pub offer: Option<OfferRef>,
}

/// A file a peer answered with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// The name the peer gave it: untrusted, never a path.
    pub name: String,
    /// Where this agent's gateway serves it: `<gateway>/blobs/<holder>/<hash>`.
    pub url: String,
}

/// How an ask ended, or where it stands when its wait ran out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The question may have reached a plain A2A agent that does not
    /// declare the idempotency extension, and it is not sent again on its
    /// own ([`PLAIN_NOT_RESENT`]): it is kept, pending, and
    /// [`Outbox::resume`] sends it again.
    #[error("{PLAIN_NOT_RESENT}; the question is kept as message {message_id}")]
    NotResent { message_id: String },
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
    /// neither a contact nor an agent id; is a plain A2A contact and there
    /// are attachments or an offer, or its card cannot be read through the
    /// gateway or does not declare streaming ([`PLAIN_NEEDS_STREAMING`]);
    /// or an attachment cannot be read or uploaded: one that is not a
    /// regular file, is inside the data directory or is a system file is
    /// refused.
    pub async fn ask(&self, req: AskRequest) -> Result<AskOutcome, AskError> {
        self.ask_kept(req, &CancellationToken::new(), |_| {}).await
    }

    /// [`Outbox::ask`], calling `kept` with the question's message id as
    /// soon as the outbox keeps it, before the first attempt to send it:
    /// from then on, [`Outbox::resume`] takes it up by that id.
    ///
    /// Until then the ask can be called off: when `cancel` fires, or
    /// `req.wait`, which counts from this call, runs out, while the
    /// attachments upload or before the question is kept, it fails with
    /// [`AskError::Local`] having kept and sent nothing.
    pub async fn ask_kept(
        &self,
        req: AskRequest,
        cancel: &CancellationToken,
        kept: impl FnOnce(&str) + Send,
    ) -> Result<AskOutcome, AskError> {
        let AskRequest {
            peer,
            question,
            attachments,
            wait,
            offer,
        } = req;
        let deadline = Instant::now() + wait;
        let prepared = async {
            let peer = self.resolve(peer).await?;
            if matches!(peer, PeerRef::Plain(_)) && !attachments.is_empty() {
                return Err(AskError::Local("attachments need a PALS peer".to_owned()));
            }
            if matches!(peer, PeerRef::Plain(_)) && offer.is_some() {
                return Err(AskError::Local(
                    "offers come only from PALS peers".to_owned(),
                ));
            }
            let plain = match &peer {
                PeerRef::Plain(contact) => {
                    let speaks = self.plain_speaks(contact).await.map_err(|why| {
                        AskError::Local(format!(
                            "cannot read the card of {} through the gateway ({why}): nothing \
                             was asked",
                            contact.name
                        ))
                    })?;
                    if !speaks.streaming {
                        return Err(AskError::Local(PLAIN_NEEDS_STREAMING.to_owned()));
                    }
                    Some(speaks)
                }
                PeerRef::Pals(_) => None,
            };
            let mut parts = vec![Part::text(question)];
            for path in &attachments {
                parts.push(self.upload(path).await?);
            }
            Ok::<_, AskError>((peer, plain, parts))
        };
        let (peer, plain, parts) = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(nothing_asked("the ask was canceled")),
            prepared = tokio::time::timeout_at(deadline, prepared) => match prepared {
                Ok(prepared) => prepared?,
                Err(_) => {
                    return Err(nothing_asked(
                        "the wait ran out while the attachments were uploading; ask again with \
                         a longer wait",
                    ));
                }
            },
        };
        let message_id = new_message_id();
        let mut message = Message::new(Role::User, parts);
        message.message_id = message_id.clone();
        if let Some(offer) = &offer {
            message.metadata = Some(HashMap::from([(
                INVENTORY_EXTENSION.to_owned(),
                offer.to_value(),
            )]));
        }
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
            peer: peer.key(),
            request_json,
            task_id: None,
            state: OUTBOX_PENDING.to_owned(),
            attempts: 0,
            created_at: now,
            updated_at: now,
        };
        let route = self.route(&peer, plain, &request).await?;
        if cancel.is_cancelled() {
            return Err(nothing_asked("the ask was canceled"));
        }
        self.with_store(move |store| store.outbox_insert(&row))
            .await?;
        kept(&message_id);
        self.drive(
            Ask {
                message_id,
                request,
                task_id: None,
                deadline,
            },
            route,
        )
        .await
    }

    /// Takes up the question `message_id` again, as [`Outbox::ask`] left
    /// it, and waits up to `wait` for its answer: sends it again while its
    /// task is not known, follows the task once it is. A question that has
    /// an outcome gets it again. A question to a plain contact whose card
    /// cannot be read through the gateway now stays pending
    /// ([`AskError::Unreachable`]); one whose card no longer declares
    /// streaming is [`AskError::Local`], and stays as it was.
    pub async fn resume(&self, message_id: &str, wait: Duration) -> Result<AskOutcome, AskError> {
        let row = self.row(message_id).await?;
        check_stored_task(&row)?;
        let request = decode_request(&row.request_json)?;
        let peer = self.peer_of(&row).await?;
        let plain = self.plain_speaks_of(&peer, &row.message_id).await?;
        if plain.is_some_and(|speaks| !speaks.streaming) {
            return Err(AskError::Local(PLAIN_NEEDS_STREAMING.to_owned()));
        }
        let route = self.route(&peer, plain, &request).await?;
        if row.state == OUTBOX_REFUSED {
            let id = row.message_id.clone();
            self.with_store(move |store| store.outbox_set_state(&id, OUTBOX_PENDING, now_unix()))
                .await?;
        }
        self.drive(
            Ask {
                message_id: row.message_id,
                request,
                task_id: row.task_id,
                deadline: Instant::now() + wait,
            },
            route,
        )
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
        let Some(task_id) = row.task_id.clone() else {
            return Err(AskError::Local(format!(
                "no task of the peer is known for message {message_id} yet: there is nothing \
                 to cancel"
            )));
        };
        let peer = self.peer_of(&row).await?;
        let asked = decode_request(&row.request_json)?;
        let plain = self.plain_speaks_of(&peer, message_id).await?;
        let client = self.route(&peer, plain, &asked).await?.client;
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

    /// Attempts `ask` on `route` until it has an outcome, is refused, runs
    /// out of retries or of time, or, to a plain agent that may run every
    /// copy, may have been sent.
    async fn drive(&self, mut ask: Ask, route: Route) -> Result<AskOutcome, AskError> {
        let deadline = ask.deadline;
        let mut failures: u32 = 0;
        // The state last seen, by this call.
        let mut seen: Option<TaskState> = None;
        loop {
            let attempts = self.count_attempt(&ask.message_id).await?;
            let exchange = self.exchange(&route, &mut ask, &mut seen);
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
                Ended::Broken {
                    progressed,
                    reason,
                    unsent,
                } => {
                    tracing::debug!(
                        message_id = %ask.message_id,
                        %reason,
                        "an attempt did not get through"
                    );
                    // Its task not known, the next attempt sends it again.
                    if ask.task_id.is_none() && !unsent && route.resends_only_unsent() {
                        return Err(AskError::NotResent {
                            message_id: ask.message_id,
                        });
                    }
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
    async fn exchange(&self, route: &Route, ask: &mut Ask, seen: &mut Option<TaskState>) -> Ended {
        let client = &route.client;
        let Some(task_id) = ask.task_id.clone() else {
            route.before_send();
            return match client.send_streaming_message(&ask.request).await {
                Ok(events) => self.follow(client, ask, events, seen).await,
                Err(err) => Ended::send_failed(&err, route.never_sent(&err)),
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
            unsent: false,
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

    /// The agent `peer` names: an agent id, a contact's name, or a plain A2A
    /// contact's name, in that order.
    async fn resolve(&self, peer: String) -> Result<PeerRef, AskError> {
        let name = peer.clone();
        let resolved = self
            .with_store(move |store| store.resolve_peer(&name))
            .await?;
        resolved.ok_or_else(|| {
            AskError::Local(format!(
                "unknown peer {peer}: neither a contact nor an agent id"
            ))
        })
    }

    /// The peer `row` was asked of, from the reference it keeps.
    async fn peer_of(&self, row: &OutboxRow) -> Result<PeerRef, AskError> {
        let key = row.peer.clone();
        let peer = self.with_store(move |store| store.peer_ref(&key)).await?;
        peer.ok_or_else(|| {
            AskError::Local(format!(
                "the peer of message {} is not known any more: {}",
                row.message_id, row.peer
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

    /// The route to `peer`, through the gateway, for the question
    /// `request`: an A2A client on the HTTP+JSON binding,
    /// `<gateway>/peers/<peer>/rest`, but for a plain contact whose card has
    /// no HTTP+JSON interface and has a JSON-RPC one (`plain`, from
    /// [`Outbox::plain_speaks`]), on the JSON-RPC binding,
    /// `<gateway>/peers/<name>/jsonrpc`, with the pinned client's JSON-RPC
    /// transport; and when a send that failed on the way goes again
    /// ([`Resend`]): for a plain contact whose card does not declare the
    /// idempotency extension, only when it certainly never left, which its
    /// own HTTP client, with no pooled connections, tells for a connection
    /// to the gateway ([`ConnectWatch`]). Every request it sends activates the
    /// [idempotency extension](IDEMPOTENCY_EXTENSION), which the resends of
    /// an ask rely on; a plain A2A agent may ignore it. For a question that
    /// names an offer, it activates the [inventory
    /// extension](INVENTORY_EXTENSION) too, and presents the grant of the
    /// offer's scope ([`Outbox::offer_grant`]) when this agent holds one, so
    /// that the task runs under the scope it was offered for; any other
    /// question goes with the grant the gateway chooses, as in milestone 1.
    async fn route(
        &self,
        peer: &PeerRef,
        plain: Option<PlainSpeaks>,
        request: &SendMessageRequest,
    ) -> Result<Route, AskError> {
        let inventory = names_an_offer(request);
        let bearer = if inventory {
            self.offer_grant(peer, request)
                .await?
                .map(|grant| format!("Bearer {}", grant.to_token()))
        } else {
            None
        };
        let base_url = format!("{}/peers/{}", self.gateway_url, path_segment(peer));
        let jsonrpc = plain.is_some_and(|speaks| speaks.jsonrpc);
        let (http, resend) = match plain {
            Some(speaks) if !speaks.idempotent => {
                let no_connection = Arc::new(AtomicBool::new(false));
                let http = reqwest::Client::builder()
                    .no_proxy()
                    .pool_max_idle_per_host(0)
                    .connector_layer(ConnectWatch(Arc::clone(&no_connection)))
                    .build()
                    .map_err(|err| AskError::Local(format!("cannot build a client: {err}")))?;
                let resend = Resend::OnlyUnsent {
                    jsonrpc,
                    no_connection,
                };
                (http, resend)
            }
            _ => (self.http.clone(), Resend::OnTheWay),
        };
        let transport: Box<dyn Transport> = if jsonrpc {
            Box::new(JsonRpcTransport::new(http, format!("{base_url}/jsonrpc")))
        } else {
            Box::new(RestTransport::new(http, format!("{base_url}/rest")))
        };
        let client = A2AClient::new(transport)
            .with_interceptors(vec![Arc::new(Extensions { inventory, bearer })]);
        Ok(Route { client, resend })
    }

    /// What the card of the plain contact `contact` says of how to ask it:
    /// the card as the gateway serves it
    /// (`<gateway>/peers/<name>/.well-known/agent-card.json`), which the
    /// gateway reads and checks now, with only the bindings the agent has;
    /// why not, in the gateway's words, when it cannot be had.
    async fn plain_speaks(&self, contact: &A2aContact) -> Result<PlainSpeaks, String> {
        let url = format!(
            "{}/peers/{}/.well-known/agent-card.json",
            self.gateway_url, contact.name
        );
        let mut response = self
            .http
            .get(url)
            .header("a2a-version", a2a::VERSION)
            .send()
            .await
            .map_err(|err| format!("the gateway cannot be reached: {err}"))?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| format!("no answer from the gateway: {err}"))?
        {
            if body.len() + chunk.len() > MAX_PLAIN_CARD_BYTES {
                return Err("the card is too large".to_owned());
            }
            body.extend_from_slice(&chunk);
        }
        if status != http::StatusCode::OK {
            let reason = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|refusal| refusal["error"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let reason = crate::printable_ascii(&reason, MAX_REASON_CHARS);
            return Err(format!("the gateway answered {status}: {reason}"));
        }
        let card: AgentCard = serde_json::from_slice(&body)
            .map_err(|_| "the gateway answered something that is not a card".to_owned())?;
        Ok(PlainSpeaks::of(&card))
    }

    /// [`Outbox::plain_speaks`] of `peer` when it is a plain contact, for
    /// the question kept as `message_id`: [`AskError::Unreachable`] when
    /// its card cannot be had, so that the question stays pending.
    async fn plain_speaks_of(
        &self,
        peer: &PeerRef,
        message_id: &str,
    ) -> Result<Option<PlainSpeaks>, AskError> {
        let PeerRef::Plain(contact) = peer else {
            return Ok(None);
        };
        match self.plain_speaks(contact).await {
            Ok(speaks) => Ok(Some(speaks)),
            Err(why) => {
                tracing::debug!(message_id, contact = %contact.name, %why, "no card of a plain contact");
                Err(AskError::Unreachable {
                    message_id: message_id.to_owned(),
                })
            }
        }
    }

    /// The grant this agent holds of `peer`'s, a PALS peer, that serves
    /// tasks in the scope of the offer `request` names, as the offer this
    /// agent keeps says it ([`Store::offer_received`]); none when it keeps
    /// no such offer any more (expired and purged: a retry of a request
    /// that ran attaches whatever grant it presents) or holds no such grant
    /// (the provider then refuses the offer as `offer_stale`).
    async fn offer_grant(
        &self,
        peer: &PeerRef,
        request: &SendMessageRequest,
    ) -> Result<Option<SignedGrant>, AskError> {
        let (PeerRef::Pals(provider), Some(Some(reference))) = (
            peer,
            OfferRef::in_metadata(request.message.metadata.as_ref()),
        ) else {
            return Ok(None);
        };
        let provider = *provider;
        self.with_store(move |store| {
            let Some(offer) = store.offer_received(&provider, &reference.offer_id)? else {
                return Ok(None);
            };
            let held = store.grants_held_for(&provider, now_unix())?;
            Ok(held.into_iter().find(|g| {
                serves_tasks(&g.grant) && Scope::for_grant(Some(&g.grant)).key() == offer.doc.scope
            }))
        })
        .await
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

/// What the card of a plain A2A contact says of how the outbox asks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlainSpeaks {
    /// On the JSON-RPC binding: the card has a JSON-RPC interface and no
    /// HTTP+JSON one.
    jsonrpc: bool,
    /// The card declares `capabilities.streaming`, which the outbox needs.
    streaming: bool,
    /// The card declares the [idempotency extension](IDEMPOTENCY_EXTENSION).
    idempotent: bool,
}

impl PlainSpeaks {
    /// What `card`, a plain contact's card as the gateway serves it, says.
    fn of(card: &AgentCard) -> Self {
        let has = |binding: &str| {
            card.supported_interfaces
                .iter()
                .any(|iface| iface.protocol_binding == binding)
        };
        Self {
            jsonrpc: !has(TRANSPORT_PROTOCOL_HTTP_JSON) && has(TRANSPORT_PROTOCOL_JSONRPC),
            streaming: card.capabilities.streaming == Some(true),
            idempotent: card
                .capabilities
                .extensions
                .iter()
                .flatten()
                .any(|extension| extension.uri == IDEMPOTENCY_EXTENSION),
        }
    }
}

/// How the outbox reaches a peer, through the gateway.
struct Route {
    client: Client,
    resend: Resend,
}

/// When a `SendStreamingMessage` that failed on the way is sent again.
enum Resend {
    /// After any failure on the way: a PALS peer, or a plain agent whose
    /// card declares the idempotency extension, runs the first copy that
    /// reaches it and attaches every other one to that run.
    OnTheWay,
    /// Only when it certainly never reached the agent, a plain one that may
    /// run every copy: the route's client could make no connection to the
    /// gateway (`no_connection`), or the gateway says it sent the agent
    /// nothing ([`PLAIN_NOT_SENT`]), as an answer on the JSON-RPC binding
    /// (`jsonrpc`) or on HTTP+JSON.
    OnlyUnsent {
        jsonrpc: bool,
        no_connection: Arc<AtomicBool>,
    },
}

impl Route {
    /// Whether a send that failed on the way goes again only when it never
    /// left.
    fn resends_only_unsent(&self) -> bool {
        matches!(self.resend, Resend::OnlyUnsent { .. })
    }

    /// Before each send: no connection failed yet.
    fn before_send(&self) {
        if let Resend::OnlyUnsent { no_connection, .. } = &self.resend {
            no_connection.store(false, Ordering::SeqCst);
        }
    }

    /// Whether `err`, the pinned client's error for the send just made,
    /// says that the question certainly never reached the agent: the
    /// client could not send the request and could make no connection to
    /// the gateway, or the gateway's answer is its own
    /// [`PLAIN_NOT_SENT`], which it never lets an agent's answer carry
    /// (`crate::gateway::plain`). Never for [`Resend::OnTheWay`], which
    /// does not need to know.
    fn never_sent(&self, err: &A2AError) -> bool {
        let Resend::OnlyUnsent {
            jsonrpc,
            no_connection,
        } = &self.resend
        else {
            return false;
        };
        if err.code != error_code::INTERNAL_ERROR {
            return false;
        }
        if err.message.starts_with(REQUEST_FAILED) && no_connection.load(Ordering::SeqCst) {
            return true;
        }
        says_not_sent(&err.message, *jsonrpc)
    }
}

/// Whether `message`, of the pinned client's `InternalError` for a send on
/// the JSON-RPC binding (`jsonrpc`) or on HTTP+JSON, is the gateway's
/// `502` [`PLAIN_NOT_SENT`]: on HTTP+JSON, `HTTP 502 Bad Gateway: <body>`
/// with that `error`; on JSON-RPC, whose client reads no HTTP status, the
/// body that is no JSON-RPC response, `{"error": <that text>}`, which its
/// message quotes.
fn says_not_sent(message: &str, jsonrpc: bool) -> bool {
    if jsonrpc {
        message.starts_with(JSONRPC_UNREAD) && message.contains(&format!("{PLAIN_NOT_SENT:?}"))
    } else {
        http_failure(message)
            .is_some_and(|(status, reason)| status == 502 && reason == PLAIN_NOT_SENT)
    }
}

/// A connector layer for an HTTP client that sets its flag when a
/// connection cannot be made: then the request it was for never left.
#[derive(Clone)]
struct ConnectWatch(Arc<AtomicBool>);

impl<S> tower::Layer<S> for ConnectWatch {
    type Service = Watched<S>;

    fn layer(&self, inner: S) -> Watched<S> {
        Watched {
            inner,
            failed: Arc::clone(&self.0),
        }
    }
}

/// A connector that [`ConnectWatch`] watches.
#[derive(Clone)]
struct Watched<S> {
    inner: S,
    failed: Arc<AtomicBool>,
}

impl<S, R> tower::Service<R> for Watched<S>
where
    S: tower::Service<R>,
    S::Future: Send + 'static,
    S::Response: 'static,
    S::Error: 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        let ready = self.inner.poll_ready(cx);
        if matches!(ready, Poll::Ready(Err(_))) {
            self.failed.store(true, Ordering::SeqCst);
        }
        ready
    }

    fn call(&mut self, request: R) -> Self::Future {
        let failed = Arc::clone(&self.failed);
        let connecting = self.inner.call(request);
        Box::pin(async move {
            let connected = connecting.await;
            if connected.is_err() {
                failed.store(true, Ordering::SeqCst);
            }
            connected
        })
    }
}

/// A question being attempted.
struct Ask {
    message_id: String,
    /// What every attempt that sends it sends.
    request: SendMessageRequest,
    task_id: Option<String>,
    /// When its wait runs out.
    deadline: Instant,
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
    /// task move first; `unsent` when what it sent certainly never reached
    /// the peer ([`Route::never_sent`]).
    Broken {
        progressed: bool,
        reason: String,
        unsent: bool,
    },
    /// The outbox could not be written.
    Local(AskError),
}

impl Ended {
    fn failed(err: &A2AError, progressed: bool) -> Self {
        match Failure::of(err) {
            Failure::Transport(reason) => Self::Broken {
                progressed,
                reason,
                unsent: false,
            },
            Failure::Refused(reason) => Self::Refused(reason),
        }
    }

    /// How sending the question ended, with `err`; `unsent` when it
    /// certainly never reached the peer.
    fn send_failed(err: &A2AError, unsent: bool) -> Self {
        match Failure::of(err) {
            Failure::Transport(reason) => Self::Broken {
                progressed: false,
                reason,
                unsent,
            },
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
                unsent: false,
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

/// The error of an ask stopped before its question was kept, for `why`.
fn nothing_asked(why: &str) -> AskError {
    AskError::Local(format!("{why}: nothing was asked"))
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
/// `A2A-Extensions` service parameter, and the [`INVENTORY_EXTENSION`]
/// after it with `inventory`; presents `bearer`, when given, as the
/// request's own `Authorization`, which the gateway passes intact.
struct Extensions {
    inventory: bool,
    bearer: Option<String>,
}

#[async_trait]
impl CallInterceptor for Extensions {
    async fn before(&self, _method: &str, params: &mut ServiceParams) -> Result<(), A2AError> {
        let mut extensions = IDEMPOTENCY_EXTENSION.to_owned();
        if self.inventory {
            extensions.push_str(", ");
            extensions.push_str(INVENTORY_EXTENSION);
        }
        params.insert(SVC_PARAM_EXTENSIONS.to_owned(), vec![extensions]);
        if let Some(bearer) = &self.bearer {
            params.insert(AUTHORIZATION.to_string(), vec![bearer.clone()]);
        }
        Ok(())
    }
}

/// Whether `request`, as the outbox keeps it, names an offer: then every
/// attempt activates the inventory extension, as the first did.
fn names_an_offer(request: &SendMessageRequest) -> bool {
    OfferRef::in_metadata(request.message.metadata.as_ref()).is_some()
}

/// Why an offer received cannot be named in an ask.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfferLookupError {
    /// The peer is no PALS peer this agent knows: only those make offers.
    #[error("{0}")]
    NotAPeer(String),
    /// This agent keeps no offer of the peer's with that id.
    #[error("no such offer of the peer's: run pals discover again")]
    Unknown,
    /// It expired, by this agent's own clock.
    #[error("offer expired: run pals discover again")]
    Expired,
    /// The store could not be read.
    #[error("cannot read the offers: {0}")]
    Store(String),
}

/// The reference to the offer `offer_id` that `peer` (a contact's name or
/// an agent id) sent this agent and `store` keeps, looked up by (the
/// peer's agent id, `offer_id`): two providers may use the same id. Refused
/// when it is expired at `now_unix` by this agent's own clock, without
/// skew: nothing is sent with an offer that no longer holds, and asking
/// for a new one is a new search.
pub fn received_offer_ref(
    store: &Store,
    peer: &str,
    offer_id: &str,
    now_unix: i64,
) -> Result<OfferRef, OfferLookupError> {
    let failed = |err: anyhow::Error| OfferLookupError::Store(format!("{err:#}"));
    let provider = match store.resolve_peer(peer).map_err(failed)? {
        Some(PeerRef::Pals(id)) => id,
        Some(PeerRef::Plain(_)) => {
            return Err(OfferLookupError::NotAPeer(format!(
                "{peer} is a plain A2A agent, which makes no offers"
            )));
        }
        None => {
            return Err(OfferLookupError::NotAPeer(format!(
                "unknown peer {peer}: neither a contact nor an agent id"
            )));
        }
    };
    let offer = store
        .offer_received(&provider, offer_id)
        .map_err(failed)?
        .ok_or(OfferLookupError::Unknown)?;
    if offer.doc.expired(now_unix) {
        return Err(OfferLookupError::Expired);
    }
    Ok(OfferRef::of(&offer.doc))
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
    fn only_the_gateways_own_refusal_says_nothing_was_sent() {
        let rest = format!(r#"HTTP 502 Bad Gateway: {{"error":"{PLAIN_NOT_SENT}"}}"#);
        assert!(says_not_sent(&rest, false));
        assert!(!says_not_sent(&rest, true));
        // As the JSON-RPC client quotes a body that is no JSON-RPC response.
        let rpc = format!(
            "failed to parse JSON-RPC response: invalid type: string {PLAIN_NOT_SENT:?}, \
             expected struct JsonRpcError at line 1 column 58"
        );
        assert!(says_not_sent(&rpc, true));
        assert!(!says_not_sent(&rpc, false));
        for other in [
            r#"HTTP 502 Bad Gateway: {"error":"peer unreachable"}"#.to_owned(),
            r#"HTTP 502 Bad Gateway: {"error":"peer protocol error"}"#.to_owned(),
            format!(r#"HTTP 503 Service Unavailable: {{"error":"{PLAIN_NOT_SENT}"}}"#),
            format!("SSE stream error: {PLAIN_NOT_SENT}"),
            "HTTP request failed: error sending request".to_owned(),
        ] {
            assert!(!says_not_sent(&other, false), "{other}");
            assert!(!says_not_sent(&other, true), "{other}");
        }
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
