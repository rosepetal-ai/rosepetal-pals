//! An executor that answers with the text it receives.

use a2a::{
    A2AError, Artifact, Message, Part, StreamResponse, Task, TaskArtifactUpdateEvent, TaskState,
    TaskStatus, TaskStatusUpdateEvent, new_artifact_id,
};
use a2a_server::{AgentExecutor, ExecutorContext};
use futures::stream::{self, BoxStream};

/// Completes every task with one text artifact, `echo: <received text>`.
///
/// The events, in order: the task in `WORKING`, the artifact, and the
/// `COMPLETED` status.
#[derive(Debug, Clone, Copy, Default)]
pub struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(
        &self,
        ctx: ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let text = ctx.message.as_ref().map(received_text).unwrap_or_default();
        let artifact = TaskArtifactUpdateEvent {
            task_id: ctx.task_id.clone(),
            context_id: ctx.context_id.clone(),
            artifact: Artifact {
                artifact_id: new_artifact_id(),
                name: Some("echo".to_owned()),
                description: None,
                parts: vec![Part::text(format!("echo: {text}"))],
                metadata: None,
                extensions: None,
            },
            append: None,
            last_chunk: Some(true),
            metadata: None,
        };
        let events = [
            StreamResponse::Task(working_task(&ctx)),
            StreamResponse::ArtifactUpdate(artifact),
            status_update(&ctx, TaskState::Completed),
        ];
        Box::pin(stream::iter(events.map(Ok)))
    }

    fn cancel(&self, ctx: ExecutorContext) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        Box::pin(stream::once(async move {
            Ok(status_update(&ctx, TaskState::Canceled))
        }))
    }
}

/// The text parts of `message`, one per line.
fn received_text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(Part::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The task as the stream's first event. A task event replaces the stored
/// task, so it carries the history, including the message being answered.
fn working_task(ctx: &ExecutorContext) -> Task {
    let stored = ctx.stored_task.as_ref();
    let mut history = stored
        .and_then(|task| task.history.clone())
        .unwrap_or_default();
    if let Some(message) = &ctx.message
        && !history.iter().any(|m| m.message_id == message.message_id)
    {
        history.push(message.clone());
    }
    Task {
        id: ctx.task_id.clone(),
        context_id: ctx.context_id.clone(),
        status: TaskStatus {
            state: TaskState::Working,
            message: None,
            timestamp: None,
        },
        artifacts: stored.and_then(|task| task.artifacts.clone()),
        history: Some(history),
        metadata: stored.and_then(|task| task.metadata.clone()),
    }
}

fn status_update(ctx: &ExecutorContext, state: TaskState) -> StreamResponse {
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: ctx.task_id.clone(),
        context_id: ctx.context_id.clone(),
        status: TaskStatus {
            state,
            message: None,
            timestamp: None,
        },
        metadata: None,
    })
}
