//! Blobs: large files travel outside the control channel, over iroh-blobs,
//! verified with BLAKE3 and resumable, and only with the permission of the
//! agent that holds them, given with the same signed grants as tasks. Between
//! peers a file part carries an `iroh-blob://<holder>/<hash>` URL; at the
//! gateway it is a plain `http://127.0.0.1…/blobs/…` URL a stock A2A client
//! uploads to and downloads from.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use a2a::{
    Message, Part, PartContent, Role, SendMessageRequest, SendMessageResponse, StreamResponse,
    TaskState,
};
use a2a_client::A2AClientFactory;
use a2a_client::agent_card::AgentCardResolver;
use axum::body::Body;
use bytes::Bytes;
use common::{
    TestNode, TestResponse, bearer, gateway_request, gateway_send_raw, grant, issue_grant, key_in,
    loopback_endpoint, message_params, now_unix, rpc_request, spawn_node, spawn_node_with,
};
use futures::StreamExt;
use http::Request;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr};
use iroh_tickets::endpoint::EndpointTicket;
use pals::blobs::{BLOB_META_KEY, BLOB_SCHEME, BLOB_SHARE_TTL_S, BlobError, BlobRef, ProgressFn};
use pals::config::{ExecCfg, ProjectCfg, WorkerCfg};
use pals::gateway::rewrite::{to_gateway, to_wire};
use pals::net::{PeerHttp, agent_id_of};
use pals::worker::MAX_INPUT_BLOBS;
use pals_core::grant::Operation::{BlobFetch, TaskRead, TaskSend};
use pals_core::grant::{GRANT_SCHEMA, Grant, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::Contact;
use serde_json::{Value, json};

/// How long a test waits for something that should happen soon: generous,
/// because the host may be heavily loaded.
const PATIENCE: Duration = Duration::from_secs(120);

const MIB: usize = 1024 * 1024;

/// `n` random bytes.
fn random_bytes(n: usize) -> Bytes {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("random bytes");
    Bytes::from(bytes)
}

/// Two echo nodes: `a` holds blobs, `c` knows `a` as contact `alice`.
async fn holder_and_requester() -> (TestNode, TestNode) {
    let a = spawn_node(WorkerCfg::Echo).await;
    let c = spawn_node(WorkerCfg::Echo).await;
    c.add_contact("alice", &a);
    (a, c)
}

/// A progress callback and the largest count of bytes it was told.
fn progress_probe() -> (ProgressFn, Arc<AtomicU64>) {
    let seen = Arc::new(AtomicU64::new(0));
    let probe = Arc::clone(&seen);
    let progress: ProgressFn = Box::new(move |bytes| {
        probe.fetch_max(bytes, Ordering::SeqCst);
    });
    (progress, seen)
}

/// Every file under `dir` whose name mentions `hash_hex`: whatever the blob
/// store keeps of that blob, partial or complete, on disk.
fn files_of_blob(dir: &Path, hash_hex: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_of_blob(&path, hash_hex));
        } else if entry.file_name().to_string_lossy().contains(hash_hex) {
            found.push(path);
        }
    }
    found
}

/// What the store of `node` keeps on disk of the blob `hash_hex`.
fn stored_files(node: &TestNode, hash_hex: &str) -> Vec<PathBuf> {
    files_of_blob(&node.dir.path().join("blobs"), hash_hex)
}

/// A grant of `issuer` that lets `subject` fetch the blob `hash_hex` from
/// it, valid from `issued_at` to `expires_at`, recorded as issued.
fn blob_grant(
    issuer: &TestNode,
    subject: AgentId,
    hash_hex: &str,
    issued_at: i64,
    expires_at: i64,
) -> SignedGrant {
    let grant = Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: format!("g-blob-{hash_hex}"),
        issuer: issuer.id(),
        subject,
        audience: issuer.id(),
        project: "blobs".to_owned(),
        resources: vec![format!("blob:{hash_hex}")],
        operations: vec![BlobFetch],
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(expires_at).unwrap(),
    };
    let g = SignedGrant::issue(&key_in(issuer.dir.path()), grant).unwrap();
    issuer.node.store.grant_issued_put(&g).unwrap();
    g
}

#[tokio::test]
async fn knowing_the_hash_is_not_enough() {
    let (a, c) = holder_and_requester().await;
    let blob = a
        .node
        .blobs
        .add_bytes(random_bytes(256 * 1024))
        .await
        .unwrap();
    let hex = blob.hash.to_hex();
    assert_eq!(blob.holder, a.id());

    // C knows the hash and the holder, but holds no grant for it.
    let (progress, seen) = progress_probe();
    let fetched = c
        .node
        .blobs
        .fetch(&blob, a.node.addr(), Some(progress))
        .await;
    assert!(matches!(fetched, Err(BlobError::Denied)), "{fetched:?}");
    assert_eq!(seen.load(Ordering::SeqCst), 0, "no byte was received");
    assert!(!c.node.blobs.has(blob.hash).await);
    assert_eq!(stored_files(&c, &hex), Vec::<PathBuf>::new());

    // Neither through C's gateway: the holder refuses, nothing is stored.
    let res = gateway_request(
        &c.gateway_url(),
        "GET",
        &format!("/blobs/{}/{hex}", a.id()),
        &[],
        None,
    )
    .await;
    assert_eq!(res.status, 403, "{}", String::from_utf8_lossy(&res.body));
    assert!(res.json["error"].is_string(), "{}", res.json);
    assert!(!c.node.blobs.has(blob.hash).await);
    assert_eq!(stored_files(&c, &hex), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn shared_blob_can_be_fetched_and_matches() {
    let (a, c) = holder_and_requester().await;
    let data = random_bytes(MIB);
    let blob = a.node.blobs.add_bytes(data.clone()).await.unwrap();
    assert_eq!(blob.size, MIB as u64);
    assert_eq!(blob.hash.as_bytes(), blake3::hash(&data).as_bytes());
    a.node.blobs.share(&blob, &c.id(), 3600).await.unwrap();

    let (progress, seen) = progress_probe();
    c.node
        .blobs
        .fetch(&blob, a.node.addr(), Some(progress))
        .await
        .expect("a shared blob is fetched");
    assert_eq!(seen.load(Ordering::SeqCst), MIB as u64);
    assert!(c.node.blobs.has(blob.hash).await);
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("fetched.bin");
    c.node.blobs.export(blob.hash, &dest).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    // Through C's gateway, which fetches it from A first.
    let other = random_bytes(300 * 1024);
    let other_blob = a.node.blobs.add_bytes(other.clone()).await.unwrap();
    a.node
        .blobs
        .share(&other_blob, &c.id(), 3600)
        .await
        .unwrap();
    let res = gateway_request(
        &c.gateway_url(),
        "GET",
        &format!("/blobs/{}/{}", a.id(), other_blob.hash.to_hex()),
        &[],
        None,
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.body, other);
    assert_eq!(res.header("content-type"), "application/octet-stream");
    assert_eq!(res.header("content-security-policy"), "sandbox");
    assert_eq!(res.header("x-content-type-options"), "nosniff");
    assert!(c.node.blobs.has(other_blob.hash).await);
}

#[tokio::test]
async fn expired_share_is_denied() {
    let (a, c) = holder_and_requester().await;
    let blob = a
        .node
        .blobs
        .add_bytes(random_bytes(64 * 1024))
        .await
        .unwrap();
    let hex = blob.hash.to_hex();
    // Expired an hour ago, well beyond the clock skew allowed.
    let now = now_unix();
    blob_grant(&a, c.id(), &hex, now - 7200, now - 3600);

    let fetched = c.node.blobs.fetch(&blob, a.node.addr(), None).await;
    assert!(matches!(fetched, Err(BlobError::Denied)), "{fetched:?}");
    assert!(!c.node.blobs.has(blob.hash).await);
    assert_eq!(stored_files(&c, &hex), Vec::<PathBuf>::new());

    // The same request with a share in force is served.
    a.node.blobs.share(&blob, &c.id(), 3600).await.unwrap();
    c.node
        .blobs
        .fetch(&blob, a.node.addr(), None)
        .await
        .expect("a share in force is served");
    assert!(c.node.blobs.has(blob.hash).await);
}

#[tokio::test]
async fn revoked_share_is_denied() {
    let (a, c) = holder_and_requester().await;
    let blob = a
        .node
        .blobs
        .add_bytes(random_bytes(64 * 1024))
        .await
        .unwrap();
    let hex = blob.hash.to_hex();
    let resource = format!("blob:{hex}");
    a.node.blobs.share(&blob, &c.id(), 3600).await.unwrap();
    let shared = a
        .node
        .store
        .grant_issued_covering(&c.id(), BlobFetch, &resource, now_unix())
        .unwrap()
        .expect("the share is a grant that covers the blob");
    assert!(
        a.node
            .store
            .revoke(&shared.grant.grant_id, now_unix())
            .unwrap()
    );

    let fetched = c.node.blobs.fetch(&blob, a.node.addr(), None).await;
    assert!(matches!(fetched, Err(BlobError::Denied)), "{fetched:?}");
    assert!(!c.node.blobs.has(blob.hash).await);
    assert_eq!(stored_files(&c, &hex), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn share_is_logged_as_event() {
    let (a, c) = holder_and_requester().await;
    let store = &a.node.store;
    let blob = a.node.blobs.add_bytes(random_bytes(1024)).await.unwrap();
    assert_eq!(store.events_count("blob.shared").unwrap(), 0);
    a.node
        .blobs
        .share(&blob, &c.id(), BLOB_SHARE_TTL_S)
        .await
        .unwrap();
    assert_eq!(store.events_count("blob.shared").unwrap(), 1);

    // The share is a grant of A, for C, to fetch exactly this blob from A.
    let hex = blob.hash.to_hex();
    let shared = store
        .grant_issued_covering(&c.id(), BlobFetch, &format!("blob:{hex}"), now_unix())
        .unwrap()
        .expect("a grant covers the blob");
    let g = &shared.grant;
    assert_eq!((g.issuer, g.subject, g.audience), (a.id(), c.id(), a.id()));
    assert_eq!(g.operations, [BlobFetch]);
    assert_eq!(g.resources, [format!("blob:{hex}")]);

    // The event names the grant, the peer and the hash, and nothing else.
    let event = store
        .events_since(&a.id(), 0)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "blob.shared")
        .expect("the blob.shared event");
    assert_eq!(
        event.payload,
        json!({"grant_id": g.grant_id, "hash": hex, "with": c.id().to_string()})
    );
    assert_eq!(store.events_verify(&a.id()).unwrap(), 1);
}

/// A resolver for Agent Cards, as a stock A2A client uses, with the crypto
/// provider its HTTP client needs (see the gateway tests).
fn card_resolver() -> AgentCardResolver {
    let _ = rustls::crypto::ring::default_provider().install_default();
    AgentCardResolver::new(None)
}

/// A provider `b` running `script` with `sh -c` for project `demo`, edited
/// by `edit`, a requester `a`, each a contact of the other, and the grant of
/// `b` that `a` holds and asks with.
async fn exec_pair(
    script: &str,
    edit: impl FnOnce(&mut pals::config::Config),
) -> (TestNode, TestNode, SignedGrant, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let path = project.path().to_path_buf();
    let b = spawn_node_with(
        WorkerCfg::Exec(ExecCfg::new("sh", &["-c", script])),
        |cfg| {
            cfg.projects.insert("demo".to_owned(), ProjectCfg { path });
            edit(cfg);
        },
    )
    .await;
    let a = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    b.add_contact("alice", &a);
    let g = grant(&b, &a, &[TaskSend, TaskRead], 3600);
    a.hold(&g);
    (a, b, g, project)
}

#[tokio::test]
async fn file_travels_both_ways_through_standard_urls() {
    let (a, b, _, _project) = exec_pair(
        r#"wc -c < "$PALS_IN_DIR/data.bin"; cp "$PALS_IN_DIR/data.bin" "$PALS_OUT_DIR/copy.bin""#,
        |_| {},
    )
    .await;
    let gateway = a.gateway_url();

    // A's stock client uploads the file to its gateway, as a plain body.
    let data = random_bytes(MIB);
    let res = TestResponse::read(
        gateway_send_raw(
            &gateway,
            "POST",
            "/blobs",
            &[],
            Some(("application/octet-stream", data.to_vec())),
        )
        .await,
    )
    .await;
    assert_eq!(res.status, 201, "{}", String::from_utf8_lossy(&res.body));
    let hash_hex = blake3::hash(&data).to_hex().to_string();
    let url = res.json["url"].as_str().expect("a url").to_owned();
    assert_eq!(url, format!("{gateway}/blobs/{}/{hash_hex}", a.id()));
    assert_eq!(res.json["hash"], hash_hex);
    assert_eq!(res.json["size"], MIB);

    // It asks B with an ordinary A2A url part, through the gateway.
    let card = card_resolver()
        .resolve(&format!("{gateway}/peers/{}", b.id()))
        .await
        .expect("resolve the card through the gateway");
    let client = A2AClientFactory::builder()
        .build()
        .create_from_card(&card)
        .await
        .expect("a client from the card");
    let request = SendMessageRequest {
        message: Message::new(
            Role::User,
            vec![
                Part::text("count the bytes and copy the file"),
                Part::url(url.clone()).with_filename("data.bin"),
            ],
        ),
        configuration: None,
        metadata: None,
        tenant: None,
    };
    let reply = client
        .send_message(&request)
        .await
        .expect("SendMessage through the gateway");
    let task = match &reply {
        SendMessageResponse::Task(task) => task,
        SendMessageResponse::Message(message) => panic!("a message, not a task: {message:?}"),
    };
    assert_eq!(task.status.state, TaskState::Completed, "{task:?}");

    // The client sees only http:// gateway URLs, never iroh-blob://.
    let seen = serde_json::to_string(&reply).unwrap();
    assert!(!seen.contains(&format!("{BLOB_SCHEME}://")), "{seen}");

    // B read the whole file, and answered with its copy as a url part.
    let parts = &task.artifacts.as_ref().expect("an artifact")[0].parts;
    assert_eq!(parts[0].as_text().map(str::trim), Some("1048576"));
    let file = parts
        .iter()
        .find(|part| matches!(part.content, PartContent::Url(_)))
        .expect("a file part");
    assert_eq!(file.filename.as_deref(), Some("copy.bin"));
    let PartContent::Url(file_url) = &file.content else {
        unreachable!()
    };
    let prefix = format!("{gateway}/blobs/{}/", b.id());
    assert!(file_url.starts_with(&prefix), "{file_url}");
    assert_eq!(&file_url[prefix.len()..], hash_hex);

    // A plain GET of that URL returns the same bytes.
    let path = file_url.strip_prefix(&gateway).unwrap();
    let res = gateway_request(&gateway, "GET", path, &[], None).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.body, data);
    assert_eq!(res.header("content-type"), "application/octet-stream");

    // Each holder shared its file with the other.
    assert_eq!(a.node.store.events_count("blob.shared").unwrap(), 1);
    assert_eq!(b.node.store.events_count("blob.shared").unwrap(), 1);

    // Streamed, the events carry the same plain URLs.
    let streamed = SendMessageRequest {
        message: Message::new(Role::User, request.message.parts.clone()),
        ..request
    };
    let events: Vec<StreamResponse> = client
        .send_streaming_message(&streamed)
        .await
        .expect("SendStreamingMessage through the gateway")
        .map(|event| event.expect("a stream event"))
        .collect()
        .await;
    let seen = serde_json::to_string(&events).unwrap();
    assert!(!seen.contains(&format!("{BLOB_SCHEME}://")), "{seen}");
    let artifact = events
        .iter()
        .find_map(|event| match event {
            StreamResponse::ArtifactUpdate(update) => Some(&update.artifact),
            _ => None,
        })
        .expect("an artifact event");
    let streamed_url = artifact
        .parts
        .iter()
        .find_map(|part| match &part.content {
            PartContent::Url(url) => Some(url),
            _ => None,
        })
        .expect("a file part");
    assert_eq!(streamed_url, file_url);
}

#[tokio::test]
async fn interrupted_transfer_resumes_and_verifies() {
    let (a, c) = holder_and_requester().await;
    let data = random_bytes(32 * MIB);
    let blob = a.node.blobs.add_bytes(data.clone()).await.unwrap();
    a.node.blobs.share(&blob, &c.id(), 3600).await.unwrap();

    // The first download is abandoned once 8 MiB have arrived.
    let enough = Arc::new(tokio::sync::Notify::new());
    let first = Arc::new(AtomicU64::new(0));
    let progress: ProgressFn = {
        let enough = Arc::clone(&enough);
        let first = Arc::clone(&first);
        Box::new(move |bytes| {
            first.fetch_max(bytes, Ordering::SeqCst);
            if bytes >= 8 * MIB as u64 {
                enough.notify_one();
            }
        })
    };
    let fetch = c.node.blobs.fetch(&blob, a.node.addr(), Some(progress));
    tokio::select! {
        biased;
        () = enough.notified() => {}
        done = fetch => panic!("the first download was not interrupted: {done:?}"),
        () = tokio::time::sleep(PATIENCE) => panic!("the first download stalled"),
    }
    let first = first.load(Ordering::SeqCst);
    assert!(first >= 8 * MIB as u64, "{first}");
    assert!(
        !c.node.blobs.has(blob.hash).await,
        "the blob is not complete"
    );

    // The second one finishes, fetching only what is missing.
    let (progress, second) = progress_probe();
    tokio::time::timeout(
        PATIENCE,
        c.node.blobs.fetch(&blob, a.node.addr(), Some(progress)),
    )
    .await
    .expect("the second download finishes")
    .expect("the second download succeeds");
    let second = second.load(Ordering::SeqCst);
    assert!(second > 0, "the second download received the rest");
    assert!(second < 32 * MIB as u64, "it fetched {second} bytes again");

    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("whole.bin");
    c.node.blobs.export(blob.hash, &dest).await.unwrap();
    let exported = std::fs::read(&dest).unwrap();
    assert_eq!(blake3::hash(&exported).as_bytes(), blob.hash.as_bytes());
    assert_eq!(exported, data);
}

/// `params` of a `SendMessage` with `text` and the file part `part`.
fn params_with_file(text: &str, part: Value) -> Value {
    let mut params = message_params(text, None);
    params["message"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(part);
    params
}

/// Sends `params` with `SendMessage` from `a` to `b` and returns the task
/// once it has ended.
async fn send(a: &TestNode, b: &TestNode, g: &SignedGrant, params: Value) -> Value {
    let res = a.rpc(b, Some(g), "SendMessage", params).await;
    assert_eq!(res.status, 200, "{}", res.json);
    let task = res.json["result"]["task"].clone();
    assert!(task["id"].is_string(), "{}", res.json);
    task
}

fn state(task: &Value) -> &str {
    task["status"]["state"].as_str().unwrap_or_default()
}

/// The text of a task's status message.
fn status_text(task: &Value) -> &str {
    task["status"]["message"]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

/// The text of the task's answer.
fn answer(task: &Value) -> &str {
    task["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

/// The file part of `blob` named `filename`, as JSON.
fn file_part(blob: &BlobRef, filename: &str) -> Value {
    serde_json::to_value(blob.to_part(filename, "application/octet-stream")).unwrap()
}

/// A command that marks in the project directory that it ran.
const MARK_RUN: &str = "touch ran";

#[tokio::test]
async fn blob_from_a_third_holder_fails_the_task() {
    let (a, b, g, project) = exec_pair(MARK_RUN, |_| {}).await;
    let c = spawn_node(WorkerCfg::Echo).await;
    b.add_contact("carol", &c);
    // C's blob, which C even shares with B: the rule, not the permission,
    // refuses it.
    let blob = c.node.blobs.add_bytes(random_bytes(4096)).await.unwrap();
    c.node.blobs.share(&blob, &b.id(), 3600).await.unwrap();

    let task = send(
        &a,
        &b,
        &g,
        params_with_file("hi", file_part(&blob, "x.bin")),
    )
    .await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob holder must be the requester");
    assert!(!b.node.blobs.has(blob.hash).await);
    assert!(!project.path().join("ran").exists(), "the command ran");
}

/// A holder that accepts connections on the blobs ALPN and never answers:
/// as unreachable, for a download, as a host that is gone, and
/// deterministic.
#[derive(Debug, Clone)]
struct NeverAnswers;

impl ProtocolHandler for NeverAnswers {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        connection.closed().await;
        Ok(())
    }
}

#[tokio::test]
async fn unreachable_holder_fails_the_task_within_the_timeout() {
    let project = tempfile::tempdir().unwrap();
    let path = project.path().to_path_buf();
    let b = spawn_node_with(
        WorkerCfg::Exec(ExecCfg::new("sh", &["-c", MARK_RUN])),
        |cfg| {
            cfg.projects.insert("demo".to_owned(), ProjectCfg { path });
            cfg.limits.blob_fetch_timeout_s = 2;
        },
    )
    .await;

    // The requester: an endpoint that asks B over the tunnel and holds the
    // blob, but never answers a download.
    let endpoint: Endpoint = loopback_endpoint().await;
    let requester = agent_id_of(&endpoint.id());
    let socket = *endpoint.bound_sockets().first().unwrap();
    let addr = EndpointAddr::new(endpoint.id()).with_ip_addr(socket);
    let _router = Router::builder(endpoint.clone())
        .accept(iroh_blobs::ALPN, NeverAnswers)
        .spawn();
    b.node
        .store
        .contact_put(&Contact {
            name: "requester".to_owned(),
            agent_id: requester,
            ticket: Some(EndpointTicket::new(addr).to_string()),
            added_at: now_unix(),
        })
        .unwrap();
    let now = now_unix();
    let g = issue_grant(
        &key_in(b.dir.path()),
        requester,
        b.id(),
        &[TaskSend, TaskRead],
        now,
        now + 3600,
    );
    b.node.store.grant_issued_put(&g).unwrap();

    let data = random_bytes(4096);
    let blob = BlobRef {
        holder: requester,
        hash: blake3::hash(&data).into(),
        size: data.len() as u64,
    };
    let body = rpc_request(
        "SendMessage",
        params_with_file("hi", file_part(&blob, "data.bin")),
    );
    let request = Request::post("/jsonrpc")
        .header("content-type", "application/json")
        .header("a2a-version", "1.0")
        .header("authorization", bearer(&g))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let started = Instant::now();
    let response = PeerHttp::new(endpoint.clone())
        .send(b.node.addr(), request)
        .await
        .expect("B answers");
    let res = TestResponse::read(response).await;
    let elapsed = started.elapsed();
    assert_eq!(res.status, 200, "{}", res.json);
    let task = &res.json["result"]["task"];
    assert_eq!(state(task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(task), "input fetch timed out");
    // The 2 s configured, not the default 120 s, bounded the wait.
    assert!(elapsed >= Duration::from_secs(2), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(60), "{elapsed:?}");
    assert!(!project.path().join("ran").exists(), "the command ran");
}

#[tokio::test]
async fn declared_size_over_limit_fails_before_fetching() {
    let (a, b, g, project) = exec_pair(MARK_RUN, |cfg| cfg.limits.max_blob_bytes = 1024).await;
    let blob = a.node.blobs.add_bytes(random_bytes(4096)).await.unwrap();
    a.node.blobs.share(&blob, &b.id(), 3600).await.unwrap();
    let hex = blob.hash.to_hex();

    let task = send(
        &a,
        &b,
        &g,
        params_with_file("hi", file_part(&blob, "big.bin")),
    )
    .await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob too large");
    // Refused on the declared size: nothing of it was fetched.
    assert!(!b.node.blobs.has(blob.hash).await);
    assert_eq!(stored_files(&b, &hex), Vec::<PathBuf>::new());
    assert!(!project.path().join("ran").exists(), "the command ran");

    // A size declared below the limit does not let more than it through.
    let understated = BlobRef { size: 512, ..blob };
    let task = send(
        &a,
        &b,
        &g,
        params_with_file("hi", file_part(&understated, "big.bin")),
    )
    .await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob too large");
    assert!(!project.path().join("ran").exists(), "the command ran");

    // Nor does a copy that B holds already.
    b.node
        .blobs
        .fetch(&blob, a.node.addr(), None)
        .await
        .expect("B fetches the whole blob itself");
    assert!(b.node.blobs.has(blob.hash).await);
    let task = send(
        &a,
        &b,
        &g,
        params_with_file("hi", file_part(&understated, "big.bin")),
    )
    .await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob too large");
    assert!(!project.path().join("ran").exists(), "the command ran");
}

#[tokio::test]
async fn hostile_filenames_stay_inside_in_dir() {
    let (a, b, g, _project) = exec_pair(r#"ls -A "$PALS_IN_DIR""#, |_| {}).await;
    let data = random_bytes(2048);
    let blob = a.node.blobs.add_bytes(data).await.unwrap();
    a.node.blobs.share(&blob, &b.id(), 3600).await.unwrap();

    // Reduced to its last component, inside the task's in_dir.
    let task = send(
        &a,
        &b,
        &g,
        params_with_file("hi", file_part(&blob, "../../etc/passwd")),
    )
    .await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task).trim(), "passwd");
    let data_dir = b.dir.path();
    for escaped in ["etc/passwd", "tasks/etc/passwd", "passwd", "tasks/passwd"] {
        assert!(!data_dir.join(escaped).exists(), "{escaped} was written");
    }

    // Names with nothing left, or only a step up, fail the task.
    for name in ["..", ".", "a/..", "dir/", "/", "..\\..\\.."] {
        let task = send(&a, &b, &g, params_with_file("hi", file_part(&blob, name))).await;
        assert_eq!(state(&task), "TASK_STATE_FAILED", "{name:?}: {task}");
        assert_eq!(status_text(&task), "invalid file name", "{name:?}");
    }

    // An empty name is no name in A2A's protobuf form: the file is named
    // after its hash.
    let task = send(&a, &b, &g, params_with_file("hi", file_part(&blob, ""))).await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task).trim(), blob.hash.to_hex());
}

#[tokio::test]
async fn a_local_copy_needs_the_holder_to_prove_it_has_the_blob() {
    // W runs a command that sends its input back, the best a thief could
    // hope for.
    let (r, w, g, project) = exec_pair(
        r#"touch ran; cp "$PALS_IN_DIR/secret.bin" "$PALS_OUT_DIR/secret.bin""#,
        |_| {},
    )
    .await;
    let ran = project.path().join("ran");
    let secret = random_bytes(256 * 1024);
    // W holds H as its own upload; R knows H, and claims to hold it.
    let own = w.node.blobs.add_bytes(secret.clone()).await.unwrap();
    let claimed = BlobRef {
        holder: r.id(),
        ..own
    };
    assert!(!r.node.blobs.has(own.hash).await);
    let shared = |node: &TestNode| node.node.store.events_count("blob.shared").unwrap();

    // Without a share from R: refused, as any download from R would be.
    let params = || params_with_file("hi", file_part(&claimed, "secret.bin"));
    let task = send(&r, &w, &g, params()).await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob fetch denied by its holder");
    assert!(!ran.exists(), "the command ran");
    assert_eq!(shared(&w), 0);

    // R shares the hash it does not hold: it cannot prove it holds it.
    r.node.blobs.share(&claimed, &w.id(), 3600).await.unwrap();
    let task = send(&r, &w, &g, params()).await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob fetch failed");
    assert!(!ran.exists(), "the command ran");
    assert_eq!(shared(&w), 0);
    assert!(!r.node.blobs.has(own.hash).await);

    // Once R does hold it, it proves so, and W uses its own copy.
    let held = r.node.blobs.add_bytes(secret).await.unwrap();
    assert_eq!(held.hash, own.hash);
    let task = send(&r, &w, &g, params()).await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert!(ran.exists(), "the command did not run");
    assert_eq!(shared(&w), 1);
}

#[tokio::test]
async fn too_many_input_blobs_fail_before_fetching() {
    let (a, b, g, project) = exec_pair(MARK_RUN, |_| {}).await;
    let blob = a.node.blobs.add_bytes(random_bytes(2048)).await.unwrap();
    a.node.blobs.share(&blob, &b.id(), 3600).await.unwrap();
    let with_files = |count: usize| {
        let mut params = message_params("hi", None);
        let parts = params["message"]["parts"].as_array_mut().unwrap();
        for n in 0..count {
            parts.push(file_part(&blob, &format!("f{n}.bin")));
        }
        params
    };

    let task = send(&a, &b, &g, with_files(MAX_INPUT_BLOBS + 1)).await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "too many input blobs");
    assert!(!b.node.blobs.has(blob.hash).await, "it was fetched");
    assert!(!project.path().join("ran").exists(), "the command ran");

    // The limit itself is allowed.
    let task = send(&a, &b, &g, with_files(MAX_INPUT_BLOBS)).await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
}

#[tokio::test]
async fn inputs_over_the_limit_together_fail_before_fetching() {
    let (a, b, g, project) = exec_pair(MARK_RUN, |cfg| cfg.limits.max_blob_bytes = 1024).await;
    let mut params = message_params("hi", None);
    let mut blobs = Vec::new();
    for n in 0..2 {
        // Each within the limit; together, beyond it.
        let blob = a.node.blobs.add_bytes(random_bytes(600)).await.unwrap();
        a.node.blobs.share(&blob, &b.id(), 3600).await.unwrap();
        let part = file_part(&blob, &format!("f{n}.bin"));
        params["message"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(part);
        blobs.push(blob);
    }

    let task = send(&a, &b, &g, params).await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "inputs too large");
    for blob in &blobs {
        assert!(!b.node.blobs.has(blob.hash).await, "it was fetched");
    }
    assert!(!project.path().join("ran").exists(), "the command ran");

    // Naming one blob again, under a smaller size, does not get more of it
    // in: each part is held to its own declaration.
    let blob = blobs[0];
    let mut params = message_params("hi", None);
    for (n, size) in [600, 1].into_iter().enumerate() {
        let part = file_part(&BlobRef { size, ..blob }, &format!("f{n}.bin"));
        params["message"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(part);
    }
    let task = send(&a, &b, &g, params).await;
    assert_eq!(state(&task), "TASK_STATE_FAILED", "{task}");
    assert_eq!(status_text(&task), "blob too large");
    assert!(!project.path().join("ran").exists(), "the command ran");
}

#[tokio::test]
async fn input_tags_are_released_after_the_task() {
    let (a, b, g, _project) = exec_pair(r#"wc -c < "$PALS_IN_DIR/data.bin""#, |_| {}).await;
    let blob = a.node.blobs.add_bytes(random_bytes(4096)).await.unwrap();
    a.node.blobs.share(&blob, &b.id(), 3600).await.unwrap();

    let task = send(
        &a,
        &b,
        &g,
        params_with_file("hi", file_part(&blob, "data.bin")),
    )
    .await;
    assert_eq!(state(&task), "TASK_STATE_COMPLETED", "{task}");
    assert_eq!(answer(&task).trim(), "4096");

    // No tag holds the input any longer: the store may reclaim it.
    let tags = b.node.blobs.tags().await.unwrap();
    assert!(
        tags.iter().all(|(_, hash)| *hash != blob.hash),
        "a tag still holds the input: {tags:?}"
    );
    assert!(
        tags.iter().all(|(name, _)| !name.starts_with("input/")),
        "{tags:?}"
    );
    // The holder keeps its upload.
    let kept = a.node.blobs.tags().await.unwrap();
    assert!(kept.iter().any(|(_, hash)| *hash == blob.hash), "{kept:?}");
}

#[test]
fn rewrite_roundtrip_and_foreign_urls_untouched() {
    let base = "http://127.0.0.1:7420";
    let holder = AgentKey::generate().id().to_string();
    let holder = holder.as_str();
    let hash = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
    let wire_url = format!("{BLOB_SCHEME}://{holder}/{hash}");
    let gateway_url = format!("{base}/blobs/{holder}/{hash}");
    let meta = json!({ BLOB_META_KEY: {"hash": hash, "size": 3} });
    let wire = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"task": {
            "id": "t1",
            "artifacts": [{"artifactId": "a1", "parts": [
                {"text": format!("see {wire_url}")},
                {"url": wire_url, "filename": "out.bin", "metadata": meta},
                {"url": "https://example.com/output.txt", "filename": "output.txt"},
                {"url": format!("{BLOB_SCHEME}://not-an-agent/{hash}")},
            ]}],
        }},
    });

    let mut value = wire.clone();
    to_gateway(&mut value, base);
    let parts = &value["result"]["task"]["artifacts"][0]["parts"];
    assert_eq!(parts[1]["url"], gateway_url);
    assert_eq!(parts[1]["metadata"], meta);
    // Text is never rewritten, nor URLs that are not PALS blobs.
    assert_eq!(parts[0]["text"], format!("see {wire_url}"));
    assert_eq!(parts[2]["url"], "https://example.com/output.txt");
    assert_eq!(parts[3], wire["result"]["task"]["artifacts"][0]["parts"][3]);

    // to_wire is the inverse.
    to_wire(&mut value, base);
    assert_eq!(value, wire);
    let mut gateway = wire.clone();
    to_gateway(&mut gateway, base);
    let mut back = gateway.clone();
    to_wire(&mut back, base);
    to_gateway(&mut back, base);
    assert_eq!(back, gateway);

    // Another gateway's URLs, or a path that is not a blob, stay.
    for foreign in [
        format!("http://127.0.0.1:7421/blobs/{holder}/{hash}"),
        format!("{base}/blobs/{holder}/{hash}/extra"),
        format!("{base}/blobs/{holder}"),
        format!("{base}/peers/{holder}/{hash}"),
        "https://example.com/output.txt".to_owned(),
    ] {
        let mut value = json!({"parts": [{"url": foreign}]});
        to_wire(&mut value, base);
        assert_eq!(value["parts"][0]["url"], foreign);
        to_gateway(&mut value, base);
        assert_eq!(value["parts"][0]["url"], foreign);
    }

    // The URL of a blob is its holder and hash, both ways.
    let (parsed_holder, parsed_hash) = BlobRef::parse_url(&wire_url).expect("a blob URL");
    assert_eq!(parsed_holder.to_string(), holder);
    assert_eq!(parsed_hash.to_hex(), hash);
    let blob = BlobRef {
        holder: parsed_holder,
        hash: parsed_hash,
        size: 3,
    };
    assert_eq!(blob.to_url(), wire_url);
    let part = blob.to_part("out.bin", "application/octet-stream");
    assert_eq!(BlobRef::from_part(&part), Some(blob));
    assert_eq!(BlobRef::parse_url(&gateway_url), None);
}
