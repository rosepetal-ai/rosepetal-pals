//! What a router adds to n0-mainline's server: it checks each put of the
//! index before the library stores it, and it sees what the library's
//! store holds.
//!
//! Both go through the library's `DatagramHook`, which gets every datagram
//! before the library decodes it ([`krpc`] says why that one):
//!
//! - **Checks.** A `put` of a mutable item goes through
//!   [`index::check_item`], an `announce_signed_peer` through
//!   [`index::check_provider`], against the newest announce of that agent
//!   the router accepted. BEP 5's `announce_peer` and immutable puts are
//!   refused: the index uses neither. So is a `put` or an
//!   `announce_signed_peer` whose arguments the hook cannot read
//!   ([`MALFORMED`]): it fails closed. A refused query never reaches the
//!   library: the hook consumes it, answers the sender with a KRPC error
//!   203 naming the reason and counts it in `records_rejected`. An accepted
//!   one goes on to the library, which applies its own checks (BEP 44's
//!   token, signature, target and `seq`, a signed peer's signature and
//!   timestamp) and may still refuse it.
//! - **What is stored.** n0-mainline gives no read of its store. So, every
//!   [`PROBE_INTERVAL`], the router asks itself, over loopback, for each
//!   target a put named, accepted or refused: a `get` for each item and a
//!   `get_signed_peers` for each infohash, the queries a searcher sends.
//!   Their answers, which the hook takes before the library sees them, say
//!   what the library would serve now, attributed by the key they carry,
//!   and are what the router's stats count. A refused put is probed too, so
//!   that the counts would show it if the library held it anyway. The
//!   router removes nothing itself: an entry leaves the counts only when the
//!   library stops serving it. BEP 5 peers are not probed: their answers
//!   carry addresses, which name no agent.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use n0_mainline::{DatagramHook, Id, MutableItem};
use pals_core::identity::AgentId;
use spike_common::{BackendStats, StoredRecords};
use tokio::sync::mpsc::UnboundedSender;

use crate::index::{self, Kind, MALFORMED, UNKNOWN_KEY};
use crate::krpc::{self, Message};

/// How often a router asks itself what its store holds.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// The KRPC error a refused put gets: BEP 5's "protocol error, such as a
/// malformed packet, invalid arguments, or bad token".
const REFUSED: i64 = 203;
/// The first two bytes of a probe's transaction id. The library takes only
/// ids of 2 or 4 bytes; its own count up from 0, and would reach these, which
/// start at 0x7072_0000, after some 1.9 billion requests.
const PROBE_TID: &[u8; 2] = b"pr";

/// A datagram to send from the DHT's socket, and where.
pub type Outgoing = (Vec<u8>, SocketAddrV4);

/// A router's checks and its view of the library's store.
#[derive(Debug)]
pub struct Router {
    state: Arc<Mutex<State>>,
    /// The node id its probes carry; any id will do.
    probe_id: Id,
}

#[derive(Debug, Default)]
struct State {
    rejected: u64,
    /// The target of every item a put named, accepted or refused, and its
    /// kind: `None` under a salt the index does not define, or for an
    /// immutable item.
    targets: HashMap<Id, Option<Kind>>,
    /// Every infohash a signed peer named, accepted or refused.
    info_hashes: BTreeSet<Id>,
    /// The `seq` and capabilities of each agent's newest accepted announce.
    announced: HashMap<AgentId, (i64, Vec<Id>)>,
    /// From the probes: each item the library holds, with the agent whose
    /// key it carries; `None` for an item with no key, or a key that is no
    /// agent's.
    held_items: HashMap<Id, Option<AgentId>>,
    /// From the probes: the agents of each infohash's signed peers.
    held_peers: HashMap<Id, BTreeSet<AgentId>>,
    /// The probes of this round still unanswered.
    pending: HashMap<Vec<u8>, Probe>,
    next_tid: u16,
}

#[derive(Debug, Clone, Copy)]
enum Probe {
    Item(Id),
    Peers(Id),
}

impl Default for Router {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            probe_id: Id::random(),
        }
    }
}

impl Router {
    /// The hook of a router whose DHT socket the probes reach at `itself`:
    /// it checks puts, sending each refusal's answer through `replies`, and
    /// takes the answers to the probes.
    pub fn hook(&self, itself: SocketAddrV4, replies: UnboundedSender<Outgoing>) -> DatagramHook {
        let state = Arc::clone(&self.state);
        DatagramHook::new(move |bytes, from| {
            let Some(message) = krpc::parse(bytes) else {
                return false;
            };
            let mut state = lock(&state);
            match message {
                Message::Query { tid, method, args } => {
                    let Some(verdict) = state.check(&method, &args) else {
                        return false;
                    };
                    let Err(reason) = verdict else {
                        return false;
                    };
                    state.rejected += 1;
                    tracing::warn!(%from, method = %String::from_utf8_lossy(&method), reason, "put refused");
                    let answer = krpc::error(&tid, REFUSED, &format!("agentnet: {reason}"));
                    let _ = replies.send((answer, from));
                    true
                }
                Message::Response { tid, body } if from == itself => state.answered(&tid, &body),
                _ => false,
            }
        })
    }

    /// This round's probes: a `get` of every learned item and a
    /// `get_signed_peers` of every learned infohash. A probe of the round
    /// before that got no answer is forgotten, and its target keeps the
    /// last answer it had.
    pub fn probes(&self) -> Vec<Vec<u8>> {
        let mut state = lock(&self.state);
        state.pending.clear();
        let probes: Vec<Probe> = (state.targets.keys().map(|t| Probe::Item(*t)))
            .chain(state.info_hashes.iter().map(|h| Probe::Peers(*h)))
            .collect();
        probes
            .into_iter()
            .map(|probe| {
                let mut tid = PROBE_TID.to_vec();
                tid.extend_from_slice(&state.next_tid.to_be_bytes());
                state.next_tid = state.next_tid.wrapping_add(1);
                let query = match probe {
                    Probe::Item(target) => krpc::get(&tid, &self.probe_id, &target),
                    Probe::Peers(info_hash) => {
                        krpc::get_signed_peers(&tid, &self.probe_id, &info_hash)
                    }
                };
                state.pending.insert(tid, probe);
                query
            })
            .collect()
    }

    /// The router's part of the stats: what the library holds, as the last
    /// probes saw it, and what the hook refused. An item with no agent's
    /// key counts in `records_stored` and nowhere in `stored_by_agent`.
    pub fn fill(&self, stats: &mut BackendStats) {
        let state = lock(&self.state);
        let mut by_agent: BTreeMap<AgentId, StoredRecords> = BTreeMap::new();
        for (target, agent) in &state.held_items {
            let Some(agent) = agent else {
                continue;
            };
            let entry = by_agent.entry(*agent).or_default();
            match state.targets.get(target) {
                Some(Some(Kind::Contact)) => entry.contact_records += 1,
                Some(Some(Kind::Announce)) => entry.announce_records += 1,
                _ => entry.other_records += 1,
            }
        }
        for agents in state.held_peers.values() {
            for agent in agents {
                by_agent.entry(*agent).or_default().provider_records += 1;
            }
        }
        stats.records_stored = state.held_items.len() as u64;
        stats.provider_records_stored = state.held_peers.values().map(|a| a.len() as u64).sum();
        stats.records_rejected = state.rejected;
        stats.stored_by_agent = by_agent;
    }
}

impl State {
    /// The verdict on a query of `method`: `None` when it stores nothing (a
    /// get, a ping…), `Ok` when the router lets it through, or `Err` with
    /// the reason to refuse it. Either way, the router learns the target of
    /// a put it can read, to probe it.
    fn check(&mut self, method: &[u8], args: &krpc::Dict) -> Option<Result<(), &'static str>> {
        match method {
            b"put" if krpc::is_immutable_put(args) => {
                if let Some(target) = krpc::put_target(args) {
                    self.targets.insert(target, None);
                }
                Some(Err(UNKNOWN_KEY))
            }
            b"put" => {
                let Some(put) = krpc::mutable_put(args) else {
                    return Some(Err(MALFORMED));
                };
                // Where the library would keep it: the key and the salt name
                // the target, and the library refuses any other.
                let target = MutableItem::target_from_key(&put.key, put.salt.as_deref());
                self.targets
                    .insert(target, Kind::of_salt(put.salt.as_deref()));
                let verdict = index::check_item(
                    &put.key,
                    put.salt.as_deref(),
                    &put.value,
                    put.seq,
                    now_unix(),
                );
                Some(verdict.map(|accepted| {
                    if accepted.kind == Kind::Announce {
                        let newest = self.announced.get(&accepted.agent);
                        if newest.is_none_or(|(seq, _)| put.seq >= *seq) {
                            self.announced
                                .insert(accepted.agent, (put.seq, accepted.capabilities));
                        }
                    }
                }))
            }
            b"announce_signed_peer" => {
                let Some(peer) = krpc::signed_peer(args) else {
                    return Some(Err(MALFORMED));
                };
                self.info_hashes.insert(peer.info_hash);
                let announced = index::agent_of(&peer.key)
                    .ok()
                    .and_then(|agent| self.announced.get(&agent))
                    .map(|(_, capabilities)| capabilities.as_slice());
                Some(index::check_provider(&peer.key, &peer.info_hash, announced).map(drop))
            }
            b"announce_peer" => Some(Err(UNKNOWN_KEY)),
            _ => None,
        }
    }

    /// Takes the answer to a probe; `false` when `tid` is no probe of this
    /// round, which the library then handles.
    fn answered(&mut self, tid: &[u8], body: &krpc::Dict) -> bool {
        let Some(probe) = self.pending.remove(tid) else {
            return false;
        };
        match probe {
            Probe::Item(target) => {
                match krpc::item_in(body) {
                    Some(key) => {
                        let agent = key.and_then(|k| index::agent_of(&k).ok());
                        self.held_items.insert(target, agent)
                    }
                    None => self.held_items.remove(&target),
                };
            }
            Probe::Peers(info_hash) => {
                let agents: BTreeSet<AgentId> = krpc::peer_keys(body)
                    .iter()
                    .filter_map(|k| index::agent_of(k).ok())
                    .collect();
                if agents.is_empty() {
                    self.held_peers.remove(&info_hash);
                } else {
                    self.held_peers.insert(info_hash, agents);
                }
            }
        }
        true
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
