//! [`SqliteTaskStore`]: the SDK's task store on the agent's SQLite database.

use std::sync::Arc;

use a2a::{A2AError, ListTasksRequest, ListTasksResponse, Task};
use a2a_server::TaskStore;
use a2a_server::task_store::TaskVersion;
use async_trait::async_trait;
use chrono::Utc;
use pals_core::identity::AgentKey;
use pals_core::store::Store;
use serde_json::json;
use tracing::error;

/// Event kind logged when a task is created.
const TASK_ACCEPTED: &str = "task.accepted";
/// Event kind logged when a task first reaches a terminal state.
const TASK_TERMINAL: &str = "task.terminal";

/// The SDK's [`TaskStore`] on the agent's [`Store`], so tasks survive a
/// restart. It behaves as the SDK's `InMemoryTaskStore` in everything a
/// client can observe; the `Store::task_*` methods it calls hold the logic.
///
/// With [`SqliteTaskStore::with_events`] it also logs, signed by the agent,
/// `task.accepted` when a task is created and `task.terminal` when a task
/// first reaches a terminal state. Every task the A2A server serves is
/// created and moved here, whether its request was unary or streaming and
/// whether it completed, failed or was canceled, so each event is logged
/// once per task. The payload is `{task_id, context_id, state}`.
///
/// Each call runs on Tokio's blocking pool, since SQLite blocks.
pub struct SqliteTaskStore {
    store: Arc<Store>,
    events: Option<Arc<AgentKey>>,
}

impl SqliteTaskStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            events: None,
        }
    }

    /// Logs the task events, signed by `key`, in the store's event log.
    pub fn with_events(mut self, key: Arc<AgentKey>) -> Self {
        self.events = Some(key);
        self
    }

    /// Runs `call` on the store in the blocking pool, with the event key.
    async fn run<T, F>(&self, call: F) -> Result<T, A2AError>
    where
        T: Send + 'static,
        F: FnOnce(&Store, Option<&AgentKey>) -> anyhow::Result<T> + Send + 'static,
    {
        let events = self.events.clone();
        blocking(&self.store, move |store| call(store, events.as_deref())).await
    }
}

/// Runs `call` on `store` in Tokio's blocking pool, its error mapped by
/// [`a2a_error`].
pub(crate) async fn blocking<T, F>(store: &Arc<Store>, call: F) -> Result<T, A2AError>
where
    T: Send + 'static,
    F: FnOnce(&Store) -> anyhow::Result<T> + Send + 'static,
{
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || call(&store))
        .await
        .map_err(|err| a2a_error(err.into()))?
        .map_err(a2a_error)
}

/// The A2A error a store error carries; otherwise the store failed, and the
/// client gets the SDK's generic internal error while the cause is logged,
/// as the SDK reports its own faults.
pub(crate) fn a2a_error(err: anyhow::Error) -> A2AError {
    err.downcast::<A2AError>().unwrap_or_else(|err| {
        error!(error = %format!("{err:#}"), "task store failure; returning a generic message to the client");
        A2AError::internal("Internal error")
    })
}

/// Appends a task event of `kind` about `task`, when there is a key.
///
/// The event follows the write it records, in its own transaction: a crash
/// between the two loses the event, never duplicates it, and an append that
/// fails fails the call with the write already made.
fn log(
    store: &Store,
    key: Option<&AgentKey>,
    kind: &str,
    task: &Task,
    now_unix: i64,
) -> anyhow::Result<()> {
    let Some(key) = key else {
        return Ok(());
    };
    let payload = json!({
        "task_id": task.id,
        "context_id": task.context_id,
        "state": task.status.state,
    });
    store.event_append(key, kind, &payload, now_unix)?;
    Ok(())
}

#[async_trait]
impl TaskStore for SqliteTaskStore {
    async fn create(&self, task: Task) -> Result<TaskVersion, A2AError> {
        self.run(move |store, key| {
            let now = Utc::now().timestamp();
            let version = store.task_create(&task, now)?;
            log(store, key, TASK_ACCEPTED, &task, now)?;
            if task.status.state.is_terminal() {
                log(store, key, TASK_TERMINAL, &task, now)?;
            }
            Ok(version)
        })
        .await
    }

    async fn update(&self, task: Task) -> Result<TaskVersion, A2AError> {
        self.run(move |store, key| {
            let now = Utc::now().timestamp();
            let (version, previous) = store.task_update_with_previous(&task, now)?;
            if !previous.is_terminal() && task.status.state.is_terminal() {
                log(store, key, TASK_TERMINAL, &task, now)?;
            }
            Ok(version)
        })
        .await
    }

    async fn get(&self, task_id: &str) -> Result<Option<Task>, A2AError> {
        let task_id = task_id.to_owned();
        self.run(move |store, _| store.task_get(&task_id)).await
    }

    async fn list(&self, req: &ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        let req = req.clone();
        self.run(move |store, _| store.tasks_list(&req)).await
    }

    async fn begin_cancel(&self, task_id: &str) -> Result<Task, A2AError> {
        let task_id = task_id.to_owned();
        self.run(move |store, key| {
            let now = Utc::now();
            // Only a task outside a terminal state can begin a cancel.
            let task = store.task_begin_cancel(&task_id, now)?;
            log(store, key, TASK_TERMINAL, &task, now.timestamp())?;
            Ok(task)
        })
        .await
    }
}
