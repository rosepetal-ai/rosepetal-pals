//! The SQLite task store: the SDK's `TaskStore` contract, checked with the
//! same assertions against the SDK's own `InMemoryTaskStore`; durability
//! across a reopen; recovery of the tasks a restart interrupted; listing
//! scoped to the peer that owns each task; and the task events it logs.

use std::collections::HashMap;
use std::fmt::Debug;
use std::future::{Future, ready};
use std::sync::Arc;

use a2a::{
    A2AError, Artifact, ListTasksRequest, ListTasksResponse, Message, Part, Role, Task, TaskState,
    TaskStatus, error_code,
};
use a2a_server::{InMemoryTaskStore, TaskStore};
use chrono::{DateTime, Duration, Utc};
use futures::future::join_all;
use pals::a2a::SqliteTaskStore;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::{INTERRUPTED_MESSAGE, Store};
use serde_json::json;

/// 2026-11-14T12:00:00Z.
const NOW: i64 = 1_794_657_600;

fn store() -> Arc<Store> {
    Arc::new(Store::open_in_memory(AgentKey::generate().id()).unwrap())
}

/// `n` seconds after [`NOW`], with a sub-second part a store must keep.
fn at(n: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(NOW, 123_456_789).unwrap() + Duration::seconds(n)
}

/// A task with a status message, history, an artifact and metadata, so a
/// round trip through a store has every kind of field to lose.
fn task(id: &str, context_id: &str, state: TaskState) -> Task {
    let mut status_message = Message::new(Role::Agent, vec![Part::text("on it")]);
    status_message.task_id = Some(id.to_owned());
    status_message.context_id = Some(context_id.to_owned());
    Task {
        id: id.to_owned(),
        context_id: context_id.to_owned(),
        status: TaskStatus {
            state,
            message: Some(status_message),
            timestamp: Some(at(0)),
        },
        artifacts: Some(vec![Artifact {
            artifact_id: format!("{id}-out"),
            name: Some("out".to_owned()),
            description: None,
            parts: vec![
                Part::text("result"),
                Part::raw(vec![0, 159, 255]).with_media_type("application/octet-stream"),
                Part::url("iroh-blob://example").with_filename("example.bin"),
            ],
            metadata: None,
            extensions: None,
        }]),
        history: Some(vec![
            Message::new(Role::User, vec![Part::text("1")]),
            Message::new(Role::Agent, vec![Part::data(json!({"n": 2, "x": 0.1}))]),
            Message::new(Role::User, vec![Part::text("3")]),
        ]),
        metadata: Some(HashMap::from([(
            "k".to_owned(),
            json!({"nested": [1, true, null, "é"]}),
        )])),
    }
}

/// A list request with no filter and no paging.
fn req() -> ListTasksRequest {
    ListTasksRequest {
        context_id: None,
        status: None,
        page_size: None,
        page_token: None,
        history_length: None,
        status_timestamp_after: None,
        include_artifacts: None,
        tenant: None,
    }
}

fn ids(resp: &ListTasksResponse) -> Vec<&str> {
    resp.tasks.iter().map(|t| t.id.as_str()).collect()
}

#[track_caller]
fn assert_error<T: Debug>(result: Result<T, A2AError>, code: i32, message: &str) {
    let err = result.expect_err("expected an error");
    assert_eq!((err.code, err.message.as_str()), (code, message));
}

/// Every page of `req`, following `next_page_token` until it is empty; each
/// page checked against `page_size` and `total_size`.
async fn walk<F, Fut>(mut list: F, req: ListTasksRequest, total: i32) -> Vec<Vec<String>>
where
    F: FnMut(ListTasksRequest) -> Fut,
    Fut: Future<Output = ListTasksResponse>,
{
    let mut pages = Vec::new();
    let mut token = None;
    loop {
        let resp = list(ListTasksRequest {
            page_token: token.take(),
            ..req.clone()
        })
        .await;
        assert_eq!(resp.total_size, total);
        assert_eq!(Some(resp.page_size), req.page_size);
        assert!(resp.tasks.len() <= resp.page_size as usize);
        pages.push(ids(&resp).into_iter().map(str::to_owned).collect());
        if resp.next_page_token.is_empty() {
            return pages;
        }
        assert!(pages.len() < 100, "the pages never end");
        token = Some(resp.next_page_token);
    }
}

/// The same assertions against any `TaskStore`.
async fn suite<S: TaskStore>(s: S) {
    list_filters_and_pages(&s).await;
    create_get_update(&s).await;
    begin_cancel(&s).await;
}

/// On an empty store: filters, ordering, history truncation and paging.
async fn list_filters_and_pages<S: TaskStore>(s: &S) {
    let rows = [
        ("t-05", "ctx-a", TaskState::Working, Some(at(5))),
        ("t-02", "ctx-a", TaskState::Submitted, Some(at(2))),
        ("t-07", "ctx-a", TaskState::Completed, Some(at(7))),
        ("t-01", "ctx-a", TaskState::Working, None),
        ("t-06", "ctx-a", TaskState::Working, Some(at(6))),
        ("t-03", "ctx-b", TaskState::Working, Some(at(3))),
        ("t-04", "ctx-a", TaskState::InputRequired, Some(at(4))),
    ];
    let mut all = Vec::new();
    for (id, context_id, state, timestamp) in rows {
        let mut t = task(id, context_id, state);
        t.status.timestamp = timestamp;
        assert_eq!(s.create(t.clone()).await.unwrap(), 1);
        all.push(t);
    }
    all.sort_by(|a, b| a.id.cmp(&b.id));
    let list = |r: ListTasksRequest| async move { s.list(&r).await.unwrap() };

    // Everything, by id, whole.
    let resp = list(req()).await;
    assert_eq!(resp.tasks, all);
    assert_eq!(resp.next_page_token, "");
    assert_eq!((resp.page_size, resp.total_size), (50, 7));

    let filtered = |context_id: Option<&str>, status: Option<TaskState>| ListTasksRequest {
        context_id: context_id.map(str::to_owned),
        status,
        ..req()
    };
    let resp = list(filtered(Some("ctx-a"), None)).await;
    assert_eq!(ids(&resp), ["t-01", "t-02", "t-04", "t-05", "t-06", "t-07"]);
    assert_eq!(resp.total_size, 6);
    let resp = list(filtered(None, Some(TaskState::Working))).await;
    assert_eq!(ids(&resp), ["t-01", "t-03", "t-05", "t-06"]);
    assert_eq!(resp.total_size, 4);
    let resp = list(filtered(Some("ctx-a"), Some(TaskState::Working))).await;
    assert_eq!(ids(&resp), ["t-01", "t-05", "t-06"]);
    let resp = list(filtered(Some("ctx-none"), None)).await;
    assert_eq!((resp.tasks.len(), resp.total_size), (0, 0));

    // Strictly after; a task without a status timestamp never qualifies.
    let resp = list(ListTasksRequest {
        status_timestamp_after: Some(at(5)),
        ..req()
    })
    .await;
    assert_eq!(ids(&resp), ["t-06", "t-07"]);
    assert_eq!(resp.total_size, 2);

    // The pages cover the filtered set once, in order.
    let paged =
        |page_size: i32, context_id: Option<&str>, status: Option<TaskState>| ListTasksRequest {
            page_size: Some(page_size),
            ..filtered(context_id, status)
        };
    assert_eq!(
        walk(list, paged(2, Some("ctx-a"), None), 6).await,
        [["t-01", "t-02"], ["t-04", "t-05"], ["t-06", "t-07"]]
    );
    assert_eq!(
        walk(list, paged(4, None, None), 7).await,
        [
            vec!["t-01", "t-02", "t-03", "t-04"],
            vec!["t-05", "t-06", "t-07"]
        ]
    );
    assert_eq!(
        walk(list, paged(1, None, Some(TaskState::Working)), 4).await,
        [["t-01"], ["t-03"], ["t-05"], ["t-06"]]
    );

    // Absent, zero and negative sizes mean the default page; larger ones are
    // capped.
    for (asked, served) in [(Some(0), 50), (Some(-3), 50), (Some(1000), 100)] {
        let resp = list(ListTasksRequest {
            page_size: asked,
            ..req()
        })
        .await;
        assert_eq!((resp.page_size, resp.tasks.len()), (served, 7));
    }

    // A token past the end is an empty last page; one that is not a token is
    // refused.
    let resp = list(ListTasksRequest {
        page_token: Some("999".to_owned()),
        ..req()
    })
    .await;
    assert!(resp.tasks.is_empty());
    assert_eq!((resp.next_page_token.as_str(), resp.total_size), ("", 7));
    assert_error(
        s.list(&ListTasksRequest {
            page_token: Some("not-a-number".to_owned()),
            ..req()
        })
        .await,
        error_code::INVALID_PARAMS,
        "invalid page token",
    );

    // History is cut in the listing only.
    let history_of = |resp: &ListTasksResponse| -> Vec<Vec<Message>> {
        resp.tasks
            .iter()
            .map(|t| t.history.clone().unwrap())
            .collect()
    };
    let resp = list(ListTasksRequest {
        history_length: Some(1),
        ..req()
    })
    .await;
    let last: Vec<Vec<Message>> = all
        .iter()
        .map(|t| t.history.as_ref().unwrap()[2..].to_vec())
        .collect();
    assert_eq!(history_of(&resp), last);
    for cut in [0, -1] {
        let resp = list(ListTasksRequest {
            history_length: Some(cut),
            ..req()
        })
        .await;
        assert!(history_of(&resp).iter().all(Vec::is_empty));
    }
    assert_eq!(s.get("t-01").await.unwrap().as_ref(), Some(&all[0]));
}

async fn create_get_update<S: TaskStore>(s: &S) {
    let original = task("u-1", "ctx-u", TaskState::Submitted);
    assert_eq!(s.create(original.clone()).await.unwrap(), 1);
    assert_eq!(s.get("u-1").await.unwrap(), Some(original.clone()));
    assert_eq!(s.get("u-absent").await.unwrap(), None);

    // Creating an existing task is refused and changes nothing.
    let mut duplicate = original.clone();
    duplicate.status.state = TaskState::Working;
    assert_error(
        s.create(duplicate).await,
        error_code::INTERNAL_ERROR,
        "Internal error",
    );
    assert_eq!(s.get("u-1").await.unwrap(), Some(original.clone()));

    // Every update is a new version.
    let mut working = original.clone();
    working.status.state = TaskState::Working;
    working.status.timestamp = Some(at(10));
    assert_eq!(s.update(working.clone()).await.unwrap(), 2);
    assert_eq!(s.get("u-1").await.unwrap(), Some(working.clone()));
    working.artifacts = None;
    working.history = Some(Vec::new());
    assert_eq!(s.update(working.clone()).await.unwrap(), 3);
    assert_eq!(s.get("u-1").await.unwrap(), Some(working.clone()));

    // An update replaces the whole task, its context too.
    working.context_id = "ctx-moved".to_owned();
    assert_eq!(s.update(working.clone()).await.unwrap(), 4);
    let in_context = |context_id: &str| ListTasksRequest {
        context_id: Some(context_id.to_owned()),
        ..req()
    };
    assert_eq!(
        ids(&s.list(&in_context("ctx-moved")).await.unwrap()),
        ["u-1"]
    );
    assert!(s.list(&in_context("ctx-u")).await.unwrap().tasks.is_empty());

    assert_error(
        s.update(task("u-absent", "ctx-u", TaskState::Working))
            .await,
        error_code::TASK_NOT_FOUND,
        "task not found: u-absent",
    );
    assert_eq!(s.get("u-absent").await.unwrap(), None);

    // A terminal state never changes; the same one can be written again.
    let done = task("u-done", "ctx-u", TaskState::Completed);
    assert_eq!(s.create(done.clone()).await.unwrap(), 1);
    assert_error(
        s.update(task("u-done", "ctx-u", TaskState::Working)).await,
        error_code::INVALID_REQUEST,
        "task u-done is in terminal state Completed and cannot transition to Working",
    );
    assert_eq!(s.get("u-done").await.unwrap(), Some(done.clone()));
    let mut done_again = done.clone();
    done_again.status.message = Some(Message::new(Role::Agent, vec![Part::text("again")]));
    assert_eq!(s.update(done_again.clone()).await.unwrap(), 2);
    assert_eq!(s.get("u-done").await.unwrap(), Some(done_again));
}

async fn begin_cancel<S: TaskStore>(s: &S) {
    let working = task("c-1", "ctx-c", TaskState::Working);
    s.create(working.clone()).await.unwrap();
    let before = Utc::now();
    let canceled = s.begin_cancel("c-1").await.unwrap();
    let after = Utc::now();
    let stamped = canceled.status.timestamp.expect("a cancel timestamp");
    assert!(before <= stamped && stamped <= after, "{stamped}");
    // Only the state and its timestamp change.
    let mut expected = working.clone();
    expected.status.state = TaskState::Canceled;
    expected.status.timestamp = Some(stamped);
    assert_eq!(canceled, expected);
    assert_eq!(s.get("c-1").await.unwrap(), Some(canceled.clone()));
    // The cancel was version 2.
    assert_eq!(s.update(canceled).await.unwrap(), 3);

    assert_error(
        s.begin_cancel("c-1").await,
        error_code::TASK_NOT_CANCELABLE,
        "task cannot be canceled: c-1",
    );
    assert_error(
        s.begin_cancel("c-absent").await,
        error_code::TASK_NOT_FOUND,
        "task not found: c-absent",
    );

    // Every terminal state refuses; every other one cancels.
    for (state, terminal) in [
        (TaskState::Unspecified, false),
        (TaskState::Submitted, false),
        (TaskState::Working, false),
        (TaskState::InputRequired, false),
        (TaskState::AuthRequired, false),
        (TaskState::Completed, true),
        (TaskState::Failed, true),
        (TaskState::Canceled, true),
        (TaskState::Rejected, true),
    ] {
        let id = format!("c-{state:?}");
        let t = task(&id, "ctx-c", state);
        s.create(t.clone()).await.unwrap();
        let result = s.begin_cancel(&id).await;
        if terminal {
            assert_error(
                result,
                error_code::TASK_NOT_CANCELABLE,
                &format!("task cannot be canceled: {id}"),
            );
            assert_eq!(s.get(&id).await.unwrap(), Some(t));
        } else {
            assert_eq!(result.unwrap().status.state, TaskState::Canceled, "{id}");
        }
    }

    // Concurrent cancels: exactly one transition.
    let racing = task("c-race", "ctx-c", TaskState::Working);
    s.create(racing).await.unwrap();
    let results = join_all((0..8).map(|_| s.begin_cancel("c-race"))).await;
    let (won, lost): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);
    assert_eq!(won.len(), 1);
    for result in lost {
        assert_error(
            result,
            error_code::TASK_NOT_CANCELABLE,
            "task cannot be canceled: c-race",
        );
    }
    let raced = s.get("c-race").await.unwrap().unwrap();
    assert_eq!(s.update(raced).await.unwrap(), 3);
}

#[tokio::test]
async fn in_memory_passes_suite() {
    suite(InMemoryTaskStore::new()).await
}

#[tokio::test]
async fn sqlite_passes_suite() {
    suite(SqliteTaskStore::new(store())).await
}

#[tokio::test]
async fn tasks_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let owner = AgentKey::generate().id();
    let peer = AgentKey::generate().id();
    let working = task("r-working", "ctx-r", TaskState::Working);
    let mut done = task("r-done", "ctx-r", TaskState::Submitted);
    {
        let store = Arc::new(Store::open(&path, owner).unwrap());
        let tasks = SqliteTaskStore::new(Arc::clone(&store));
        tasks.create(working.clone()).await.unwrap();
        tasks.create(done.clone()).await.unwrap();
        done.status.state = TaskState::Completed;
        done.status.timestamp = Some(at(30));
        assert_eq!(tasks.update(done.clone()).await.unwrap(), 2);
        store.task_owner_put("r-working", &peer).unwrap();
    }

    let store = Arc::new(Store::open(&path, owner).unwrap());
    let tasks = SqliteTaskStore::new(Arc::clone(&store));
    assert_eq!(tasks.get("r-working").await.unwrap(), Some(working.clone()));
    assert_eq!(tasks.get("r-done").await.unwrap(), Some(done.clone()));
    assert_eq!(
        tasks.list(&req()).await.unwrap().tasks,
        [done.clone(), working.clone()]
    );
    assert_eq!(store.task_owner("r-working").unwrap(), Some(peer));
    assert_eq!(store.task_owner("r-done").unwrap(), None);

    // Versions go on from where they were, and a terminal task stays so.
    assert_eq!(tasks.update(done.clone()).await.unwrap(), 3);
    assert_eq!(tasks.update(working).await.unwrap(), 2);
    assert_error(
        tasks
            .update(task("r-done", "ctx-r", TaskState::Working))
            .await,
        error_code::INVALID_REQUEST,
        "task r-done is in terminal state Completed and cannot transition to Working",
    );
}

#[tokio::test]
async fn working_tasks_become_failed_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let owner = AgentKey::generate().id();
    let working = task("i-working", "ctx-i", TaskState::Working);
    let completed = task("i-completed", "ctx-i", TaskState::Completed);
    {
        let tasks = SqliteTaskStore::new(Arc::new(Store::open(&path, owner).unwrap()));
        tasks.create(working.clone()).await.unwrap();
        tasks.create(completed.clone()).await.unwrap();
    }

    // The daemon restarts.
    let store = Arc::new(Store::open(&path, owner).unwrap());
    assert_eq!(store.tasks_fail_interrupted(NOW).unwrap(), 1);
    let tasks = SqliteTaskStore::new(Arc::clone(&store));

    let failed = tasks.get("i-working").await.unwrap().unwrap();
    assert_eq!(failed.status.state, TaskState::Failed);
    assert_eq!(failed.status.timestamp, DateTime::from_timestamp(NOW, 0));
    let message = failed.status.message.as_ref().unwrap();
    assert_eq!(message.role, Role::Agent);
    assert_eq!(message.text(), Some(INTERRUPTED_MESSAGE));
    assert!(INTERRUPTED_MESSAGE.contains("interrupted"));
    assert_eq!(message.task_id.as_deref(), Some("i-working"));
    assert_eq!(message.context_id.as_deref(), Some("ctx-i"));
    // Only the status changed.
    let mut expected = working.clone();
    expected.status = failed.status.clone();
    assert_eq!(failed, expected);

    assert_eq!(tasks.get("i-completed").await.unwrap(), Some(completed));

    // Listing by state sees the new state.
    let in_state = |status: TaskState| ListTasksRequest {
        status: Some(status),
        ..req()
    };
    let listed = tasks.list(&in_state(TaskState::Failed)).await.unwrap();
    assert_eq!(ids(&listed), ["i-working"]);
    let listed = tasks.list(&in_state(TaskState::Working)).await.unwrap();
    assert!(listed.tasks.is_empty());

    // The failure was version 2; a second recovery finds nothing to do.
    assert_eq!(tasks.update(failed).await.unwrap(), 3);
    assert_eq!(store.tasks_fail_interrupted(NOW + 1).unwrap(), 0);
}

#[tokio::test]
async fn list_for_owner_never_returns_other_owners_tasks() {
    let store = store();
    let tasks = SqliteTaskStore::new(Arc::clone(&store));
    let alice = AgentKey::generate().id();
    let bob = AgentKey::generate().id();
    let carol = AgentKey::generate().id();
    for (id, context_id, state, owner) in [
        ("o-1", "ctx-1", TaskState::Working, alice),
        ("o-2", "ctx-1", TaskState::Working, bob),
        ("o-3", "ctx-2", TaskState::Completed, alice),
        ("o-4", "ctx-1", TaskState::Completed, bob),
        ("o-5", "ctx-1", TaskState::Working, alice),
        ("o-6", "ctx-2", TaskState::Working, bob),
    ] {
        tasks.create(task(id, context_id, state)).await.unwrap();
        store.task_owner_put(id, &owner).unwrap();
    }
    // A task nobody owns.
    tasks
        .create(task("o-7", "ctx-1", TaskState::Working))
        .await
        .unwrap();
    assert_eq!(tasks.list(&req()).await.unwrap().total_size, 7);

    let for_owner = |owner: AgentId, r: ListTasksRequest| {
        let resp = store.tasks_list_for_owner(&owner, &r).unwrap();
        for t in &resp.tasks {
            assert_eq!(store.task_owner(&t.id).unwrap(), Some(owner), "{}", t.id);
        }
        resp
    };

    let resp = for_owner(alice, req());
    assert_eq!(ids(&resp), ["o-1", "o-3", "o-5"]);
    assert_eq!((resp.total_size, resp.next_page_token.as_str()), (3, ""));
    assert_eq!(
        Some(&resp.tasks[0]),
        tasks.get("o-1").await.unwrap().as_ref()
    );
    assert_eq!(ids(&for_owner(bob, req())), ["o-2", "o-4", "o-6"]);
    let resp = for_owner(carol, req());
    assert_eq!((resp.tasks.len(), resp.total_size), (0, 0));

    // Filters apply within the owner's tasks.
    let resp = for_owner(
        alice,
        ListTasksRequest {
            context_id: Some("ctx-1".to_owned()),
            ..req()
        },
    );
    assert_eq!(ids(&resp), ["o-1", "o-5"]);
    let resp = for_owner(
        bob,
        ListTasksRequest {
            context_id: Some("ctx-1".to_owned()),
            status: Some(TaskState::Working),
            ..req()
        },
    );
    assert_eq!(ids(&resp), ["o-2"]);
    let resp = for_owner(
        alice,
        ListTasksRequest {
            history_length: Some(0),
            ..req()
        },
    );
    assert!(resp.tasks.iter().all(|t| t.history == Some(Vec::new())));

    // Pages walk the owner's tasks only, whatever token is presented.
    let paged = ListTasksRequest {
        page_size: Some(1),
        ..req()
    };
    assert_eq!(
        walk(|r| ready(for_owner(alice, r)), paged.clone(), 3).await,
        [["o-1"], ["o-3"], ["o-5"]]
    );
    assert_eq!(
        walk(|r| ready(for_owner(bob, r)), paged, 3).await,
        [["o-2"], ["o-4"], ["o-6"]]
    );
    let resp = for_owner(
        alice,
        ListTasksRequest {
            page_token: Some("2".to_owned()),
            ..req()
        },
    );
    assert_eq!(ids(&resp), ["o-5"]);

    // A bad token is the A2A error the SDK store gives.
    let err = store
        .tasks_list_for_owner(
            &alice,
            &ListTasksRequest {
                page_token: Some("not-a-number".to_owned()),
                ..req()
            },
        )
        .unwrap_err();
    let err = err.downcast_ref::<A2AError>().expect("an A2A error");
    assert_eq!(err.code, error_code::INVALID_PARAMS);
}

#[tokio::test]
async fn task_events_are_logged_once_per_task() {
    let store = store();
    let key = Arc::new(AgentKey::generate());
    let tasks = SqliteTaskStore::new(Arc::clone(&store)).with_events(Arc::clone(&key));
    let count = |kind: &str| store.events_count(kind).unwrap();

    // Created, worked on, completed, and the completion written again.
    tasks
        .create(task("e-1", "ctx", TaskState::Submitted))
        .await
        .unwrap();
    assert_eq!((count("task.accepted"), count("task.terminal")), (1, 0));
    tasks
        .update(task("e-1", "ctx", TaskState::Working))
        .await
        .unwrap();
    tasks
        .update(task("e-1", "ctx", TaskState::Working))
        .await
        .unwrap();
    assert_eq!((count("task.accepted"), count("task.terminal")), (1, 0));
    tasks
        .update(task("e-1", "ctx", TaskState::Completed))
        .await
        .unwrap();
    tasks
        .update(task("e-1", "ctx", TaskState::Completed))
        .await
        .unwrap();
    assert_eq!((count("task.accepted"), count("task.terminal")), (1, 1));
    // A refused transition out of a terminal state logs nothing.
    tasks
        .update(task("e-1", "ctx", TaskState::Working))
        .await
        .unwrap_err();
    assert_eq!((count("task.accepted"), count("task.terminal")), (1, 1));

    // Canceled: the cancel is the terminal transition; the executor's
    // CANCELED that follows is not another one.
    tasks
        .create(task("e-2", "ctx", TaskState::Submitted))
        .await
        .unwrap();
    tasks
        .update(task("e-2", "ctx", TaskState::Working))
        .await
        .unwrap();
    let canceled = tasks.begin_cancel("e-2").await.unwrap();
    tasks.update(canceled).await.unwrap();
    tasks.begin_cancel("e-2").await.unwrap_err();
    assert_eq!((count("task.accepted"), count("task.terminal")), (2, 2));

    // Created already terminal: both at once.
    tasks
        .create(task("e-3", "ctx", TaskState::Failed))
        .await
        .unwrap();
    assert_eq!((count("task.accepted"), count("task.terminal")), (3, 3));
    // A duplicate create is refused and logs nothing.
    tasks
        .create(task("e-3", "ctx", TaskState::Submitted))
        .await
        .unwrap_err();
    assert_eq!((count("task.accepted"), count("task.terminal")), (3, 3));

    // Every event is the key's, chained, with ids and states only.
    assert_eq!(store.events_verify(&key.id()).unwrap(), 6);
    let events = store.events_since(&key.id(), 0).unwrap();
    let logged: Vec<(&str, &serde_json::Value)> = events
        .iter()
        .map(|e| (e.kind.as_str(), &e.payload))
        .collect();
    let payload =
        |id: &str, state: &str| json!({"task_id": id, "context_id": "ctx", "state": state});
    assert_eq!(
        logged,
        [
            ("task.accepted", &payload("e-1", "TASK_STATE_SUBMITTED")),
            ("task.terminal", &payload("e-1", "TASK_STATE_COMPLETED")),
            ("task.accepted", &payload("e-2", "TASK_STATE_SUBMITTED")),
            ("task.terminal", &payload("e-2", "TASK_STATE_CANCELED")),
            ("task.accepted", &payload("e-3", "TASK_STATE_FAILED")),
            ("task.terminal", &payload("e-3", "TASK_STATE_FAILED")),
        ]
    );

    // Without an event key the store logs nothing.
    let quiet = SqliteTaskStore::new(Arc::clone(&store));
    quiet
        .create(task("e-4", "ctx", TaskState::Completed))
        .await
        .unwrap();
    assert_eq!((count("task.accepted"), count("task.terminal")), (3, 3));
}
