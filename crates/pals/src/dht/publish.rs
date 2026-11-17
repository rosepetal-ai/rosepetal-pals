//! What a node publishes in the DHT, and how it finds the providers of a
//! capability (`docs/protocols/agentnet-kad-0.md`, section 8). Both run on
//! the swarm's task ([`Task`]), as many at once as there are: every query
//! belongs to one round or one lookup, by its `QueryId`.
//!
//! **What is published.** With `[dht] announce`, the node's
//! [`PublishedRecords`]: its signed contact and announce in force, and the
//! capabilities of its public services, which that announce lists (H3-D4).
//! Without it, nothing, not even the contact. A node that stops sends
//! nothing: the routers' copies expire on their own, `record_ttl` after the
//! last round that reached them, with `libp2p-kad`'s own expiry. A copy of
//! the contact or the announce never outlives its signature: it goes with
//! the smaller of `record_ttl` and what its signature has left by the
//! node's clock, which `libp2p-kad` sends as the record's TTL, and a router
//! keeps the smaller of that, its own `record_ttl` and what the signature
//! has left by its own clock, whoever put the copy.
//!
//! **A round of publication.** There is one when the node enters the
//! network, one every `republish`, one each time a record is issued again
//! ([`PublishedRecords::changes`]: an address change, or the renewal at
//! 12 h) and, after a round that reached no router, one after
//! `retry_after`. A round the timer makes due while another runs waits for
//! it, and starts as it ends: every round ends in bounded time, each of its
//! queries lasting `query_timeout` at most. A round of records issued again
//! does not wait: it supersedes the one running, which ends there, and
//! finishes the queries it left (`query_mut(id).finish()`). What the
//! superseded round had not provided yet goes to `failed_capabilities`, and
//! `last_error` says a new round superseded it, until the new one ends. A
//! query of it still looking for its routers then sends to those it found,
//! which the routers' rules make harmless: an older record is kept out, and
//! a provider record of a capability the newer announce no longer lists is
//! refused, or dropped once that announce arrives. A round:
//! 1. stops providing the capabilities the records no longer list, and
//!    holds the node's own provider record of each of the others, in the
//!    records' order, with the addresses the node declares;
//! 2. puts the contact and the announce with `Quorum::All`. A put reached
//!    a router once one answered: `Ok`, or `QuorumFailed` or `Timeout` with
//!    some `success`. `libp2p-kad` 0.49 answers a put the router refused
//!    too, so the router's status says whether it stored it;
//! 3. once the announce's put ended, takes the capabilities to the
//!    routers, [`CAPABILITIES_AT_ONCE`] at a time in the records' order,
//!    the next starting as one ends. For each, it finds the routers closest
//!    to its key (`get_closest_peers`) and puts the announce at them with
//!    `Quorum::All` (`put_record_to`): the put waits for every one of them.
//!    When it reached at least one, the node provides the capability
//!    (`start_providing`). The announce travels with every provider record:
//!    each router that stores one already holds the announce that
//!    [`validate::check_provider`] asks for, even when the routers closest
//!    to the announce's key are not those closest to the capability's.
//!
//! A capability fails alone, and the others go on with nothing undone,
//! when the store has no room for it, when no router closest to its key
//! answered the announce, or when its provider record never went (its
//! query ended first). It goes to `failed_capabilities`, in the records'
//! order, for the next round to try again, and `last_error` says how many
//! failed, unless something worse went wrong. Those a superseded round had
//! not provided yet go there too. A round in which no router answered ends
//! with `last_error` too, and the next comes `retry_after` later.
//!
//! **A lookup** ([`super::Dht::providers`]) asks for the providers of a
//! capability's key and, for each provider as it arrives, up to its limit,
//! for its contact and its announce. The agents it is told it knows
//! ([`super::Dht::providers_except`]) are left out as they arrive: neither
//! taken nor counted, nor looked up further. Each copy is checked with
//! [`validate::contact_for`] and [`validate::announce_for`] at the node's
//! clock, and the valid one of highest `seq` stays. A provider with a valid
//! contact and a valid announce that lists the capability is a candidate;
//! the others are discarded, with why: the refusal of the record they lack
//! (the announce's when they lack both), or that none came. The node never finds itself. At its
//! deadline, a lookup answers with what it has and finishes its queries.
//! Its requests are those of its queries, the dials [`NoDial`] refused
//! them left out, each counted by its query
//! ([`Kademlia`](super::swarm::Kademlia)).
//!
//! [`NoDial`]: super::swarm::NoDial

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use libp2p::PeerId;
use libp2p::kad::{
    AddProviderError, AddProviderOk, GetClosestPeersError, GetClosestPeersOk, GetProvidersError,
    GetProvidersOk, GetRecordError, GetRecordOk, PutRecordError, PutRecordOk, QueryId, QueryStats,
    Quorum, Record, RecordKey,
};
use pals_core::grant::parse_timestamp;
use pals_core::identity::AgentId;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::{Signed, SignedDoc};
use serde::Serialize;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

use super::keys::{Slot, agent_of, announce_key, contact_key, provider_key, slot};
use super::swarm::Task;
use super::validate::{self, Rejection};
use super::{DhtCandidate, DhtError, DhtLookup};

/// What a node publishes in the DHT: its own signed records in force, and
/// the public capabilities its announce lists.
pub trait PublishedRecords: Send + Sync + 'static {
    /// The contact and the announce in force, each as its signed JSON.
    fn current(&self) -> (Vec<u8>, Vec<u8>);
    /// The capabilities of the public services, as the inventory writes
    /// them: those the announce lists.
    fn capabilities(&self) -> Vec<String>;
    /// Rises with every record issued again.
    fn changes(&self) -> watch::Receiver<u64>;
}

/// What the node publishes, as `pals dht status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicationStatus {
    /// The public capabilities of the last round; before the first ends,
    /// those of the records in force.
    pub capabilities: Vec<String>,
    /// When the last round ended, by the node's clock.
    pub last_round_unix: Option<i64>,
    /// Why the last round fell short, if it did; the worst reason: the
    /// node's own store had no room for its records, a new round superseded
    /// it, no router answered any of its puts, or how many capabilities it
    /// did not provide.
    pub last_error: Option<String>,
    /// The capabilities the last round did not provide, in the records'
    /// order: no room in the node's own store, no router closest to their
    /// key answered their announce, their provider record never went, or a
    /// new round superseded it before they went. The next round tries them
    /// again.
    pub failed_capabilities: Vec<String>,
}

/// The node's publication: what it publishes, and its rounds.
pub(crate) struct Publication {
    records: Arc<dyn PublishedRecords>,
    /// The records' changes, until nothing can change them any more.
    pub(super) changes: Option<watch::Receiver<u64>>,
    /// The keys of the capabilities the node provides now.
    provided: Vec<RecordKey>,
    /// The round running, if one is.
    round: Option<Round>,
    /// When the next round is due; none before the node entered the
    /// network, and none while a round that came due waits for the one
    /// running.
    next_at: Option<Instant>,
    /// Whether a round came due while one ran: it starts as that one ends.
    due: bool,
    status: PublicationStatus,
}

impl Publication {
    pub(super) fn new(records: Arc<dyn PublishedRecords>) -> Self {
        let status = PublicationStatus {
            capabilities: records.capabilities(),
            last_round_unix: None,
            last_error: None,
            failed_capabilities: Vec::new(),
        };
        Self {
            changes: Some(records.changes()),
            records,
            provided: Vec::new(),
            round: None,
            next_at: None,
            due: false,
            status,
        }
    }

    pub(super) fn status(&self) -> PublicationStatus {
        self.status.clone()
    }
}

/// A round of publication.
struct Round {
    /// The capabilities the records list.
    capabilities: Vec<String>,
    /// The announce, which goes with every provider record.
    announce: Record,
    /// The capabilities waiting for their turn: the announce's put to end,
    /// and a place among [`CAPABILITIES_AT_ONCE`], in the records' order.
    waiting: VecDeque<(String, RecordKey)>,
    /// The capabilities on their way, at most [`CAPABILITIES_AT_ONCE`].
    on_their_way: usize,
    /// What each of its queries running is for.
    queries: HashMap<QueryId, Step>,
    /// Whether a router answered any of its puts.
    reached: bool,
    /// The capabilities not provided: no room in the node's store, no
    /// router that answered their announce, a provider record that never
    /// went, or a new round that superseded this one first.
    failed: Vec<String>,
    error: Option<String>,
    /// Whether a new round superseded it.
    superseded: bool,
}

/// The capabilities a round takes to the routers at a time: finding their
/// routers, putting the announce at them and sending the provider record.
/// So a round of many capabilities never queues behind itself for the
/// node's own streams to a router
/// ([`OUTBOUND_STREAMS_PER_PEER`](super::muxer::OUTBOUND_STREAMS_PER_PEER)),
/// which count in each request's time.
const CAPABILITIES_AT_ONCE: usize = 4;

/// What a query of a round is for.
enum Step {
    /// The put of the contact.
    Contact,
    /// The put of the announce, which the capabilities wait for.
    Announce,
    /// The routers closest to a capability's key.
    Closest(String, RecordKey),
    /// The announce, put at those routers.
    AnnounceAt(String, RecordKey),
    /// The provider record of a capability, sent.
    Providing(String),
}

/// The lookups running.
#[derive(Default)]
pub(crate) struct Lookups {
    next: u64,
    running: HashMap<u64, Lookup>,
    /// Which lookup each query running is of.
    queries: HashMap<QueryId, u64>,
}

/// A lookup of a capability's providers.
struct Lookup {
    key: RecordKey,
    /// The agents it leaves out.
    known: HashSet<AgentId>,
    limit: usize,
    deadline: Instant,
    reply: oneshot::Sender<Result<DhtLookup, DhtError>>,
    /// Its `get_providers`, while it runs.
    providers_query: Option<QueryId>,
    /// Whether a router answered its `get_providers`.
    answered: bool,
    /// The providers it took, at most `limit`, in the order they arrived.
    providers: Vec<Provider>,
    /// Its `get_record`s running: of which provider, and of which record.
    records: HashMap<QueryId, (usize, Slot)>,
    /// The requests of its queries that ended.
    requests: u64,
}

/// A provider a lookup found, and its newest valid records so far.
struct Provider {
    agent: AgentId,
    contact: Option<(Signed<ContactRecord>, Vec<u8>)>,
    announce: Option<(Signed<AnnounceRecord>, Vec<u8>)>,
    /// Why the last copy of its contact refused was.
    contact_refused: Option<Rejection>,
    /// Why the last copy of its announce refused was.
    announce_refused: Option<Rejection>,
}

impl Provider {
    fn new(agent: AgentId) -> Self {
        Self {
            agent,
            contact: None,
            announce: None,
            contact_refused: None,
            announce_refused: None,
        }
    }

    /// A copy of the record `slot` names, checked at `now_unix`: kept when
    /// valid and newer than the one held.
    fn take(&mut self, slot: Slot, value: Vec<u8>, now_unix: i64) {
        match slot {
            Slot::Contact(agent) => match validate::contact_for(&agent, &value, now_unix) {
                Ok(contact) => {
                    if self
                        .contact
                        .as_ref()
                        .is_none_or(|(held, _)| contact.doc.seq > held.doc.seq)
                    {
                        self.contact = Some((contact, value));
                    }
                }
                Err(why) => self.contact_refused = Some(why),
            },
            Slot::Announce(agent) => match validate::announce_for(&agent, &value, now_unix) {
                Ok(announce) => {
                    if self
                        .announce
                        .as_ref()
                        .is_none_or(|(held, _)| announce.doc.seq > held.doc.seq)
                    {
                        self.announce = Some((announce, value));
                    }
                }
                Err(why) => self.announce_refused = Some(why),
            },
        }
    }

    /// A candidate, when it has a valid contact and a valid announce that
    /// lists the capability of `key`; why not, otherwise: why the record
    /// it lacks was refused, or the announce's when it lacks both, or that
    /// none came.
    fn verdict(self, key: &RecordKey) -> Result<DhtCandidate, (AgentId, String)> {
        let agent = self.agent;
        let why = match (self.contact, self.announce) {
            (Some((contact, contact_json)), Some((announce, announce_json))) => {
                let listed = announce
                    .doc
                    .capabilities
                    .iter()
                    .any(|capability| provider_key(capability).is_ok_and(|k| k == *key));
                if listed {
                    return Ok(DhtCandidate {
                        agent_id: agent,
                        contact,
                        contact_json,
                        announce,
                        announce_json,
                    });
                }
                Rejection::CapabilityNotAnnounced.as_str()
            }
            (None, Some(_)) => self
                .contact_refused
                .map_or("no valid contact", Rejection::as_str),
            (Some(_), None) => self
                .announce_refused
                .map_or("no valid announce", Rejection::as_str),
            (None, None) => self
                .announce_refused
                .or(self.contact_refused)
                .map_or("no valid contact", Rejection::as_str),
        };
        Err((agent, why.to_owned()))
    }
}

impl Task {
    /// The node entered the network: its first round is due.
    pub(super) fn entered(&mut self) {
        if let Some(publication) = &mut self.publication
            && publication.next_at.is_none()
        {
            publication.next_at = Some(Instant::now());
        }
    }

    /// When the next round is due, or the first lookup's deadline comes.
    pub(super) fn next_wake(&self) -> Option<Instant> {
        let round = self.publication.as_ref().and_then(|p| p.next_at);
        let deadlines = self.lookups.running.values().map(|lookup| lookup.deadline);
        round.into_iter().chain(deadlines).min()
    }

    /// Ends the lookups whose deadline came, and starts the round that is
    /// due; or, while one runs, keeps it due until that one ends.
    pub(super) fn on_timers(&mut self) {
        let now = Instant::now();
        let due: Vec<u64> = self
            .lookups
            .running
            .iter()
            .filter(|(_, lookup)| lookup.deadline <= now)
            .map(|(n, _)| *n)
            .collect();
        for n in due {
            self.end_lookup(n, true);
        }
        let round_due = self
            .publication
            .as_ref()
            .and_then(|p| p.next_at)
            .is_some_and(|at| at <= now);
        if !round_due {
            return;
        }
        match self.publication.as_mut() {
            Some(publication) if publication.round.is_some() => {
                publication.next_at = None;
                publication.due = true;
                tracing::debug!("a DHT round is due while one runs: it starts once that one ends");
            }
            _ => self.start_round(),
        }
    }

    /// The records changed, and a round starts; or nothing can change them
    /// any more.
    pub(super) fn on_change(&mut self, changed: bool) {
        if changed {
            self.start_round();
        } else if let Some(publication) = &mut self.publication {
            publication.changes = None;
        }
    }

    /// Starts a round of publication, as the module says, superseding the
    /// one running, if any.
    pub(super) fn start_round(&mut self) {
        let republish = self.timing.republish;
        let Some(publication) = self.publication.as_mut() else {
            return;
        };
        publication.due = false;
        if let Some(previous) = publication.round.take() {
            self.supersede(previous);
        }
        let Some(publication) = self.publication.as_mut() else {
            return;
        };
        publication.next_at = Some(Instant::now() + republish);
        let provided = std::mem::take(&mut publication.provided);
        let records = Arc::clone(&publication.records);
        let (contact, announce) = records.current();
        let capabilities = records.capabilities();

        let mut failed = Vec::new();
        let mut keyed = Vec::new();
        for capability in &capabilities {
            match provider_key(capability) {
                Ok(key) => keyed.push((capability.clone(), key)),
                Err(error) => {
                    tracing::warn!(%capability, %error, "not a capability: not provided");
                    failed.push(capability.clone());
                }
            }
        }
        for key in &provided {
            if !keyed.iter().any(|(_, listed)| listed == key) {
                self.kad().stop_providing(key);
            }
        }
        let mut waiting = Vec::new();
        for (capability, key) in keyed {
            match self.own_provider_record(&key) {
                Ok(()) => waiting.push((capability, key)),
                Err(error) => {
                    tracing::debug!(%capability, %error, "no room to provide a capability");
                    failed.push(capability);
                }
            }
        }
        if let Some(publication) = self.publication.as_mut() {
            publication.provided = waiting.iter().map(|(_, key)| key.clone()).collect();
        }

        let mut contact_record = Record::new(contact_key(&self.own), contact.clone());
        contact_record.expires = Some(self.expiry(signed_expiry::<ContactRecord>(&contact)));
        let mut announce_record = Record::new(announce_key(&self.own), announce.clone());
        announce_record.expires = Some(self.expiry(signed_expiry::<AnnounceRecord>(&announce)));
        announce_record.publisher = Some(self.local);
        let mut round = Round {
            capabilities,
            announce: announce_record.clone(),
            waiting: waiting.into(),
            on_their_way: 0,
            queries: HashMap::new(),
            reached: false,
            failed,
            error: None,
            superseded: false,
        };
        match self.put_own(contact_record) {
            Ok(id) => {
                round.queries.insert(id, Step::Contact);
            }
            Err(error) => round.error = Some(error),
        }
        match self.put_own(announce_record) {
            Ok(id) => {
                round.queries.insert(id, Step::Announce);
            }
            Err(error) => {
                round.error = Some(error);
                self.start_capabilities(&mut round);
            }
        }
        self.keep_round(round);
    }

    /// `round` ends here, as a new round starts: the queries it left are
    /// finished, and the capabilities it had not provided yet, waiting or
    /// on their way, count as not provided.
    fn supersede(&mut self, mut round: Round) {
        for id in round.queries.keys() {
            if let Some(mut query) = self.kad().query_mut(id) {
                query.finish();
            }
        }
        let on_their_way = round.queries.drain().filter_map(|(_, step)| match step {
            Step::Closest(capability, _)
            | Step::AnnounceAt(capability, _)
            | Step::Providing(capability) => Some(capability),
            Step::Contact | Step::Announce => None,
        });
        let left: Vec<String> = round
            .waiting
            .drain(..)
            .map(|(capability, _)| capability)
            .chain(on_their_way)
            .collect();
        round.failed.extend(left);
        round.superseded = true;
        self.end_round(round);
    }

    /// When a copy of a signed record stops living, at the routers when it
    /// is one of the node's own, here when the node is a router:
    /// `record_ttl` from now, or sooner, when its signature expires first at
    /// `signed_expires_unix`, by the node's clock. A router keeps the smaller
    /// of what the copy came with and this.
    fn expiry(&self, signed_expires_unix: Option<i64>) -> std::time::Instant {
        let now = std::time::Instant::now();
        let ttl_end = now + self.timing.record_ttl;
        let Some(expires) = signed_expires_unix else {
            return ttl_end;
        };
        let left = u64::try_from(expires - self.clock.now_unix()).unwrap_or(0);
        ttl_end.min(now + Duration::from_secs(left))
    }

    /// When a copy of `value`, the signed contact or announce under `key`,
    /// stops living here: [`Task::expiry`] of its signature's `expires_at`.
    pub(super) fn expiry_of(&self, key: &RecordKey, value: &[u8]) -> std::time::Instant {
        let signed = match slot(key) {
            Some(Slot::Contact(_)) => signed_expiry::<ContactRecord>(value),
            Some(Slot::Announce(_)) => signed_expiry::<AnnounceRecord>(value),
            None => None,
        };
        self.expiry(signed)
    }

    /// Puts one of the node's own records at the routers closest to its
    /// key, and holds it in its own store.
    fn put_own(&mut self, record: Record) -> Result<QueryId, String> {
        self.with_room(|kad| kad.put_record(record.clone(), Quorum::All))
            .map_err(|error| format!("the node's own DHT store: {error}"))
    }

    /// Step 3 of the module for the capabilities waiting, as many as there
    /// is room for among [`CAPABILITIES_AT_ONCE`], in the records' order.
    fn start_capabilities(&mut self, round: &mut Round) {
        while round.on_their_way < CAPABILITIES_AT_ONCE {
            let Some((capability, key)) = round.waiting.pop_front() else {
                return;
            };
            round.on_their_way += 1;
            let id = self.kad().get_closest_peers(key.to_vec());
            round.queries.insert(id, Step::Closest(capability, key));
        }
    }

    /// A capability's way to the routers ended, whether it was provided or
    /// not: the next one waiting starts.
    fn capability_done(&mut self, round: &mut Round) {
        round.on_their_way = round.on_their_way.saturating_sub(1);
        self.start_capabilities(round);
    }

    /// `capability` was not provided in this round, for `why`.
    fn capability_failed(&mut self, round: &mut Round, capability: String, why: &str) {
        tracing::debug!(%capability, why, "a capability not provided in the DHT");
        round.failed.push(capability);
        self.capability_done(round);
    }

    /// Provides `capability` at the routers closest to its `key`.
    fn provide(&mut self, round: &mut Round, capability: String, key: RecordKey) {
        match self.with_room(|kad| kad.start_providing(key.clone())) {
            Ok(id) => {
                // `start_providing` holds the record with no address, which
                // `libp2p-kad` fills with its listen addresses: in place,
                // the declared ones go back.
                let _ = self.own_provider_record(&key);
                round.queries.insert(id, Step::Providing(capability));
            }
            Err(_) => self.capability_failed(round, capability, "no room in the store"),
        }
    }

    /// The round that query `id` is of, taken out, and what the query was
    /// for; none when no round runs it.
    fn round_step(&mut self, id: QueryId) -> Option<(Round, Step)> {
        let publication = self.publication.as_mut()?;
        let step = publication.round.as_mut()?.queries.remove(&id)?;
        Some((publication.round.take()?, step))
    }

    /// Puts `round` back, or ends it when no query of it runs.
    fn keep_round(&mut self, round: Round) {
        if round.queries.is_empty() {
            self.end_round(round);
        } else if let Some(publication) = self.publication.as_mut() {
            publication.round = Some(round);
        }
    }

    /// A put of a round ended.
    pub(super) fn on_round_put(
        &mut self,
        id: QueryId,
        result: &Result<PutRecordOk, PutRecordError>,
    ) {
        let Some((mut round, step)) = self.round_step(id) else {
            return;
        };
        let reached = match result {
            Ok(_) => true,
            Err(
                PutRecordError::QuorumFailed { success, .. }
                | PutRecordError::Timeout { success, .. },
            ) => !success.is_empty(),
        };
        round.reached |= reached;
        match step {
            Step::Announce => self.start_capabilities(&mut round),
            Step::AnnounceAt(capability, key) if reached => {
                self.provide(&mut round, capability, key);
            }
            Step::AnnounceAt(capability, _) => self.capability_failed(
                &mut round,
                capability,
                "no router closest to it answered the announce",
            ),
            Step::Contact | Step::Closest(..) | Step::Providing(_) => {}
        }
        self.keep_round(round);
    }

    /// The routers closest to a capability's key, found: the announce goes
    /// to them, and then the provider record.
    pub(super) fn on_round_closest(
        &mut self,
        id: QueryId,
        result: Result<GetClosestPeersOk, GetClosestPeersError>,
    ) {
        let Some((mut round, step)) = self.round_step(id) else {
            return;
        };
        if let Step::Closest(capability, key) = step {
            let (Ok(GetClosestPeersOk { peers, .. })
            | Err(GetClosestPeersError::Timeout { peers, .. })) = result;
            let peers: Vec<PeerId> = peers.into_iter().map(|peer| peer.peer_id).collect();
            if peers.is_empty() {
                self.provide(&mut round, capability, key);
            } else {
                let at = self.kad().put_record_to(
                    round.announce.clone(),
                    peers.into_iter(),
                    Quorum::All,
                );
                round.queries.insert(at, Step::AnnounceAt(capability, key));
            }
        }
        self.keep_round(round);
    }

    /// A provider record of a round, sent; or not, when its query ended
    /// first.
    pub(super) fn on_round_provided(
        &mut self,
        id: QueryId,
        result: Result<AddProviderOk, AddProviderError>,
    ) {
        let Some((mut round, step)) = self.round_step(id) else {
            return;
        };
        if let Step::Providing(capability) = step {
            match result {
                Ok(_) => self.capability_done(&mut round),
                Err(_) => {
                    self.capability_failed(
                        &mut round,
                        capability,
                        "its provider record never went",
                    );
                }
            }
        }
        self.keep_round(round);
    }

    /// `round` is over: what it did becomes the publication's status. Then,
    /// unless a new round superseded it, the round that came due while it
    /// ran starts, and a round that reached no router is tried again after
    /// `retry_after`.
    fn end_round(&mut self, round: Round) {
        let now_unix = self.clock.now_unix();
        let now = Instant::now();
        let retry = now + self.timing.retry_after;
        let Some(publication) = self.publication.as_mut() else {
            return;
        };
        let Round {
            capabilities,
            reached,
            mut failed,
            error,
            superseded,
            ..
        } = round;
        failed.sort_by_key(|failed| capabilities.iter().position(|c| c == failed));
        // The worst first: the node's own store, then a new round, which
        // cut this one short, then no router at all.
        let error = error
            .or_else(|| superseded.then(|| superseded_error(failed.len())))
            .or_else(|| (!reached).then(|| DhtError::NoRouters.to_string()))
            .or_else(|| (!failed.is_empty()).then(|| not_provided(failed.len())));
        // A superseded round's next one is the round that superseded it.
        if !superseded {
            if std::mem::take(&mut publication.due) {
                publication.next_at = Some(now);
            }
            if !reached {
                publication.next_at = Some(publication.next_at.map_or(retry, |at| at.min(retry)));
            }
        }
        if !failed.is_empty() {
            if failed != publication.status.failed_capabilities {
                tracing::warn!(
                    ?failed,
                    "capabilities not provided in the DHT: the next round tries them again"
                );
            }
        } else if error != publication.status.last_error {
            match &error {
                Some(error) => tracing::info!(%error, "a round of DHT publication fell short"),
                None => tracing::info!("this agent's records are in the DHT"),
            }
        }
        tracing::debug!(
            capabilities = capabilities.len(),
            reached,
            "a DHT round ended"
        );
        publication.status = PublicationStatus {
            capabilities,
            last_round_unix: Some(now_unix),
            last_error: error,
            failed_capabilities: failed,
        };
    }

    /// Starts a lookup of the providers of `key`, as the module says.
    pub(super) fn start_lookup(
        &mut self,
        key: RecordKey,
        known: HashSet<AgentId>,
        limit: usize,
        deadline: Instant,
        reply: oneshot::Sender<Result<DhtLookup, DhtError>>,
    ) {
        if limit == 0 {
            let _ = reply.send(Ok(DhtLookup::default()));
            return;
        }
        let id = self.kad().get_providers(key.clone());
        let lookups = &mut self.lookups;
        let n = lookups.next;
        lookups.next += 1;
        lookups.queries.insert(id, n);
        lookups.running.insert(
            n,
            Lookup {
                key,
                known,
                limit,
                deadline,
                reply,
                providers_query: Some(id),
                answered: false,
                providers: Vec::new(),
                records: HashMap::new(),
                requests: 0,
            },
        );
    }

    /// A step of a lookup's `get_providers`; the last one with its
    /// `requests`.
    pub(super) fn on_lookup_providers(
        &mut self,
        id: QueryId,
        result: Result<GetProvidersOk, GetProvidersError>,
        stats: &QueryStats,
        requests: Option<u64>,
    ) {
        let Some(&n) = self.lookups.queries.get(&id) else {
            return;
        };
        if let Ok(GetProvidersOk::FoundProviders { providers, .. }) = result {
            self.take_providers(n, id, providers);
        }
        if let Some(requests) = requests {
            self.lookups.queries.remove(&id);
            if let Some(lookup) = self.lookups.running.get_mut(&n) {
                lookup.requests += requests;
                lookup.answered = stats.num_successes() > 0;
                lookup.providers_query = None;
            }
            self.end_lookup_if_done(n);
        }
    }

    /// Providers lookup `n`'s `get_providers` found: each new one that it
    /// does not know, up to the limit, gets the lookups of its contact and
    /// its announce. Once the limit is reached, the `get_providers` ends.
    fn take_providers(&mut self, n: u64, id: QueryId, providers: HashSet<PeerId>) {
        let local = self.local;
        let Some(lookup) = self.lookups.running.get_mut(&n) else {
            return;
        };
        let mut new = Vec::new();
        for peer in providers {
            if peer == local || lookup.providers.len() >= lookup.limit {
                continue;
            }
            let Some(agent) = agent_of(&peer) else {
                tracing::debug!(%peer, "a provider that is no agent, left out");
                continue;
            };
            if lookup.known.contains(&agent) || lookup.providers.iter().any(|p| p.agent == agent) {
                continue;
            }
            new.push((lookup.providers.len(), agent));
            lookup.providers.push(Provider::new(agent));
        }
        let full = lookup.providers.len() >= lookup.limit;
        for (index, agent) in new {
            for (slot, key) in [
                (Slot::Contact(agent), contact_key(&agent)),
                (Slot::Announce(agent), announce_key(&agent)),
            ] {
                let record = self.kad().get_record(key);
                self.lookups.queries.insert(record, n);
                if let Some(lookup) = self.lookups.running.get_mut(&n) {
                    lookup.records.insert(record, (index, slot));
                }
            }
        }
        if full && let Some(mut query) = self.kad().query_mut(&id) {
            query.finish();
        }
    }

    /// A step of a lookup's `get_record`; the last one with its
    /// `requests`.
    pub(super) fn on_lookup_record(
        &mut self,
        id: QueryId,
        result: Result<GetRecordOk, GetRecordError>,
        requests: Option<u64>,
    ) {
        let Some(&n) = self.lookups.queries.get(&id) else {
            return;
        };
        let now = self.clock.now_unix();
        let Some(lookup) = self.lookups.running.get_mut(&n) else {
            return;
        };
        let Some(&(index, slot)) = lookup.records.get(&id) else {
            return;
        };
        if let Ok(GetRecordOk::FoundRecord(found)) = result {
            lookup.providers[index].take(slot, found.record.value, now);
        }
        if let Some(requests) = requests {
            lookup.requests += requests;
            lookup.records.remove(&id);
            self.lookups.queries.remove(&id);
            self.end_lookup_if_done(n);
        }
    }

    /// Ends lookup `n` once none of its queries runs.
    fn end_lookup_if_done(&mut self, n: u64) {
        let done =
            self.lookups.running.get(&n).is_some_and(|lookup| {
                lookup.providers_query.is_none() && lookup.records.is_empty()
            });
        if done {
            self.end_lookup(n, false);
        }
    }

    /// Answers lookup `n` with what it has, finishing its queries that
    /// still run: `cut` when its deadline ended it.
    fn end_lookup(&mut self, n: u64, cut: bool) {
        let Some(mut lookup) = self.lookups.running.remove(&n) else {
            return;
        };
        let running: Vec<QueryId> = lookup
            .providers_query
            .iter()
            .chain(lookup.records.keys())
            .copied()
            .collect();
        for id in running {
            self.lookups.queries.remove(&id);
            lookup.requests += self.finish_query(&id);
        }
        let Lookup {
            key,
            reply,
            answered,
            providers,
            requests,
            ..
        } = lookup;
        let mut found = Vec::new();
        let mut discarded = Vec::new();
        for provider in providers {
            match provider.verdict(&key) {
                Ok(candidate) => found.push(candidate),
                Err(why) => discarded.push(why),
            }
        }
        tracing::debug!(
            found = found.len(),
            discarded = discarded.len(),
            requests,
            cut,
            "a DHT lookup ended"
        );
        let answer = if !cut && !answered && found.is_empty() && discarded.is_empty() {
            Err(DhtError::NoRouters)
        } else {
            Ok(DhtLookup {
                found,
                discarded,
                requests,
                cut,
            })
        };
        let _ = reply.send(answer);
    }
}

/// A signed record that expires.
trait Expiring: SignedDoc {
    fn expires_at(&self) -> &str;
}

impl Expiring for ContactRecord {
    fn expires_at(&self) -> &str {
        &self.expires_at
    }
}

impl Expiring for AnnounceRecord {
    fn expires_at(&self) -> &str {
        &self.expires_at
    }
}

/// When the signed record `json` of kind `T` expires, as its `expires_at`
/// says; none for one that does not read.
fn signed_expiry<T: Expiring>(json: &[u8]) -> Option<i64> {
    let signed = Signed::<T>::from_json(json).ok()?;
    parse_timestamp(signed.doc.expires_at())
}

/// The last error of a round that left `count` capabilities not provided.
fn not_provided(count: usize) -> String {
    match count {
        1 => "1 capability not provided".to_owned(),
        n => format!("{n} capabilities not provided"),
    }
}

/// The last error of a round a new round superseded, leaving `count`
/// capabilities not provided.
fn superseded_error(count: usize) -> String {
    match count {
        0 => "superseded by a new round".to_owned(),
        n => format!("superseded by a new round: {}", not_provided(n)),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use libp2p::kad::store::RecordStore;
    use libp2p::kad::{PeerInfo, QueryInfo};
    use pals_core::clock::{Clock, ManualClock, SystemClock};
    use pals_core::identity::AgentKey;
    use pals_core::records::RECORD_TTL_S;

    use super::*;
    use crate::config::{DhtCfg, DhtMode};
    use crate::dht::swarm::tests::{Fixed, endpoint, task_with};

    /// A client of `key` that publishes `published` and knows no router:
    /// its rounds' queries never leave, and the test gives their results.
    async fn publisher(key: &AgentKey, published: Arc<dyn PublishedRecords>) -> Task {
        publisher_at(key, published, Arc::new(SystemClock)).await
    }

    /// [`publisher`], at the time `clock` says.
    async fn publisher_at(
        key: &AgentKey,
        published: Arc<dyn PublishedRecords>,
        clock: Arc<dyn Clock>,
    ) -> Task {
        let cfg = DhtCfg {
            enabled: true,
            mode: DhtMode::Client,
            announce: true,
            ..DhtCfg::default()
        };
        let endpoint = endpoint(key).await;
        let key = AgentKey::from_bytes(&key.to_bytes());
        task_with(key, endpoint, cfg, Some(published), clock).0
    }

    /// The query of the round running whose step `which` wants: exactly
    /// one.
    fn query(task: &Task, which: impl Fn(&Step) -> bool) -> QueryId {
        let round = task
            .publication
            .as_ref()
            .and_then(|p| p.round.as_ref())
            .expect("a round runs");
        let found: Vec<QueryId> = round
            .queries
            .iter()
            .filter(|(_, step)| which(step))
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(found.len(), 1, "{} such steps", found.len());
        found[0]
    }

    /// The capabilities whose routers the round is looking for, in the
    /// records' order.
    fn looking_for(task: &Task, capabilities: &[String]) -> Vec<String> {
        let round = task
            .publication
            .as_ref()
            .and_then(|p| p.round.as_ref())
            .expect("a round runs");
        capabilities
            .iter()
            .filter(|capability| {
                round
                    .queries
                    .values()
                    .any(|step| matches!(step, Step::Closest(c, _) if c == *capability))
            })
            .cloned()
            .collect()
    }

    /// A put a router answered.
    fn answered() -> Result<PutRecordOk, PutRecordError> {
        Err(PutRecordError::QuorumFailed {
            key: RecordKey::new(b"k"),
            success: vec![PeerId::random()],
            quorum: NonZeroUsize::new(20).unwrap(),
        })
    }

    /// A put no router answered.
    fn unanswered() -> Result<PutRecordOk, PutRecordError> {
        Err(PutRecordError::QuorumFailed {
            key: RecordKey::new(b"k"),
            success: Vec::new(),
            quorum: NonZeroUsize::new(1).unwrap(),
        })
    }

    /// The routers closest to a key: `peers`.
    fn closest(peers: &[PeerId]) -> Result<GetClosestPeersOk, GetClosestPeersError> {
        Ok(GetClosestPeersOk {
            key: Vec::new(),
            peers: peers
                .iter()
                .map(|peer| PeerInfo {
                    peer_id: *peer,
                    addrs: Vec::new(),
                })
                .collect(),
        })
    }

    /// The contact's and the announce's puts, answered.
    fn records_answered(task: &mut Task) {
        let contact = query(task, |s| matches!(s, Step::Contact));
        let announce = query(task, |s| matches!(s, Step::Announce));
        task.on_round_put(contact, &answered());
        task.on_round_put(announce, &answered());
    }

    #[tokio::test]
    async fn capabilities_the_network_did_not_take_are_reported() {
        let key = AgentKey::generate();
        let capabilities = ["vision.ocr", "knowledge.documentation", "code.rust.review"];
        let mut task = publisher(&key, Fixed::of(&key, &capabilities)).await;
        task.start_round();
        records_answered(&mut task);
        let router = PeerId::random();
        for capability in capabilities {
            let id = query(
                &task,
                |s| matches!(s, Step::Closest(c, _) if c == capability),
            );
            task.on_round_closest(id, closest(&[router]));
        }
        let announce_at = |task: &Task, capability: &str| {
            query(
                task,
                |s| matches!(s, Step::AnnounceAt(c, _) if c == capability),
            )
        };
        let ocr = provider_key("vision.ocr").unwrap();

        // vision.ocr: its routers answered the announce, and its provider
        // record was never sent.
        task.on_round_put(announce_at(&task, "vision.ocr"), &answered());
        let providing = query(&task, |s| matches!(s, Step::Providing { .. }));
        task.on_round_provided(
            providing,
            Err(AddProviderError::Timeout { key: ocr.clone() }),
        );
        // knowledge.documentation: all of it went.
        task.on_round_put(announce_at(&task, "knowledge.documentation"), &answered());
        let providing = query(&task, |s| matches!(s, Step::Providing { .. }));
        task.on_round_provided(providing, Ok(AddProviderOk { key: ocr }));
        // code.rust.review: none of its routers answered the announce.
        task.on_round_put(announce_at(&task, "code.rust.review"), &unanswered());

        let publication = task.publication.as_ref().unwrap();
        assert!(publication.round.is_none(), "the round ended");
        let status = publication.status();
        assert_eq!(
            status.failed_capabilities,
            ["vision.ocr", "code.rust.review"]
        );
        assert_eq!(
            status.last_error.as_deref(),
            Some("2 capabilities not provided")
        );
    }

    #[tokio::test]
    async fn a_round_runs_four_capabilities_at_a_time() {
        let key = AgentKey::generate();
        let capabilities: Vec<String> = (0..6).map(|n| format!("test.capability-{n}")).collect();
        let listed: Vec<&str> = capabilities.iter().map(String::as_str).collect();
        let mut task = publisher(&key, Fixed::of(&key, &listed)).await;
        task.start_round();
        records_answered(&mut task);
        assert_eq!(looking_for(&task, &capabilities), capabilities[..4]);

        // The first has no router to go to: it is provided here, and once
        // its provider record is done, the fifth starts.
        let first = query(
            &task,
            |s| matches!(s, Step::Closest(c, _) if *c == capabilities[0]),
        );
        task.on_round_closest(first, closest(&[]));
        assert_eq!(looking_for(&task, &capabilities), capabilities[1..4]);
        let providing = query(&task, |s| matches!(s, Step::Providing { .. }));
        let key0 = provider_key(&capabilities[0]).unwrap();
        task.on_round_provided(providing, Ok(AddProviderOk { key: key0 }));
        assert_eq!(looking_for(&task, &capabilities), capabilities[1..5]);
    }

    /// Ends every capability of the round running, each with no router to
    /// go to and its provider record sent, until the round is over.
    fn provide_all(task: &mut Task) {
        loop {
            let Some(round) = task.publication.as_ref().and_then(|p| p.round.as_ref()) else {
                return;
            };
            let (id, closest_step) = round
                .queries
                .iter()
                .find_map(|(id, step)| match step {
                    Step::Closest(..) => Some((*id, true)),
                    Step::Providing(_) => Some((*id, false)),
                    _ => None,
                })
                .expect("a capability on its way");
            if closest_step {
                task.on_round_closest(id, closest(&[]));
            } else {
                let key = RecordKey::new(b"k");
                task.on_round_provided(id, Ok(AddProviderOk { key }));
            }
        }
    }

    /// The round that is due comes due now, as `republish` after the last
    /// one started would make it.
    fn due_now(task: &mut Task) {
        task.publication.as_mut().unwrap().next_at = Some(Instant::now());
    }

    #[tokio::test]
    async fn a_round_due_while_one_runs_waits_for_it_to_end() {
        let key = AgentKey::generate();
        let capabilities: Vec<String> = (0..6).map(|n| format!("test.capability-{n}")).collect();
        let listed: Vec<&str> = capabilities.iter().map(String::as_str).collect();
        let mut task = publisher(&key, Fixed::of(&key, &listed)).await;
        task.start_round();
        records_answered(&mut task);
        let first = query(
            &task,
            |s| matches!(s, Step::Closest(c, _) if *c == capabilities[0]),
        );

        // The timer of the next round comes due while this one runs: the
        // round goes on as it was, with its queries.
        due_now(&mut task);
        task.on_timers();
        assert_eq!(looking_for(&task, &capabilities), capabilities[..4]);
        let still = query(
            &task,
            |s| matches!(s, Step::Closest(c, _) if *c == capabilities[0]),
        );
        assert_eq!(still, first, "the same query");
        assert_eq!(task.next_wake(), None, "no wake for a round while one runs");

        // Once every capability went, the round ends whole, and the next
        // one is due at once.
        provide_all(&mut task);
        let publication = task.publication.as_ref().unwrap();
        assert!(publication.round.is_none(), "the round ended");
        assert_eq!(publication.status.failed_capabilities, Vec::<String>::new());
        assert_eq!(publication.status.last_error, None);
        assert!(task.next_wake().is_some(), "the next round is due");
        task.on_timers();
        let next = task.publication.as_ref().unwrap();
        assert!(next.round.is_some(), "the next round started");
        assert_eq!(looking_for(&task, &capabilities), Vec::<String>::new());
        query(&task, |s| matches!(s, Step::Contact));
    }

    #[tokio::test]
    async fn a_round_superseded_by_new_records_reports_what_it_left() {
        let key = AgentKey::generate();
        let capabilities: Vec<String> = (0..6).map(|n| format!("test.capability-{n}")).collect();
        let listed: Vec<&str> = capabilities.iter().map(String::as_str).collect();
        let mut task = publisher(&key, Fixed::of(&key, &listed)).await;
        task.start_round();
        records_answered(&mut task);
        // The first capability goes whole; the next three are on their
        // way, and the last two wait.
        let first = query(
            &task,
            |s| matches!(s, Step::Closest(c, _) if *c == capabilities[0]),
        );
        task.on_round_closest(first, closest(&[]));
        let providing = query(&task, |s| matches!(s, Step::Providing(_)));
        let key0 = provider_key(&capabilities[0]).unwrap();
        task.on_round_provided(providing, Ok(AddProviderOk { key: key0 }));

        // Records issued again start a round at once: the one running ends
        // there, and the five it had not provided are
        // what the status says it left.
        task.on_change(true);
        let publication = task.publication.as_ref().unwrap();
        assert!(publication.round.is_some(), "the new round runs");
        assert_eq!(looking_for(&task, &capabilities), Vec::<String>::new());
        let status = publication.status();
        assert_eq!(status.failed_capabilities, capabilities[1..]);
        assert_eq!(
            status.last_error.as_deref(),
            Some("superseded by a new round: 5 capabilities not provided")
        );
        assert!(status.last_round_unix.is_some());

        // The new round, once it ends whole, says so.
        records_answered(&mut task);
        provide_all(&mut task);
        let status = task.publication.as_ref().unwrap().status();
        assert_eq!(status.failed_capabilities, Vec::<String>::new());
        assert_eq!(status.last_error, None);
    }

    #[tokio::test]
    async fn own_records_last_no_longer_than_their_signature() {
        // Records whose signature has 600 s left, against the DHT's 3600 s,
        // by the one clock the records are issued at and the node reads:
        // exactly 600 s from when the round sends them.
        let clock = Arc::new(ManualClock::new(SystemClock.now_unix()));
        let key = AgentKey::generate();
        let records = Fixed::expiring(&key, &["vision.ocr"], clock.now_unix(), 600);
        let mut task = publisher_at(&key, records, clock.clone()).await;
        assert_eq!(task.timing.record_ttl, Duration::from_secs(3600));
        let before = std::time::Instant::now();
        task.start_round();
        records_answered(&mut task);
        let id = query(&task, |s| matches!(s, Step::Closest(..)));
        task.on_round_closest(id, closest(&[PeerId::random()]));
        let after = std::time::Instant::now();
        let at = query(&task, |s| matches!(s, Step::AnnounceAt(..)));
        let own = task.own;
        let store = task.kad().store_mut();
        let contact = store.get(&contact_key(&own)).unwrap().expires;
        let announce = store.get(&announce_key(&own)).unwrap().expires;
        let announce_at = match task.kad().query(&at).unwrap().info() {
            QueryInfo::PutRecord { record, .. } => record.expires,
            other => panic!("not a put: {other:?}"),
        };
        for expires in [contact, announce, announce_at] {
            let expires = expires.expect("an expiry");
            assert!(expires >= before + Duration::from_secs(600), "{expires:?}");
            assert!(expires <= after + Duration::from_secs(600), "{expires:?}");
        }

        // Records with a whole day left go for the DHT's 3600 s.
        let key = AgentKey::generate();
        let records = Fixed::expiring(&key, &["vision.ocr"], clock.now_unix(), RECORD_TTL_S - 60);
        let mut task = publisher_at(&key, records, clock).await;
        let before = std::time::Instant::now();
        task.start_round();
        let after = std::time::Instant::now();
        let own = task.own;
        let expires = task
            .kad()
            .store_mut()
            .get(&contact_key(&own))
            .unwrap()
            .expires
            .expect("an expiry");
        assert!(expires >= before + Duration::from_secs(3600), "{expires:?}");
        assert!(expires <= after + Duration::from_secs(3600), "{expires:?}");
    }

    #[test]
    fn a_discard_names_the_record_that_is_missing() {
        let key = AgentKey::generate();
        let agent = key.id();
        let (contact, announce) = Fixed::of(&key, &["vision.ocr"]).current();
        let ocr = provider_key("vision.ocr").unwrap();
        let now = SystemClock.now_unix();
        let (c, a) = (Slot::Contact(agent), Slot::Announce(agent));
        let verdict = |copies: &[(Slot, &[u8])]| {
            let mut provider = Provider::new(agent);
            for (slot, value) in copies {
                provider.take(*slot, value.to_vec(), now);
            }
            match provider.verdict(&ocr) {
                Ok(_) => "a candidate".to_owned(),
                Err((_, why)) => why,
            }
        };
        let forged: &[u8] = b"forged";
        // A valid contact, a forged copy of it, and no announce: what is
        // missing is the announce.
        assert_eq!(verdict(&[(c, &contact), (c, forged)]), "no valid announce");
        assert_eq!(verdict(&[(c, forged), (a, &announce)]), "bad_contact");
        assert_eq!(verdict(&[(a, &announce)]), "no valid contact");
        // Both missing: the announce's refusal, if it had one.
        assert_eq!(verdict(&[(c, forged), (a, forged)]), "bad_announce");
        assert_eq!(verdict(&[(c, forged)]), "bad_contact");
        assert_eq!(verdict(&[(c, &contact), (a, &announce)]), "a candidate");
    }
}
