//! [`GuardedHandler`]: every task belongs to the peer that created it, and
//! every message a peer sends runs once.

use std::sync::Arc;
use std::time::Duration;

use a2a::{
    A2AError, AgentCard, CancelTaskRequest, DeleteTaskPushNotificationConfigRequest,
    GetExtendedAgentCardRequest, GetTaskPushNotificationConfigRequest, GetTaskRequest,
    ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse,
    ListTasksRequest, ListTasksResponse, Message, SendMessageRequest, SendMessageResponse,
    StreamResponse, SubscribeToTaskRequest, Task, TaskPushNotificationConfig,
};
use a2a_server::{RequestHandler, ServiceParams};
use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::jcs;
use pals_core::store::{DEDUP_INFLIGHT_WAIT_S, DedupBegin, DedupRow, Store};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, warn};

use super::task_store::{a2a_error, blocking};
use crate::net::PEER_HEADER;

/// The message of the `InvalidParamsError` a request gets when its
/// `messageId` came before with other content.
const REUSED_MESSAGE_ID: &str = "messageId reused with different content";
/// The message of the `InternalError` a repeated request gets when the first
/// request with its `messageId` still has no outcome after
/// [`DEDUP_INFLIGHT_WAIT_S`].
const STILL_IN_FLIGHT: &str = "the first request with this messageId is still in progress";
/// How often a repeated request looks again for the outcome of the first.
const INFLIGHT_POLL: Duration = Duration::from_millis(50);
/// Events of a first streaming request held for a requester that reads
/// slower than they come: as many as the SDK holds for each subscriber.
const EVENT_BUFFER: usize = 32;

type Events = BoxStream<'static, Result<StreamResponse, A2AError>>;

/// A [`RequestHandler`] that confines each peer to its own tasks and runs
/// each message once.
///
/// **Ownership.** The owner of a task is the peer, [`PEER_HEADER`], whose
/// request created it: the guard records it with the first task the inner
/// handler returns or streams, before the peer sees the task's id. Getting,
/// canceling or subscribing to a task of another peer, or continuing one
/// with `SendMessage`, fails with `TaskNotFoundError`, exactly as for a task
/// that does not exist. `ListTasks` never reaches the inner handler: it
/// lists the requesting peer's tasks only.
///
/// **Idempotency.** `SendMessage` and `SendStreamingMessage` are keyed by
/// `(peer, messageId)`, with the BLAKE3 of the JCS form of the request's
/// params (the same for both methods and both bindings) as its content:
///
/// - The first request with a key runs, and the key is bound to its
///   outcome: the first task it returns or streams, or the message it
///   answers with when there is no task. It runs detached from the
///   requester, so the key is bound even if the requester goes away.
/// - A repeated request with the same content gets that outcome without
///   running again. A unary one gets the task as it is now; a streaming one
///   follows the task's execution with `SubscribeToTask`, or gets one event
///   with the task when it is terminal or has no execution to follow. A
///   message is returned, or streamed as the one event, as it was.
/// - A repeated request whose first one has no outcome yet waits up to
///   [`DEDUP_INFLIGHT_WAIT_S`] for it, and then fails with `InternalError`.
/// - A request whose `messageId` came before with other content fails with
///   `InvalidParamsError`, `messageId reused with different content`.
/// - A first request that fails before it has an outcome frees its key, so
///   that a retry runs.
///
/// Push notification configs and the extended card go to the inner handler
/// unchecked, and it refuses them because the agent card declares neither.
/// Serving push configs would need an ownership check here first.
pub struct GuardedHandler<H> {
    inner: Arc<H>,
    store: Arc<Store>,
}

impl<H: RequestHandler> GuardedHandler<H> {
    /// Guards `inner` with the ownership and dedup records of `store`.
    ///
    /// `key` is the agent's key. The guard signs nothing in milestone 1: the
    /// task events are signed where every task transition is written, by
    /// [`SqliteTaskStore::with_events`](super::SqliteTaskStore::with_events).
    pub fn new(inner: H, store: Arc<Store>, key: Arc<AgentKey>) -> Self {
        let _ = key;
        Self {
            inner: Arc::new(inner),
            store,
        }
    }

    /// Fails with `TaskNotFoundError` unless `peer` owns `task_id`.
    async fn ensure_owner(&self, peer: AgentId, task_id: &str) -> Result<(), A2AError> {
        let id = task_id.to_owned();
        let owner = blocking(&self.store, move |store| store.task_owner(&id)).await?;
        if owner == Some(peer) {
            Ok(())
        } else {
            Err(A2AError::task_not_found(task_id))
        }
    }

    /// Whether a request with `key` runs or repeats an earlier one, waiting
    /// up to [`DEDUP_INFLIGHT_WAIT_S`] for the earlier one's outcome.
    async fn attempt(&self, key: &Key) -> Result<Attempt, A2AError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(DEDUP_INFLIGHT_WAIT_S);
        loop {
            let begin = {
                let key = key.clone();
                blocking(&self.store, move |store| {
                    let now = Utc::now().timestamp();
                    store.dedup_begin(&key.peer, &key.message_id, &key.hash, now)
                })
                .await?
            };
            let row = match begin {
                DedupBegin::New => return Ok(Attempt::First),
                DedupBegin::Existing(row) => row,
            };
            if row.request_hash != key.hash {
                return Err(A2AError::invalid_params(REUSED_MESSAGE_ID));
            }
            if let Some(outcome) = Outcome::of(row)? {
                return Ok(Attempt::Repeat(outcome));
            }
            // In flight. Should it fail without an outcome, its row is gone
            // and the next look begins anew.
            if tokio::time::Instant::now() >= deadline {
                return Err(A2AError::internal(STILL_IN_FLIGHT));
            }
            tokio::time::sleep(INFLIGHT_POLL).await;
        }
    }

    /// Runs the first `SendMessage` with `key` and binds the key to its
    /// response.
    async fn first_send(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
        key: Key,
    ) -> Result<SendMessageResponse, A2AError> {
        let inner = Arc::clone(&self.inner);
        let store = Arc::clone(&self.store);
        let params = params.clone();
        // Detached: should the requester go away, the run still binds the
        // key, and a retry finds its task.
        let run = tokio::spawn(async move {
            let response = match inner.send_message(&params, req).await {
                Ok(response) => response,
                Err(err) => {
                    release(&store, &key).await;
                    return Err(err);
                }
            };
            let outcome = match &response {
                SendMessageResponse::Task(task) => {
                    own(&store, &task.id, key.peer).await?;
                    Outcome::Task(task.id.clone())
                }
                SendMessageResponse::Message(message) => {
                    if let Some(task_id) = &message.task_id {
                        own(&store, task_id, key.peer).await?;
                    }
                    Outcome::Message(message.clone())
                }
            };
            bind(&store, &key, &outcome).await?;
            Ok(response)
        });
        run.await.map_err(|err| a2a_error(err.into()))?
    }

    /// Runs the first `SendStreamingMessage` with `key`: see [`forward`].
    async fn first_stream(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
        key: Key,
    ) -> Result<Events, A2AError> {
        let inner = Arc::clone(&self.inner);
        let store = Arc::clone(&self.store);
        let params = params.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (events_tx, events_rx) = mpsc::channel(EVENT_BUFFER);
        tokio::spawn(async move {
            match inner.send_streaming_message(&params, req).await {
                Ok(events) => {
                    let _ = started_tx.send(Ok(()));
                    forward(events, Settler::new(store, key), events_tx).await;
                }
                Err(err) => {
                    release(&store, &key).await;
                    let _ = started_tx.send(Err(err));
                }
            }
        });
        started_rx.await.map_err(|_| {
            error!("a streaming request ended before it started");
            A2AError::internal("Internal error")
        })??;
        Ok(Box::pin(stream::unfold(
            events_rx,
            |mut events| async move { events.recv().await.map(|event| (event, events)) },
        )))
    }

    /// What a repeated `SendMessage` gets: the message, or the task as it is
    /// now.
    async fn repeat_send(
        &self,
        params: &ServiceParams,
        req: &SendMessageRequest,
        peer: AgentId,
        outcome: Outcome,
    ) -> Result<SendMessageResponse, A2AError> {
        match outcome {
            Outcome::Message(message) => Ok(SendMessageResponse::Message(message)),
            Outcome::Task(task_id) => {
                let history_length = req
                    .configuration
                    .as_ref()
                    .and_then(|configuration| configuration.history_length);
                let task = self
                    .current_task(params, peer, &task_id, history_length, &req.tenant)
                    .await?;
                Ok(SendMessageResponse::Task(task))
            }
        }
    }

    /// What a repeated `SendStreamingMessage` gets: the task's execution
    /// from now on, or one event with the message or with the task when it
    /// is terminal or has no execution to follow.
    async fn repeat_stream(
        &self,
        params: &ServiceParams,
        req: &SendMessageRequest,
        peer: AgentId,
        outcome: Outcome,
    ) -> Result<Events, A2AError> {
        let task_id = match outcome {
            Outcome::Message(message) => return Ok(single(StreamResponse::Message(message))),
            Outcome::Task(task_id) => task_id,
        };
        let mut task = self
            .current_task(params, peer, &task_id, None, &req.tenant)
            .await?;
        if !task.status.state.is_terminal() {
            let resubscribe = SubscribeToTaskRequest {
                id: task_id.clone(),
                tenant: req.tenant.clone(),
            };
            if let Ok(events) = self.inner.subscribe_to_task(params, resubscribe).await {
                return Ok(events);
            }
            // No execution to follow: it ended since, or the task waits for
            // input. The task as it is now.
            task = self
                .current_task(params, peer, &task_id, None, &req.tenant)
                .await?;
        }
        Ok(single(StreamResponse::Task(task)))
    }

    /// `peer`'s task `task_id`, from the inner handler.
    async fn current_task(
        &self,
        params: &ServiceParams,
        peer: AgentId,
        task_id: &str,
        history_length: Option<i32>,
        tenant: &Option<String>,
    ) -> Result<Task, A2AError> {
        self.ensure_owner(peer, task_id).await?;
        let req = GetTaskRequest {
            id: task_id.to_owned(),
            history_length,
            tenant: tenant.clone(),
        };
        self.inner.get_task(params, req).await
    }
}

/// The authenticated peer of a request, set by the tunnel and required by the
/// authentication layer before any request reaches the handler.
fn peer(params: &ServiceParams) -> Result<AgentId, A2AError> {
    match params.get(PEER_HEADER).map(Vec::as_slice) {
        Some([peer]) => peer.parse().map_err(|_| no_peer()),
        _ => Err(no_peer()),
    }
}

fn no_peer() -> A2AError {
    error!("a request reached the task guard without an authenticated peer");
    A2AError::internal("Internal error")
}

/// Records `peer` as the owner of `task_id`; ownership never changes hands.
async fn own(store: &Arc<Store>, task_id: &str, peer: AgentId) -> Result<(), A2AError> {
    let task_id = task_id.to_owned();
    blocking(store, move |store| store.task_owner_put(&task_id, &peer)).await
}

/// The task an event reveals the id of.
fn event_task_id(event: &StreamResponse) -> Option<&str> {
    match event {
        StreamResponse::Task(task) => Some(&task.id),
        StreamResponse::StatusUpdate(update) => Some(&update.task_id),
        StreamResponse::ArtifactUpdate(update) => Some(&update.task_id),
        StreamResponse::Message(message) => message.task_id.as_deref(),
    }
}

/// A stream of one event.
fn single(event: StreamResponse) -> Events {
    Box::pin(stream::iter([Ok(event)]))
}

/// The dedup key of a message request, and the hash of its content.
#[derive(Clone)]
struct Key {
    peer: AgentId,
    message_id: String,
    /// BLAKE3, in hex, of the JCS form of the request's params: the same
    /// for `SendMessage` and `SendStreamingMessage`, over either binding.
    hash: String,
}

impl Key {
    fn of(peer: AgentId, req: &SendMessageRequest) -> Result<Self, A2AError> {
        let params = serde_json::to_value(req)
            .context("a message request does not serialize")
            .map_err(a2a_error)?;
        Ok(Self {
            peer,
            message_id: req.message.message_id.clone(),
            hash: blake3::hash(&jcs::canonical(&params)).to_hex().to_string(),
        })
    }
}

/// The outcome of the first request with a key, which repeated requests
/// get.
enum Outcome {
    /// The task it created or continued.
    Task(String),
    /// The message the agent answered with instead of a task.
    Message(Message),
}

impl Outcome {
    /// The outcome `row` is bound to; none while its request is in flight.
    fn of(row: DedupRow) -> Result<Option<Self>, A2AError> {
        match (row.response_json, row.task_id) {
            (Some(json), _) => serde_json::from_str(&json)
                .context("a stored message response does not parse")
                .map(|message| Some(Self::Message(message)))
                .map_err(a2a_error),
            (None, Some(task_id)) => Ok(Some(Self::Task(task_id))),
            (None, None) => Ok(None),
        }
    }

    /// The outcome the first event of a stream reveals.
    fn of_event(event: &StreamResponse) -> Self {
        match event {
            StreamResponse::Task(task) => Self::Task(task.id.clone()),
            StreamResponse::StatusUpdate(update) => Self::Task(update.task_id.clone()),
            StreamResponse::ArtifactUpdate(update) => Self::Task(update.task_id.clone()),
            StreamResponse::Message(message) => Self::Message(message.clone()),
        }
    }
}

/// What a request with a key does.
enum Attempt {
    /// It is the first with its key: it runs.
    First,
    /// It repeats the first, which has this outcome.
    Repeat(Outcome),
}

/// Binds `key` to `outcome`.
async fn bind(store: &Arc<Store>, key: &Key, outcome: &Outcome) -> Result<(), A2AError> {
    let (task_id, response_json) = match outcome {
        Outcome::Task(task_id) => (Some(task_id.clone()), None),
        Outcome::Message(message) => {
            let json = serde_json::to_string(message)
                .context("a message response does not serialize")
                .map_err(a2a_error)?;
            (None, Some(json))
        }
    };
    let key = key.clone();
    blocking(store, move |store| {
        store.dedup_bind(
            &key.peer,
            &key.message_id,
            task_id.as_deref(),
            response_json.as_deref(),
        )
    })
    .await
}

/// Frees `key` after its first request failed with no outcome, so that a
/// retry runs. Should that fail too, retries wait out
/// [`DEDUP_INFLIGHT_WAIT_S`] until the row is purged.
async fn release(store: &Arc<Store>, key: &Key) {
    let released = {
        let key = key.clone();
        blocking(store, move |store| {
            store.dedup_release(&key.peer, &key.message_id)
        })
        .await
    };
    if released.is_err() {
        warn!(message_id = %key.message_id, "cannot free the messageId of a failed request");
    }
}

/// The bookkeeping of a first streaming request: the owner of each task its
/// events name, and its key's outcome.
struct Settler {
    store: Arc<Store>,
    key: Key,
    /// The last task recorded as owned.
    owned: Option<String>,
    /// Whether the key is bound, or freed after a failure.
    settled: bool,
}

impl Settler {
    fn new(store: Arc<Store>, key: Key) -> Self {
        Self {
            store,
            key,
            owned: None,
            settled: false,
        }
    }

    /// Records what `event` reveals before it goes out: the owner of the
    /// task it names, and, for the first event, the key's outcome.
    async fn event(&mut self, event: &StreamResponse) -> Result<(), A2AError> {
        if let Some(task_id) = event_task_id(event)
            && self.owned.as_deref() != Some(task_id)
        {
            own(&self.store, task_id, self.key.peer).await?;
            self.owned = Some(task_id.to_owned());
        }
        if !self.settled {
            bind(&self.store, &self.key, &Outcome::of_event(event)).await?;
            self.settled = true;
        }
        Ok(())
    }

    /// Frees the key if the request fails, or ends, with no outcome.
    async fn no_outcome(&mut self) {
        if !self.settled {
            release(&self.store, &self.key).await;
            self.settled = true;
        }
    }
}

/// Passes `events` to `requester`, each after `settler` records what it
/// reveals; if that fails, the stream ends with the error.
///
/// Should the requester go away, it keeps reading until the key is settled,
/// so that a retry finds the task.
async fn forward(
    mut events: Events,
    mut settler: Settler,
    requester: mpsc::Sender<Result<StreamResponse, A2AError>>,
) {
    let mut requester = Some(requester);
    while let Some(item) = events.next().await {
        let item = match item {
            Ok(event) => match settler.event(&event).await {
                Ok(()) => Ok(event),
                Err(err) => {
                    if let Some(requester) = &requester {
                        let _ = requester.send(Err(err)).await;
                    }
                    return;
                }
            },
            Err(err) => {
                settler.no_outcome().await;
                Err(err)
            }
        };
        if let Some(sender) = &requester
            && sender.send(item).await.is_err()
        {
            requester = None;
        }
        if requester.is_none() && settler.settled {
            return;
        }
    }
    settler.no_outcome().await;
}

#[async_trait]
impl<H: RequestHandler> RequestHandler for GuardedHandler<H> {
    async fn send_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2AError> {
        let peer = peer(params)?;
        if let Some(task_id) = &req.message.task_id {
            self.ensure_owner(peer, task_id).await?;
        }
        let key = Key::of(peer, &req)?;
        match self.attempt(&key).await? {
            Attempt::First => self.first_send(params, req, key).await,
            Attempt::Repeat(outcome) => self.repeat_send(params, &req, peer, outcome).await,
        }
    }

    async fn send_streaming_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<Events, A2AError> {
        let peer = peer(params)?;
        if let Some(task_id) = &req.message.task_id {
            self.ensure_owner(peer, task_id).await?;
        }
        let key = Key::of(peer, &req)?;
        match self.attempt(&key).await? {
            Attempt::First => self.first_stream(params, req, key).await,
            Attempt::Repeat(outcome) => self.repeat_stream(params, &req, peer, outcome).await,
        }
    }

    async fn get_task(
        &self,
        params: &ServiceParams,
        req: GetTaskRequest,
    ) -> Result<Task, A2AError> {
        self.ensure_owner(peer(params)?, &req.id).await?;
        self.inner.get_task(params, req).await
    }

    async fn list_tasks(
        &self,
        params: &ServiceParams,
        req: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2AError> {
        let peer = peer(params)?;
        blocking(&self.store, move |store| {
            store.tasks_list_for_owner(&peer, &req)
        })
        .await
    }

    async fn cancel_task(
        &self,
        params: &ServiceParams,
        req: CancelTaskRequest,
    ) -> Result<Task, A2AError> {
        self.ensure_owner(peer(params)?, &req.id).await?;
        self.inner.cancel_task(params, req).await
    }

    async fn subscribe_to_task(
        &self,
        params: &ServiceParams,
        req: SubscribeToTaskRequest,
    ) -> Result<Events, A2AError> {
        self.ensure_owner(peer(params)?, &req.id).await?;
        self.inner.subscribe_to_task(params, req).await
    }

    async fn create_push_config(
        &self,
        params: &ServiceParams,
        req: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.inner.create_push_config(params, req).await
    }

    async fn get_push_config(
        &self,
        params: &ServiceParams,
        req: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.inner.get_push_config(params, req).await
    }

    async fn list_push_configs(
        &self,
        params: &ServiceParams,
        req: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        self.inner.list_push_configs(params, req).await
    }

    async fn delete_push_config(
        &self,
        params: &ServiceParams,
        req: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2AError> {
        self.inner.delete_push_config(params, req).await
    }

    async fn get_extended_agent_card(
        &self,
        params: &ServiceParams,
        req: GetExtendedAgentCardRequest,
    ) -> Result<AgentCard, A2AError> {
        self.inner.get_extended_agent_card(params, req).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use a2a::{Part, Role, TaskState, TaskStatus, TaskStatusUpdateEvent, error_code};
    use a2a_server::{AgentExecutor, DefaultRequestHandler, ExecutorContext};

    use super::*;
    use crate::a2a::SqliteTaskStore;

    /// What [`Scripted`] does when it runs.
    #[derive(Clone, Copy)]
    enum Script {
        /// Answers with a bare message, no task.
        Answer,
        /// Fails its first run with `UnsupportedOperationError`, then
        /// answers.
        FailFirst,
        /// Streams the task in `WORKING` and never ends.
        Hang,
        /// Streams the task in `WORKING`, and `COMPLETED` 300 ms later.
        Slow,
    }

    /// An executor that follows its script and counts its runs.
    #[derive(Clone)]
    struct Scripted {
        script: Script,
        runs: Arc<AtomicUsize>,
    }

    impl Scripted {
        fn new(script: Script) -> Self {
            Self {
                script,
                runs: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn runs(&self) -> usize {
            self.runs.load(Ordering::SeqCst)
        }
    }

    impl AgentExecutor for Scripted {
        fn execute(&self, ctx: ExecutorContext) -> Events {
            let run = self.runs.fetch_add(1, Ordering::SeqCst);
            let mut answer = Message::new(Role::Agent, vec![Part::text(format!("pong {run}"))]);
            answer.context_id = Some(ctx.context_id.clone());
            match self.script {
                Script::FailFirst if run == 0 => Box::pin(stream::iter([Err(
                    A2AError::unsupported_operation("not this time"),
                )])),
                Script::Answer | Script::FailFirst => single(StreamResponse::Message(answer)),
                Script::Hang | Script::Slow => {
                    let status = |state| TaskStatus {
                        state,
                        message: None,
                        timestamp: None,
                    };
                    let working = single(StreamResponse::Task(Task {
                        id: ctx.task_id.clone(),
                        context_id: ctx.context_id.clone(),
                        status: status(TaskState::Working),
                        artifacts: None,
                        history: None,
                        metadata: None,
                    }));
                    if matches!(self.script, Script::Hang) {
                        return Box::pin(working.chain(stream::pending()));
                    }
                    let completed = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                        task_id: ctx.task_id,
                        context_id: ctx.context_id,
                        status: status(TaskState::Completed),
                        metadata: None,
                    });
                    Box::pin(working.chain(stream::once(async move {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        Ok(completed)
                    })))
                }
            }
        }

        fn cancel(&self, _ctx: ExecutorContext) -> Events {
            Box::pin(stream::empty())
        }
    }

    /// A guard around the SDK's handler with `executor`, and the params of
    /// a request from one peer.
    fn guarded(executor: Scripted) -> (GuardedHandler<DefaultRequestHandler>, ServiceParams) {
        let store = Arc::new(Store::open_in_memory(AgentKey::generate().id()).unwrap());
        let inner = DefaultRequestHandler::new(executor, SqliteTaskStore::new(Arc::clone(&store)));
        let handler = GuardedHandler::new(inner, store, Arc::new(AgentKey::generate()));
        let peer = AgentKey::generate().id().to_string();
        let params = ServiceParams::from([(PEER_HEADER.to_owned(), vec![peer])]);
        (handler, params)
    }

    fn request(text: &str) -> SendMessageRequest {
        SendMessageRequest {
            message: Message::new(Role::User, vec![Part::text(text)]),
            configuration: None,
            metadata: None,
            tenant: None,
        }
    }

    async fn collect(events: Events) -> Vec<StreamResponse> {
        events.map(|event| event.unwrap()).collect().await
    }

    #[tokio::test]
    async fn a_bare_message_answer_is_replayed() {
        let executor = Scripted::new(Script::Answer);
        let (handler, params) = guarded(executor.clone());

        let req = request("ping");
        let first = handler.send_message(&params, req.clone()).await.unwrap();
        let SendMessageResponse::Message(answer) = &first else {
            panic!("expected a message: {first:?}");
        };
        let retry = handler.send_message(&params, req.clone()).await.unwrap();
        assert_eq!(retry, first);
        let events = handler.send_streaming_message(&params, req).await.unwrap();
        assert_eq!(
            collect(events).await,
            [StreamResponse::Message(answer.clone())]
        );
        assert_eq!(executor.runs(), 1);

        // Streamed first, then retried as a unary request.
        let req = request("ping again");
        let events = handler
            .send_streaming_message(&params, req.clone())
            .await
            .unwrap();
        let events = collect(events).await;
        let [StreamResponse::Message(answer)] = events.as_slice() else {
            panic!("expected one message: {events:?}");
        };
        let retry = handler.send_message(&params, req).await.unwrap();
        assert_eq!(retry, SendMessageResponse::Message(answer.clone()));
        assert_eq!(executor.runs(), 2);
    }

    #[tokio::test]
    async fn a_failed_first_attempt_frees_its_message_id() {
        let executor = Scripted::new(Script::FailFirst);
        let (handler, params) = guarded(executor.clone());

        let req = request("ping");
        let err = handler
            .send_message(&params, req.clone())
            .await
            .unwrap_err();
        assert_eq!(err.code, error_code::UNSUPPORTED_OPERATION, "{err}");
        // The retry runs at once, instead of waiting for an outcome that
        // will never come.
        let started = Instant::now();
        let retry = handler.send_message(&params, req).await.unwrap();
        assert!(
            matches!(retry, SendMessageResponse::Message(_)),
            "{retry:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(DEDUP_INFLIGHT_WAIT_S));
        assert_eq!(executor.runs(), 2);

        // A streaming request that is refused before it starts frees its
        // messageId too.
        let mut req = request("ping");
        req.message.task_id = Some("no-such-task".to_owned());
        let (handler, params) = guarded(Scripted::new(Script::Answer));
        let peer: AgentId = params[PEER_HEADER][0].parse().unwrap();
        handler.store.task_owner_put("no-such-task", &peer).unwrap();
        for _ in 0..2 {
            let err = match handler.send_streaming_message(&params, req.clone()).await {
                Ok(_) => panic!("a continuation of a missing task started"),
                Err(err) => err,
            };
            assert_eq!(err.code, error_code::TASK_NOT_FOUND, "{err}");
        }
    }

    #[tokio::test]
    async fn an_abandoned_first_attempt_still_binds_its_message_id() {
        let executor = Scripted::new(Script::Slow);
        let (handler, params) = guarded(executor.clone());

        // The requester goes away before the unary answer.
        let req = request("ping");
        let abandoned = tokio::time::timeout(
            Duration::from_millis(50),
            handler.send_message(&params, req.clone()),
        )
        .await;
        assert!(
            abandoned.is_err(),
            "the request answered before it was abandoned"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let started = Instant::now();
        let SendMessageResponse::Task(task) = handler.send_message(&params, req).await.unwrap()
        else {
            panic!("expected the task");
        };
        assert_eq!(task.status.state, TaskState::Completed);
        assert!(started.elapsed() < Duration::from_secs(1));

        // The requester drops the stream before reading any event.
        let req = request("ping again");
        drop(
            handler
                .send_streaming_message(&params, req.clone())
                .await
                .unwrap(),
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let started = Instant::now();
        let events = handler.send_streaming_message(&params, req).await.unwrap();
        let events = collect(events).await;
        let [StreamResponse::Task(task)] = events.as_slice() else {
            panic!("expected one task: {events:?}");
        };
        assert_eq!(task.status.state, TaskState::Completed);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(executor.runs(), 2);
    }

    #[tokio::test]
    async fn a_retry_gives_up_on_a_first_attempt_with_no_outcome() {
        let (handler, params) = guarded(Scripted::new(Script::Hang));
        let req = request("ping");
        let started = Instant::now();
        // The unary request never ends, so its messageId is never bound.
        let retry = tokio::select! {
            _ = handler.send_message(&params, req.clone()) => panic!("the hanging request ended"),
            retry = async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                handler.send_message(&params, req).await
            } => retry,
        };
        let err = retry.unwrap_err();
        assert_eq!(err.code, error_code::INTERNAL_ERROR, "{err}");
        assert_eq!(err.message, STILL_IN_FLIGHT);
        assert!(started.elapsed() >= Duration::from_secs(DEDUP_INFLIGHT_WAIT_S));
    }
}
