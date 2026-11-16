//! The control API the harness drives a node with, on [`CONTROL_ADDR`]
//! inside its container, and the loop that runs its commands on the
//! backend.
//!
//! The HTTP handlers only pass commands to that loop, which calls the
//! backend's methods one at a time from `run_node`'s own task: the trait's
//! futures need not be `Send`, and a call waits for the one before it. The
//! routes, all JSON:
//!
//! - `POST /search {"capability", "limit"}`: the backend's search, each
//!   candidate verified, one per agent, the valid ones first, at most
//!   `limit`: `{"elapsed_ms", "first_valid_ms", "first_valid_source",
//!   "candidates": [{"agent_id", "valid", "reason", "direct_addrs",
//!   "contact_seq"}], "requests"}`. `first_valid_ms` is the earliest
//!   `found_ms` of a valid candidate (`first_valid_source: "backend"`) or,
//!   when no valid candidate has one, `elapsed_ms`, when the search
//!   returned (`"completion"`); both are `null` when none is valid.
//!   `requests` is `null` when the backend does not count them;
//! - `POST /announce {"capabilities"}`: issues the records again, with the
//!   node's addresses now and the next `seq`, and announces them:
//!   `{"seq", "direct_addrs"}`;
//! - `POST /unannounce`: `{}`;
//! - `GET /stats`: the [`BackendStats`] fields and `rss_kib`;
//! - `GET /healthz`: `{"name", "role", "agent_id", "announced",
//!   "announce_error", "declared"}`, served only once the backend started
//!   and the announcement of `--announce` was attempted: a node that
//!   answers is ready. `announced` is what the last announce that worked
//!   listed, empty after `/unannounce`; `announce_error` is why the last
//!   announce failed, `null` once one works or after `/unannounce`.
//!
//! A backend error is `500 {"error"}`; a request the API cannot take, `400`
//! or `422`.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::records::{
    dedupe, issue_announce, issue_contact, local_addrs, tamper_signature, verify,
};
use crate::stats::{BackendStats, rss_kib};
use crate::{Backend, Candidate, Declared, NodeConfig, RawSearch, Role};

/// Where the control API listens, inside the node's container.
pub const CONTROL_ADDR: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 7600);
/// The largest `limit` a search takes.
const MAX_LIMIT: usize = 64;

/// The node behind the control API.
pub(crate) struct Node<B> {
    pub backend: B,
    pub cfg: NodeConfig,
    pub key: AgentKey,
    pub rogue: bool,
    /// The `seq` of the records issued last.
    pub seq: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest {
    capability: String,
    limit: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct SearchAnswer {
    elapsed_ms: u64,
    first_valid_ms: Option<u64>,
    first_valid_source: Option<&'static str>,
    candidates: Vec<Candidate>,
    requests: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnnounceRequest {
    capabilities: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Announced {
    seq: u64,
    direct_addrs: Vec<String>,
}

/// `{}`.
#[derive(Debug, Serialize)]
struct Done {}

#[derive(Debug, Serialize)]
struct StatsAnswer {
    #[serde(flatten)]
    stats: BackendStats,
    rss_kib: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct Health {
    name: String,
    role: Role,
    agent_id: AgentId,
    announced: Vec<String>,
    announce_error: Option<String>,
    declared: &'static [Declared],
}

type Reply<T> = oneshot::Sender<Result<T, String>>;
type SharedHealth = Arc<RwLock<Health>>;

enum Command {
    Search(SearchRequest, Reply<SearchAnswer>),
    Announce(AnnounceRequest, Reply<Announced>),
    Unannounce(Reply<Done>),
    Stats(Reply<StatsAnswer>),
}

#[derive(Clone)]
struct App {
    commands: mpsc::Sender<Command>,
    health: SharedHealth,
}

impl<B: Backend> Node<B> {
    /// Announces `announce` if there is any, then serves the control API
    /// until the process ends.
    pub(crate) async fn serve(mut self, announce: Vec<String>) -> anyhow::Result<()> {
        let health = Arc::new(RwLock::new(Health {
            name: self.cfg.name.clone(),
            role: self.cfg.role,
            agent_id: self.key.id(),
            announced: Vec::new(),
            announce_error: None,
            declared: B::DECLARED,
        }));
        if !announce.is_empty() {
            let result = self.announce(&announce).await;
            note_announce(&health, &announce, &result);
        }
        let (commands, mut queue) = mpsc::channel(16);
        let app = App {
            commands,
            health: Arc::clone(&health),
        };
        let router = Router::new()
            .route("/search", post(search))
            .route("/announce", post(announce_route))
            .route("/unannounce", post(unannounce))
            .route("/stats", get(stats))
            .route("/healthz", get(healthz))
            .with_state(app);
        let listener = tokio::net::TcpListener::bind(CONTROL_ADDR)
            .await
            .with_context(|| format!("cannot bind the control API on {CONTROL_ADDR}"))?;
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        tracing::info!(name = %self.cfg.name, "control API on {CONTROL_ADDR}");

        while let Some(command) = queue.recv().await {
            match command {
                Command::Search(request, reply) => {
                    let _ = reply.send(self.search(request).await.map_err(|e| format!("{e:#}")));
                }
                Command::Announce(request, reply) => {
                    let result = self.announce(&request.capabilities).await;
                    note_announce(&health, &request.capabilities, &result);
                    let _ = reply.send(result.map_err(|e| format!("{e:#}")));
                }
                Command::Unannounce(reply) => {
                    let result = self.backend.unannounce().await.map(|()| Done {});
                    if result.is_ok() {
                        let mut health = health.write().unwrap_or_else(|e| e.into_inner());
                        health.announced.clear();
                        health.announce_error = None;
                    }
                    let _ = reply.send(result.map_err(|e| format!("{e:#}")));
                }
                Command::Stats(reply) => {
                    let _ = reply.send(Ok(StatsAnswer {
                        stats: self.backend.stats(),
                        rss_kib: rss_kib(),
                    }));
                }
            }
        }
        server.await?.context("the control API stopped")
    }

    async fn search(&mut self, request: SearchRequest) -> anyhow::Result<SearchAnswer> {
        let started = Instant::now();
        let found = self
            .backend
            .search(&request.capability, request.limit)
            .await?;
        let elapsed_ms = millis(started);
        Ok(checked(
            &request.capability,
            request.limit,
            found,
            now_unix(),
            elapsed_ms,
        ))
    }

    /// Issues both records with the addresses now and announces them; with
    /// `--rogue`, the contact's signature does not verify.
    async fn announce(&mut self, capabilities: &[String]) -> anyhow::Result<Announced> {
        let (contact, announce) = self.issue(capabilities)?;
        let direct_addrs = contact.doc.iroh.direct_addrs.clone();
        self.backend
            .announce(capabilities, &contact, &announce)
            .await
            .context("the backend did not announce")?;
        tracing::info!(
            seq = self.seq,
            ?direct_addrs,
            ?capabilities,
            rogue = self.rogue,
            "announced"
        );
        Ok(Announced {
            seq: self.seq,
            direct_addrs,
        })
    }

    fn issue(
        &mut self,
        capabilities: &[String],
    ) -> anyhow::Result<(Signed<ContactRecord>, Signed<AnnounceRecord>)> {
        let now = now_unix();
        // The spikes keep no state across restarts, and the harness restarts
        // nodes: a `seq` from the clock never goes back, as a DHT that keeps
        // the newest record (BEP 44) requires.
        let seq = (self.seq + 1).max(u64::try_from(now).unwrap_or(0));
        let contact = issue_contact(&self.key, &local_addrs(self.cfg.listen), seq, now)?;
        let contact = if self.rogue {
            tamper_signature(&contact)
        } else {
            contact
        };
        let announce = issue_announce(&self.key, capabilities, seq, now)?;
        self.seq = seq;
        Ok((contact, announce))
    }
}

/// What `/healthz` says after an announce of `capabilities`.
fn note_announce(
    health: &SharedHealth,
    capabilities: &[String],
    result: &anyhow::Result<Announced>,
) {
    let mut health = health.write().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(_) => {
            health.announced = capabilities.to_vec();
            health.announce_error = None;
        }
        Err(error) => {
            tracing::error!("the announcement failed: {error:#}");
            health.announce_error = Some(format!("{error:#}"));
        }
    }
}

/// The answer to a search that returned `found` after `elapsed_ms`: every
/// candidate verified at `now_unix`, one per agent, the valid ones first
/// (so that invalid ones never push a valid one past `limit`), at most
/// `limit`.
fn checked(
    capability: &str,
    limit: usize,
    found: RawSearch,
    now_unix: i64,
    elapsed_ms: u64,
) -> SearchAnswer {
    let mut any_valid = false;
    let mut first_found: Option<u64> = None;
    let mut verified = Vec::with_capacity(found.candidates.len());
    for raw in found.candidates {
        let found_ms = raw.found_ms;
        let candidate = verify(capability, raw, now_unix);
        if candidate.valid {
            any_valid = true;
            first_found = match (first_found, found_ms) {
                (Some(first), Some(ms)) => Some(first.min(ms)),
                (first, ms) => first.or(ms),
            };
        }
        verified.push(candidate);
    }
    let mut candidates = dedupe(verified);
    // Stable: the order among valid ones, and among invalid ones, stays.
    candidates.sort_by_key(|c| !c.valid);
    candidates.truncate(limit);
    let (first_valid_ms, first_valid_source) = match (any_valid, first_found) {
        (false, _) => (None, None),
        (true, Some(ms)) => (Some(ms), Some("backend")),
        (true, None) => (Some(elapsed_ms), Some("completion")),
    };
    SearchAnswer {
        elapsed_ms,
        first_valid_ms,
        first_valid_source,
        candidates,
        requests: found.requests,
    }
}

async fn search(State(app): State<App>, Json(request): Json<SearchRequest>) -> Response {
    if request.limit == 0 || request.limit > MAX_LIMIT {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("limit is 1 to {MAX_LIMIT}")})),
        )
            .into_response();
    }
    ask(&app, |reply| Command::Search(request, reply)).await
}

async fn announce_route(State(app): State<App>, Json(request): Json<AnnounceRequest>) -> Response {
    ask(&app, |reply| Command::Announce(request, reply)).await
}

async fn unannounce(State(app): State<App>) -> Response {
    ask(&app, Command::Unannounce).await
}

async fn stats(State(app): State<App>) -> Response {
    ask(&app, Command::Stats).await
}

async fn healthz(State(app): State<App>) -> Json<Health> {
    Json(app.health.read().unwrap_or_else(|e| e.into_inner()).clone())
}

/// Passes a command to the node's loop and answers with what it replies.
async fn ask<T: Serialize>(app: &App, command: impl FnOnce(Reply<T>) -> Command) -> Response {
    let (reply, answer) = oneshot::channel();
    if app.commands.send(command(reply)).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match answer.await {
        Ok(Ok(answer)) => Json(answer).into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error})),
        )
            .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawCandidate, issue_announce, issue_contact, tamper_signature};

    const OCR: &str = "agentnet:capability:v1:vision.ocr";
    const ISSUED: i64 = 1_800_000_000;

    fn raw(seed: u8, rogue: bool, found_ms: Option<u64>) -> RawCandidate {
        let key = AgentKey::from_bytes(&[seed; 32]);
        let addr: SocketAddr = "192.0.2.10:4433".parse().unwrap();
        let contact = issue_contact(&key, &[addr], 1, ISSUED).unwrap();
        let contact = if rogue {
            tamper_signature(&contact)
        } else {
            contact
        };
        let announce = issue_announce(&key, &[OCR.to_owned()], 1, ISSUED).unwrap();
        RawCandidate {
            contact: Some(contact.to_json()),
            announce: Some(announce.to_json()),
            found_ms,
        }
    }

    fn search(candidates: Vec<RawCandidate>, limit: usize) -> SearchAnswer {
        let found = RawSearch {
            candidates,
            requests: Some(3),
        };
        checked(OCR, limit, found, ISSUED + 60, 900)
    }

    #[test]
    fn the_first_valid_is_the_earliest_found_valid_one() {
        // The invalid one was found first, and does not count.
        let answer = search(
            vec![
                raw(1, true, Some(5)),
                raw(2, false, Some(40)),
                raw(3, false, Some(20)),
            ],
            8,
        );
        assert_eq!(answer.first_valid_ms, Some(20));
        assert_eq!(answer.first_valid_source, Some("backend"));
        assert_eq!(answer.elapsed_ms, 900);
        assert_eq!(answer.requests, Some(3));
    }

    #[test]
    fn with_no_time_from_the_backend_the_first_valid_is_at_completion() {
        let answer = search(vec![raw(1, false, None)], 8);
        assert_eq!(answer.first_valid_ms, Some(900));
        assert_eq!(answer.first_valid_source, Some("completion"));
        let none = search(vec![raw(1, true, Some(5))], 8);
        assert_eq!((none.first_valid_ms, none.first_valid_source), (None, None));
    }

    #[test]
    fn invalid_candidates_never_crowd_out_valid_ones() {
        let answer = search(
            vec![
                raw(1, true, None),
                raw(2, true, None),
                raw(3, false, None),
                raw(4, false, None),
            ],
            2,
        );
        assert_eq!(answer.candidates.len(), 2);
        assert!(answer.candidates.iter().all(|c| c.valid));
        let ids: Vec<_> = answer.candidates.iter().map(|c| c.agent_id).collect();
        let expected: Vec<_> = [3, 4]
            .map(|seed| Some(AgentKey::from_bytes(&[seed; 32]).id()))
            .to_vec();
        assert_eq!(ids, expected);
    }
}
