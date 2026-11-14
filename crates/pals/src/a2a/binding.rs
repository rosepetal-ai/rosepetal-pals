//! [`BindingExecutor`]: an execution is bound to the request that started it
//! before it does any work.

use std::sync::Arc;

use a2a::{
    A2AError, Message, Part, Role, StreamResponse, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use a2a_server::{AgentExecutor, ExecutorContext};
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use pals_core::store::Store;
use tracing::{error, warn};

use super::guard::{idempotency_requested, peer};
use super::task_store::blocking;

type Events = BoxStream<'static, Result<StreamResponse, A2AError>>;

/// The agent's executor, with every execution bound to its request when it
/// starts.
///
/// Before the inner executor runs, the requesting peer, the
/// `x-agentnet-peer` service param, is recorded as the owner of the task;
/// and, when the request activates the
/// [idempotency extension](super::IDEMPOTENCY_EXTENSION), the request's
/// `(peer, messageId)` dedup key is bound to the task. From then on a retry
/// of the request attaches to the task, however long it runs, even after a
/// restart fails it (see [`GuardedHandler`](super::GuardedHandler)). If
/// that record fails, the inner executor does not run and the execution
/// ends with the error.
///
/// An error from the inner executor would leave the task in its last state,
/// with no execution left to move it, and a retry would attach to it for
/// good. Such an error instead ends the execution with the task `FAILED`,
/// its status message holding the error's message.
///
/// Cancellation goes to the inner executor unchanged.
pub(crate) struct BindingExecutor<E> {
    inner: Arc<E>,
    store: Arc<Store>,
}

impl<E: AgentExecutor> BindingExecutor<E> {
    pub(crate) fn new(inner: E, store: Arc<Store>) -> Self {
        Self {
            inner: Arc::new(inner),
            store,
        }
    }
}

impl<E: AgentExecutor> AgentExecutor for BindingExecutor<E> {
    fn execute(&self, ctx: ExecutorContext) -> Events {
        let inner = Arc::clone(&self.inner);
        let store = Arc::clone(&self.store);
        let events = stream::once(async move {
            if let Err(err) = bind(&store, &ctx).await {
                return Box::pin(stream::iter([Err(err)])) as Events;
            }
            let (task_id, context_id) = ctx.task_info();
            fail_on_error(inner.execute(ctx), task_id, context_id)
        });
        Box::pin(events.flatten())
    }

    fn cancel(&self, ctx: ExecutorContext) -> Events {
        self.inner.cancel(ctx)
    }
}

/// Records the peer of `ctx` as the owner of its task, and, when the
/// request is idempotent, binds the dedup key of its message to the task.
async fn bind(store: &Arc<Store>, ctx: &ExecutorContext) -> Result<(), A2AError> {
    let peer = peer(&ctx.service_params)?;
    let Some(message) = &ctx.message else {
        error!(task_id = %ctx.task_id, "an execution started without a message");
        return Err(A2AError::internal("Internal error"));
    };
    let task_id = ctx.task_id.clone();
    let message_id = idempotency_requested(&ctx.service_params).then(|| message.message_id.clone());
    blocking(store, move |store| {
        store.task_owner_put(&task_id, &peer)?;
        match message_id {
            Some(message_id) => store.dedup_bind(&peer, &message_id, Some(&task_id), None),
            None => Ok(()),
        }
    })
    .await
}

/// `events`, ending with the task `FAILED` instead of with an error.
fn fail_on_error(events: Events, task_id: String, context_id: String) -> Events {
    Box::pin(stream::unfold(Some(events), move |events| {
        let (task_id, context_id) = (task_id.clone(), context_id.clone());
        async move {
            let mut events = events?;
            match events.next().await? {
                Ok(event) => Some((Ok(event), Some(events))),
                Err(err) => {
                    warn!(%task_id, error = %err, "the executor failed; failing its task");
                    let failed = failed(task_id, context_id, err.message);
                    Some((Ok(failed), None))
                }
            }
        }
    }))
}

/// The status update that fails `task_id`, with `reason` as its message.
fn failed(task_id: String, context_id: String, reason: String) -> StreamResponse {
    let mut message = Message::new(Role::Agent, vec![Part::text(reason)]);
    message.task_id = Some(task_id.clone());
    message.context_id = Some(context_id.clone());
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id,
        context_id,
        status: TaskStatus {
            state: TaskState::Failed,
            message: Some(message),
            timestamp: None,
        },
        metadata: None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use a2a_server::ServiceParams;
    use pals_core::identity::{AgentId, AgentKey};
    use pals_core::store::{DedupBegin, DedupRow};

    use super::*;
    use crate::net::PEER_HEADER;

    /// What [`Probe`] found in the store when it ran.
    type Seen = Arc<Mutex<Option<(Option<AgentId>, DedupBegin)>>>;

    /// An executor that looks up its task's owner and its message's dedup
    /// row when it runs, then fails with `fail` or ends.
    struct Probe {
        store: Arc<Store>,
        peer: AgentId,
        seen: Seen,
        fail: Option<A2AError>,
    }

    impl AgentExecutor for Probe {
        fn execute(&self, ctx: ExecutorContext) -> Events {
            let message_id = &ctx.message.as_ref().unwrap().message_id;
            let owner = self.store.task_owner(&ctx.task_id).unwrap();
            let row = self
                .store
                .dedup_begin(&self.peer, message_id, "h", 0)
                .unwrap();
            *self.seen.lock().unwrap() = Some((owner, row));
            Box::pin(stream::iter(self.fail.clone().map(Err)))
        }

        fn cancel(&self, _ctx: ExecutorContext) -> Events {
            Box::pin(stream::empty())
        }
    }

    /// A store, a peer, and a binding executor around a [`Probe`].
    fn probed(fail: Option<A2AError>) -> (Arc<Store>, AgentId, Seen, BindingExecutor<Probe>) {
        let store = Arc::new(Store::open_in_memory(AgentKey::generate().id()).unwrap());
        let peer = AgentKey::generate().id();
        let seen = Seen::default();
        let probe = Probe {
            store: Arc::clone(&store),
            peer,
            seen: Arc::clone(&seen),
            fail,
        };
        let executor = BindingExecutor::new(probe, Arc::clone(&store));
        (store, peer, seen, executor)
    }

    fn context(peer: AgentId) -> ExecutorContext {
        ExecutorContext {
            message: Some(Message::new(Role::User, vec![Part::text("hola")])),
            task_id: "t-1".to_owned(),
            stored_task: None,
            context_id: "c-1".to_owned(),
            metadata: None,
            user: None,
            service_params: ServiceParams::from([
                (PEER_HEADER.to_owned(), vec![peer.to_string()]),
                (
                    "a2a-extensions".to_owned(),
                    vec![crate::a2a::IDEMPOTENCY_EXTENSION.to_owned()],
                ),
            ]),
            tenant: None,
        }
    }

    #[tokio::test]
    async fn an_execution_is_bound_before_the_executor_runs() {
        let (store, peer, seen, executor) = probed(None);
        let ctx = context(peer);
        let message_id = ctx.message.as_ref().unwrap().message_id.clone();
        store.dedup_begin(&peer, &message_id, "h", 0).unwrap();

        let events: Vec<_> = executor.execute(ctx).collect().await;
        assert!(events.is_empty());
        let (owner, row) = seen.lock().unwrap().take().expect("the executor ran");
        assert_eq!(owner, Some(peer));
        assert_eq!(
            row,
            DedupBegin::Existing(DedupRow {
                request_hash: "h".to_owned(),
                task_id: Some("t-1".to_owned()),
                response_json: None,
            })
        );
    }

    #[tokio::test]
    async fn an_execution_that_cannot_be_bound_does_not_run() {
        // No dedup row for the message: nothing to bind it to.
        let (_store, peer, seen, executor) = probed(None);
        let events: Vec<_> = executor.execute(context(peer)).collect().await;
        assert!(matches!(events.as_slice(), [Err(_)]), "{events:?}");
        assert!(seen.lock().unwrap().is_none());

        // Nor without an authenticated peer.
        let (_store, peer, seen, executor) = probed(None);
        let mut ctx = context(peer);
        ctx.service_params.clear();
        let events: Vec<_> = executor.execute(ctx).collect().await;
        assert!(matches!(events.as_slice(), [Err(_)]), "{events:?}");
        assert!(seen.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn without_idempotency_an_execution_records_its_owner_only() {
        // No dedup row for the message, and none needed.
        let (_store, peer, seen, executor) = probed(None);
        let mut ctx = context(peer);
        ctx.service_params.remove("a2a-extensions");
        let events: Vec<_> = executor.execute(ctx).collect().await;
        assert!(events.is_empty(), "{events:?}");
        let (owner, row) = seen.lock().unwrap().take().expect("the executor ran");
        assert_eq!(owner, Some(peer));
        assert_eq!(row, DedupBegin::New);
    }

    #[tokio::test]
    async fn an_executor_error_ends_with_the_task_failed() {
        let (store, peer, _seen, executor) = probed(Some(A2AError::internal("boom")));
        let ctx = context(peer);
        let message_id = ctx.message.as_ref().unwrap().message_id.clone();
        store.dedup_begin(&peer, &message_id, "h", 0).unwrap();

        let events: Vec<_> = executor.execute(ctx).collect().await;
        let [Ok(StreamResponse::StatusUpdate(update))] = events.as_slice() else {
            panic!("expected one status update: {events:?}");
        };
        assert_eq!(update.task_id, "t-1");
        assert_eq!(update.context_id, "c-1");
        assert_eq!(update.status.state, TaskState::Failed);
        let message = update.status.message.as_ref().unwrap();
        assert_eq!(message.parts[0].as_text(), Some("boom"));
        assert_eq!(message.task_id.as_deref(), Some("t-1"));
    }
}
