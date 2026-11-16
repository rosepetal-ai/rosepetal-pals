//! Blobs: files that travel outside the control channel, over iroh-blobs,
//! with the permission of the agent that holds them.
//!
//! A file is a blob in the agent's iroh-blobs store ([`BlobService`], under
//! `<data_dir>/`[`BLOBS_DIR`]), named by its BLAKE3 hash and verified chunk
//! by chunk as it arrives; an interrupted download keeps what it verified,
//! and the next one fetches only the rest.
//!
//! Knowing a hash is never enough: the holder serves a blob only to an agent
//! it has [shared](BlobService::share) it with, through a grant it issued,
//! as for tasks: `blob.fetch` on `blob:<hash hex>`, with the holder as issuer
//! and audience. Every request is checked before any byte is sent, against
//! the peer that the connection's handshake authenticated. Nor does a
//! reference to a blob this agent happens to store already make its copy
//! the referrer's: [`BlobService::fetch`] uses what the store holds only once
//! the named holder proves, now, that it holds that blob too.
//!
//! The store keeps what is tagged: the files an agent adds (uploads through
//! the gateway, the files its tasks answer with), under `kept/<hash hex>`,
//! for as long as the store lasts, since their retention is not decided yet;
//! and the inputs of a task in progress, under `input/<task id>/<hash hex>`,
//! until they are in its `in_dir`. Garbage collection, every
//! [`BLOB_GC_INTERVAL`], removes the rest: inputs once used, downloads left
//! unfinished, blobs the gateway fetched once it has served them.
//!
//! In A2A messages a blob is a `url` part ([`BlobRef::to_part`]): between
//! peers its URL is `iroh-blob://<holder agent id>/<hash hex>`
//! ([`BLOB_SCHEME`]) and its metadata, under [`BLOB_META_KEY`], declares
//! its hash and size. The local gateway turns those URLs into plain `http://`
//! ones for stock A2A clients ([`crate::gateway::rewrite`]).

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use a2a::{Part, PartContent};
use anyhow::{Context, anyhow, bail, ensure};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_blobs::api::TempTag;
use iroh_blobs::api::blobs::{AddProgressItem, BlobStatus};
use iroh_blobs::api::proto::ExportRangesItem;
use iroh_blobs::api::remote::GetProgressItem;
use iroh_blobs::get::GetError;
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt, ERR_PERMISSION, GetRequest};
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage, RequestMode,
    ThrottleMode,
};
use iroh_blobs::store::GcConfig;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::fs::options::Options;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::{BlobsProtocol, Hash, HashAndFormat};
use pals_core::grant::{GRANT_SCHEMA, Grant, Operation, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::Store;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::net::{agent_id_of, endpoint_id_of, peer_addr};

/// The URL scheme of a blob between peers:
/// `iroh-blob://<holder agent id>/<hash hex>`.
pub const BLOB_SCHEME: &str = "iroh-blob";
/// The key, in a file part's metadata, of `{"hash": "<hex>", "size": <u64>}`.
pub const BLOB_META_KEY: &str = "agentnet.blob/1";
/// How long the requester of a task may fetch the files it is answered with.
pub const BLOB_SHARE_TTL_S: i64 = 24 * 3600;
/// The blob store, in the data directory.
pub const BLOBS_DIR: &str = "blobs";
/// The project of the grants [`BlobService::share`] issues.
pub const BLOB_PROJECT: &str = "blobs";

/// How long [`BlobService::shutdown`] waits for the blob store to close.
const BLOB_STORE_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the store removes the blobs no tag holds.
pub const BLOB_GC_INTERVAL: Duration = Duration::from_secs(3600);

/// The prefix of the tags of the blobs the store keeps.
const KEPT_TAG_PREFIX: &str = "kept/";
/// The prefix of the tags of the inputs of tasks in progress.
const INPUT_TAG_PREFIX: &str = "input/";
/// How many chunks, besides the last one, a holder proves it holds.
const POSSESSION_SAMPLES: usize = 3;
/// The size of a BLAKE3 chunk.
const CHUNK_BYTES: u64 = 1024;
/// How long dialing a blob's holder may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How many provider events may wait for the authorizer.
const EVENT_CAPACITY: usize = 64;
/// What the provider asks the authorizer: every connection, to learn its
/// peer, and every single-blob request, to allow or refuse it. Requests for
/// several hashes, pushes and observations are refused outright.
const PROVIDER_EVENTS: EventMask = EventMask {
    connected: ConnectMode::Intercept,
    get: RequestMode::Intercept,
    get_many: RequestMode::Disabled,
    push: RequestMode::Disabled,
    observe: ObserveMode::Disabled,
    throttle: ThrottleMode::None,
};

/// Told the bytes a download has received so far.
pub type ProgressFn = Box<dyn FnMut(u64) + Send>;

/// A blob as a file part refers to it: who holds it, its hash, and its size
/// as declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobRef {
    pub holder: AgentId,
    pub hash: Hash,
    pub size: u64,
}

impl BlobRef {
    /// `iroh-blob://<holder>/<hash hex>`.
    pub fn to_url(&self) -> String {
        wire_url(&self.holder, &self.hash)
    }

    /// The holder and hash of an `iroh-blob://<holder>/<hash hex>` URL: an
    /// agent id and 64 lowercase hex characters, nothing more.
    pub fn parse_url(url: &str) -> Option<(AgentId, Hash)> {
        let rest = url.strip_prefix(BLOB_SCHEME)?.strip_prefix("://")?;
        parse_holder_and_hash(rest)
    }

    /// A file part for this blob: its URL, `filename`, `media_type`, and the
    /// hash and size under [`BLOB_META_KEY`].
    pub fn to_part(&self, filename: &str, media_type: &str) -> Part {
        Part {
            content: PartContent::Url(self.to_url()),
            filename: Some(filename.to_owned()),
            media_type: Some(media_type.to_owned()),
            metadata: Some(HashMap::from([(BLOB_META_KEY.to_owned(), self.meta())])),
        }
    }

    /// The blob a file part refers to: its URL is a blob URL, and its
    /// metadata declares the same hash and a size.
    ///
    /// The size is a whole number, which may come as a double: the A2A SDK
    /// carries metadata as a protobuf `Struct`, whose numbers are all
    /// doubles (`2048.0`).
    pub fn from_part(part: &Part) -> Option<BlobRef> {
        let PartContent::Url(url) = &part.content else {
            return None;
        };
        let (holder, hash) = Self::parse_url(url)?;
        let meta = part.metadata.as_ref()?.get(BLOB_META_KEY)?;
        if meta.get("hash")?.as_str()? != hash.to_hex() {
            return None;
        }
        let size = declared_size(meta.get("size")?)?;
        Some(BlobRef { holder, hash, size })
    }

    /// What a file part's metadata holds under [`BLOB_META_KEY`].
    pub(crate) fn meta(&self) -> Value {
        json!({ "hash": self.hash.to_hex(), "size": self.size })
    }
}

/// The largest whole number a double holds exactly, 2^53.
const MAX_EXACT_DOUBLE: f64 = 9_007_199_254_740_992.0;

/// `value` as a size: a whole number, from 0 to 2^53 when it is a double.
fn declared_size(value: &Value) -> Option<u64> {
    if let Some(size) = value.as_u64() {
        return Some(size);
    }
    let size = value.as_f64()?;
    let whole = (0.0..=MAX_EXACT_DOUBLE).contains(&size) && size.fract() == 0.0;
    // Exact: a whole double within 2^53 converts without loss.
    whole.then_some(size as u64)
}

/// `iroh-blob://<holder>/<hash hex>`.
pub(crate) fn wire_url(holder: &AgentId, hash: &Hash) -> String {
    format!("{BLOB_SCHEME}://{holder}/{}", hash.to_hex())
}

/// `<agent id>/<hash hex>`, exactly.
pub(crate) fn parse_holder_and_hash(path: &str) -> Option<(AgentId, Hash)> {
    let (holder, hash) = path.split_once('/')?;
    Some((holder.parse().ok()?, parse_hash(hash)?))
}

/// A hash as 64 lowercase hex characters, its only spelling here.
pub(crate) fn parse_hash(text: &str) -> Option<Hash> {
    let hex = text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !hex {
        return None;
    }
    text.parse().ok()
}

/// The resource a grant names to let its subject fetch `hash`.
fn blob_resource(hash: &Hash) -> String {
    format!("blob:{}", hash.to_hex())
}

/// Why a blob was not fetched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    /// The holder refused: no grant of its covers the request.
    #[error("the holder denied the blob")]
    Denied,
    /// The holder could not be reached, or the connection was lost.
    #[error("the holder is unreachable")]
    Unreachable,
    /// The blob is larger than its declared size.
    #[error("the blob is larger than declared")]
    TooLarge,
    /// The download took longer than allowed.
    #[error("the download timed out")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

/// The agent's blobs: its iroh-blobs store, the provider that serves them to
/// the peers it shared them with, and downloads from other holders.
///
/// Cloning is cheap; clones share the store.
#[derive(Clone)]
pub struct BlobService {
    inner: Arc<Inner>,
}

struct Inner {
    id: AgentId,
    fs: FsStore,
    protocol: BlobsProtocol,
    endpoint: Endpoint,
    store: Arc<Store>,
    key: Arc<AgentKey>,
    /// The task that decides the provider's requests, until
    /// [`BlobService::shutdown`] ends it.
    authorizer: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl BlobService {
    /// Opens the blob store in `dir`, creating it if missing, for the agent
    /// of `key`, whose `endpoint` and `store` these are, with garbage
    /// collection every [`BLOB_GC_INTERVAL`]. The input tags a previous run
    /// left are dropped: their tasks are over.
    ///
    /// Its [`protocol`](BlobService::protocol) serves only what the agent
    /// shared, as the grants in `store` say.
    pub async fn open(
        dir: &Path,
        endpoint: Endpoint,
        store: Arc<Store>,
        key: Arc<AgentKey>,
    ) -> anyhow::Result<Self> {
        let id = key.id();
        ensure!(
            endpoint.id().as_bytes() == id.as_bytes(),
            "the endpoint is not the agent's"
        );
        ensure!(*store.owner() == id, "the store is not the agent's");
        let mut options = Options::new(dir);
        options.gc = Some(GcConfig {
            interval: BLOB_GC_INTERVAL,
            add_protected: None,
        });
        let fs = FsStore::load_with_opts(dir.join("blobs.db"), options)
            .await
            .map_err(|err| anyhow!("cannot open the blob store {}: {err}", dir.display()))?;
        fs.tags()
            .delete_prefix(INPUT_TAG_PREFIX)
            .await
            .map_err(|err| anyhow!("cannot drop the input tags of a previous run: {err}"))?;
        let (events, requests) = EventSender::channel(EVENT_CAPACITY, PROVIDER_EVENTS);
        let authorizer = tokio::spawn(authorize(requests, Arc::clone(&store)));
        let protocol = BlobsProtocol::new(&fs, Some(events));
        Ok(Self {
            inner: Arc::new(Inner {
                id,
                fs,
                protocol,
                endpoint,
                store,
                key,
                authorizer: std::sync::Mutex::new(Some(authorizer)),
            }),
        })
    }

    /// Stops serving and storing: ends the task that decides the
    /// provider's requests, which holds the agent's store, and shuts the
    /// blob store down, within [`BLOB_STORE_STOP_TIMEOUT`]. Nothing of this
    /// service works afterwards.
    pub async fn shutdown(&self) {
        let authorizer = self
            .inner
            .authorizer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(authorizer) = authorizer {
            authorizer.abort();
            // Cancelled, as asked; nothing else to learn.
            let _ = authorizer.await;
        }
        match tokio::time::timeout(BLOB_STORE_STOP_TIMEOUT, self.inner.fs.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => debug!(error = %err, "the blob store did not shut down cleanly"),
            Err(_) => warn!("the blob store did not shut down in time"),
        }
    }

    /// The provider, to serve on [`iroh_blobs::ALPN`].
    pub fn protocol(&self) -> iroh_blobs::BlobsProtocol {
        self.inner.protocol.clone()
    }

    /// Adds the file at `path` to the store, as a copy.
    pub async fn add_path(&self, path: &Path) -> anyhow::Result<BlobRef> {
        let path = std::path::absolute(path)
            .with_context(|| format!("no absolute path for {}", path.display()))?;
        let added = self.inner.fs.blobs().add_path(&path).stream().await;
        self.added(added)
            .await
            .with_context(|| format!("cannot add {} to the blob store", path.display()))
    }

    /// Adds `bytes` to the store.
    pub async fn add_bytes(&self, bytes: Bytes) -> anyhow::Result<BlobRef> {
        let added = self.inner.fs.blobs().add_bytes(bytes).stream().await;
        self.added(added)
            .await
            .context("cannot add the bytes to the blob store")
    }

    /// Adds the bytes of `stream` to the store, writing them to disk as they
    /// come. An error in the stream abandons the import.
    pub async fn add_stream(
        &self,
        stream: impl Stream<Item = io::Result<Bytes>> + Send + 'static,
    ) -> anyhow::Result<BlobRef> {
        let added = self
            .inner
            .fs
            .blobs()
            .add_stream(stream)
            .await
            .stream()
            .await;
        self.added(added)
            .await
            .context("cannot add the stream to the blob store")
    }

    /// The blob an import ends with, [kept](BlobService::keep).
    async fn added(
        &self,
        progress: impl Stream<Item = AddProgressItem>,
    ) -> anyhow::Result<BlobRef> {
        let mut progress = std::pin::pin!(progress);
        let mut size = None;
        while let Some(item) = progress.next().await {
            match item {
                AddProgressItem::Size(bytes) => size = Some(bytes),
                AddProgressItem::Done(tag) => {
                    let hash = tag.hash();
                    self.keep(hash).await?;
                    return Ok(BlobRef {
                        holder: self.inner.id,
                        hash,
                        size: size.context("the import reported no size")?,
                    });
                }
                AddProgressItem::Error(err) => return Err(err.into()),
                _ => {}
            }
        }
        bail!("the blob store stopped before the import finished")
    }

    /// Tags `hash` as `kept/<hash hex>`, so that the store keeps it.
    ///
    /// What this agent adds, uploads and the files its tasks answer with, is
    /// kept for as long as the store lasts: their retention policy is beyond
    /// milestone 1.
    async fn keep(&self, hash: Hash) -> anyhow::Result<()> {
        self.inner
            .fs
            .tags()
            .set(
                format!("{KEPT_TAG_PREFIX}{}", hash.to_hex()),
                HashAndFormat::raw(hash),
            )
            .await
            .map_err(|err| anyhow!("cannot tag blob {hash}: {err}"))
    }

    /// Holds `hash` in the store, against garbage collection, for as long as
    /// the returned tag lives.
    pub(crate) async fn protect(&self, hash: Hash) -> anyhow::Result<TempTag> {
        self.inner
            .fs
            .tags()
            .temp_tag(HashAndFormat::raw(hash))
            .await
            .map_err(|err| anyhow!("cannot protect blob {hash}: {err}"))
    }

    /// Holds `hash` in the store as an input of task `task_id`, until
    /// [`release_inputs`](BlobService::release_inputs).
    pub(crate) async fn hold_input(&self, task_id: &str, hash: Hash) -> anyhow::Result<()> {
        self.inner
            .fs
            .tags()
            .set(
                format!("{INPUT_TAG_PREFIX}{task_id}/{}", hash.to_hex()),
                HashAndFormat::raw(hash),
            )
            .await
            .map_err(|err| anyhow!("cannot hold input {hash}: {err}"))
    }

    /// Lets go of the inputs of task `task_id`, which garbage collection may
    /// then remove.
    pub(crate) async fn release_inputs(&self, task_id: &str) {
        let prefix = format!("{INPUT_TAG_PREFIX}{task_id}/");
        if let Err(err) = self.inner.fs.tags().delete_prefix(prefix).await {
            warn!(%task_id, error = %err, "cannot release the inputs of a task");
        }
    }

    /// Lets agent `with` fetch `blob`, which this agent holds, for `ttl_s`
    /// seconds from now: a grant of this agent, as issuer and audience, for
    /// `blob.fetch` on `blob:<hash hex>` in project [`BLOB_PROJECT`], stored
    /// as issued and logged as a `blob.shared` event.
    pub async fn share(&self, blob: &BlobRef, with: &AgentId, ttl_s: i64) -> anyhow::Result<()> {
        let inner = &self.inner;
        ensure!(
            blob.holder == inner.id,
            "only the holder of a blob shares it"
        );
        ensure!(ttl_s > 0, "a share lasts a positive time, not {ttl_s} s");
        let issued_at = chrono::Utc::now().timestamp();
        let instant = |at: i64| format_timestamp(at).context("a share past the year 9999");
        let grant = Grant {
            schema: GRANT_SCHEMA.to_owned(),
            grant_id: new_grant_id(),
            issuer: inner.id,
            subject: *with,
            audience: inner.id,
            project: BLOB_PROJECT.to_owned(),
            resources: vec![blob_resource(&blob.hash)],
            operations: vec![Operation::BlobFetch],
            issued_at: instant(issued_at)?,
            expires_at: instant(issued_at.checked_add(ttl_s).context("a share too long")?)?,
        };
        let shared = SignedGrant::issue(&inner.key, grant)
            .map_err(|err| anyhow!("cannot sign the share: {err}"))?;
        let payload = json!({
            "grant_id": shared.grant.grant_id,
            "hash": blob.hash.to_hex(),
            "with": with.to_string(),
        });
        let store = Arc::clone(&inner.store);
        let key = Arc::clone(&inner.key);
        tokio::task::spawn_blocking(move || {
            store.grant_issued_put(&shared)?;
            store.event_append(&key, "blob.shared", &payload, issued_at)?;
            anyhow::Ok(())
        })
        .await
        .context("the share did not finish")?
    }

    /// Gets `blob` into the store from its holder, at `holder`; `progress`
    /// is told the bytes received.
    ///
    /// What the store holds already is used only if the holder proves it
    /// holds the blob too: a hash this agent stores, its own or fetched
    /// from another holder, is not the holder's to have because it names
    /// it. When the store holds all of it, the holder must send, verified
    /// against the hash, its last chunk and three chunks chosen at random,
    /// into a scratch store; when it holds part of it, the
    /// holder sends the rest, and then the same proof. Otherwise the holder
    /// sends it all. A holder that cannot fails the fetch.
    ///
    /// So a download that was interrupted resumes from what it verified. The
    /// declared size bounds it: a blob that turns out larger, in the store
    /// already or as it arrives, is [`BlobError::TooLarge`], and a download
    /// stops then. It does not bound the time it takes, beyond dialing.
    ///
    /// The store keeps what arrives only while the download lasts: a caller
    /// that needs it longer tags it first, as the worker does with the
    /// inputs of a task.
    pub async fn fetch(
        &self,
        blob: &BlobRef,
        holder: EndpointAddr,
        progress: Option<ProgressFn>,
    ) -> Result<(), BlobError> {
        if holder.id != endpoint_id_of(&blob.holder) {
            return Err(BlobError::Other(
                "the address is not the holder's".to_owned(),
            ));
        }
        if blob.holder == self.inner.id {
            // This agent's own blob: its store is the only source.
            return match self.complete_size(blob.hash).await {
                Some(size) => within_declared(size, blob),
                None => Err(BlobError::Other("this agent holds no such blob".to_owned())),
            };
        }
        let _downloading = self.protect(blob.hash).await.map_err(store_failure)?;
        let local = self
            .inner
            .fs
            .remote()
            .local(blob.hash)
            .await
            .map_err(|err| BlobError::Other(format!("cannot read the blob store: {err}")))?;
        if local.is_complete() {
            let size = self
                .complete_size(blob.hash)
                .await
                .ok_or_else(|| BlobError::Other("cannot read the blob store".to_owned()))?;
            within_declared(size, blob)?;
            let connection = self.dial(holder, &blob.holder).await?;
            let proved = prove_possession(&connection, blob.hash, size).await;
            connection.close(0u32.into(), b"done");
            return proved;
        }
        let had_some = local.local_bytes() > 0;
        let connection = self.dial(holder, &blob.holder).await?;
        let mut fetched = self
            .download(&connection, blob, local.missing(), progress)
            .await;
        if had_some && fetched.is_ok() {
            // Part of it was in the store already, from whoever sent it.
            fetched = match self.complete_size(blob.hash).await {
                Some(size) => prove_possession(&connection, blob.hash, size).await,
                None => Err(BlobError::Other("the download ended incomplete".to_owned())),
            };
        }
        connection.close(0u32.into(), b"done");
        fetched
    }

    /// A connection to `holder`, at `addr`, for blobs.
    async fn dial(&self, addr: EndpointAddr, holder: &AgentId) -> Result<Connection, BlobError> {
        let dial = self.inner.endpoint.connect(addr, iroh_blobs::ALPN);
        match tokio::time::timeout(CONNECT_TIMEOUT, dial).await {
            Ok(Ok(connection)) => Ok(connection),
            Ok(Err(err)) => {
                debug!(%holder, error = %err, "cannot dial a blob's holder");
                Err(BlobError::Unreachable)
            }
            Err(_) => Err(BlobError::Unreachable),
        }
    }

    /// Gets what `request` asks for of `blob` over `connection` into the
    /// store, which must then hold all of it, within its declared size.
    async fn download(
        &self,
        connection: &Connection,
        blob: &BlobRef,
        request: GetRequest,
        mut progress: Option<ProgressFn>,
    ) -> Result<(), BlobError> {
        let items = self
            .inner
            .fs
            .remote()
            .execute_get(connection.clone(), request)
            .stream();
        let mut items = std::pin::pin!(items);
        while let Some(item) = items.next().await {
            match item {
                GetProgressItem::Progress(bytes) => {
                    // Dropping the download stops it; the store keeps what
                    // it verified.
                    if bytes > blob.size {
                        return Err(BlobError::TooLarge);
                    }
                    if let Some(progress) = progress.as_mut() {
                        progress(bytes);
                    }
                }
                GetProgressItem::Done(_) => {
                    return match self.complete_size(blob.hash).await {
                        Some(size) => within_declared(size, blob),
                        None => Err(BlobError::Other("the download ended incomplete".to_owned())),
                    };
                }
                GetProgressItem::Error(err) => return Err(download_failure(&err, connection)),
            }
        }
        Err(BlobError::Other(
            "the download ended without a result".to_owned(),
        ))
    }

    /// Every tag of the store, by name, and the blob it holds.
    pub async fn tags(&self) -> anyhow::Result<Vec<(String, Hash)>> {
        let tags = self.inner.fs.tags();
        let mut listed = tags
            .list()
            .await
            .map_err(|err| anyhow!("cannot list the tags: {err}"))?;
        let mut found = Vec::new();
        while let Some(tag) = listed.next().await {
            let tag = tag.map_err(|err| anyhow!("cannot list the tags: {err}"))?;
            found.push((
                String::from_utf8_lossy(tag.name.as_ref()).into_owned(),
                tag.hash,
            ));
        }
        Ok(found)
    }

    /// Whether the store holds all of `hash`.
    pub async fn has(&self, hash: Hash) -> bool {
        self.complete_size(hash).await.is_some()
    }

    /// The size of `hash`, when the store holds all of it.
    pub async fn complete_size(&self, hash: Hash) -> Option<u64> {
        match self.inner.fs.blobs().status(hash).await {
            Ok(BlobStatus::Complete { size }) => Some(size),
            Ok(_) => None,
            Err(err) => {
                warn!(%hash, error = %err, "cannot read the blob store");
                None
            }
        }
    }

    /// Writes the whole of `hash`, which the store must hold, to `dest`,
    /// replacing whatever file is there.
    pub async fn export(&self, hash: Hash, dest: &Path) -> anyhow::Result<()> {
        let dest = std::path::absolute(dest)
            .with_context(|| format!("no absolute path for {}", dest.display()))?;
        self.inner
            .fs
            .blobs()
            .export(hash, &dest)
            .finish()
            .await
            .map_err(|err| anyhow!("cannot export blob {hash} to {}: {err}", dest.display()))?;
        Ok(())
    }

    /// The `size` bytes of `hash`, which the store must hold, as they are
    /// read from it.
    pub fn read(
        &self,
        hash: Hash,
        size: u64,
    ) -> impl Stream<Item = io::Result<Bytes>> + Send + 'static {
        self.inner
            .fs
            .blobs()
            .export_ranges(hash, 0..size)
            .stream()
            .filter_map(|item| async move {
                match item {
                    ExportRangesItem::Size(_) => None,
                    ExportRangesItem::Data(leaf) => Some(Ok(leaf.data)),
                    ExportRangesItem::Error(err) => Some(Err(io::Error::other(err))),
                }
            })
    }

    /// Where to dial `holder`: at the addresses of its contact's ticket,
    /// else by its id alone (see [`peer_addr`]).
    pub(crate) async fn holder_addr(&self, holder: &AgentId) -> EndpointAddr {
        let store = Arc::clone(&self.inner.store);
        let id = *holder;
        let contact = tokio::task::spawn_blocking(move || store.contact_by_id(&id)).await;
        let ticket = match contact {
            Ok(Ok(contact)) => contact.and_then(|contact| contact.ticket),
            Ok(Err(err)) => {
                warn!(%holder, error = %format!("{err:#}"), "cannot read the contacts");
                None
            }
            Err(err) => {
                warn!(%holder, error = %err, "cannot read the contacts");
                None
            }
        };
        peer_addr(holder, ticket.as_deref())
    }
}

/// Asks the holder at the other end of `connection` for the last chunk of
/// `hash`, `size` bytes long, and [`POSSESSION_SAMPLES`] chunks chosen at
/// random, into a scratch store, the agent's own untouched: `Ok` when they
/// all arrive, verified against the hash.
async fn prove_possession(connection: &Connection, hash: Hash, size: u64) -> Result<(), BlobError> {
    let chunks = size.div_ceil(CHUNK_BYTES);
    if chunks == 0 {
        // The empty blob holds nothing to give away.
        return Ok(());
    }
    let mut ranges = ChunkRanges::chunk(chunks - 1);
    for _ in 0..POSSESSION_SAMPLES {
        ranges |= ChunkRanges::chunk(random_below(chunks));
    }
    let scratch = MemStore::new();
    let request = GetRequest::blob_ranges(hash, ranges.clone());
    let received = scratch
        .remote()
        .execute_get(connection.clone(), request)
        .complete()
        .await;
    if let Err(err) = received {
        return Err(download_failure(&err, connection));
    }
    let proved =
        scratch.blobs().observe(hash).await.is_ok_and(|held| {
            held.ranges.is_superset(&ranges) && held.validated_size() == Some(size)
        });
    if proved {
        Ok(())
    } else {
        Err(BlobError::Other(
            "the holder did not prove it holds the blob".to_owned(),
        ))
    }
}

/// A random number below `bound`, which is not 0.
fn random_below(bound: u64) -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the OS random number generator failed");
    u64::from_le_bytes(bytes) % bound
}

/// A failure of the blob store, as a [`BlobError`].
fn store_failure(err: anyhow::Error) -> BlobError {
    BlobError::Other(format!("{err:#}"))
}

/// `Ok` when a blob of `size` bytes is within the size `blob` declares.
fn within_declared(size: u64, blob: &BlobRef) -> Result<(), BlobError> {
    if size <= blob.size {
        Ok(())
    } else {
        Err(BlobError::TooLarge)
    }
}

/// What a failed download tells of its cause.
fn download_failure(err: &GetError, connection: &Connection) -> BlobError {
    if err.iroh_error_code() == Some(ERR_PERMISSION) {
        return BlobError::Denied;
    }
    if connection.close_reason().is_some() {
        debug!(error = %err, "the connection to a blob's holder was lost");
        return BlobError::Unreachable;
    }
    BlobError::Other(format!("the download failed: {err}"))
}

/// Answers the provider's questions until it is gone: every connection is
/// allowed and its peer remembered; a request is allowed only for a single
/// blob that the peer holds a grant of this agent to fetch.
async fn authorize(mut events: mpsc::Receiver<ProviderMessage>, store: Arc<Store>) {
    let mut peers: HashMap<u64, EndpointId> = HashMap::new();
    while let Some(event) = events.recv().await {
        match event {
            ProviderMessage::ClientConnected(msg) => {
                let verdict = match msg.inner.endpoint_id {
                    Some(peer) => {
                        peers.insert(msg.inner.connection_id, peer);
                        Ok(())
                    }
                    None => Err(AbortReason::Permission),
                };
                msg.tx.send(verdict).await.ok();
            }
            ProviderMessage::ConnectionClosed(msg) => {
                peers.remove(&msg.inner.connection_id);
            }
            ProviderMessage::GetRequestReceived(msg) => {
                let peer = peers.get(&msg.inner.connection_id).copied();
                let store = Arc::clone(&store);
                tokio::spawn(async move {
                    let allowed = match peer {
                        Some(peer) => may_fetch(store, peer, &msg.inner.request).await,
                        None => false,
                    };
                    let verdict = if allowed {
                        Ok(())
                    } else {
                        Err(AbortReason::Permission)
                    };
                    msg.tx.send(verdict).await.ok();
                });
            }
            // Disabled in the mask; refused should they come anyway.
            ProviderMessage::GetManyRequestReceived(msg) => {
                msg.tx.send(Err(AbortReason::Permission)).await.ok();
            }
            ProviderMessage::PushRequestReceived(msg) => {
                msg.tx.send(Err(AbortReason::Permission)).await.ok();
            }
            ProviderMessage::ObserveRequestReceived(msg) => {
                msg.tx.send(Err(AbortReason::Permission)).await.ok();
            }
            ProviderMessage::Throttle(msg) => {
                msg.tx.send(Ok(())).await.ok();
            }
            ProviderMessage::ClientConnectedNotify(_)
            | ProviderMessage::GetRequestReceivedNotify(_)
            | ProviderMessage::GetManyRequestReceivedNotify(_)
            | ProviderMessage::PushRequestReceivedNotify(_)
            | ProviderMessage::ObserveRequestReceivedNotify(_) => {}
        }
    }
}

/// Whether `peer` may have what `request` asks for: a single blob (no hash
/// sequence or collection), covered now by a grant this agent issued it.
async fn may_fetch(store: Arc<Store>, peer: EndpointId, request: &GetRequest) -> bool {
    let subject = agent_id_of(&peer);
    if !request.ranges.is_blob() {
        debug!(peer = %subject, hash = %request.hash, "refused a request beyond a single blob");
        return false;
    }
    let resource = blob_resource(&request.hash);
    let now = chrono::Utc::now().timestamp();
    let checked = tokio::task::spawn_blocking(move || {
        store.grant_issued_covering(&subject, Operation::BlobFetch, &resource, now)
    })
    .await;
    match checked {
        Ok(Ok(Some(_))) => true,
        Ok(Ok(None)) => {
            debug!(peer = %subject, hash = %request.hash, "refused a blob without a grant");
            false
        }
        Ok(Err(err)) => {
            warn!(error = %format!("{err:#}"), "cannot check a blob grant");
            false
        }
        Err(err) => {
            warn!(error = %err, "the blob grant check did not finish");
            false
        }
    }
}

/// A new grant id: `g-` and 32 random hex digits.
fn new_grant_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random number generator failed");
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("g-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_urls_are_exactly_holder_and_lowercase_hash() {
        let holder = AgentKey::generate().id();
        let hash = Hash::new(b"hello");
        let url = wire_url(&holder, &hash);
        assert_eq!(BlobRef::parse_url(&url), Some((holder, hash)));
        let hex = hash.to_hex();
        for bad in [
            format!("{BLOB_SCHEME}://{holder}/{}", hex.to_uppercase()),
            format!("{BLOB_SCHEME}://{holder}/{hex}/"),
            format!("{BLOB_SCHEME}://{holder}/{hex}?x=1"),
            format!("{BLOB_SCHEME}://{holder}"),
            format!("{BLOB_SCHEME}:/{holder}/{hex}"),
            format!("https://{holder}/{hex}"),
            format!(
                "{BLOB_SCHEME}://{}/{hex}",
                holder.to_string().to_uppercase()
            ),
            // A base32 hash, which iroh-blobs would also read.
            format!("{BLOB_SCHEME}://{holder}/{}", "a".repeat(52)),
        ] {
            assert_eq!(BlobRef::parse_url(&bad), None, "{bad}");
        }
    }

    #[test]
    fn a_part_is_a_blob_only_with_a_matching_declaration() {
        let blob = BlobRef {
            holder: AgentKey::generate().id(),
            hash: Hash::new(b"hello"),
            size: 5,
        };
        let part = blob.to_part("hello.txt", "text/plain");
        assert_eq!(part.filename.as_deref(), Some("hello.txt"));
        assert_eq!(part.media_type.as_deref(), Some("text/plain"));
        assert_eq!(BlobRef::from_part(&part), Some(blob));

        let mut undeclared = part.clone();
        undeclared.metadata = None;
        assert_eq!(BlobRef::from_part(&undeclared), None);
        let mut other_hash = part.clone();
        other_hash.metadata = Some(HashMap::from([(
            BLOB_META_KEY.to_owned(),
            json!({"hash": Hash::new(b"other").to_hex(), "size": 5}),
        )]));
        assert_eq!(BlobRef::from_part(&other_hash), None);
        let mut no_size = part.clone();
        no_size.metadata = Some(HashMap::from([(
            BLOB_META_KEY.to_owned(),
            json!({"hash": blob.hash.to_hex()}),
        )]));
        assert_eq!(BlobRef::from_part(&no_size), None);
        assert_eq!(BlobRef::from_part(&Part::text(blob.to_url())), None);
    }

    #[test]
    fn a_declared_size_is_a_whole_number_even_as_a_double() {
        assert_eq!(declared_size(&json!(2048)), Some(2048));
        assert_eq!(declared_size(&json!(2048.0)), Some(2048));
        assert_eq!(declared_size(&json!(0.0)), Some(0));
        assert_eq!(declared_size(&json!(2_147_483_648.0)), Some(2_147_483_648));
        for bad in [
            json!(2048.5),
            json!(-1),
            json!(-1.0),
            json!(1e300),
            json!("2048"),
            json!(null),
        ] {
            assert_eq!(declared_size(&bad), None, "{bad}");
        }
    }
}
