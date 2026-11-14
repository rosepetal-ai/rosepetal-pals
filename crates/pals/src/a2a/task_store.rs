//! [`SqliteTaskStore`]: the SDK's task store on the agent's SQLite database.

use std::sync::Arc;

use a2a::{A2AError, ListTasksRequest, ListTasksResponse, Task};
use a2a_server::TaskStore;
use a2a_server::task_store::TaskVersion;
use async_trait::async_trait;
use chrono::Utc;
use pals_core::store::Store;
use tracing::error;

/// The SDK's [`TaskStore`] on the agent's [`Store`], so tasks survive a
/// restart. It behaves as the SDK's `InMemoryTaskStore` in everything a
/// client can observe; the `Store::task_*` methods it calls hold the logic.
///
/// Each call runs on Tokio's blocking pool, since SQLite blocks.
pub struct SqliteTaskStore {
    store: Arc<Store>,
}

impl SqliteTaskStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Runs `call` on the store in the blocking pool.
    async fn run<T, F>(&self, call: F) -> Result<T, A2AError>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> anyhow::Result<T> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || call(&store))
            .await
            .map_err(|err| a2a_error(err.into()))?
            .map_err(a2a_error)
    }
}

/// The A2A error a store error carries; otherwise the store failed, and the
/// client gets the SDK's generic internal error while the cause is logged,
/// as the SDK reports its own faults.
fn a2a_error(err: anyhow::Error) -> A2AError {
    err.downcast::<A2AError>().unwrap_or_else(|err| {
        error!(error = %format!("{err:#}"), "task store failure; returning a generic message to the client");
        A2AError::internal("Internal error")
    })
}

#[async_trait]
impl TaskStore for SqliteTaskStore {
    async fn create(&self, task: Task) -> Result<TaskVersion, A2AError> {
        self.run(move |store| store.task_create(&task, Utc::now().timestamp()))
            .await
    }

    async fn update(&self, task: Task) -> Result<TaskVersion, A2AError> {
        self.run(move |store| store.task_update(&task, Utc::now().timestamp()))
            .await
    }

    async fn get(&self, task_id: &str) -> Result<Option<Task>, A2AError> {
        let task_id = task_id.to_owned();
        self.run(move |store| store.task_get(&task_id)).await
    }

    async fn list(&self, req: &ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        let req = req.clone();
        self.run(move |store| store.tasks_list(&req)).await
    }

    async fn begin_cancel(&self, task_id: &str) -> Result<Task, A2AError> {
        let task_id = task_id.to_owned();
        self.run(move |store| store.task_begin_cancel(&task_id, Utc::now()))
            .await
    }
}
