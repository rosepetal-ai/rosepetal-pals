//! [`GuardedHandler`]: every task belongs to the peer that created it.

use std::sync::Arc;

use a2a::{
    A2AError, AgentCard, CancelTaskRequest, DeleteTaskPushNotificationConfigRequest,
    GetExtendedAgentCardRequest, GetTaskPushNotificationConfigRequest, GetTaskRequest,
    ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse,
    ListTasksRequest, ListTasksResponse, SendMessageRequest, SendMessageResponse, StreamResponse,
    SubscribeToTaskRequest, Task, TaskPushNotificationConfig,
};
use a2a_server::{RequestHandler, ServiceParams};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::Store;
use tracing::error;

use super::task_store::blocking;
use crate::net::PEER_HEADER;

/// A [`RequestHandler`] that confines each peer to its own tasks.
///
/// The owner of a task is the peer, [`PEER_HEADER`], whose request created
/// it: the guard records it with the first task the inner handler returns
/// or streams, before the peer sees the task's id. Getting, canceling or
/// subscribing to a task of another peer, or continuing one with
/// `SendMessage`, fails with `TaskNotFoundError`, exactly as for a task that
/// does not exist. `ListTasks` never reaches the inner handler: it lists the
/// requesting peer's tasks only.
///
/// Push notification configs and the extended card go to the inner handler
/// unchecked, and it refuses them because the agent card declares neither.
/// Serving push configs would need an ownership check here first.
pub struct GuardedHandler<H> {
    inner: H,
    store: Arc<Store>,
}

impl<H: RequestHandler> GuardedHandler<H> {
    /// Guards `inner` with the ownership records of `store`.
    ///
    /// `key` is the agent's key. The guard signs nothing in milestone 1: the
    /// task events are signed where every task transition is written, by
    /// [`SqliteTaskStore::with_events`](super::SqliteTaskStore::with_events).
    pub fn new(inner: H, store: Arc<Store>, key: Arc<AgentKey>) -> Self {
        let _ = key;
        Self { inner, store }
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

/// `events`, with `peer` recorded as the owner of each task before the first
/// event that names it goes out. If the record fails, the stream ends with
/// that error.
fn owned_stream(
    events: BoxStream<'static, Result<StreamResponse, A2AError>>,
    store: Arc<Store>,
    peer: AgentId,
) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
    struct State {
        events: BoxStream<'static, Result<StreamResponse, A2AError>>,
        store: Arc<Store>,
        peer: AgentId,
        owned: Option<String>,
        done: bool,
    }
    let state = State {
        events,
        store,
        peer,
        owned: None,
        done: false,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        if state.done {
            return None;
        }
        let item = state.events.next().await?;
        if let Ok(event) = &item
            && let Some(task_id) = event_task_id(event)
            && state.owned.as_deref() != Some(task_id)
        {
            if let Err(err) = own(&state.store, task_id, state.peer).await {
                state.done = true;
                return Some((Err(err), state));
            }
            state.owned = Some(task_id.to_owned());
        }
        Some((item, state))
    }))
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
        let response = self.inner.send_message(params, req).await?;
        let task_id = match &response {
            SendMessageResponse::Task(task) => Some(task.id.as_str()),
            SendMessageResponse::Message(message) => message.task_id.as_deref(),
        };
        if let Some(task_id) = task_id {
            own(&self.store, task_id, peer).await?;
        }
        Ok(response)
    }

    async fn send_streaming_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        let peer = peer(params)?;
        if let Some(task_id) = &req.message.task_id {
            self.ensure_owner(peer, task_id).await?;
        }
        let events = self.inner.send_streaming_message(params, req).await?;
        Ok(owned_stream(events, Arc::clone(&self.store), peer))
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
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
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
