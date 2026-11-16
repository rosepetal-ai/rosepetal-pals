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
//! last round that reached them, with `libp2p-kad`'s own expiry.
//!
//! **A round of publication.** There is one when the node enters the
//! network, one every `republish`, one each time a record is issued again
//! ([`PublishedRecords::changes`]: an address change, or the renewal at
//! 12 h) and, after a round that reached no router, one after
//! `retry_after`. A new round finishes whatever queries the previous one
//! left (`query_mut(id).finish()`). One still looking for its routers then
//! sends to those it found, which the routers' rules make harmless: an
//! older record is kept out, and a provider record of a capability the
//! newer announce no longer lists is refused, or dropped once that announce
//! arrives. A round:
//! 1. stops providing the capabilities the records no longer list, and
//!    holds the node's own provider record of each of the others, in the
//!    records' order, with the addresses the node declares. A capability
//!    the store has no room for fails alone and goes to
//!    `failed_capabilities`, for the next round to try again; the others go
//!    on, and nothing is undone;
//! 2. puts the contact and the announce with `Quorum::All`. A put is done
//!    once a router stored it: `QuorumFailed` with some `success`;
//! 3. once the announce's put ended, for each capability, finds the routers
//!    closest to its key (`get_closest_peers`) and puts the announce at
//!    them with `Quorum::All` (`put_record_to`): the put waits for every
//!    one of them. When it reached at least one (`Ok`, or `QuorumFailed` or
//!    `Timeout` with some `success`), the node provides the capability
//!    (`start_providing`); when it reached none, it does not, and the next
//!    round tries again. The announce travels with every provider record:
//!    each router that stores one already holds the announce that
//!    [`validate::check_provider`] asks for, even when the routers closest
//!    to the announce's key are not those closest to the capability's.
//!
//! A round in which no router answered that it stored anything ends with
//! `last_error`, and the next comes `retry_after` later.
//!
//! **A lookup** ([`super::Dht::providers`]) asks for the providers of a
//! capability's key and, for each provider as it arrives, up to its limit,
//! for its contact and its announce. Each copy is checked with
//! [`validate::contact_for`] and [`validate::announce_for`] at the node's
//! clock, and the valid one of highest `seq` stays. A provider with a valid
//! contact and a valid announce that lists the capability is a candidate;
//! the others are discarded, with the name of the last refusal of one of
//! their copies or what they lack. The node never finds itself. At its
//! deadline, a lookup answers with what it has and finishes its queries.
//! Its requests are those of its queries, the dials [`NoDial`] refused
//! them left out, each counted by its query
//! ([`Kademlia`](super::swarm::Kademlia)).
//!
//! [`NoDial`]: super::swarm::NoDial

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use libp2p::PeerId;
use libp2p::kad::{
    AddProviderError, AddProviderOk, GetClosestPeersError, GetClosestPeersOk, GetProvidersError,
    GetProvidersOk, GetRecordError, GetRecordOk, PutRecordError, PutRecordOk, QueryId, QueryStats,
    Quorum, Record, RecordKey,
};
use pals_core::identity::AgentId;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use serde::Serialize;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

use super::keys::{Slot, agent_of, announce_key, contact_key, provider_key};
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
    /// Why the last round fell short, if it did: no router answered that
    /// it stored any of its records, or the node's own store had no room
    /// for them.
    pub last_error: Option<String>,
    /// The capabilities the last round could not provide, for want of room
    /// in the node's own store; the next round tries them again.
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
    /// network.
    next_at: Option<Instant>,
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
    /// The capabilities waiting for the announce's put to end.
    waiting: Vec<(String, RecordKey)>,
    /// What each of its queries running is for.
    queries: HashMap<QueryId, Step>,
    /// Whether a router stored any of its records.
    reached: bool,
    failed: Vec<String>,
    error: Option<String>,
}

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
    Providing,
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
    /// Why the last copy refused was.
    refused: Option<Rejection>,
}

impl Provider {
    fn new(agent: AgentId) -> Self {
        Self {
            agent,
            contact: None,
            announce: None,
            refused: None,
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
                Err(why) => self.refused = Some(why),
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
                Err(why) => self.refused = Some(why),
            },
        }
    }

    /// A candidate, when it has a valid contact and a valid announce that
    /// lists the capability of `key`; why not, otherwise.
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
            (contact, _) => match self.refused {
                Some(why) => why.as_str(),
                None if contact.is_none() => "no valid contact",
                None => "no valid announce",
            },
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
    /// due.
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
        if round_due {
            self.start_round();
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

    /// Starts a round of publication, as the module says, finishing the
    /// previous one's queries.
    pub(super) fn start_round(&mut self) {
        let republish = self.timing.republish;
        let Some(publication) = self.publication.as_mut() else {
            return;
        };
        publication.next_at = Some(Instant::now() + republish);
        let previous = publication.round.take();
        let provided = std::mem::take(&mut publication.provided);
        let records = Arc::clone(&publication.records);
        for id in previous.iter().flat_map(|round| round.queries.keys()) {
            if let Some(mut query) = self.kad().query_mut(id) {
                query.finish();
            }
        }
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

        let mut announce = Record::new(announce_key(&self.own), announce);
        announce.publisher = Some(self.local);
        let mut round = Round {
            capabilities,
            announce: announce.clone(),
            waiting,
            queries: HashMap::new(),
            reached: false,
            failed,
            error: None,
        };
        match self.put_own(Record::new(contact_key(&self.own), contact)) {
            Ok(id) => {
                round.queries.insert(id, Step::Contact);
            }
            Err(error) => round.error = Some(error),
        }
        match self.put_own(announce) {
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

    /// Puts one of the node's own records at the routers closest to its
    /// key, and holds it in its own store.
    fn put_own(&mut self, record: Record) -> Result<QueryId, String> {
        self.with_room(|kad| kad.put_record(record.clone(), Quorum::All))
            .map_err(|error| format!("the node's own DHT store: {error}"))
    }

    /// Step 3 of the module for each capability waiting.
    fn start_capabilities(&mut self, round: &mut Round) {
        for (capability, key) in std::mem::take(&mut round.waiting) {
            let id = self.kad().get_closest_peers(key.to_vec());
            round.queries.insert(id, Step::Closest(capability, key));
        }
    }

    /// Provides `capability` at the routers closest to its `key`.
    fn provide(&mut self, round: &mut Round, capability: String, key: RecordKey) {
        match self.with_room(|kad| kad.start_providing(key.clone())) {
            Ok(id) => {
                // `start_providing` holds the record with no address, which
                // `libp2p-kad` fills with its listen addresses: in place,
                // the declared ones go back.
                let _ = self.own_provider_record(&key);
                round.queries.insert(id, Step::Providing);
            }
            Err(error) => {
                tracing::debug!(%capability, %error, "no room to provide a capability");
                round.failed.push(capability);
            }
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
            Step::AnnounceAt(capability, _) => tracing::debug!(
                %capability,
                "the announce reached none of the routers closest to a capability: not provided \
                 this round"
            ),
            Step::Contact | Step::Closest(..) | Step::Providing => {}
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

    /// A provider record of a round, sent.
    pub(super) fn on_round_provided(
        &mut self,
        id: QueryId,
        result: Result<AddProviderOk, AddProviderError>,
    ) {
        let Some((round, _)) = self.round_step(id) else {
            return;
        };
        if let Err(error) = result {
            tracing::debug!(%error, "providing a capability");
        }
        self.keep_round(round);
    }

    /// `round` is over: what it did becomes the publication's status, and a
    /// round that reached no router is tried again after `retry_after`.
    fn end_round(&mut self, round: Round) {
        let now_unix = self.clock.now_unix();
        let retry = Instant::now() + self.timing.retry_after;
        let Some(publication) = self.publication.as_mut() else {
            return;
        };
        let Round {
            capabilities,
            reached,
            mut failed,
            error,
            ..
        } = round;
        failed.sort_by_key(|failed| capabilities.iter().position(|c| c == failed));
        let error = error.or_else(|| (!reached).then(|| DhtError::NoRouters.to_string()));
        if !reached {
            publication.next_at = Some(publication.next_at.map_or(retry, |at| at.min(retry)));
        }
        if error != publication.status.last_error {
            match &error {
                Some(error) => tracing::info!(%error, "a round of DHT publication fell short"),
                None => tracing::info!("this agent's records are in the DHT"),
            }
        }
        if !failed.is_empty() && failed != publication.status.failed_capabilities {
            tracing::warn!(
                ?failed,
                "no room in the DHT store to provide these capabilities: the next round tries \
                 them again"
            );
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

    /// Providers lookup `n`'s `get_providers` found: each new one, up to
    /// the limit, gets the lookups of its contact and its announce. Once
    /// the limit is reached, the `get_providers` ends.
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
            if lookup.providers.iter().any(|p| p.agent == agent) {
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
