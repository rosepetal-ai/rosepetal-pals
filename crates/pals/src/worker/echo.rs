//! An executor that answers with the text it receives.

use a2a::{
    A2AError, Artifact, Part, StreamResponse, TaskArtifactUpdateEvent, TaskState, new_artifact_id,
};
use a2a_server::{AgentExecutor, ExecutorContext};
use futures::stream::{self, BoxStream};

use super::{message_text, status_update, working_task};

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
        let text = ctx.message.as_ref().map(message_text).unwrap_or_default();
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
        let (task_id, context_id) = ctx.task_info();
        let events = [
            StreamResponse::Task(working_task(&ctx)),
            StreamResponse::ArtifactUpdate(artifact),
            status_update(task_id, context_id, TaskState::Completed, None),
        ];
        Box::pin(stream::iter(events.map(Ok)))
    }

    fn cancel(&self, ctx: ExecutorContext) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let (task_id, context_id) = ctx.task_info();
        Box::pin(stream::once(async move {
            Ok(status_update(
                task_id,
                context_id,
                TaskState::Canceled,
                None,
            ))
        }))
    }
}
