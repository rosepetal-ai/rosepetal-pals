//! A2A tasks this agent serves, the peer that owns each one, and the
//! recovery of the tasks a restart interrupted.
//!
//! The task methods follow the SDK's `TaskStore` contract as
//! `a2a_server::InMemoryTaskStore` keeps it, in the errors, versions and
//! list filters a client can observe, but for listing, where A2A v1.0.1
//! §3.1.4 binds and that store departs from it (ruling R53): tasks come most
//! recently updated first, by status timestamp, newest first, then by task
//! id, last first, a task with no status timestamp after every other; page
//! tokens are cursors, where the page before ended, so that a walk through
//! the pages repeats no task and skips none of those not updated meanwhile;
//! and a task's artifacts are left out unless `includeArtifacts` is true.
//! The daemon's `SqliteTaskStore` only adapts them to the async trait.
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
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tracing::warn;

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

/// The `status_at` of a task with no status timestamp: before any other.
const NO_STATUS_AT: i64 = i64::MIN;

/// The `status_at` column of `task`: its status timestamp in nanoseconds
/// since the Unix epoch, held to what an `i64` counts (the years 1677 to
/// 2262), or [`NO_STATUS_AT`] when it has none.
pub(super) fn status_at(task: &Task) -> i64 {
    task.status.timestamp.map_or(NO_STATUS_AT, |at| nanos(&at))
}

/// `at` in nanoseconds since the Unix epoch, held to what an `i64` counts,
/// above [`NO_STATUS_AT`].
fn nanos(at: &DateTime<Utc>) -> i64 {
    at.timestamp_nanos_opt().unwrap_or(if at.timestamp() < 0 {
        NO_STATUS_AT + 1
    } else {
        i64::MAX
    })
}

/// Where a page of a listing ended: its last task's `status_at` and id. As a
/// page token it is opaque: the base64url of `<status_at>:<task id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cursor {
    status_at: i64,
    task_id: String,
}

impl Cursor {
    fn token(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!("{}:{}", self.status_at, self.task_id))
    }

    /// The cursor `token` holds; `INVALID_PARAMS` when it holds none.
    fn of_token(token: &str) -> Result<Self, A2AError> {
        let invalid = || A2AError::invalid_params("invalid page token");
        let text = URL_SAFE_NO_PAD
            .decode(token)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(invalid)?;
        let (status_at, task_id) = text.split_once(':').ok_or_else(invalid)?;
        let status_at = status_at.parse::<i64>().map_err(|_| invalid())?;
        if task_id.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            status_at,
            task_id: task_id.to_owned(),
        })
    }
}

/// Adds `tasks.status_at` and its index to a database of this schema
/// version written before they existed, filled from each task's JSON (a
/// row whose task does not decode gets [`NO_STATUS_AT`]). Does nothing to
/// one that has them.
pub(super) fn add_status_at(conn: &Connection) -> anyhow::Result<()> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'status_at'",
        [],
        |r| r.get(0),
    )?;
    if present > 0 {
        return Ok(());
    }
    conn.execute_batch(
        "ALTER TABLE tasks ADD COLUMN status_at INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX tasks_by_status_at ON tasks (status_at, task_id);",
    )?;
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT task_id, task_json FROM tasks")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (task_id, json) in rows {
        let at = decode(&task_id, &json).map_or(NO_STATUS_AT, |task| status_at(&task));
        conn.execute(
            "UPDATE tasks SET status_at = ?2 WHERE task_id = ?1",
            params![task_id, at],
        )?;
    }
    Ok(())
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
         SET context_id = ?2, state = ?3, updated_at = ?4, version = ?5, task_json = ?6,
             status_at = ?7
         WHERE task_id = ?1",
        params![
            task.id,
            task.context_id,
            state_name(&task.status.state)?,
            now_unix,
            version,
            serde_json::to_string(task)?,
            status_at(task)
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
            "INSERT INTO tasks (task_id, context_id, state, updated_at, version, task_json,
                                status_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)
             ON CONFLICT (task_id) DO NOTHING",
            params![
                task.id,
                task.context_id,
                state_name(&task.status.state)?,
                now_unix,
                serde_json::to_string(task)?,
                status_at(task)
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
        self.task_update_with_previous(task, now_unix)
            .map(|(version, _)| version)
    }

    /// [`Store::task_update`], also returning the state the task was in
    /// before this write, read in the same transaction: of all the writes of
    /// a task, exactly one finds it outside a terminal state and leaves it in
    /// one.
    pub fn task_update_with_previous(
        &self,
        task: &Task,
        now_unix: i64,
    ) -> anyhow::Result<(u64, TaskState)> {
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
        Ok((to_version(version + 1)?, state))
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
    /// status timestamp strictly after `status_timestamp_after`), most
    /// recently updated first: by status timestamp, newest first, then by
    /// task id, last first. History is cut to `history_length`; artifacts
    /// are left out unless `include_artifacts` is true.
    ///
    /// A page token is the cursor of where the page before ended
    /// ([`Cursor`]); `INVALID_PARAMS` for one that is not. `next_page_token`
    /// is empty on the last page, and `total_size` counts the filtered list.
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
        const FILTERS: &str = "(?1 IS NULL OR context_id = ?1)
            AND (?2 IS NULL OR state = ?2)
            AND (?3 IS NULL OR task_id IN (SELECT task_id FROM task_owners WHERE owner = ?3))
            AND (?4 IS NULL OR status_at > ?4)";
        let page_size = page_size(req.page_size);
        let after = req
            .page_token
            .as_deref()
            .map(Cursor::of_token)
            .transpose()?;
        let state = req.status.as_ref().map(state_name).transpose()?;
        let since = req.status_timestamp_after.as_ref().map(nanos);
        let (total, rows): (i64, Vec<(String, String, i64)>) = {
            let mut conn = self.conn();
            // One snapshot for the count and the page.
            let tx = conn.transaction()?;
            let total = tx.query_row(
                &format!("SELECT COUNT(*) FROM tasks WHERE {FILTERS}"),
                params![req.context_id, state, owner, since],
                |r| r.get(0),
            )?;
            let mut stmt = tx.prepare(&format!(
                "SELECT task_id, task_json, status_at FROM tasks
                 WHERE {FILTERS}
                   AND (?5 IS NULL OR status_at < ?5 OR (status_at = ?5 AND task_id < ?6))
                 ORDER BY status_at DESC, task_id DESC
                 LIMIT ?7"
            ))?;
            let rows = stmt
                .query_map(
                    params![
                        req.context_id,
                        state,
                        owner,
                        since,
                        after.as_ref().map(|cursor| cursor.status_at),
                        after.as_ref().map(|cursor| cursor.task_id.as_str()),
                        // One more, to know whether a page follows.
                        i64::try_from(page_size)? + 1
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            tx.commit()?;
            (total, rows)
        };
        let next_page_token = match rows.get(page_size) {
            Some(_) => {
                let (task_id, _, status_at) = &rows[page_size - 1];
                Cursor {
                    status_at: *status_at,
                    task_id: task_id.clone(),
                }
                .token()
            }
            None => String::new(),
        };
        let include_artifacts = req.include_artifacts == Some(true);
        let tasks = rows
            .iter()
            .take(page_size)
            .map(|(task_id, json, _)| {
                let mut task = decode(task_id, json)?;
                apply_history_length(&mut task, req.history_length);
                if !include_artifacts {
                    task.artifacts = None;
                }
                Ok(task)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(ListTasksResponse {
            tasks,
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
    /// authorization are left as they are, and so is a row whose task does
    /// not decode, skipped with a warning so one bad row cannot stop the
    /// daemon from starting.
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
        let mut failed = 0;
        for (task_id, json, version) in &rows {
            let mut task = match decode(task_id, json) {
                Ok(task) => task,
                Err(err) => {
                    warn!(
                        %task_id,
                        error = %format_args!("{err:#}"),
                        "skipping an unreadable task row while failing interrupted tasks"
                    );
                    continue;
                }
            };
            let mut message = Message::new(Role::Agent, vec![Part::text(INTERRUPTED_MESSAGE)]);
            message.task_id = Some(task.id.clone());
            message.context_id = Some(task.context_id.clone());
            task.status = TaskStatus {
                state: TaskState::Failed,
                message: Some(message),
                timestamp: Some(now),
            };
            rewrite(&tx, &task, version + 1, now_unix)?;
            failed += 1;
        }
        tx.commit()?;
        Ok(failed)
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
    fn a_database_from_before_status_at_gets_it_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pals.db");
        let owner = AgentKey::generate().id();
        // The schema as this version first wrote it, and two tasks in it.
        let old_schema = super::super::SCHEMA
            .replace(",\n    status_at  INTEGER NOT NULL", "")
            .replace(
                "CREATE INDEX tasks_by_status_at ON tasks (status_at, task_id);\n",
                "",
            );
        assert!(!old_schema.contains("status_at  INTEGER"));
        assert!(!old_schema.contains("tasks_by_status_at"));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&old_schema).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            for (id, at) in [("t-old", NOW - 60), ("t-new", NOW)] {
                let mut t = task(id, TaskState::Working);
                t.status.timestamp = DateTime::from_timestamp(at, 0);
                conn.execute(
                    "INSERT INTO tasks (task_id, context_id, state, updated_at, version, task_json)
                     VALUES (?1, 'ctx', 'TASK_STATE_WORKING', ?2, 1, ?3)",
                    params![id, at, serde_json::to_string(&t).unwrap()],
                )
                .unwrap();
            }
        }

        for _ in 0..2 {
            let store = Store::open(&path, owner).unwrap();
            let all = ListTasksRequest {
                context_id: None,
                status: None,
                page_size: None,
                page_token: None,
                history_length: None,
                status_timestamp_after: None,
                include_artifacts: None,
                tenant: None,
            };
            let listed: Vec<String> = store
                .tasks_list(&all)
                .unwrap()
                .tasks
                .into_iter()
                .map(|t| t.id)
                .collect();
            assert_eq!(listed, ["t-new", "t-old"]);
            store
                .task_create(&task("t-after", TaskState::Working), NOW)
                .unwrap();
            store
                .conn()
                .execute("DELETE FROM tasks WHERE task_id = 't-after'", [])
                .unwrap();
        }
    }

    #[test]
    fn page_tokens_are_opaque_cursors() {
        let cursor = Cursor {
            status_at: -5,
            task_id: "t:1".to_owned(),
        };
        assert_eq!(Cursor::of_token(&cursor.token()).unwrap(), cursor);
        assert!(!cursor.token().contains("t:1"));
        for bad in ["", "2", "!!", "LTU"] {
            let err = Cursor::of_token(bad).unwrap_err();
            assert_eq!(err.code, a2a::error_code::INVALID_PARAMS, "{bad}");
        }
    }

    #[test]
    fn status_timestamps_order_as_nanoseconds() {
        let mut t = task("t", TaskState::Working);
        t.status.timestamp = None;
        assert_eq!(status_at(&t), NO_STATUS_AT);
        let early = DateTime::from_timestamp(NOW, 1).unwrap();
        let late = DateTime::from_timestamp(NOW, 2).unwrap();
        assert!(nanos(&early) < nanos(&late));
        let far = DateTime::from_timestamp(i64::MAX / 1_000_000_000 + 1, 0).unwrap();
        assert_eq!(nanos(&far), i64::MAX);
        let ancient = DateTime::from_timestamp(i64::MIN / 1_000_000_000 - 1, 0).unwrap();
        assert_eq!(nanos(&ancient), NO_STATUS_AT + 1);
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
    fn an_unreadable_task_row_does_not_stop_recovery() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let readable = task("t-readable", TaskState::Working);
        store.task_create(&readable, NOW - 60).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO tasks (task_id, context_id, state, updated_at, version, task_json,
                                    status_at)
                 VALUES ('t-corrupt', 'ctx', 'TASK_STATE_WORKING', ?1, 1, '{not json', 0)",
                params![NOW - 60],
            )
            .unwrap();
        let corrupt_row = |store: &Store| -> (String, i64, i64, String) {
            store
                .conn()
                .query_row(
                    "SELECT state, version, updated_at, task_json FROM tasks
                     WHERE task_id = 't-corrupt'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap()
        };
        let before = corrupt_row(&store);

        assert_eq!(store.tasks_fail_interrupted(NOW).unwrap(), 1);

        let failed = store.task_get("t-readable").unwrap().unwrap();
        assert_eq!(failed.status.state, TaskState::Failed);
        assert_eq!(
            failed.status.message.as_ref().unwrap().text(),
            Some(INTERRUPTED_MESSAGE)
        );
        assert_eq!(corrupt_row(&store), before);
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

    #[test]
    fn an_update_reports_the_state_it_replaced() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        store
            .task_create(&task("t-1", TaskState::Submitted), NOW)
            .unwrap();
        let update = |state| store.task_update_with_previous(&task("t-1", state), NOW);
        assert_eq!(
            update(TaskState::Working).unwrap(),
            (2, TaskState::Submitted)
        );
        assert_eq!(
            update(TaskState::Completed).unwrap(),
            (3, TaskState::Working)
        );
        assert_eq!(
            update(TaskState::Completed).unwrap(),
            (4, TaskState::Completed)
        );
        // A refused write reports nothing and changes nothing.
        update(TaskState::Working).unwrap_err();
        assert_eq!(columns(&store, "t-1").1, 4);
    }
}
