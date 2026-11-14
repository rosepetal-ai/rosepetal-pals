//! Executors that do the work behind the A2A server: [`echo`], and
//! [`WorkerExecutor`], which answers each task by running an [`Adapter`],
//! such as the command of [`exec`].

pub mod echo;
#[cfg(unix)]
pub mod exec;

use std::collections::{HashMap, HashSet};
use std::io;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use a2a::{
    A2AError, Artifact, Message, Part, PartContent, Role, StreamResponse, Task,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent, new_artifact_id,
};
use a2a_server::{AgentExecutor, ExecutorContext, ServiceParams};
use async_trait::async_trait;
use futures::FutureExt;
use futures::stream::{self, BoxStream, StreamExt};
use iroh_blobs::Hash;
use pals_core::identity::AgentId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::a2a::auth::PROJECT_HEADER;
use crate::a2a::is_safe_task_id;
use crate::blobs::{BLOB_SCHEME, BLOB_SHARE_TTL_S, BlobError, BlobRef, BlobService};
use crate::config::{Config, ExecCfg};
use crate::net::PEER_HEADER;

/// The directory, in the data directory, that holds the working directories
/// of the tasks in progress, one per task: `tasks/<task id>/{in,out}`.
pub const TASKS_DIR: &str = "tasks";

/// The name of the artifact that holds the answer.
pub const ANSWER_ARTIFACT: &str = "answer";

/// The most files a question may come with.
pub const MAX_INPUT_BLOBS: usize = 32;

/// How long [`WorkerExecutor`]'s `cancel` waits for the run it stops to be
/// over before it reports the task `CANCELED` anyway.
const CANCEL_WAIT: Duration = Duration::from_secs(10);

/// The media type of the files an answer comes with: the worker does not
/// guess what they hold.
const FILE_MEDIA_TYPE: &str = "application/octet-stream";

type Events = BoxStream<'static, Result<StreamResponse, A2AError>>;

/// What an [`Adapter`] gets to answer one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInput {
    pub task_id: String,
    /// The peer that asked.
    pub peer: AgentId,
    /// The project of the grant the peer asked with.
    pub project: String,
    /// The text parts of the message, one per line.
    pub text: String,
    /// A new directory that holds the files the question comes with, and
    /// nothing else.
    pub in_dir: PathBuf,
    /// A new, empty directory for files to answer with.
    pub out_dir: PathBuf,
}

/// An adapter's answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterOutput {
    pub text: String,
    /// Files to answer with, each sent as a blob that the requester may
    /// fetch, named after its last path component.
    pub files: Vec<PathBuf>,
}

/// Why an adapter has no answer. The reason becomes the status message of
/// the task, which the peer reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// The task ran and failed: it ends `FAILED`.
    #[error("{0}")]
    Failed(String),
    /// The task was not run: it ends `REJECTED`.
    #[error("{0}")]
    Rejected(String),
}

/// The runtime that answers the questions peers ask.
#[async_trait]
pub trait Adapter: Send + Sync + 'static {
    /// Answers `input`. Once `cancel` fires the answer is no longer wanted:
    /// the run stops as soon as it can, leaving nothing running behind it,
    /// and returns whatever it likes.
    async fn run(
        &self,
        input: TaskInput,
        cancel: CancellationToken,
    ) -> Result<AdapterOutput, AdapterError>;
}

/// How many tasks a [`WorkerExecutor`] takes at once: `max_concurrent`
/// running their adapter, `max_queue` more waiting for their turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub max_concurrent: usize,
    pub max_queue: usize,
}

impl From<&ExecCfg> for Capacity {
    fn from(cfg: &ExecCfg) -> Self {
        Self {
            max_concurrent: cfg.max_concurrent,
            max_queue: cfg.max_queue,
        }
    }
}

/// Answers every task by running an [`Adapter`], and always with a task.
///
/// The events, in order: the task in `WORKING`; then the `answer` artifact,
/// with the adapter's text and then one file part per file it answered
/// with, and `COMPLETED`; or, instead of both, `FAILED` or `REJECTED` (per
/// [`AdapterError`]) with the reason as the status message. A task beyond
/// the [`Capacity`] is `REJECTED` at once as `busy`.
///
/// Each run gets a new `<data_dir>/tasks/<task id>/in` and `out`
/// ([`TASKS_DIR`]), removed when the run ends, whatever its outcome.
///
/// Files come and go as blobs ([`crate::blobs`]). Before the adapter runs,
/// each file part of the message being answered whose URL is an
/// `iroh-blob://` one is fetched into `in`, and any that cannot be fails the
/// task, with the adapter never run:
///
/// | Rule | Reason the task fails with |
/// |-|-|
/// | At most [`MAX_INPUT_BLOBS`] such parts, checked before fetching | `too many input blobs` |
/// | The part declares its blob's hash and size ([`BlobRef::from_part`]) | `invalid blob reference` |
/// | The blob's holder is the peer that asks: no one else's files are fetched on its behalf | `blob holder must be the requester` |
/// | The declared size is at most `limits.max_blob_bytes`, checked before fetching; the download stops if the blob turns out larger than declared | `blob too large` |
/// | The declared sizes add up to at most `limits.max_blob_bytes`, checked before fetching | `inputs too large` |
/// | All the downloads end within `limits.blob_fetch_timeout_s`, together | `input fetch timed out` |
/// | The holder lets it be fetched, can be reached, and holds it: a copy this agent stores already is used only once the holder proves it holds it too ([`BlobService::fetch`]) | `blob fetch denied by its holder`, `blob holder unreachable`, `blob fetch failed` |
/// | The file's name is the last component of the part's `filename` (the hash, without one), neither empty, `.` nor `..`, and no other file's | `invalid file name`, `duplicate file name` |
///
/// The store holds a task's inputs from before their download until they
/// are in `in`, whatever the outcome; then its garbage collection may
/// remove them.
///
/// After the adapter, each file it answered with is added to the agent's
/// store and [shared](BlobService::share) with the peer for
/// [`BLOB_SHARE_TTL_S`], and becomes a file part
/// ([`BlobRef::to_part`], `application/octet-stream`).
///
/// `cancel` stops the task's run, through the [`CancellationToken`] the
/// adapter got, and waits up to 10 s for the run to be over,
/// working directories removed and place freed, before it reports the task
/// `CANCELED`. A canceled run adds nothing to the task: the SDK has made it
/// `CANCELED` already, and that stays. A run whose events are dropped before
/// it ends is stopped the same way.
pub struct WorkerExecutor {
    adapter: Arc<dyn Adapter>,
    tasks_dir: PathBuf,
    /// One permit per task taken, running or waiting.
    taken: Arc<Semaphore>,
    /// One permit per running adapter.
    running: Arc<Semaphore>,
    runs: Arc<Runs>,
    files: Files,
}

/// How a run gets and sends files.
#[derive(Clone)]
struct Files {
    blobs: BlobService,
    max_blob_bytes: u64,
    fetch_timeout: Duration,
}

impl WorkerExecutor {
    /// An executor running `adapter` for the agent of `cfg`, whose data
    /// directory holds the tasks' working directories, taking `capacity`
    /// tasks at once, with the files of its tasks in `blobs`.
    pub fn new(
        adapter: Arc<dyn Adapter>,
        cfg: &Config,
        capacity: Capacity,
        blobs: BlobService,
    ) -> Self {
        Self {
            adapter,
            tasks_dir: cfg.data_dir.join(TASKS_DIR),
            taken: Arc::new(Semaphore::new(
                capacity.max_concurrent.saturating_add(capacity.max_queue),
            )),
            running: Arc::new(Semaphore::new(capacity.max_concurrent)),
            runs: Arc::default(),
            files: Files {
                blobs,
                max_blob_bytes: cfg.limits.max_blob_bytes,
                fetch_timeout: Duration::from_secs(cfg.limits.blob_fetch_timeout_s),
            },
        }
    }

    /// The adapter's input for the execution `ctx`, without its directories
    /// made yet, and the files it comes with.
    ///
    /// Only the message being answered is read. The worker never loads
    /// other tasks, the history of the context or the `referenceTaskIds` of
    /// the message: tasks belong to the peer that created them (see
    /// [`GuardedHandler`](crate::a2a::GuardedHandler)), and a context id or
    /// a task id is just a string any peer can send, so following one would
    /// hand another peer's work to this one.
    fn input(&self, ctx: &ExecutorContext) -> Result<(TaskInput, Vec<InputFile>), AdapterError> {
        // Only such an id names a directory as it is.
        if !is_safe_task_id(&ctx.task_id) {
            error!(task_id = %ctx.task_id, "a task id unfit for a directory name");
            return Err(AdapterError::Failed("invalid task id".to_owned()));
        }
        let Some(peer) = single(&ctx.service_params, PEER_HEADER).and_then(|p| p.parse().ok())
        else {
            error!(task_id = %ctx.task_id, "an execution without an authenticated peer");
            return Err(AdapterError::Failed("Internal error".to_owned()));
        };
        // The grant's project; without one, a project no adapter knows.
        let project = single(&ctx.service_params, PROJECT_HEADER).unwrap_or_default();
        let text = ctx.message.as_ref().map(message_text).unwrap_or_default();
        let inputs = match &ctx.message {
            Some(message) => input_files(message, &peer, self.files.max_blob_bytes)?,
            None => Vec::new(),
        };
        let task_dir = self.tasks_dir.join(&ctx.task_id);
        let input = TaskInput {
            task_id: ctx.task_id.clone(),
            peer,
            project: project.to_owned(),
            text,
            in_dir: task_dir.join("in"),
            out_dir: task_dir.join("out"),
        };
        Ok((input, inputs))
    }
}

/// A file a question comes with: a blob of the requester's, and the name it
/// gets in `in_dir`.
struct InputFile {
    blob: BlobRef,
    name: String,
}

/// The files `message`, from `peer`, comes with: one per part whose URL is
/// an `iroh-blob://` one, checked as [`WorkerExecutor`] says.
fn input_files(
    message: &Message,
    peer: &AgentId,
    max_blob_bytes: u64,
) -> Result<Vec<InputFile>, AdapterError> {
    let failed = |reason: &str| AdapterError::Failed(reason.to_owned());
    let blob_parts: Vec<&Part> = message
        .parts
        .iter()
        .filter(|part| matches!(&part.content, PartContent::Url(url) if is_blob_url(url)))
        .collect();
    if blob_parts.len() > MAX_INPUT_BLOBS {
        return Err(failed("too many input blobs"));
    }
    let mut files = Vec::new();
    let mut names = HashSet::new();
    let mut total: u64 = 0;
    for part in blob_parts {
        let blob = BlobRef::from_part(part).ok_or_else(|| failed("invalid blob reference"))?;
        if blob.holder != *peer {
            return Err(failed("blob holder must be the requester"));
        }
        if blob.size > max_blob_bytes {
            return Err(failed("blob too large"));
        }
        let name = match &part.filename {
            Some(filename) => {
                safe_file_name(filename).ok_or_else(|| failed("invalid file name"))?
            }
            None => blob.hash.to_hex(),
        };
        if !names.insert(name.clone()) {
            return Err(failed("duplicate file name"));
        }
        total = total.saturating_add(blob.size);
        files.push(InputFile { blob, name });
    }
    if total > max_blob_bytes {
        return Err(failed("inputs too large"));
    }
    Ok(files)
}

/// Whether `url` has the blob scheme, in any case.
fn is_blob_url(url: &str) -> bool {
    url.split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case(BLOB_SCHEME))
}

/// The name a file named `filename` by a peer gets in `in_dir`: its last
/// path component, with `/` or `\` as separator; `None` when that is empty,
/// `.` or `..`, or holds a NUL.
fn safe_file_name(filename: &str) -> Option<String> {
    let last = filename.rsplit(['/', '\\']).next().unwrap_or_default();
    let unfit = last.is_empty() || last == "." || last == ".." || last.contains('\0');
    (!unfit).then(|| last.to_owned())
}

impl AgentExecutor for WorkerExecutor {
    fn execute(&self, ctx: ExecutorContext) -> Events {
        let working = StreamResponse::Task(working_task(&ctx));
        let (task_id, context_id) = ctx.task_info();
        let end = move |outcome: Result<Answer, AdapterError>| {
            outcome_events(task_id, context_id, outcome)
        };
        let (input, inputs) = match self.input(&ctx) {
            Ok(input) => input,
            Err(err) => return events(working, end(Err(err))),
        };
        // Taken now or never: a task that finds no place is not queued.
        let Ok(taken) = Arc::clone(&self.taken).try_acquire_owned() else {
            return events(working, end(Err(AdapterError::Rejected("busy".to_owned()))));
        };

        let run = Arc::new(Run::default());
        self.runs.insert(&input.task_id, &run);
        let finished = tokio::spawn(run_task(
            Arc::clone(&self.adapter),
            Arc::clone(&self.running),
            RunSlot {
                task_id: input.task_id.clone(),
                taken: Some(taken),
                runs: Arc::clone(&self.runs),
                run: Arc::clone(&run),
            },
            input,
            (self.files.clone(), inputs),
        ));
        // Should these events be dropped before the run ends, the run stops.
        let stop_if_dropped = run.cancel.clone().drop_guard();
        let rest = stream::once(async move {
            let outcome = finished.await;
            let cancel = stop_if_dropped.disarm();
            if cancel.is_cancelled() {
                // `CancelTask`: the SDK has made the task `CANCELED`.
                return Vec::new();
            }
            end(outcome.unwrap_or_else(|err| {
                error!(error = %err, "a worker run ended abnormally");
                Err(AdapterError::Failed("the worker crashed".to_owned()))
            }))
        })
        .flat_map(|events| stream::iter(events.into_iter().map(Ok)));
        Box::pin(stream::iter([Ok(working)]).chain(rest))
    }

    fn cancel(&self, ctx: ExecutorContext) -> Events {
        let run = self.runs.get(&ctx.task_id);
        if let Some(run) = &run {
            run.cancel.cancel();
        }
        let (task_id, context_id) = ctx.task_info();
        Box::pin(stream::once(async move {
            if let Some(run) = run
                && tokio::time::timeout(CANCEL_WAIT, run.done.cancelled())
                    .await
                    .is_err()
            {
                warn!(%task_id, "a canceled run is still going");
            }
            Ok(status_update(
                task_id,
                context_id,
                TaskState::Canceled,
                None,
            ))
        }))
    }
}

/// The runs in progress, by task id.
#[derive(Default)]
struct Runs(Mutex<HashMap<String, Arc<Run>>>);

impl Runs {
    fn insert(&self, task_id: &str, run: &Arc<Run>) {
        self.lock().insert(task_id.to_owned(), Arc::clone(run));
    }

    fn get(&self, task_id: &str) -> Option<Arc<Run>> {
        self.lock().get(task_id).cloned()
    }

    /// Removes `run`, if it is still the run of `task_id`.
    fn remove(&self, task_id: &str, run: &Arc<Run>) {
        let mut runs = self.lock();
        if runs.get(task_id).is_some_and(|r| Arc::ptr_eq(r, run)) {
            runs.remove(task_id);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Run>>> {
        // The map stays consistent whatever a panicking holder was doing.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One run of the adapter.
#[derive(Default)]
struct Run {
    /// Stops the run.
    cancel: CancellationToken,
    /// Fires once the run is over: adapter returned, directories removed,
    /// place freed.
    done: CancellationToken,
}

/// A run's place in the executor, given back when dropped: its permit, its
/// entry in [`Runs`], and then [`Run::done`].
struct RunSlot {
    task_id: String,
    taken: Option<OwnedSemaphorePermit>,
    runs: Arc<Runs>,
    run: Arc<Run>,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        drop(self.taken.take());
        self.runs.remove(&self.task_id, &self.run);
        self.run.done.cancel();
    }
}

/// What a task ends with: the adapter's text and the file parts of its
/// files.
struct Answer {
    text: String,
    files: Vec<Part>,
}

/// Waits for a turn to run, unless canceled first, and runs `adapter` on
/// `input` in new working directories, with the `inputs` fetched into
/// `in_dir` first and the files it answers with published after; the
/// directories are removed afterwards.
async fn run_task(
    adapter: Arc<dyn Adapter>,
    running: Arc<Semaphore>,
    slot: RunSlot,
    input: TaskInput,
    (files, inputs): (Files, Vec<InputFile>),
) -> Result<Answer, AdapterError> {
    let cancel = slot.run.cancel.clone();
    let outcome = async {
        let _turn = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(canceled()),
            turn = running.acquire_owned() => turn.expect("the semaphore is never closed"),
        };
        let task_dir = input
            .in_dir
            .parent()
            .expect("the input directory is in the task's directory")
            .to_path_buf();
        if let Err(err) = new_dirs(&task_dir, &input).await {
            error!(
                task_dir = %task_dir.display(),
                error = %err,
                "cannot make a task's directories"
            );
            remove_dir(&task_dir).await;
            return Err(AdapterError::Failed(
                "cannot prepare the task's working directories".to_owned(),
            ));
        }
        let fetched = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(canceled()),
            fetched = files.fetch_inputs(&input.task_id, &inputs, &input.in_dir) => fetched,
        };
        // In `in_dir` or not, the inputs are no longer the store's to keep.
        files.blobs.release_inputs(&input.task_id).await;
        if let Err(err) = fetched {
            remove_dir(&task_dir).await;
            return Err(err);
        }
        let peer = input.peer;
        let outcome = AssertUnwindSafe(adapter.run(input, cancel.clone()))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                error!("the worker adapter panicked");
                Err(AdapterError::Failed("the worker crashed".to_owned()))
            });
        let answer = match outcome {
            Ok(output) => files.publish(output, &peer).await,
            Err(err) => Err(err),
        };
        remove_dir(&task_dir).await;
        answer
    }
    .await;
    // Directories removed and turn given back: now the place, then done.
    drop(slot);
    outcome
}

fn canceled() -> AdapterError {
    AdapterError::Failed("canceled".to_owned())
}

impl Files {
    /// Fetches each of `inputs` of task `task_id` from its holder, the
    /// requester, into `in_dir`, all of them within the fetch timeout. Each
    /// is [held](BlobService::hold_input) in the store from before its
    /// download until the caller releases the task's inputs.
    async fn fetch_inputs(
        &self,
        task_id: &str,
        inputs: &[InputFile],
        in_dir: &Path,
    ) -> Result<(), AdapterError> {
        let all = async {
            let mut fetched = HashMap::new();
            for input in inputs {
                self.fetch_input(task_id, input, in_dir, &mut fetched)
                    .await?;
            }
            Ok(())
        };
        tokio::time::timeout(self.fetch_timeout, all)
            .await
            .unwrap_or_else(|_| Err(AdapterError::Failed("input fetch timed out".to_owned())))
    }

    /// Fetches `input` into `in_dir`, unless this task fetched the same
    /// blob already: `fetched` has the size of each blob it did. Each part is
    /// held to its own declared size.
    async fn fetch_input(
        &self,
        task_id: &str,
        input: &InputFile,
        in_dir: &Path,
        fetched: &mut HashMap<Hash, u64>,
    ) -> Result<(), AdapterError> {
        let blob = &input.blob;
        let size = match fetched.get(&blob.hash) {
            Some(size) => *size,
            None => {
                if let Err(err) = self.blobs.hold_input(task_id, blob.hash).await {
                    error!(error = %format!("{err:#}"), "cannot hold an input in the store");
                    return Err(AdapterError::Failed(
                        "cannot write an input file".to_owned(),
                    ));
                }
                let holder = self.blobs.holder_addr(&blob.holder).await;
                self.blobs
                    .fetch(blob, holder, None)
                    .await
                    .map_err(|err| fetch_failure(&err))?;
                let size = self
                    .blobs
                    .complete_size(blob.hash)
                    .await
                    .ok_or_else(|| AdapterError::Failed("blob fetch failed".to_owned()))?;
                fetched.insert(blob.hash, size);
                size
            }
        };
        if size > blob.size {
            return Err(fetch_failure(&BlobError::TooLarge));
        }
        let dest = in_dir.join(&input.name);
        if let Err(err) = self.blobs.export(blob.hash, &dest).await {
            error!(dest = %dest.display(), error = %format!("{err:#}"), "cannot write an input file");
            return Err(AdapterError::Failed(
                "cannot write an input file".to_owned(),
            ));
        }
        Ok(())
    }

    /// `output`, with each of its files added to the store, shared with
    /// `peer` and turned into a file part.
    async fn publish(&self, output: AdapterOutput, peer: &AgentId) -> Result<Answer, AdapterError> {
        let mut parts = Vec::with_capacity(output.files.len());
        for path in &output.files {
            let published = async {
                let blob = self.blobs.add_path(path).await?;
                self.blobs.share(&blob, peer, BLOB_SHARE_TTL_S).await?;
                anyhow::Ok(blob)
            };
            let blob = published.await.map_err(|err| {
                error!(file = %path.display(), error = %format!("{err:#}"), "cannot send a file");
                AdapterError::Failed("cannot send the answer's files".to_owned())
            })?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| blob.hash.to_hex());
            parts.push(blob.to_part(&name, FILE_MEDIA_TYPE));
        }
        Ok(Answer {
            text: output.text,
            files: parts,
        })
    }
}

/// The reason a task fails with when one of its files cannot be fetched.
fn fetch_failure(err: &BlobError) -> AdapterError {
    let reason = match err {
        BlobError::Denied => "blob fetch denied by its holder",
        BlobError::Unreachable => "blob holder unreachable",
        BlobError::Timeout => "blob fetch timed out",
        BlobError::TooLarge => "blob too large",
        BlobError::Other(detail) => {
            warn!(error = %detail, "a blob fetch failed");
            "blob fetch failed"
        }
    };
    AdapterError::Failed(reason.to_owned())
}

/// Makes `task_dir` anew, with the input's empty `in` and `out` in it.
async fn new_dirs(task_dir: &Path, input: &TaskInput) -> io::Result<()> {
    match tokio::fs::remove_dir_all(task_dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    tokio::fs::create_dir_all(task_dir).await?;
    tokio::fs::create_dir(&input.in_dir).await?;
    tokio::fs::create_dir(&input.out_dir).await
}

/// Removes `dir` and everything in it, if it is there.
async fn remove_dir(dir: &Path) {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => warn!(dir = %dir.display(), error = %err, "cannot remove a task's directory"),
    }
}

/// The one value of the service param `name`.
fn single<'a>(params: &'a ServiceParams, name: &str) -> Option<&'a str> {
    match params.get(name).map(Vec::as_slice) {
        Some([value]) => Some(value),
        _ => None,
    }
}

/// `first`, then `rest`.
fn events(first: StreamResponse, rest: Vec<StreamResponse>) -> Events {
    Box::pin(stream::iter(
        std::iter::once(first)
            .chain(rest)
            .map(Ok)
            .collect::<Vec<_>>(),
    ))
}

/// The events that end task `task_id` with `outcome`.
fn outcome_events(
    task_id: String,
    context_id: String,
    outcome: Result<Answer, AdapterError>,
) -> Vec<StreamResponse> {
    match outcome {
        Ok(answer) => vec![
            StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
                artifact: Artifact {
                    artifact_id: new_artifact_id(),
                    name: Some(ANSWER_ARTIFACT.to_owned()),
                    description: None,
                    parts: std::iter::once(Part::text(answer.text))
                        .chain(answer.files)
                        .collect(),
                    metadata: None,
                    extensions: None,
                },
                append: None,
                last_chunk: Some(true),
                metadata: None,
            }),
            status_update(task_id, context_id, TaskState::Completed, None),
        ],
        Err(AdapterError::Failed(reason)) => vec![status_update(
            task_id,
            context_id,
            TaskState::Failed,
            Some(reason),
        )],
        Err(AdapterError::Rejected(reason)) => vec![status_update(
            task_id,
            context_id,
            TaskState::Rejected,
            Some(reason),
        )],
    }
}

/// The text parts of `message`, one per line.
pub(crate) fn message_text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(Part::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The task as an execution's first event. A task event replaces the stored
/// task, so it carries the task's own history, including the message being
/// answered.
pub(crate) fn working_task(ctx: &ExecutorContext) -> Task {
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

/// The task moving to `state`, with `reason` as its status message.
pub(crate) fn status_update(
    task_id: String,
    context_id: String,
    state: TaskState,
    reason: Option<String>,
) -> StreamResponse {
    let message = reason.map(|reason| {
        let mut message = Message::new(Role::Agent, vec![Part::text(reason)]);
        message.task_id = Some(task_id.clone());
        message.context_id = Some(context_id.clone());
        message
    });
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id,
        context_id,
        status: TaskStatus {
            state,
            message,
            timestamp: None,
        },
        metadata: None,
    })
}
