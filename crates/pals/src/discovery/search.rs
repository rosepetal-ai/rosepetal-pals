//! [`Searcher`]: choosing a provider among this agent's contacts before
//! delegating to it (design §8), within the limits of design §7.3, with no
//! candidate running any work until the one chosen is asked.
//!
//! **Candidates.** Every contact, PALS peers and plain A2A agents, or those
//! the request names; each agent once (a PALS peer by its agent id, a plain
//! agent by its origin); first those of which this agent holds a grant that
//! serves tasks in the request's project (in any project when it names
//! none), then by name; this agent never. Only the first
//! [`DiscoveryLimits::max_candidates`] are contacted: the coverage of a
//! search that leaves some out is partial, `candidate limit`.
//!
//! **Each candidate** takes one of the [`DiscoveryLimits::max_concurrent_queries`]
//! places of the whole daemon, waiting for it no longer than the search's
//! budget lasts, and then has `min(per_candidate_timeout, the budget left)`
//! for all of it:
//!
//! 1. **Its card**, stage 1 ([`stage_one`]). A PALS peer's public card, and
//!    its extended card when this agent holds a grant for the project,
//!    verified as signed by the peer ([`PeerCards`]). A plain A2A agent's
//!    card ([`fetch_plain_card`]), which goes on only when its signature is
//!    [`Verified`](SignatureStatus::Verified): an unverifiable or unsigned
//!    one is discarded, `card not verified`, and can still be asked by its
//!    name. Its extended card is never read: a verified public card says
//!    nothing of it. Nothing is taken from the absence of
//!    `securityRequirements`, which one canonical form drops.
//! 2. **Its offer**, PALS peers only. The query
//!    ([`SearchRequest::query`]) is waited for the time the candidate has
//!    left, and says so in `reply_timeout_ms`, at most 3000 ms
//!    ([`ServiceQuery::reply_timeout`](pals_core::discovery::query::ServiceQuery::reply_timeout)):
//!    with the default 3 s per candidate, the two are the same; never the
//!    gateway's own times. It presents the grant this agent holds of the
//!    peer's for the project (its newest that serves tasks when the request
//!    names no project), and then carries the project and the inventory ids
//!    of the requirements, required and preferred. A peer of which this
//!    agent holds no such grant gets neither grant, project nor ids, and a
//!    required id counts as unknown, which discards it (§7.1, §9.9).
//!    Whatever the client refuses (an offer of another agent, for another
//!    requester or query, expired, badly signed; a record that goes back)
//!    is a discard with its reason. The announce record of the reply must
//!    name the public card read, and the offer one of the cards read: else
//!    the card is discarded. Plain agents are never queried.
//! 3. **Its benchmark reports**, when the request wants some and the offer
//!    is compatible: those the offer refers to of the profile wanted, still
//!    in force, at most [`MAX_REPORTS_PER_CANDIDATE`], read with
//!    `GetBenchmarkReport` in the candidate's time, verified
//!    ([`DiscoveryClient::benchmark_report`]), and counted only when
//!    [usable](comparable) for what is wanted, by this agent's clock.
//!    Reports not read in time are none, not a discard.
//!
//! When the budget ends nothing more starts, and what had not finished is
//! discarded `time budget`, the coverage partial; a candidate that had its
//! whole time and did not finish is discarded `timeout`. Every request,
//! byte, and the time the stages took is counted ([`SearchMetrics`]).
//!
//! **Ranking**: stage 2 ([`stage_two`]), at this agent's clock.
//!
//! **Choosing** ([`Searcher::choose_and_ask`]): down the ranking, at most
//! [`MAX_ASKED`] candidates are asked. A PALS peer of which this agent
//! holds no grant that serves tasks is never asked (§8, a permit checked
//! before execution): it is set aside, `no grant held`, and does not count
//! among them. An offer expired by this agent's clock, without skew, is
//! renewed once if the budget allows, and the question goes with the
//! offer it has ([`OfferRef`]). A refusal for `offer_expired` or
//! `offer_stale` renews it once, if the budget allows and it was not
//! renewed already, and asks again; `offer_unknown`, another refusal of the
//! first request, before the peer named any task, or a task `REJECTED` as
//! `busy` goes on to the next. Any other end of the task is the outcome.
//! An ask that cannot reach the peer, or that is refused once the peer has
//! a task for it (a follow-up call refused, a task id this agent does not
//! take), stops: its question may still run there, it is kept for `pals
//! task resume`, and asking another could run it twice.
//!
//! **Quotas**, for the whole daemon: at most [`DiscoveryLimits::max_searches`]
//! searches at once, the next refused at once, `too many searches`, and
//! the queries of all of them share the same places, so many searches never
//! get past the limits of one. Nothing here passes a query on: a candidate
//! answers for itself.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use iroh::EndpointAddr;
use pals_core::benchmark::{Comparability, MeasurementKind, Wanted, comparable};
use pals_core::card_jws::{CardHash, SignatureStatus, card_hash};
use pals_core::clock::Clock;
use pals_core::discovery::query::{
    MAX_QUERY_BYTES, MAX_REPLY_TIMEOUT_MS, Met, OFFER_EXPIRED, OFFER_STALE, OFFER_UNKNOWN,
    OfferRef, ServiceOffer, new_query_id,
};
use pals_core::discovery::selection::{
    Candidate, Coverage, Discarded, SearchMetrics, SearchOutcome, SearchRequest, Stage, stage_one,
    stage_two,
};
use pals_core::grant::{SignedGrant, parse_timestamp};
use pals_core::identity::AgentId;
use pals_core::store::{A2aContact, PLAIN_PEER_PREFIX, PeerRef, Store};
use serde::Serialize;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::cards::{PeerCards, signed_card_json};
use super::client::{DiscoveryClient, DiscoveryError};
use crate::config::DiscoveryCfg;
use crate::gateway::plain::{fetch_plain_card, keep_plain_card};
use crate::gateway::serves_tasks;
use crate::net::peer_addr;
use crate::outbox::{AskError, AskOutcome, AskRequest, INVALID_TASK_ID, Outbox};

/// The most candidates [`Searcher::choose_and_ask`] asks.
pub const MAX_ASKED: usize = 3;
/// The most benchmark reports read of one candidate.
pub const MAX_REPORTS_PER_CANDIDATE: usize = 4;
/// Why a candidate that had its whole time did not finish.
const TIMEOUT: &str = "timeout";
/// Why a candidate was cut, or never started, when the budget ended.
const TIME_BUDGET: &str = "time budget";
/// Why some contacts were left out.
const CANDIDATE_LIMIT: &str = "candidate limit";
/// Why a peer was not asked.
const NO_GRANT: &str = "no grant held";
/// The reason a task refused for want of a place ends with
/// (`WorkerExecutor`).
const BUSY: &str = "busy";
/// The longest reason quoted from a refusal.
const MAX_REASON_CHARS: usize = 200;

/// What a search may take (design §7.3), from `[discovery]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryLimits {
    /// Candidates contacted per search.
    pub max_candidates: usize,
    /// Queries in flight at once, for the whole daemon.
    pub max_concurrent_queries: usize,
    /// The time a candidate has, card, query and reports.
    pub per_candidate_timeout: Duration,
    /// The time a search has.
    pub total_budget: Duration,
    /// Searches at once, for the whole daemon.
    pub max_searches: usize,
}

impl Default for DiscoveryLimits {
    /// Design §7.3: 8 candidates, 3 queries at once, 3 s per candidate,
    /// 15 s in all; and 4 searches at once.
    fn default() -> Self {
        Self {
            max_candidates: 8,
            max_concurrent_queries: 3,
            per_candidate_timeout: Duration::from_secs(3),
            total_budget: Duration::from_secs(15),
            max_searches: 4,
        }
    }
}

impl DiscoveryLimits {
    /// The limits `cfg` configures; 4 searches at once.
    pub fn from_config(cfg: &DiscoveryCfg) -> Self {
        Self {
            max_candidates: cfg.max_candidates,
            max_concurrent_queries: cfg.max_concurrent_queries,
            per_candidate_timeout: Duration::from_millis(cfg.per_candidate_timeout_ms),
            total_budget: Duration::from_millis(cfg.total_budget_ms),
            ..Self::default()
        }
    }
}

/// Why a search, or a choice, has no outcome.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SearchError {
    /// [`DiscoveryLimits::max_searches`] are running already.
    #[error("too many searches")]
    TooManySearches,
    /// The request cannot be a query.
    #[error("invalid search: {0}")]
    Invalid(String),
    /// This agent's store could not be read.
    #[error("{0}")]
    Local(String),
    /// No candidate took the task: those asked, with why they did not, and
    /// those set aside.
    #[error("no candidate took the task")]
    NoProvider {
        tried: Vec<(String, String)>,
        discarded: Vec<Discarded>,
    },
    /// Asking `peer` failed so that no other may be asked: its question is
    /// kept, or the outbox failed.
    #[error("asking {peer} failed: {error}")]
    Ask {
        peer: String,
        error: AskError,
        tried: Vec<(String, String)>,
        discarded: Vec<Discarded>,
    },
}

/// The candidate a question went to, and how it ended.
#[derive(Debug, Clone, Serialize)]
pub struct Chosen {
    /// With the offer it was asked with.
    pub candidate: Candidate,
    pub outcome: AskOutcome,
    /// The candidates asked before it, and why they did not take it.
    pub tried: Vec<(String, String)>,
    /// The candidates not asked: `no grant held`.
    pub discarded: Vec<Discarded>,
}

/// What a [`Searcher`] works with.
pub struct SearcherParts {
    pub store: Arc<Store>,
    pub cards: PeerCards,
    pub discovery: DiscoveryClient,
    /// The questions this agent asks, through its gateway.
    pub outbox: Outbox,
    /// The client for plain A2A agents (`crate::gateway::plain::http_client`).
    pub plain_http: reqwest::Client,
    /// The node's clock, which offers and reports are judged by.
    pub clock: Arc<dyn Clock>,
    pub limits: DiscoveryLimits,
}

/// Searches this agent's contacts for a provider (see the [module](self)).
/// Clones share the quotas: one for the whole daemon.
#[derive(Clone)]
pub struct Searcher {
    inner: Arc<Inner>,
}

struct Inner {
    parts: SearcherParts,
    /// One permit per query in flight, of every search.
    queries: Arc<Semaphore>,
    /// One permit per search running.
    searches: Arc<Semaphore>,
}

impl fmt::Debug for Searcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Searcher")
            .field("limits", &self.inner.parts.limits)
            .finish_non_exhaustive()
    }
}

/// A search's outcome, with what choosing needs to ask its candidates again.
struct Searched {
    outcome: SearchOutcome,
    /// By candidate name, the PALS peers ranked.
    targets: HashMap<String, Target>,
}

/// A PALS candidate, as its offer is asked for.
#[derive(Clone)]
struct Target {
    id: AgentId,
    addr: EndpointAddr,
    grant: Option<SignedGrant>,
    /// The inventory ids go to it: it holds a grant for the project.
    with_ids: bool,
    /// The public card's hash, which its announce record must name.
    public_hash: CardHash,
    /// Every card read: the offer must name one.
    hashes: Vec<CardHash>,
}

/// A contact to evaluate.
struct Entry {
    name: String,
    kind: EntryKind,
}

enum EntryKind {
    Pals(Box<PalsPeer>),
    Plain(A2aContact),
}

/// A PALS contact, with the grant this agent would present to it.
struct PalsPeer {
    id: AgentId,
    addr: EndpointAddr,
    grant: Option<SignedGrant>,
    /// The grant serves tasks in the request's project.
    grant_held: bool,
}

impl Entry {
    fn grant_held(&self) -> bool {
        matches!(&self.kind, EntryKind::Pals(peer) if peer.grant_held)
    }
}

/// What evaluating one candidate cost, and where it got to.
struct Tally {
    contacted: bool,
    messages: usize,
    bytes_sent: u64,
    bytes_received: u64,
    matching: Duration,
    stage: Stage,
}

impl Tally {
    fn new() -> Self {
        Self {
            contacted: false,
            messages: 0,
            bytes_sent: 0,
            bytes_received: 0,
            matching: Duration::ZERO,
            stage: Stage::Card,
        }
    }
}

/// One candidate evaluated.
struct Evaluated {
    tally: Tally,
    finished: Instant,
    result: Result<(Candidate, Option<Target>), Discarded>,
}

/// How asking a candidate went.
enum Attempt {
    Took(Box<Candidate>, AskOutcome),
    Next(String),
    Failed(AskError),
}

impl Searcher {
    pub fn new(parts: SearcherParts) -> Self {
        let limits = parts.limits;
        Self {
            inner: Arc::new(Inner {
                parts,
                queries: Arc::new(Semaphore::new(limits.max_concurrent_queries)),
                searches: Arc::new(Semaphore::new(limits.max_searches)),
            }),
        }
    }

    /// The limits it keeps.
    pub fn limits(&self) -> &DiscoveryLimits {
        &self.inner.parts.limits
    }

    /// How many searches are running now.
    pub fn searches_running(&self) -> usize {
        self.limits().max_searches - self.inner.searches.available_permits()
    }

    /// The candidates for `req`, ranked, those discarded with why, the
    /// coverage and what it took (see the [module](self)). Nothing runs on
    /// any of them.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchOutcome, SearchError> {
        let _running = self.start()?;
        Ok(self.run(&req, Instant::now()).await?.outcome)
    }

    /// Searches as [`Searcher::search`] does, and asks `question` of the
    /// best candidate that takes it, waiting up to `wait` for its answer
    /// (see the [module](self)).
    pub async fn choose_and_ask(
        &self,
        req: SearchRequest,
        question: String,
        wait: Duration,
    ) -> Result<Chosen, SearchError> {
        self.search_then_ask(req, question, wait).await?.1
    }

    /// [`Searcher::choose_and_ask`], with the search's outcome as well.
    pub async fn search_then_ask(
        &self,
        req: SearchRequest,
        question: String,
        wait: Duration,
    ) -> Result<(SearchOutcome, Result<Chosen, SearchError>), SearchError> {
        let started = Instant::now();
        let searched = {
            let _running = self.start()?;
            self.run(&req, started).await?
        };
        let budget_end = started + self.limits().total_budget;
        let chosen = self
            .choose(&req, &searched, &question, wait, budget_end)
            .await;
        Ok((searched.outcome, chosen))
    }

    /// A search's place, or [`SearchError::TooManySearches`].
    fn start(&self) -> Result<tokio::sync::OwnedSemaphorePermit, SearchError> {
        Arc::clone(&self.inner.searches)
            .try_acquire_owned()
            .map_err(|_| SearchError::TooManySearches)
    }

    async fn run(&self, req: &SearchRequest, started: Instant) -> Result<Searched, SearchError> {
        check(req)?;
        let limits = self.limits();
        let budget_end = started + limits.total_budget;
        let (entries, limited, mut discarded) = self.entries(req).await?;
        let experience = self.experience().await?;
        let evaluations = entries
            .into_iter()
            .map(|entry| self.evaluate(req, entry, budget_end, &experience));
        let mut metrics = SearchMetrics::default();
        let mut matching = Duration::ZERO;
        let mut candidates = Vec::new();
        let mut finished = HashMap::new();
        let mut targets = HashMap::new();
        for evaluated in join_all(evaluations).await {
            let tally = &evaluated.tally;
            metrics.contacted += usize::from(tally.contacted);
            metrics.messages += tally.messages;
            metrics.bytes_sent += tally.bytes_sent;
            metrics.bytes_received += tally.bytes_received;
            matching += tally.matching;
            match evaluated.result {
                Ok((candidate, target)) => {
                    finished.insert(candidate.peer.clone(), evaluated.finished);
                    if let Some(target) = target {
                        targets.insert(candidate.peer.clone(), target);
                    }
                    candidates.push(candidate);
                }
                Err(discard) => discarded.push(discard),
            }
        }
        let ranking = Instant::now();
        let (ranked, more) = stage_two(candidates, req, self.inner.parts.clock.now_unix());
        matching += ranking.elapsed();
        discarded.extend(more);

        let mut reasons = Vec::new();
        if limited {
            reasons.push(CANDIDATE_LIMIT.to_owned());
        }
        if discarded.iter().any(|d| d.reason == TIME_BUDGET) {
            reasons.push(TIME_BUDGET.to_owned());
        }
        let coverage = if reasons.is_empty() {
            Coverage::Complete
        } else {
            Coverage::Partial { reasons }
        };
        metrics.time_to_first_valid_ms = ranked
            .iter()
            .filter_map(|candidate| finished.get(&candidate.peer))
            .min()
            .map(|at| millis(at.duration_since(started)));
        metrics.matching_us = u64::try_from(matching.as_micros()).unwrap_or(u64::MAX);
        metrics.elapsed_ms = millis(started.elapsed());
        Ok(Searched {
            outcome: SearchOutcome {
                ranked,
                discarded,
                coverage,
                metrics,
            },
            targets,
        })
    }

    /// The contacts to evaluate, at most `max_candidates` of them, whether
    /// some were left out, and the names `req.peers` gives that name no
    /// one, discarded.
    async fn entries(
        &self,
        req: &SearchRequest,
    ) -> Result<(Vec<Entry>, bool, Vec<Discarded>), SearchError> {
        let store = Arc::clone(&self.inner.parts.store);
        let (names, project) = (req.peers.clone(), req.project.clone());
        let now = self.inner.parts.clock.now_unix();
        let max = self.limits().max_candidates;
        let read = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            let mut unknown = Vec::new();
            match names {
                Some(names) => {
                    for name in names {
                        match store.resolve_peer(&name)? {
                            Some(PeerRef::Pals(id)) => {
                                let contact = store.contact_by_id(&id)?;
                                let ticket = contact.as_ref().and_then(|c| c.ticket.clone());
                                let shown = contact.map_or(name, |c| c.name);
                                entries.push(pals_entry(&store, shown, id, ticket, &project, now)?);
                            }
                            Some(PeerRef::Plain(contact)) => entries.push(Entry {
                                name: contact.name.clone(),
                                kind: EntryKind::Plain(contact),
                            }),
                            None => unknown.push(Discarded {
                                peer: name,
                                stage: Stage::Card,
                                reason: "unknown peer".to_owned(),
                            }),
                        }
                    }
                }
                None => {
                    for contact in store.contacts()? {
                        let entry = pals_entry(
                            &store,
                            contact.name,
                            contact.agent_id,
                            contact.ticket,
                            &project,
                            now,
                        )?;
                        entries.push(entry);
                    }
                    for contact in store.a2a_contacts()? {
                        entries.push(Entry {
                            name: contact.name.clone(),
                            kind: EntryKind::Plain(contact),
                        });
                    }
                }
            }
            anyhow::Ok((entries, unknown, *store.owner()))
        })
        .await;
        let (mut entries, unknown, own) = match read {
            Ok(Ok(read)) => read,
            Ok(Err(err)) => {
                return Err(SearchError::Local(format!(
                    "cannot read the contacts: {err:#}"
                )));
            }
            Err(err) => {
                return Err(SearchError::Local(format!(
                    "cannot read the contacts: {err}"
                )));
            }
        };
        // Each agent once, and never this one.
        let mut seen_ids = Vec::new();
        let mut seen_origins = Vec::new();
        entries.retain(|entry| match &entry.kind {
            EntryKind::Pals(peer) if peer.id == own || seen_ids.contains(&peer.id) => false,
            EntryKind::Pals(peer) => {
                seen_ids.push(peer.id);
                true
            }
            EntryKind::Plain(contact) if seen_origins.contains(&contact.origin) => false,
            EntryKind::Plain(contact) => {
                seen_origins.push(contact.origin.clone());
                true
            }
        });
        entries.sort_by(|x, y| {
            y.grant_held()
                .cmp(&x.grant_held())
                .then_with(|| x.name.cmp(&y.name))
        });
        let limited = entries.len() > max;
        entries.truncate(max);
        Ok((entries, limited, unknown))
    }

    /// The questions completed and failed with each peer, by its key.
    async fn experience(&self) -> Result<HashMap<String, (u32, u32)>, SearchError> {
        let store = Arc::clone(&self.inner.parts.store);
        match tokio::task::spawn_blocking(move || store.outbox_outcomes_by_peer()).await {
            Ok(Ok(outcomes)) => Ok(outcomes),
            Ok(Err(err)) => Err(SearchError::Local(format!(
                "cannot read the outbox: {err:#}"
            ))),
            Err(err) => Err(SearchError::Local(format!("cannot read the outbox: {err}"))),
        }
    }

    /// `entry` evaluated, in a place of the daemon's and within its time.
    async fn evaluate(
        &self,
        req: &SearchRequest,
        entry: Entry,
        budget_end: Instant,
        experience: &HashMap<String, (u32, u32)>,
    ) -> Evaluated {
        let mut tally = Tally::new();
        let place =
            tokio::time::timeout_at(budget_end, Arc::clone(&self.inner.queries).acquire_owned())
                .await;
        let Ok(Ok(_place)) = place else {
            return Evaluated {
                tally,
                finished: Instant::now(),
                result: Err(discard(&entry.name, Stage::Card, TIME_BUDGET)),
            };
        };
        let (deadline, cut) = cut_at(
            Instant::now(),
            self.limits().per_candidate_timeout,
            budget_end,
        );
        let evaluated = tokio::time::timeout_at(
            deadline,
            self.candidate(req, &entry, deadline, cut, experience, &mut tally),
        )
        .await;
        let mut result = match evaluated {
            Ok(result) => result,
            Err(_) => Err(discard(&entry.name, tally.stage, cut)),
        };
        // Reports not read in time are none: the candidate stays.
        if let (Ok((candidate, Some(target))), Some(wanted)) = (&mut result, &req.benchmark)
            && let Some(offer) = candidate.offer.as_ref().filter(|offer| offer.compatible)
        {
            let offer = offer.clone();
            let best = tokio::time::timeout_at(
                deadline,
                self.best_report(target, &offer, wanted, deadline, &mut tally),
            )
            .await;
            candidate.benchmark = best.ok().flatten();
        }
        Evaluated {
            tally,
            finished: Instant::now(),
            result,
        }
    }

    /// Stages 1 and the offer of `entry`.
    async fn candidate(
        &self,
        req: &SearchRequest,
        entry: &Entry,
        deadline: Instant,
        cut: &'static str,
        experience: &HashMap<String, (u32, u32)>,
        tally: &mut Tally,
    ) -> Result<(Candidate, Option<Target>), Discarded> {
        let name = entry.name.as_str();
        tally.contacted = true;
        match &entry.kind {
            EntryKind::Pals(peer) => {
                let PalsPeer {
                    id,
                    addr,
                    grant,
                    grant_held,
                } = &**peer;
                let cards = &self.inner.parts.cards;
                let (public, size) = cards
                    .public_sized(id, addr.clone())
                    .await
                    .map_err(|err| discard(name, Stage::Card, &format!("card: {err}")))?;
                tally.messages += 1;
                tally.bytes_received += size;
                let public_hash = card_hash(&signed_card_json(&public));
                let mut hashes = vec![public_hash.clone()];
                let mut card = public.card;
                if let (true, Some(grant)) = (*grant_held, grant) {
                    let (extended, size) = cards
                        .extended_sized(id, addr.clone(), grant)
                        .await
                        .map_err(|err| {
                            discard(name, Stage::Card, &format!("extended card: {err}"))
                        })?;
                    tally.messages += 1;
                    tally.bytes_received += size;
                    hashes.push(card_hash(&signed_card_json(&extended)));
                    card = extended.card;
                }
                let skill = self.first_stage(&card, req, name, tally)?;
                tally.stage = Stage::Offer;
                let target = Target {
                    id: *id,
                    addr: addr.clone(),
                    grant: grant.clone(),
                    with_ids: *grant_held,
                    public_hash,
                    hashes,
                };
                let offer = self
                    .offer(req, &target, deadline, cut, tally)
                    .await
                    .map_err(|(stage, why)| discard(name, stage, &why))?;
                let card_hash = target
                    .hashes
                    .last()
                    .expect("the public card at least")
                    .clone();
                let candidate = Candidate {
                    peer: name.to_owned(),
                    agent_id: Some(*id),
                    skill_id: offer.skill_id.clone().or(skill),
                    preferred_met: preferred_met(&offer),
                    offer: Some(offer),
                    card_hash,
                    card_signature: SignatureStatus::Verified {
                        kid: id.to_string(),
                    },
                    benchmark: None,
                    experience: experience.get(&id.to_string()).copied().unwrap_or_default(),
                    grant_held: *grant_held,
                };
                Ok((candidate, Some(target)))
            }
            EntryKind::Plain(contact) => {
                let http = &self.inner.parts.plain_http;
                let plain = fetch_plain_card(http, contact)
                    .await
                    .map_err(|err| discard(name, Stage::Card, &format!("card: {err}")))?;
                tally.messages += 1;
                tally.bytes_received +=
                    serde_json::to_vec(&plain.json).map_or(0, |json| json.len() as u64);
                keep_plain_card(&self.inner.parts.store, &contact.name, &plain).await;
                if !matches!(plain.signature, SignatureStatus::Verified { .. }) {
                    return Err(discard(name, Stage::Card, "card not verified"));
                }
                let skill = self.first_stage(&plain.card, req, name, tally)?;
                let key = format!("{PLAIN_PEER_PREFIX}{}", contact.name);
                let candidate = Candidate {
                    peer: name.to_owned(),
                    agent_id: None,
                    skill_id: skill,
                    offer: None,
                    card_hash: plain.hash,
                    card_signature: plain.signature,
                    preferred_met: 0,
                    benchmark: None,
                    experience: experience.get(&key).copied().unwrap_or_default(),
                    grant_held: false,
                };
                Ok((candidate, None))
            }
        }
    }

    /// [`stage_one`] of `card`, timed.
    fn first_stage(
        &self,
        card: &a2a::AgentCard,
        req: &SearchRequest,
        name: &str,
        tally: &mut Tally,
    ) -> Result<Option<String>, Discarded> {
        let started = Instant::now();
        let skill = stage_one(card, req);
        tally.matching += started.elapsed();
        skill.map_err(|why| discard(name, Stage::Card, &why))
    }

    /// `target`'s offer for `req`, asked by `deadline`; a refusal, and the
    /// stage it discards at, otherwise.
    async fn offer(
        &self,
        req: &SearchRequest,
        target: &Target,
        deadline: Instant,
        cut: &'static str,
        tally: &mut Tally,
    ) -> Result<ServiceOffer, (Stage, String)> {
        // The candidate's time left is what it is waited for, and what the
        // query says, as far as its field says (3000 ms): with the default
        // 3 s per candidate, exactly `reply_timeout()`.
        let left = deadline.saturating_duration_since(Instant::now());
        let reply_ms = u32::try_from(left.as_millis())
            .unwrap_or(u32::MAX)
            .clamp(1, MAX_REPLY_TIMEOUT_MS);
        let query = req.query(&new_query_id(), reply_ms, target.with_ids);
        tally.messages += 1;
        let (offer, reply) = self
            .inner
            .parts
            .discovery
            .query(
                &target.id,
                target.addr.clone(),
                &query,
                target.grant.as_ref(),
                left,
            )
            .await
            .map_err(|err| (Stage::Offer, failure(&err, cut)))?;
        tally.bytes_sent += reply.bytes_sent;
        tally.bytes_received += reply.bytes_received;
        if reply.announce.doc.agent_card_hash != target.public_hash {
            return Err((
                Stage::Card,
                "the card does not match its announce record".to_owned(),
            ));
        }
        if !target.hashes.contains(&offer.doc.agent_card_hash) {
            return Err((
                Stage::Card,
                "the offer names another card than the one read".to_owned(),
            ));
        }
        Ok(offer.doc)
    }

    /// The best report `offer` refers to that is usable for `wanted`: its
    /// kind and the value of its first result.
    async fn best_report(
        &self,
        target: &Target,
        offer: &ServiceOffer,
        wanted: &Wanted,
        deadline: Instant,
        tally: &mut Tally,
    ) -> Option<(String, f64)> {
        let clock = &self.inner.parts.clock;
        let now = clock.now_unix();
        let references = offer
            .benchmark_refs
            .iter()
            .filter(|reference| {
                reference.profile == wanted.profile
                    && parse_timestamp(&reference.expires_at).is_some_and(|at| now < at)
            })
            .take(MAX_REPORTS_PER_CANDIDATE);
        // A measurement before a declared specification, the most recent
        // first, as the offer lists them.
        let mut best: Option<(bool, String, f64)> = None;
        for reference in references {
            let left = deadline.saturating_duration_since(Instant::now());
            tally.messages += 1;
            let fetched = self
                .inner
                .parts
                .discovery
                .benchmark_report(
                    &target.id,
                    target.addr.clone(),
                    reference,
                    target.grant.as_ref(),
                    left,
                )
                .await;
            // A report not had is none; why is logged, the peer's words
            // escaped.
            let (report, reply) = match fetched {
                Ok(fetched) => fetched,
                Err(DiscoveryError::Report(why)) => {
                    tracing::warn!(
                        peer = %target.id,
                        benchmark_id = ?reference.benchmark_id,
                        why,
                        "a peer's benchmark report is refused"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::debug!(
                        peer = %target.id,
                        benchmark_id = ?reference.benchmark_id,
                        ?error,
                        "a peer's benchmark report was not had"
                    );
                    continue;
                }
            };
            tally.bytes_sent += reply.bytes_sent;
            tally.bytes_received += reply.bytes_received;
            let report = report.doc;
            if comparable(&report, wanted, clock.now_unix()) != Comparability::Usable {
                continue;
            }
            let Some(first) = report.results.first() else {
                continue;
            };
            let declared = report.measurement_kind == MeasurementKind::DeclaredSpec;
            if best
                .as_ref()
                .is_none_or(|(was_declared, ..)| *was_declared && !declared)
            {
                best = Some((
                    declared,
                    report.measurement_kind.as_str().to_owned(),
                    first.value,
                ));
            }
            if !declared {
                break;
            }
        }
        best.map(|(_, kind, value)| (kind, value))
    }

    /// Asks the ranked candidates of `searched`, as the [module](self)
    /// says, until one takes `question`.
    async fn choose(
        &self,
        req: &SearchRequest,
        searched: &Searched,
        question: &str,
        wait: Duration,
        budget_end: Instant,
    ) -> Result<Chosen, SearchError> {
        let mut tried = Vec::new();
        let mut discarded = Vec::new();
        let mut asked = 0;
        for candidate in &searched.outcome.ranked {
            if candidate.agent_id.is_some() && !candidate.grant_held {
                discarded.push(discard(&candidate.peer, Stage::Offer, NO_GRANT));
                continue;
            }
            if asked == MAX_ASKED {
                break;
            }
            asked += 1;
            let target = searched.targets.get(&candidate.peer);
            let attempt = self
                .attempt(req, candidate.clone(), target, question, wait, budget_end)
                .await;
            match attempt {
                Attempt::Took(candidate, outcome) => {
                    return Ok(Chosen {
                        candidate: *candidate,
                        outcome,
                        tried,
                        discarded,
                    });
                }
                Attempt::Next(why) => tried.push((candidate.peer.clone(), why)),
                Attempt::Failed(error) => {
                    return Err(SearchError::Ask {
                        peer: candidate.peer.clone(),
                        error,
                        tried,
                        discarded,
                    });
                }
            }
        }
        Err(SearchError::NoProvider { tried, discarded })
    }

    /// Asks `candidate` `question`, renewing its offer as the
    /// [module](self) says.
    async fn attempt(
        &self,
        req: &SearchRequest,
        mut candidate: Candidate,
        target: Option<&Target>,
        question: &str,
        wait: Duration,
        budget_end: Instant,
    ) -> Attempt {
        let mut renewed = false;
        let now = self.inner.parts.clock.now_unix();
        if let (Some(target), Some(offer)) = (target, &candidate.offer)
            && offer.expired(now)
        {
            if Instant::now() >= budget_end {
                return Attempt::Next(OFFER_EXPIRED.to_owned());
            }
            match self.renew(req, target, &candidate, budget_end).await {
                Ok(fresh) => candidate = fresh,
                Err(why) => return Attempt::Next(why),
            }
            renewed = true;
        }
        loop {
            let peer = candidate
                .agent_id
                .map_or_else(|| candidate.peer.clone(), |id| id.to_string());
            let ask = AskRequest {
                peer,
                question: question.to_owned(),
                attachments: Vec::new(),
                wait,
                offer: candidate.offer.as_ref().map(OfferRef::of),
            };
            let mut kept = None;
            let asked = self
                .inner
                .parts
                .outbox
                .ask_kept(ask, &CancellationToken::new(), |id| {
                    kept = Some(id.to_owned());
                })
                .await;
            let reason = match asked {
                Ok(outcome)
                    if outcome.state == "TASK_STATE_REJECTED"
                        && outcome.answer.as_deref() == Some(BUSY) =>
                {
                    return Attempt::Next(BUSY.to_owned());
                }
                Ok(outcome) => return Attempt::Took(Box::new(candidate), outcome),
                Err(AskError::Rejected(reason)) => reason,
                Err(error) => return Attempt::Failed(error),
            };
            // Another may be asked only if no task of this one's exists:
            // else the question could run twice.
            let offer_refused = [OFFER_UNKNOWN, OFFER_EXPIRED, OFFER_STALE]
                .iter()
                .any(|refusal| reason.contains(refusal));
            match self
                .before_any_task(kept.as_deref(), &reason, offer_refused)
                .await
            {
                Ok(true) => {}
                Ok(false) => return Attempt::Failed(AskError::Rejected(reason)),
                Err(error) => return Attempt::Failed(error),
            }
            let renewable = (reason.contains(OFFER_EXPIRED) || reason.contains(OFFER_STALE))
                && !renewed
                && Instant::now() < budget_end;
            let Some(target) = target.filter(|_| renewable) else {
                return Attempt::Next(quoted(&reason));
            };
            match self.renew(req, target, &candidate, budget_end).await {
                Ok(fresh) => candidate = fresh,
                Err(why) => return Attempt::Next(why),
            }
            renewed = true;
        }
    }

    /// Whether the question kept as `message_id`, refused for `reason`, was
    /// refused before any task of the peer's existed, so that another
    /// candidate may be asked without running it twice: nothing was kept,
    /// or the outbox knows no task of it and either the peer refused the
    /// offer it names (`offer_refused`: a provider checks the offer only
    /// for a request that is new, so none of its copies runs) or the very
    /// first exchange was refused. A peer that named a task id this agent
    /// does not take ([`INVALID_TASK_ID`]) had a task.
    async fn before_any_task(
        &self,
        message_id: Option<&str>,
        reason: &str,
        offer_refused: bool,
    ) -> Result<bool, AskError> {
        let Some(message_id) = message_id else {
            return Ok(true);
        };
        if reason == INVALID_TASK_ID {
            return Ok(false);
        }
        let (store, id) = (Arc::clone(&self.inner.parts.store), message_id.to_owned());
        let row = match tokio::task::spawn_blocking(move || store.outbox_get(&id)).await {
            Ok(Ok(row)) => row,
            Ok(Err(err)) => return Err(AskError::Local(format!("the outbox failed: {err:#}"))),
            Err(err) => return Err(AskError::Local(format!("the outbox failed: {err}"))),
        };
        Ok(row.is_some_and(|row| row.task_id.is_none() && (offer_refused || row.attempts == 1)))
    }

    /// `candidate` with a new offer of `target`'s, asked within its time and
    /// the budget, and still passing stage 2; why not otherwise.
    async fn renew(
        &self,
        req: &SearchRequest,
        target: &Target,
        candidate: &Candidate,
        budget_end: Instant,
    ) -> Result<Candidate, String> {
        let place =
            tokio::time::timeout_at(budget_end, Arc::clone(&self.inner.queries).acquire_owned())
                .await;
        let Ok(Ok(_place)) = place else {
            return Err(TIME_BUDGET.to_owned());
        };
        let (deadline, cut) = cut_at(
            Instant::now(),
            self.limits().per_candidate_timeout,
            budget_end,
        );
        let mut tally = Tally::new();
        let offer =
            tokio::time::timeout_at(deadline, self.offer(req, target, deadline, cut, &mut tally))
                .await
                .map_err(|_| cut.to_owned())?
                .map_err(|(_, why)| format!("renewal: {why}"))?;
        let fresh = Candidate {
            preferred_met: preferred_met(&offer),
            skill_id: offer
                .skill_id
                .clone()
                .or_else(|| candidate.skill_id.clone()),
            offer: Some(offer),
            ..candidate.clone()
        };
        let (mut kept, discarded) = stage_two(vec![fresh], req, self.inner.parts.clock.now_unix());
        match (kept.pop(), discarded.into_iter().next()) {
            (Some(fresh), _) => Ok(fresh),
            (None, Some(why)) => Err(format!("renewed offer: {}", why.reason)),
            (None, None) => Err("renewed offer refused".to_owned()),
        }
    }
}

/// The PALS contact `name`, `id` dialed with `ticket`, with the grant this
/// agent holds of it that serves tasks in `project`, or its newest that
/// serves tasks when `project` names none. A grant of another project is
/// never presented: the query would be answered in that project's scope,
/// which says nothing of this one.
fn pals_entry(
    store: &Store,
    name: String,
    id: AgentId,
    ticket: Option<String>,
    project: &Option<String>,
    now_unix: i64,
) -> anyhow::Result<Entry> {
    let grants: Vec<SignedGrant> = store
        .grants_held_for(&id, now_unix)?
        .into_iter()
        .filter(|g| serves_tasks(&g.grant))
        .collect();
    let for_project = project
        .as_ref()
        .and_then(|project| grants.iter().position(|g| g.grant.project == *project));
    let grant_held = match project {
        Some(_) => for_project.is_some(),
        None => !grants.is_empty(),
    };
    let grant = match project {
        Some(_) => for_project.and_then(|at| grants.get(at)),
        None => grants.first(),
    }
    .cloned();
    Ok(Entry {
        name,
        kind: EntryKind::Pals(Box::new(PalsPeer {
            id,
            addr: peer_addr(&id, ticket.as_deref()),
            grant,
            grant_held,
        })),
    })
}

/// Whether `req` makes a query: its own rules, and at most
/// [`MAX_QUERY_BYTES`].
fn check(req: &SearchRequest) -> Result<(), SearchError> {
    let query = req.query("q-check", MAX_REPLY_TIMEOUT_MS, true);
    query
        .validate()
        .map_err(|err| SearchError::Invalid(err.to_string()))?;
    if query.to_jcs().len() > MAX_QUERY_BYTES {
        return Err(SearchError::Invalid(format!(
            "the query is larger than {MAX_QUERY_BYTES} bytes: ask for fewer things"
        )));
    }
    Ok(())
}

/// The deadline of a candidate that starts at `now`, and why it is cut
/// there: its own time, or the budget's end when that comes first.
fn cut_at(now: Instant, per_candidate: Duration, budget_end: Instant) -> (Instant, &'static str) {
    let own = now + per_candidate;
    if own <= budget_end {
        (own, TIMEOUT)
    } else {
        (budget_end, TIME_BUDGET)
    }
}

/// Why a discovery call got no offer, in this agent's words: `cut` when it
/// ran out of time.
fn failure(err: &DiscoveryError, cut: &str) -> String {
    match err {
        DiscoveryError::Timeout => cut.to_owned(),
        DiscoveryError::Unreachable(_) => "unreachable".to_owned(),
        DiscoveryError::TooLarge => "frame too large".to_owned(),
        DiscoveryError::Protocol(_) => "protocol error".to_owned(),
        DiscoveryError::BadRecord(_) => "bad record".to_owned(),
        DiscoveryError::Regression { .. } => "record regression".to_owned(),
        DiscoveryError::Remote(refusal) => format!("refused: {:?}", refusal.code()),
        DiscoveryError::Offer(refused) => format!("offer refused: {refused}"),
        DiscoveryError::Report(why) => format!("report refused: {why}"),
    }
}

/// `reason`, a refusal that may quote a peer, as printable ASCII and short.
fn quoted(reason: &str) -> String {
    crate::printable_ascii(reason, MAX_REASON_CHARS)
}

/// The preferences `offer` says are met.
fn preferred_met(offer: &ServiceOffer) -> usize {
    offer
        .requirement_results
        .iter()
        .filter(|result| !result.mandatory && result.result == Met::Met)
        .count()
}

fn discard(peer: &str, stage: Stage, reason: &str) -> Discarded {
    Discarded {
        peer: peer.to_owned(),
        stage,
        reason: reason.to_owned(),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
