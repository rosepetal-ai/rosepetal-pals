//! The `tck` adapter: an executor that plays the scenarios of the A2A
//! Technology Compatibility Kit, so that the TCK can be run against a PALS
//! agent. Selected only by `worker.adapter = "tck"`; it answers nothing
//! else.
//!
//! The scenarios are those of `scenarios/core_operations.feature` and
//! `scenarios/streaming.feature` of `a2aproject/a2a-tck` at commit
//! `104a7b4ae0d8`. The TCK picks one with the prefix of the `messageId` it
//! sends; the longest matching prefix wins, so `tck-artifact-file-url-…`
//! plays `tck-artifact-file-url`, not `tck-artifact-file`. Any other
//! `messageId` completes the task with the message `Hello from TCK`.

use std::time::Duration;

use a2a::{
    A2AError, Artifact, Message, Part, Role, StreamResponse, Task, TaskArtifactUpdateEvent,
    TaskState, TaskStatus, new_artifact_id,
};
use a2a_server::{AgentExecutor, ExecutorContext};
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::json;
use tracing::warn;

use super::{status_update, working_task};
use Step::{Answer, Chunk, LastChunk, Status, Wait};
use TaskState::{Completed, InputRequired, Rejected, Working};

/// The environment variable the TCK's streaming timeout is read from, in
/// seconds, as in the TCK's `docs/SUT_REQUIREMENTS.md`.
pub const STREAMING_TIMEOUT_ENV: &str = "TCK_STREAMING_TIMEOUT";

/// The TCK's streaming timeout when [`STREAMING_TIMEOUT_ENV`] is not set.
pub const DEFAULT_STREAMING_TIMEOUT: Duration = Duration::from_secs(2);

type Events = BoxStream<'static, Result<StreamResponse, A2AError>>;

/// What one step of a scenario does.
#[derive(Debug)]
enum Step {
    /// The task moves to the state, with the text as its status message.
    Status(TaskState, Option<&'static str>),
    /// A new artifact, complete, with one part.
    Artifact(Content),
    /// The first chunk of the scenario's one chunked artifact: it creates
    /// the artifact.
    Chunk(&'static str),
    /// The last chunk of that artifact, appended to it.
    LastChunk(&'static str),
    /// Waits twice the streaming timeout.
    Wait,
    /// The agent answers with a message, and no task.
    Answer(&'static str),
}

/// The part an artifact holds.
#[derive(Debug, Clone, Copy)]
enum Content {
    Text(&'static str),
    /// `output.txt`, `text/plain`, with its bytes (`tck`) inline.
    File,
    /// `output.txt`, `text/plain`, at `https://example.com/output.txt`.
    FileUrl,
    /// `{"key": "value", "count": 42}`.
    Data,
}

/// The message a task completes with when the scenario does not say
/// otherwise.
const HELLO: &str = "Hello from TCK";

/// What the default scenario does: complete the task with [`HELLO`].
const COMPLETE: &[Step] = &[Status(Completed, Some(HELLO))];

/// Every scenario, by the `messageId` prefix that plays it, with its steps
/// in order, as the feature files word them.
const SCENARIOS: &[(&str, &[Step])] = &[
    // core_operations.feature
    ("tck-complete-task", COMPLETE),
    (
        "tck-artifact-text",
        &[
            Step::Artifact(Content::Text("Generated text content")),
            Status(Completed, None),
        ],
    ),
    (
        "tck-artifact-file",
        &[Step::Artifact(Content::File), Status(Completed, None)],
    ),
    (
        "tck-artifact-file-url",
        &[Step::Artifact(Content::FileUrl), Status(Completed, None)],
    ),
    (
        "tck-artifact-data",
        &[Step::Artifact(Content::Data), Status(Completed, None)],
    ),
    ("tck-message-response", &[Answer("Direct message response")]),
    ("tck-input-required", &[Status(InputRequired, None)]),
    ("tck-reject-task", &[Status(Rejected, Some("rejected"))]),
    // streaming.feature
    (
        "tck-stream-001",
        &[
            Status(Working, None),
            Step::Artifact(Content::Text("Stream hello from TCK")),
            Status(Completed, None),
        ],
    ),
    ("tck-stream-002", &[Status(Completed, None)]),
    (
        "tck-stream-003",
        &[
            Status(Working, None),
            Step::Artifact(Content::Text("Stream task lifecycle")),
            Status(Completed, None),
        ],
    ),
    (
        "tck-stream-ordering-001",
        &[
            Status(Working, None),
            Step::Artifact(Content::Text("Ordered output")),
            Status(Completed, None),
        ],
    ),
    (
        "tck-stream-artifact-text",
        &[
            Status(Working, None),
            Step::Artifact(Content::Text("Streamed text content")),
            Status(Completed, None),
        ],
    ),
    (
        "tck-stream-artifact-file",
        &[
            Status(Working, None),
            Step::Artifact(Content::File),
            Status(Completed, None),
        ],
    ),
    (
        "tck-stream-artifact-chunked",
        &[
            Status(Working, None),
            Chunk("chunk-1 "),
            LastChunk("chunk-2"),
            Status(Completed, None),
        ],
    ),
    (
        "test-resubscribe-message-id",
        &[Status(Working, None), Wait, Status(Completed, None)],
    ),
];

/// The prefix and the steps of the scenario `message_id` plays: the one
/// with the longest prefix of it; `None` when no prefix matches.
fn scenario(message_id: &str) -> Option<(&'static str, &'static [Step])> {
    SCENARIOS
        .iter()
        .filter(|(prefix, _)| message_id.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .copied()
}

/// The streaming timeout `value`, the text of [`STREAMING_TIMEOUT_ENV`],
/// sets: seconds, fractions allowed. [`DEFAULT_STREAMING_TIMEOUT`] when
/// it is not set, and, with a warning, when it is not a number of seconds.
pub fn streaming_timeout(value: Option<&str>) -> Duration {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return DEFAULT_STREAMING_TIMEOUT;
    };
    match text.parse::<f64>().map(Duration::try_from_secs_f64) {
        Ok(Ok(timeout)) => timeout,
        _ => {
            warn!(
                value = text,
                "{STREAMING_TIMEOUT_ENV} is not a number of seconds; using the default"
            );
            DEFAULT_STREAMING_TIMEOUT
        }
    }
}

/// Plays the TCK scenario that the `messageId` of each message names.
///
/// A scenario that answers with a message streams that message alone: an
/// agent `Message` with one text part, in the request's context, naming no
/// task. Every other scenario streams the task first, as it is stored, with
/// the message being answered in its history (`SUBMITTED` for a new task),
/// and then, in order, one event per step of the scenario: status updates,
/// each with the scenario's text as an agent status message when it has
/// one, and artifact updates, each a new artifact with one part. The two
/// chunks of `tck-stream-artifact-chunked` are one artifact: the first
/// creates it, the last is appended to it (`append` and `lastChunk` true).
/// The wait of
/// `test-resubscribe-message-id` keeps the task `WORKING` for twice the
/// streaming timeout before it completes.
///
/// Whether the message came with `SendMessage` or `SendStreamingMessage`
/// makes no difference.
///
/// `cancel` reports the task `CANCELED`.
#[derive(Debug, Clone, Copy)]
pub struct TckExecutor {
    streaming_timeout: Duration,
}

impl TckExecutor {
    /// An executor whose streaming timeout is `streaming_timeout`.
    pub fn new(streaming_timeout: Duration) -> Self {
        Self { streaming_timeout }
    }

    /// An executor with the streaming timeout that [`STREAMING_TIMEOUT_ENV`]
    /// sets, see [`streaming_timeout`].
    pub fn from_env() -> Self {
        let value = std::env::var(STREAMING_TIMEOUT_ENV).ok();
        Self::new(streaming_timeout(value.as_deref()))
    }
}

impl AgentExecutor for TckExecutor {
    fn execute(&self, ctx: ExecutorContext) -> Events {
        let message_id = ctx.message.as_ref().map(|m| m.message_id.as_str());
        let steps = scenario(message_id.unwrap_or_default()).map_or(COMPLETE, |(_, steps)| steps);
        if let [Answer(text)] = steps {
            let mut answer = Message::new(Role::Agent, vec![Part::text(*text)]);
            answer.context_id = Some(ctx.context_id.clone());
            return Box::pin(stream::iter([Ok(StreamResponse::Message(answer))]));
        }

        let first = StreamResponse::Task(current_task(&ctx));
        let (task_id, context_id) = ctx.task_info();
        let chunked_id = new_artifact_id();
        let wait = self
            .streaming_timeout
            .checked_mul(2)
            .unwrap_or(Duration::MAX);
        let rest = stream::iter(steps).filter_map(move |step| {
            let (task_id, context_id) = (task_id.clone(), context_id.clone());
            let chunked_id = chunked_id.clone();
            async move {
                let artifact = |artifact_id: String,
                                part: Part,
                                append: Option<bool>,
                                last_chunk: Option<bool>| {
                    StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
                        task_id: task_id.clone(),
                        context_id: context_id.clone(),
                        artifact: Artifact {
                            artifact_id,
                            name: None,
                            description: None,
                            parts: vec![part],
                            metadata: None,
                            extensions: None,
                        },
                        append,
                        last_chunk,
                        metadata: None,
                    })
                };
                let event = match step {
                    Status(state, text) => status_update(
                        task_id.clone(),
                        context_id.clone(),
                        state.clone(),
                        text.map(str::to_owned),
                    ),
                    Step::Artifact(content) => {
                        artifact(new_artifact_id(), part(*content), None, None)
                    }
                    Chunk(text) => artifact(chunked_id, Part::text(*text), None, Some(false)),
                    LastChunk(text) => {
                        artifact(chunked_id, Part::text(*text), Some(true), Some(true))
                    }
                    Wait => {
                        tokio::time::sleep(wait).await;
                        return None;
                    }
                    Answer(_) => unreachable!("an answer is the scenario's only step"),
                };
                Some(Ok(event))
            }
        });
        Box::pin(stream::iter([Ok(first)]).chain(rest))
    }

    fn cancel(&self, ctx: ExecutorContext) -> Events {
        let (task_id, context_id) = ctx.task_info();
        let canceled = status_update(task_id, context_id, TaskState::Canceled, None);
        Box::pin(stream::iter([Ok(canceled)]))
    }
}

/// The task as it is stored, with the message being answered in its
/// history; a new task is `SUBMITTED`.
fn current_task(ctx: &ExecutorContext) -> Task {
    let mut task = working_task(ctx);
    task.status = match &ctx.stored_task {
        Some(stored) => stored.status.clone(),
        None => TaskStatus {
            state: TaskState::Submitted,
            message: None,
            timestamp: None,
        },
    };
    task
}

/// The part that holds `content`.
fn part(content: Content) -> Part {
    match content {
        Content::Text(text) => Part::text(text),
        Content::File => Part::raw(b"tck".to_vec())
            .with_filename("output.txt")
            .with_media_type("text/plain"),
        Content::FileUrl => Part::url("https://example.com/output.txt")
            .with_filename("output.txt")
            .with_media_type("text/plain"),
        Content::Data => Part::data(json!({"key": "value", "count": 42})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_prefix_names_the_scenario() {
        let prefix = |message_id: &str| scenario(message_id).map(|(prefix, _)| prefix);
        assert_eq!(
            prefix("tck-artifact-file-url-1"),
            Some("tck-artifact-file-url")
        );
        assert_eq!(prefix("tck-artifact-file-1"), Some("tck-artifact-file"));
        assert_eq!(
            prefix("tck-stream-artifact-file-1"),
            Some("tck-stream-artifact-file")
        );
        assert_eq!(
            prefix("test-resubscribe-message-id-abc"),
            Some("test-resubscribe-message-id")
        );
        // No scenario: the default, which completes the task.
        for other in ["tck-send-001-x", "", "TCK-ARTIFACT-TEXT", "tck-artifact"] {
            assert_eq!(prefix(other), None, "{other}");
        }
    }

    #[test]
    fn every_prefix_is_distinct() {
        let mut prefixes: Vec<&str> = SCENARIOS.iter().map(|(prefix, _)| *prefix).collect();
        prefixes.sort_unstable();
        prefixes.dedup();
        assert_eq!(prefixes.len(), SCENARIOS.len());
    }

    #[test]
    fn streaming_timeout_is_seconds_with_a_default() {
        assert_eq!(streaming_timeout(None), DEFAULT_STREAMING_TIMEOUT);
        assert_eq!(DEFAULT_STREAMING_TIMEOUT, Duration::from_secs(2));
        assert_eq!(streaming_timeout(Some("")), DEFAULT_STREAMING_TIMEOUT);
        assert_eq!(streaming_timeout(Some("0.2")), Duration::from_millis(200));
        assert_eq!(streaming_timeout(Some(" 5 ")), Duration::from_secs(5));
        assert_eq!(streaming_timeout(Some("30.0")), Duration::from_secs(30));
        for invalid in ["fast", "-1", "NaN", "inf", "1e400"] {
            assert_eq!(
                streaming_timeout(Some(invalid)),
                DEFAULT_STREAMING_TIMEOUT,
                "{invalid}"
            );
        }
    }
}
