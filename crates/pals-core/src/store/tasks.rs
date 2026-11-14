//! A2A tasks this agent serves, the peer that owns each one, and the
//! recovery of the tasks a restart interrupted.
//!
//! The task methods follow the SDK's `TaskStore` contract as
//! `a2a_server::InMemoryTaskStore` keeps it, in everything a client can
//! observe: the same errors, versions, list filters, order (by task id) and
//! offset page tokens. The daemon's `SqliteTaskStore` only adapts them to the
//! async trait.
//!
//! An error an A2A client must see (`TASK_NOT_FOUND`, `TASK_NOT_CANCELABLE`,
//! `INVALID_REQUEST`, `INVALID_PARAMS`) is an [`A2AError`] inside the
//! `anyhow::Error`, found with `downcast_ref`; any other error is a fault of
//! the store, for the caller to report as an internal error. Creating a task
//! that already exists is such a fault, as in the SDK store.

use a2a::{
    A2AError, ListTasksRequest, ListTasksResponse, Message, Part, Role, Task, TaskState, TaskStatus,
};
use anyhow::{Context, bail, ensure};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::Store;
use crate::identity::AgentId;

/// The text of the status message of a task failed by
/// [`Store::tasks_fail_interrupted`].
pub const INTERRUPTED_MESSAGE: &str = "interrupted: daemon restarted";

/// The page served when a list request asks for none, or for zero or fewer
/// tasks: the SDK's `a2a_server::pagination::DEFAULT_PAGE_SIZE`.
const DEFAULT_PAGE_SIZE: usize = 50;
/// The largest page served, whatever a list request asks for: the SDK's
/// `a2a_server::pagination::MAX_PAGE_SIZE`.
const MAX_PAGE_SIZE: usize = 100;

/// The SDK's `resolve_page_size`.
fn page_size(requested: Option<i32>) -> usize {
    match requested {
        Some(size) if size > 0 => (size as usize).min(MAX_PAGE_SIZE),
        _ => DEFAULT_PAGE_SIZE,
    }
}

/// Keeps the last `history_length` messages of the task's history, none
/// when it is zero or negative, all when it is `None`: the SDK's
/// `apply_history_length`.
fn apply_history_length(task: &mut Task, history_length: Option<i32>) {
    let (Some(requested), Some(history)) = (history_length, task.history.as_mut()) else {
        return;
    };
    let keep = usize::try_from(requested).unwrap_or(0);
    if history.len() > keep {
        history.drain(..history.len() - keep);
    }
}

/// The `state` column: the A2A name of the state.
fn state_name(state: &TaskState) -> anyhow::Result<String> {
    serde_json::to_value(state)?
        .as_str()
        .map(str::to_owned)
        .context("a task state is not a JSON string")
}

/// The state a `state` column names.
fn parse_state(name: &str) -> anyhow::Result<TaskState> {
    serde_json::from_value(name.into()).with_context(|| format!("unknown task state {name}"))
}

/// The task in a `task_json` column, which must be the task the row's
/// `task_id` names.
fn decode(task_id: &str, json: &str) -> anyhow::Result<Task> {
    let task: Task = serde_json::from_str(json)
        .with_context(|| format!("stored task {task_id} does not parse"))?;
    ensure!(
        task.id == task_id,
        "the row of task {task_id} holds task {}",
        task.id
    );
    Ok(task)
}

/// Writes `task` over its row as `version`, at `now_unix`.
fn rewrite(conn: &Connection, task: &Task, version: i64, now_unix: i64) -> anyhow::Result<()> {
    let changed = conn.execute(
        "UPDATE tasks
         SET context_id = ?2, state = ?3, updated_at = ?4, version = ?5, task_json = ?6
         WHERE task_id = ?1",
        params![
            task.id,
            task.context_id,
            state_name(&task.status.state)?,
            now_unix,
            version,
            serde_json::to_string(task)?
        ],
    )?;
    ensure!(changed == 1, "task {} is not stored", task.id);
    Ok(())
}

fn to_version(version: i64) -> anyhow::Result<u64> {
    u64::try_from(version).with_context(|| format!("task version {version} out of range"))
}

impl Store {
    /// Stores a new task, at `now_unix`, as version 1. A task with that id
    /// already stored is refused, and stays as it was.
    pub fn task_create(&self, task: &Task, now_unix: i64) -> anyhow::Result<u64> {
        let inserted = self.conn().execute(
            "INSERT INTO tasks (task_id, context_id, state, updated_at, version, task_json)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT (task_id) DO NOTHING",
            params![
                task.id,
                task.context_id,
                state_name(&task.status.state)?,
                now_unix,
                serde_json::to_string(task)?
            ],
        )?;
        ensure!(inserted == 1, "task {} already exists", task.id);
        Ok(1)
    }

    /// Replaces a stored task, at `now_unix`, and returns its new version.
    ///
    /// `TASK_NOT_FOUND` when no task with that id is stored. A task in a
    /// terminal state keeps it: `INVALID_REQUEST` when `task` is in another
    /// state, while the same terminal state can be written again.
    pub fn task_update(&self, task: &Task, now_unix: i64) -> anyhow::Result<u64> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored: Option<(String, i64)> = tx
            .query_row(
                "SELECT state, version FROM tasks WHERE task_id = ?1",
                [&task.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((state, version)) = stored else {
            bail!(A2AError::task_not_found(&task.id));
        };
        let state = parse_state(&state)?;
        if state.is_terminal() && state != task.status.state {
            bail!(A2AError::invalid_request(format!(
                "task {} is in terminal state {:?} and cannot transition to {:?}",
                task.id, state, task.status.state
            )));
        }
        rewrite(&tx, task, version + 1, now_unix)?;
        tx.commit()?;
        to_version(version + 1)
    }

    /// The task with that id, whole.
    pub fn task_get(&self, task_id: &str) -> anyhow::Result<Option<Task>> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT task_json FROM tasks WHERE task_id = ?1",
                [task_id],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|json| decode(task_id, &json)).transpose()
    }

    /// One page of the tasks that match `req`'s filters (context, state, a
    /// status timestamp strictly after `status_timestamp_after`), by task
    /// id, with their history cut to `history_length`.
    ///
    /// A page token is the offset of the page's first task in the filtered
    /// list; `INVALID_PARAMS` for one that is not a number, and an empty last
    /// page for one past the end. `total_size` counts the filtered list.
    pub fn tasks_list(&self, req: &ListTasksRequest) -> anyhow::Result<ListTasksResponse> {
        self.tasks_list_where(None, req)
    }

    /// [`Store::tasks_list`] over the tasks `owner` owns only.
    pub fn tasks_list_for_owner(
        &self,
        owner: &AgentId,
        req: &ListTasksRequest,
    ) -> anyhow::Result<ListTasksResponse> {
        self.tasks_list_where(Some(owner), req)
    }

    fn tasks_list_where(
        &self,
        owner: Option<&AgentId>,
        req: &ListTasksRequest,
    ) -> anyhow::Result<ListTasksResponse> {
        let page_size = page_size(req.page_size);
        let start = match &req.page_token {
            Some(token) => token
                .parse::<usize>()
                .map_err(|_| A2AError::invalid_params("invalid page token"))?,
            None => 0,
        };
        let state = req.status.as_ref().map(state_name).transpose()?;
        let rows: Vec<(String, String)> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT task_id, task_json FROM tasks
                 WHERE (?1 IS NULL OR context_id = ?1)
                   AND (?2 IS NULL OR state = ?2)
                   AND (?3 IS NULL
                        OR task_id IN (SELECT task_id FROM task_owners WHERE owner = ?3))
                 ORDER BY task_id",
            )?;
            stmt.query_map(params![req.context_id, state, owner], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?
        };
        let mut tasks = rows
            .iter()
            .map(|(task_id, json)| decode(task_id, json))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if let Some(after) = req.status_timestamp_after {
            tasks.retain(|t| t.status.timestamp.is_some_and(|at| at > after));
        }

        let total = tasks.len();
        let start = start.min(total);
        let end = start.saturating_add(page_size).min(total);
        let next_page_token = if end < total {
            end.to_string()
        } else {
            String::new()
        };
        let page = tasks
            .drain(start..end)
            .map(|mut task| {
                apply_history_length(&mut task, req.history_length);
                task
            })
            .collect();
        Ok(ListTasksResponse {
            tasks: page,
            next_page_token,
            page_size: i32::try_from(page_size)?,
            total_size: i32::try_from(total).context("too many tasks to count")?,
        })
    }

    /// Moves a task that is not in a terminal state to `CANCELED`, stamped
    /// `now`, and returns it: check and move in one transaction, so of two
    /// concurrent cancels only one succeeds.
    ///
    /// `TASK_NOT_FOUND` when no task with that id is stored,
    /// `TASK_NOT_CANCELABLE` when it is in a terminal state.
    pub fn task_begin_cancel(&self, task_id: &str, now: DateTime<Utc>) -> anyhow::Result<Task> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored: Option<(String, i64)> = tx
            .query_row(
                "SELECT task_json, version FROM tasks WHERE task_id = ?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((json, version)) = stored else {
            bail!(A2AError::task_not_found(task_id));
        };
        let mut task = decode(task_id, &json)?;
        if task.status.state.is_terminal() {
            bail!(A2AError::task_not_cancelable(task_id));
        }
        task.status.state = TaskState::Canceled;
        task.status.timestamp = Some(now);
        rewrite(&tx, &task, version + 1, now.timestamp())?;
        tx.commit()?;
        Ok(task)
    }

    /// Records `owner` as the peer that owns `task_id`, stored or not yet.
    ///
    /// Ownership never changes hands: recording the same owner again does
    /// nothing, and another owner is refused.
    pub fn task_owner_put(&self, task_id: &str, owner: &AgentId) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO task_owners (task_id, owner) VALUES (?1, ?2)
             ON CONFLICT (task_id) DO NOTHING",
            params![task_id, owner],
        )?;
        // Never updated nor deleted: this is whoever recorded it first.
        let stored: AgentId = conn.query_row(
            "SELECT owner FROM task_owners WHERE task_id = ?1",
            [task_id],
            |r| r.get(0),
        )?;
        ensure!(
            stored == *owner,
            "task {task_id} already belongs to {stored}, not to {owner}"
        );
        Ok(())
    }

    /// The peer that owns `task_id`, if one was recorded.
    pub fn task_owner(&self, task_id: &str) -> anyhow::Result<Option<AgentId>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT owner FROM task_owners WHERE task_id = ?1",
                [task_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Fails every task a restart interrupted, those `SUBMITTED` or
    /// `WORKING`, and returns how many: each becomes `FAILED` at `now_unix`
    /// with an agent status message whose text is [`INTERRUPTED_MESSAGE`],
    /// as a new version. Tasks in a terminal state or waiting for input or
    /// authorization are left as they are.
    ///
    /// The daemon calls it when it starts, before it accepts connections:
    /// no work survives a restart, so a task left in progress would never
    /// end.
    pub fn tasks_fail_interrupted(&self, now_unix: i64) -> anyhow::Result<u64> {
        let now = DateTime::from_timestamp(now_unix, 0)
            .with_context(|| format!("no instant for {now_unix}"))?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows: Vec<(String, String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT task_id, task_json, version FROM tasks WHERE state IN (?1, ?2)
                 ORDER BY task_id",
            )?;
            stmt.query_map(
                params![
                    state_name(&TaskState::Submitted)?,
                    state_name(&TaskState::Working)?
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<Result<_, _>>()?
        };
        for (task_id, json, version) in &rows {
            let mut task = decode(task_id, json)?;
            let mut message = Message::new(Role::Agent, vec![Part::text(INTERRUPTED_MESSAGE)]);
            message.task_id = Some(task.id.clone());
            message.context_id = Some(task.context_id.clone());
            task.status = TaskStatus {
                state: TaskState::Failed,
                message: Some(message),
                timestamp: Some(now),
            };
            rewrite(&tx, &task, version + 1, now_unix)?;
        }
        tx.commit()?;
        Ok(rows.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentKey;

    /// 2026-11-14T12:00:00Z.
    const NOW: i64 = 1_794_657_600;

    fn task(id: &str, state: TaskState) -> Task {
        Task {
            id: id.to_owned(),
            context_id: "ctx".to_owned(),
            status: TaskStatus {
                state,
                message: Some(Message::new(Role::Agent, vec![Part::text("before")])),
                timestamp: DateTime::from_timestamp(NOW - 60, 5),
            },
            artifacts: None,
            history: Some(vec![Message::new(Role::User, vec![Part::text("hi")])]),
            metadata: None,
        }
    }

    /// The `(state, version, updated_at)` columns of a task's row.
    fn columns(store: &Store, task_id: &str) -> (String, i64, i64) {
        store
            .conn()
            .query_row(
                "SELECT state, version, updated_at FROM tasks WHERE task_id = ?1",
                params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    #[test]
    fn a_task_owner_is_set_once() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let alice = AgentKey::generate().id();
        let bob = AgentKey::generate().id();
        assert_eq!(store.task_owner("t-1").unwrap(), None);

        store.task_owner_put("t-1", &alice).unwrap();
        store.task_owner_put("t-1", &alice).unwrap();
        assert_eq!(store.task_owner("t-1").unwrap(), Some(alice));

        let err = store.task_owner_put("t-1", &bob).unwrap_err();
        assert!(err.to_string().contains("already belongs to"), "{err}");
        assert_eq!(store.task_owner("t-1").unwrap(), Some(alice));
    }

    #[test]
    fn only_submitted_and_working_tasks_are_interrupted() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let states = [
            (TaskState::Unspecified, false),
            (TaskState::Submitted, true),
            (TaskState::Working, true),
            (TaskState::InputRequired, false),
            (TaskState::AuthRequired, false),
            (TaskState::Completed, false),
            (TaskState::Failed, false),
            (TaskState::Canceled, false),
            (TaskState::Rejected, false),
        ];
        let created: Vec<Task> = states
            .iter()
            .map(|(state, _)| task(&format!("{state:?}"), state.clone()))
            .collect();
        for t in &created {
            assert_eq!(store.task_create(t, NOW - 60).unwrap(), 1);
        }

        assert_eq!(store.tasks_fail_interrupted(NOW).unwrap(), 2);

        for (before, (_, interrupted)) in created.into_iter().zip(states) {
            let id = before.id.clone();
            let after = store.task_get(&id).unwrap().unwrap();
            if !interrupted {
                assert_eq!(after, before, "{id}");
                let (_, version, updated_at) = columns(&store, &id);
                assert_eq!((version, updated_at), (1, NOW - 60), "{id}");
                continue;
            }
            assert_eq!(after.status.state, TaskState::Failed, "{id}");
            assert_eq!(after.status.timestamp, DateTime::from_timestamp(NOW, 0));
            let message = after.status.message.as_ref().unwrap();
            assert_eq!(message.text(), Some(INTERRUPTED_MESSAGE));
            assert_eq!(
                (after.history, after.context_id),
                (before.history, before.context_id)
            );
            assert_eq!(
                columns(&store, &id),
                ("TASK_STATE_FAILED".to_owned(), 2, NOW)
            );
        }
        assert_eq!(store.tasks_fail_interrupted(NOW + 1).unwrap(), 0);
    }

    #[test]
    fn a_protocol_error_is_an_a2a_error() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let err = store
            .task_update(&task("t-absent", TaskState::Working), NOW)
            .unwrap_err();
        let err = err.downcast_ref::<A2AError>().expect("an A2A error");
        assert_eq!(err.code, a2a::error_code::TASK_NOT_FOUND);

        // A store fault is not.
        store
            .task_create(&task("t-1", TaskState::Working), NOW)
            .unwrap();
        let err = store
            .task_create(&task("t-1", TaskState::Working), NOW)
            .unwrap_err();
        assert!(err.downcast_ref::<A2AError>().is_none(), "{err}");
        assert!(err.to_string().contains("already exists"), "{err}");
    }
}
