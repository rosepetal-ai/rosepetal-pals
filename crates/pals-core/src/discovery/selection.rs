//! Choosing a provider in two stages (design §8): first the candidates that
//! cannot serve are discarded, then the others are ordered. Pure: what the
//! search read of its candidates comes in, a ranking and the reasons for
//! each discard come out. The daemon's search (`pals::discovery::search`)
//! reads the cards and asks for the offers, within the limits of §7.3.
//!
//! **Stage 1, the card** ([`stage_one`]): cheap filters on a card verified
//! first. A skill tagged with the capability asked for
//! ([`capability_tag`]), whose input and output modes, or the card's when
//! the skill declares none, cover every media type asked for
//! ([`covers_media`]). A card that declares no modes at all says nothing,
//! and the offer decides.
//!
//! **Stage 2, the offer** ([`stage_two`]). Discarded:
//!
//! - an offer that is not compatible (`not_offered`, `needs_information`),
//!   with the first requirement of the request it fails, or does not know;
//! - an offer that does not say every mandatory requirement of the request
//!   is met ([`SearchRequest::mandatory`]): a requirement the provider left
//!   out, or one never sent, as the inventory ids that go only to
//!   candidates with a grant for the project (to none when the request
//!   names no project), is unknown, and an unknown requirement is never met
//!   (§7.2, §9.1);
//! - a plain A2A agent, which has a card and no offer, when the request asks
//!   for anything a card cannot confirm: inventory, region, cloud, language
//!   or a requirement this agent does not know.
//!
//! The rest are ordered by group: an offer `available` and in force by this
//! agent's clock (a recent confirmation, §8), then a card alone, or an
//! offer that no longer confirms anything, then `busy`. Within a group:
//! more preferences met; a usable benchmark report of the profile asked
//! for, a measurement before a declared specification; the least
//! `estimated_start_delay_ms + estimated_duration_ms`, an unknown one last;
//! more questions completed with the peer; the agent id, or the plain
//! contact's name. A requirement not met is never made up for by a better
//! time: it discards.
//!
//! Every reason is this agent's own text, naming what the request asked:
//! nothing a peer sent is quoted.

use std::cmp::Ordering;

use a2a::AgentCard;
use serde::{Deserialize, Serialize};

use super::matching::covers_media;
use super::query::{
    Met, OfferStatus, QUERY_SCHEMA, RequirementSet, Requirements, ServiceOffer, ServiceQuery,
};
use crate::benchmark::{MeasurementKind, Wanted};
use crate::card_jws::{CardHash, SignatureStatus};
use crate::grant::parse_timestamp;
use crate::identity::AgentId;
use crate::inventory::capability_tag;

/// What a requester looks for (design §7.1): a capability and its media
/// types, requirements, the project it is for, the contacts to ask (all
/// when it names none), when it wants benchmarks to count, what they must
/// have measured, and whether the search keeps out of the DHT. A request of
/// milestone 2, without `exclude_dht`, reads the same.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub capability: String,
    /// The minimum context of the case; never data to work on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_media_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_media_types: Vec<String>,
    #[serde(default)]
    pub requirements: Requirements,
    /// The project the task is for: only candidates with a grant for it
    /// get the inventory ids of the requirements, and a request that names
    /// none sends them to no one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Contact names, plain contacts' names or agent ids; every contact
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peers: Option<Vec<String>>,
    /// The benchmark reports that count: those usable for it
    /// ([`crate::benchmark::comparable`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<Wanted>,
    /// The search looks only among the contacts, never in the DHT, which
    /// sees which agent looks for which capability. A request that names
    /// `peers` never looks there either.
    #[serde(default, skip_serializing_if = "is_false")]
    pub exclude_dht: bool,
}

/// Whether `value` is `false`: such a flag is left out of the JSON.
fn is_false(value: &bool) -> bool {
    !*value
}

impl SearchRequest {
    /// The query a candidate is asked, as `query_id`, waiting
    /// `reply_timeout_ms`: the request's capability, media types, summary,
    /// project and requirements, with the project and every inventory id,
    /// required and preferred, only `with_ids` (§7.1, §9.9: integrations
    /// and sources go only to authorized candidates, and a candidate without
    /// a grant for the project is not told which project it is).
    pub fn query(&self, query_id: &str, reply_timeout_ms: u32, with_ids: bool) -> ServiceQuery {
        let query = ServiceQuery {
            schema: QUERY_SCHEMA.to_owned(),
            query_id: query_id.to_owned(),
            capability: self.capability.clone(),
            input_media_types: self.input_media_types.clone(),
            output_media_types: self.output_media_types.clone(),
            requirements: self.requirements.clone(),
            summary: self.summary.clone(),
            reply_timeout_ms: Some(reply_timeout_ms),
            requested_inventory_sections: Vec::new(),
            project: self.project.clone(),
            execution_authorized: false,
        };
        if with_ids { query } else { query.withheld() }
    }

    /// The mandatory requirements of the request, named as an offer's
    /// results name them: the media types (`input_media_type:<t>`,
    /// `output_media_type:<t>`), `language`, `external_cloud_allowed`,
    /// `required_region`, the unknown ones (`<key>`), and those of
    /// `inventory.required` (`mcp_integration:<id>`, `information_source:<id>`,
    /// `skill_package:<id>`, `plugin:<id>`, `model:<id>`,
    /// `execution_external_cloud_allowed`, `min_offered_ram_mib`,
    /// `min_offered_vram_mib`, `accelerator:<a>`, `inventory.<key>`).
    pub fn mandatory(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        names.extend(
            self.input_media_types
                .iter()
                .map(|t| format!("input_media_type:{t}")),
        );
        names.extend(
            self.output_media_types
                .iter()
                .map(|t| format!("output_media_type:{t}")),
        );
        let requirements = &self.requirements;
        if requirements.language.is_some() {
            names.push("language".to_owned());
        }
        if requirements.external_cloud_allowed == Some(false) {
            names.push("external_cloud_allowed".to_owned());
        }
        if requirements.required_region.is_some() {
            names.push("required_region".to_owned());
        }
        names.extend(requirements.other.keys().cloned());
        if let Some(inventory) = &requirements.inventory {
            let set = &inventory.required;
            names.extend(id_requirements(set));
            if set.execution_external_cloud_allowed == Some(false) {
                names.push("execution_external_cloud_allowed".to_owned());
            }
            if set.min_offered_ram_mib.is_some() {
                names.push("min_offered_ram_mib".to_owned());
            }
            if set.min_offered_vram_mib.is_some() {
                names.push("min_offered_vram_mib".to_owned());
            }
            names.extend(set.accelerators.iter().map(|a| format!("accelerator:{a}")));
            names.extend(set.other.keys().map(|key| format!("inventory.{key}")));
        }
        names
    }

    /// Whether it asks for anything a card alone cannot confirm: inventory,
    /// region, no external cloud, language, or a requirement this agent
    /// does not know.
    pub fn needs_an_offer(&self) -> bool {
        let requirements = &self.requirements;
        requirements.inventory.is_some()
            || requirements.required_region.is_some()
            || requirements.external_cloud_allowed == Some(false)
            || requirements.language.is_some()
            || !requirements.other.is_empty()
    }
}

/// `set` without the ids of its entries: MCP connections, information
/// sources, skill packages, plugins and models.
/// The requirements of the ids of `set`, as an offer names them.
fn id_requirements(set: &RequirementSet) -> impl Iterator<Item = String> + '_ {
    set.mcp_integration_ids
        .iter()
        .map(|id| format!("mcp_integration:{id}"))
        .chain(
            set.information_source_ids
                .iter()
                .map(|id| format!("information_source:{id}")),
        )
        .chain(
            set.skill_package_ids
                .iter()
                .map(|id| format!("skill_package:{id}")),
        )
        .chain(set.plugin_ids.iter().map(|id| format!("plugin:{id}")))
        .chain(set.model_ids.iter().map(|id| format!("model:{id}")))
}

/// Whether `name` is the requirement of an inventory id.
fn is_id_requirement(name: &str) -> bool {
    [
        "mcp_integration:",
        "information_source:",
        "skill_package:",
        "plugin:",
        "model:",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

/// A candidate that passed the first stage, as the second stage orders it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// How this agent names it: a contact's name, a plain contact's name,
    /// or an agent id.
    pub peer: String,
    /// A PALS peer's; none for a plain A2A agent.
    pub agent_id: Option<AgentId>,
    /// The service, the card skill, chosen.
    pub skill_id: Option<String>,
    /// None: a plain A2A agent, of which there is only the card.
    pub offer: Option<ServiceOffer>,
    /// The card read: the extended card when a grant showed one.
    pub card_hash: CardHash,
    pub card_signature: SignatureStatus,
    /// The preferences the offer says are met.
    pub preferred_met: usize,
    /// The best usable report of the profile asked for, a measurement
    /// before a declared specification: its kind (`own_measurement`,
    /// `peer_observation`, `declared_spec`) and the value of its first
    /// result.
    pub benchmark: Option<(String, f64)>,
    /// The questions to the peer that completed, and that failed, in this
    /// agent's outbox.
    pub experience: (u32, u32),
    /// This agent holds a grant of the peer's that serves tasks, of the
    /// request's project when it names one. Always `false` for a plain A2A
    /// agent, which needs none.
    pub grant_held: bool,
    /// The DHT found it, and it is no contact. Left out of the JSON when
    /// `false`, as in milestone 2.
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_dht: bool,
}

impl Candidate {
    /// What ties are broken by last: the agent id, else the name.
    fn id(&self) -> String {
        self.agent_id
            .map_or_else(|| self.peer.clone(), |id| id.to_string())
    }
}

/// Where a candidate was discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Its card: not had, not valid or not verified, not of the capability
    /// or the media types, not the card its announce names, or a plain
    /// agent's that cannot confirm what is required.
    Card,
    /// Its offer: not had in time, refused, not compatible, not saying
    /// every requirement is met; or, when asking, no grant held.
    Offer,
}

/// A candidate left out, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discarded {
    pub peer: String,
    pub stage: Stage,
    pub reason: String,
}

/// Whether every candidate was considered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    /// Some were left out by the limits, or cut by them: `candidate limit`,
    /// `dht timeout` (the lookup in the DHT was cut), `dht unavailable` (no
    /// DHT router answered), `time budget`, `timeout` (a candidate that had
    /// its whole time and did not finish).
    Partial {
        reasons: Vec<String>,
    },
}

/// What a search took (design §14, the metrics that decide what comes
/// next).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMetrics {
    /// From the start of the search to the first candidate it ranked.
    pub time_to_first_valid_ms: Option<u64>,
    /// Candidates sent anything.
    pub contacted: usize,
    /// Requests sent: cards, queries, benchmark reports.
    pub messages: usize,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// The time the two stages took, in microseconds.
    pub matching_us: u64,
    pub elapsed_ms: u64,
    /// The requests the lookup in the DHT sent; 0 when there was none, or
    /// when no router answered it. Omitted when 0, so that a search with
    /// the DHT off writes the metrics of milestone 2 as they were.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dht_requests: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchOutcome {
    /// The best first.
    pub ranked: Vec<Candidate>,
    pub discarded: Vec<Discarded>,
    pub coverage: Coverage,
    pub metrics: SearchMetrics,
}

/// The first stage for a candidate whose `card` was verified: the skill
/// chosen, the first of the card's skills tagged with the capability that
/// takes and gives every media type asked for; or why there is none.
/// `Ok(None)` is never returned for now: every card that passes has a
/// skill to name.
pub fn stage_one(card: &AgentCard, req: &SearchRequest) -> Result<Option<String>, String> {
    let tag = capability_tag(&req.capability).map_err(|_| "invalid capability".to_owned())?;
    let mut first_failure = None;
    let tagged = card.skills.iter().filter(|skill| skill.tags.contains(&tag));
    for skill in tagged {
        let inputs = skill
            .input_modes
            .as_deref()
            .filter(|modes| !modes.is_empty())
            .unwrap_or(&card.default_input_modes);
        let outputs = skill
            .output_modes
            .as_deref()
            .filter(|modes| !modes.is_empty())
            .unwrap_or(&card.default_output_modes);
        let refused = req
            .input_media_types
            .iter()
            .find(|wanted| covers_media(inputs, wanted) == Some(false))
            .map(|wanted| format!("input media type {wanted} not accepted"))
            .or_else(|| {
                req.output_media_types
                    .iter()
                    .find(|wanted| covers_media(outputs, wanted) == Some(false))
                    .map(|wanted| format!("output media type {wanted} not produced"))
            });
        match refused {
            None => return Ok(Some(skill.id.clone())),
            Some(why) => {
                first_failure.get_or_insert(why);
            }
        }
    }
    Err(first_failure.unwrap_or_else(|| format!("no skill of {}", req.capability)))
}

/// The second stage: `candidates`, which passed the first, ranked, and
/// those discarded, at `now_unix` by this agent's own clock, without skew
/// (see the [module](self)). Offers of another provider, expired or badly
/// signed never get here: the client refused them
/// ([`super::query::accept_offer`]).
pub fn stage_two(
    candidates: Vec<Candidate>,
    req: &SearchRequest,
    now_unix: i64,
) -> (Vec<Candidate>, Vec<Discarded>) {
    let mandatory = req.mandatory();
    let mut ranked = Vec::new();
    let mut discarded = Vec::new();
    for candidate in candidates {
        // The inventory ids went to it: with a grant for the project named.
        let sent_ids = candidate.grant_held && req.project.is_some();
        let why = match &candidate.offer {
            Some(offer) => {
                offer_refusal(offer, &mandatory, sent_ids).map(|reason| (Stage::Offer, reason))
            }
            None if req.needs_an_offer() => Some((
                Stage::Card,
                "a plain A2A agent cannot confirm the requirements".to_owned(),
            )),
            None => None,
        };
        match why {
            Some((stage, reason)) => discarded.push(Discarded {
                peer: candidate.peer,
                stage,
                reason,
            }),
            None => ranked.push(candidate),
        }
    }
    let wants_benchmark = req.benchmark.is_some();
    ranked.sort_by(|x, y| order(x, y, now_unix, wants_benchmark));
    (ranked, discarded)
}

/// Why `offer` does not serve: not compatible, with the first of the
/// `mandatory` requirements it fails, or does not know; or compatible
/// without saying one of them is met, an inventory id not sent, when
/// `authorized` is false, among them.
fn offer_refusal(offer: &ServiceOffer, mandatory: &[String], authorized: bool) -> Option<String> {
    let result_of = |name: &str| {
        offer
            .requirement_results
            .iter()
            .find(|result| result.mandatory && result.requirement == name)
            .map(|result| result.result)
    };
    if !offer.compatible {
        let failed = mandatory
            .iter()
            .find(|name| result_of(name) == Some(Met::NotMet));
        if let Some(name) = failed {
            return Some(if name.starts_with("mcp_integration:") {
                format!("{name} not operational")
            } else {
                format!("{name} not met")
            });
        }
        let unknown = mandatory
            .iter()
            .find(|name| result_of(name) != Some(Met::Met));
        if let Some(name) = unknown {
            return Some(unknown_reason(name, authorized));
        }
        return Some(
            if offer.skill_id.is_none() {
                "capability not offered"
            } else {
                "not compatible"
            }
            .to_owned(),
        );
    }
    mandatory
        .iter()
        .find(|name| result_of(name) != Some(Met::Met))
        .map(|name| unknown_reason(name, authorized))
}

/// Why the requirement `name` counts as unknown.
fn unknown_reason(name: &str, authorized: bool) -> String {
    if !authorized && is_id_requirement(name) {
        format!("{name} unknown: inventory ids go only to peers with a grant for the project")
    } else {
        format!("{name} unknown")
    }
}

/// The order of two candidates (see the [module](self)).
fn order(x: &Candidate, y: &Candidate, now_unix: i64, wants_benchmark: bool) -> Ordering {
    group(x, now_unix)
        .cmp(&group(y, now_unix))
        .then_with(|| y.preferred_met.cmp(&x.preferred_met))
        .then_with(|| {
            if wants_benchmark {
                evidence(x).cmp(&evidence(y))
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| time(x).cmp(&time(y)))
        .then_with(|| y.experience.0.cmp(&x.experience.0))
        .then_with(|| x.id().cmp(&y.id()))
}

/// 0 for an offer available and in force, 1 for a card alone or an offer
/// that confirms nothing any more, 2 for a busy one.
fn group(candidate: &Candidate, now_unix: i64) -> u8 {
    let Some(offer) = &candidate.offer else {
        return 1;
    };
    let in_force = parse_timestamp(&offer.expires_at).is_some_and(|at| now_unix < at);
    match offer.status {
        OfferStatus::Available if in_force => 0,
        OfferStatus::Busy => 2,
        _ => 1,
    }
}

/// 0 for a measurement, 1 for a declared specification, 2 for no usable
/// report.
fn evidence(candidate: &Candidate) -> u8 {
    match &candidate.benchmark {
        Some((kind, _)) if kind == MeasurementKind::DeclaredSpec.as_str() => 1,
        Some(_) => 0,
        None => 2,
    }
}

/// The time the offer says it would take, unknown last.
fn time(candidate: &Candidate) -> (bool, u64) {
    let declared = candidate.offer.as_ref().and_then(|offer| {
        offer
            .estimated_start_delay_ms?
            .checked_add(offer.estimated_duration_ms?)
    });
    match declared {
        Some(ms) => (false, ms),
        None => (true, 0),
    }
}

#[cfg(test)]
mod tests {
    use a2a::{AgentCapabilities, AgentSkill};

    use super::*;
    use crate::discovery::matching::Evaluation;
    use crate::discovery::query::{
        Basis, Conditions, InventoryRequirements, OfferParts, RequirementResult,
    };
    use crate::grant::format_timestamp;
    use crate::identity::AgentKey;

    /// 2026-11-14T00:00:00Z.
    const NOW: i64 = 1_794_614_400;

    fn skill(id: &str, tags: &[&str], inputs: Option<&[&str]>) -> AgentSkill {
        AgentSkill {
            id: id.to_owned(),
            name: id.to_owned(),
            description: id.to_owned(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            examples: None,
            input_modes: inputs.map(|modes| modes.iter().map(|m| (*m).to_owned()).collect()),
            output_modes: None,
            security_requirements: None,
        }
    }

    fn card(skills: Vec<AgentSkill>) -> AgentCard {
        AgentCard {
            name: "c".to_owned(),
            description: "c".to_owned(),
            version: "1".to_owned(),
            supported_interfaces: Vec::new(),
            capabilities: AgentCapabilities::default(),
            default_input_modes: vec!["text/plain".to_owned()],
            default_output_modes: vec!["text/plain".to_owned()],
            skills,
            provider: None,
            documentation_url: None,
            icon_url: None,
            security_schemes: None,
            security_requirements: None,
            signatures: None,
        }
    }

    fn request(inputs: &[&str]) -> SearchRequest {
        SearchRequest {
            capability: "code.rust.review".to_owned(),
            input_media_types: inputs.iter().map(|t| (*t).to_owned()).collect(),
            ..SearchRequest::default()
        }
    }

    const TAG: &str = "agentnet:capability:v1:code.rust.review";

    #[test]
    fn the_card_needs_a_skill_of_the_capability_and_its_media_types() {
        let req = request(&["text/x-rust"]);
        // Not of the capability.
        let other = card(vec![skill("ask", &["ask"], None)]);
        assert_eq!(
            stage_one(&other, &req),
            Err("no skill of code.rust.review".to_owned())
        );
        // Of it, but its own modes do not take the type; the next skill's
        // do, and the card's defaults do not count for a skill with modes.
        let first = skill("png", &[TAG], Some(&["image/png"]));
        let second = skill("rust", &[TAG], Some(&["text/*"]));
        assert_eq!(
            stage_one(&card(vec![first.clone()]), &req),
            Err("input media type text/x-rust not accepted".to_owned())
        );
        assert_eq!(
            stage_one(&card(vec![first, second]), &req),
            Ok(Some("rust".to_owned()))
        );
        // Without modes of its own, the card's.
        let plain = card(vec![skill("plain", &["ocr", TAG], None)]);
        assert_eq!(
            stage_one(&plain, &request(&["text/plain"])),
            Ok(Some("plain".to_owned()))
        );
        assert!(stage_one(&plain, &request(&["image/png"])).is_err());
        // A card that declares no modes leaves the formats to the offer.
        let mut silent = plain.clone();
        silent.default_input_modes.clear();
        assert_eq!(
            stage_one(&silent, &request(&["image/png"])),
            Ok(Some("plain".to_owned()))
        );
    }

    fn offer(
        status: OfferStatus,
        results: Vec<RequirementResult>,
        times: (u64, u64),
    ) -> ServiceOffer {
        let key = AgentKey::from_bytes(&[7; 32]);
        let compatible = matches!(status, OfferStatus::Available | OfferStatus::Busy);
        let evaluation = Evaluation {
            skill_id: Some("repo-review".to_owned()),
            status,
            compatible,
            conditions: Conditions {
                external_cloud_used: None,
                processing_region: None,
                authorization_required: false,
            },
            requirement_results: results,
            unknown_requirements: Vec::new(),
            estimated_start_delay_ms: Some(times.0),
            estimated_duration_ms: Some(times.1),
            limitations: Vec::new(),
        };
        let parts = OfferParts {
            offer_id: "offer-1".to_owned(),
            agent_id: key.id(),
            requester: key.id(),
            query_id: "q-1".to_owned(),
            query_hash: format!("blake3:{}", "1".repeat(64)),
            agent_card_hash: format!("blake3:{}", "0".repeat(64)).parse().unwrap(),
            scope: "task:demo".to_owned(),
            inventory_revision: 1,
            valid_for_ms: 15_000,
            now_unix: NOW,
        };
        ServiceOffer::new(parts, evaluation).unwrap()
    }

    fn met(name: &str, mandatory: bool, result: Met) -> RequirementResult {
        RequirementResult {
            requirement: name.to_owned(),
            mandatory,
            result,
            basis: Some(Basis::Declared),
        }
    }

    fn candidate(peer: &str, offer: Option<ServiceOffer>) -> Candidate {
        Candidate {
            peer: peer.to_owned(),
            agent_id: None,
            skill_id: Some("repo-review".to_owned()),
            offer,
            card_hash: format!("blake3:{}", "0".repeat(64)).parse().unwrap(),
            card_signature: SignatureStatus::Unsigned,
            preferred_met: 0,
            benchmark: None,
            experience: (0, 0),
            grant_held: true,
            from_dht: false,
        }
    }

    fn names(ranked: &[Candidate]) -> Vec<&str> {
        ranked.iter().map(|c| c.peer.as_str()).collect()
    }

    #[test]
    fn incompatible_offers_go_and_the_rest_are_ordered() {
        let mut req = request(&[]);
        req.requirements.inventory = Some(InventoryRequirements {
            required: RequirementSet {
                mcp_integration_ids: vec!["example:repo-reader".to_owned()],
                min_offered_ram_mib: Some(4096),
                ..RequirementSet::default()
            },
            preferred: RequirementSet {
                model_ids: vec!["example:coder".to_owned()],
                ..RequirementSet::default()
            },
        });
        let both = || {
            vec![
                met("mcp_integration:example:repo-reader", true, Met::Met),
                met("min_offered_ram_mib", true, Met::Met),
            ]
        };
        let fast = offer(OfferStatus::Available, both(), (10, 100));
        let slow = offer(OfferStatus::Available, both(), (10, 900));
        let busy = offer(OfferStatus::Busy, both(), (1, 1));
        let failed = offer(
            OfferStatus::NotOffered,
            vec![
                met("mcp_integration:example:repo-reader", true, Met::NotMet),
                met("min_offered_ram_mib", true, Met::Met),
            ],
            (1, 1),
        );
        let small = offer(
            OfferStatus::NotOffered,
            vec![
                met("mcp_integration:example:repo-reader", true, Met::Met),
                met("min_offered_ram_mib", true, Met::NotMet),
            ],
            (1, 1),
        );
        // Compatible, but silent on the RAM: unknown, never met.
        let silent = offer(
            OfferStatus::Available,
            vec![met("mcp_integration:example:repo-reader", true, Met::Met)],
            (1, 1),
        );
        // Never told the id: no grant for the project.
        let mut unauthorized = candidate(
            "stranger",
            Some(offer(
                OfferStatus::Available,
                vec![met("min_offered_ram_mib", true, Met::Met)],
                (1, 1),
            )),
        );
        unauthorized.grant_held = false;
        let mut preferring = candidate("pref", Some(slow.clone()));
        preferring.preferred_met = 1;
        let mut known = candidate("known", Some(slow.clone()));
        known.experience = (3, 0);
        let (ranked, discarded) = stage_two(
            vec![
                candidate("busy", Some(busy)),
                candidate("slow", Some(slow)),
                candidate("e", Some(failed)),
                candidate("fast", Some(fast)),
                candidate("g", Some(small)),
                candidate("plain", None),
                candidate("silent", Some(silent)),
                unauthorized,
                preferring,
                known,
            ],
            &req,
            NOW,
        );
        // More preferences beat a better time; then the time; then the
        // experience; busy last of all.
        assert_eq!(names(&ranked), ["pref", "fast", "known", "slow", "busy"]);
        let reasons: Vec<(&str, Stage, &str)> = discarded
            .iter()
            .map(|d| (d.peer.as_str(), d.stage, d.reason.as_str()))
            .collect();
        assert_eq!(
            reasons,
            [
                (
                    "e",
                    Stage::Offer,
                    "mcp_integration:example:repo-reader not operational"
                ),
                ("g", Stage::Offer, "min_offered_ram_mib not met"),
                (
                    "plain",
                    Stage::Card,
                    "a plain A2A agent cannot confirm the requirements"
                ),
                ("silent", Stage::Offer, "min_offered_ram_mib unknown"),
                (
                    "stranger",
                    Stage::Offer,
                    "mcp_integration:example:repo-reader unknown: inventory ids go only to \
                     peers with a grant for the project"
                ),
            ]
        );
    }

    #[test]
    fn a_card_alone_ranks_between_available_and_busy_and_benchmarks_count_when_asked() {
        let req = request(&[]);
        let available = offer(OfferStatus::Available, Vec::new(), (5, 5));
        let busy = offer(OfferStatus::Busy, Vec::new(), (1, 1));
        let mut expired = available.clone();
        expired.expires_at = format_timestamp(NOW).unwrap();
        let (ranked, discarded) = stage_two(
            vec![
                candidate("busy", Some(busy)),
                candidate("plain", None),
                candidate("expired", Some(expired)),
                candidate("available", Some(available.clone())),
            ],
            &req,
            NOW,
        );
        assert!(discarded.is_empty());
        // An offer past its expiry confirms nothing now: with the card.
        assert_eq!(names(&ranked), ["available", "expired", "plain", "busy"]);

        // A usable report counts only when one is asked for: a
        // measurement first, then a declared specification, then none.
        let mut measured = candidate("measured", Some(available.clone()));
        measured.benchmark = Some(("own_measurement".to_owned(), 900.0));
        let mut declared = candidate("declared", Some(available.clone()));
        declared.benchmark = Some(("declared_spec".to_owned(), 100.0));
        // Faster, with no report.
        let faster = candidate(
            "faster",
            Some(offer(OfferStatus::Available, Vec::new(), (0, 1))),
        );
        let all = vec![faster, declared, measured];
        let (ranked, _) = stage_two(all.clone(), &req, NOW);
        assert_eq!(names(&ranked), ["faster", "declared", "measured"]);
        let wanting = SearchRequest {
            benchmark: Some(Wanted {
                profile: "service.task".to_owned(),
                profile_version: "1".to_owned(),
                config: serde_json::json!({}),
                model_revision: None,
                route: None,
                peer_pair: None,
            }),
            ..req
        };
        let (ranked, _) = stage_two(all, &wanting, NOW);
        assert_eq!(names(&ranked), ["measured", "declared", "faster"]);
    }

    #[test]
    fn the_dht_fields_are_optional() {
        // A request and a candidate of milestone 2 read as before, and
        // write as before while the flags are false.
        let req: SearchRequest =
            serde_json::from_value(serde_json::json!({"capability": "vision.ocr"})).unwrap();
        assert!(!req.exclude_dht);
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            serde_json::json!({"capability": "vision.ocr", "requirements": {}})
        );
        let excluded = SearchRequest {
            exclude_dht: true,
            ..req
        };
        assert_eq!(
            serde_json::to_value(&excluded).unwrap()["exclude_dht"],
            true
        );
        let plain = candidate("plain", None);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("from_dht").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<Candidate>(json).unwrap(), plain);
        let found = Candidate {
            from_dht: true,
            ..plain
        };
        assert_eq!(serde_json::to_value(&found).unwrap()["from_dht"], true);
        // The metrics of milestone 2 read with no lookup, and write back
        // byte for byte; a lookup's requests write when there are some.
        let hito2 = r#"{"time_to_first_valid_ms":null,"contacted":0,"messages":0,"bytes_sent":0,"bytes_received":0,"matching_us":0,"elapsed_ms":0}"#;
        let metrics: SearchMetrics = serde_json::from_str(hito2).unwrap();
        assert_eq!(metrics, SearchMetrics::default());
        assert_eq!(serde_json::to_string(&metrics).unwrap(), hito2);
        let looked_up = SearchMetrics {
            dht_requests: 20,
            ..metrics
        };
        let json = serde_json::to_value(&looked_up).unwrap();
        assert_eq!(json["dht_requests"], 20, "{json}");
        assert_eq!(
            serde_json::from_value::<SearchMetrics>(json).unwrap(),
            looked_up
        );
    }

    #[test]
    fn inventory_ids_go_only_with_ids() {
        let mut req = request(&["text/plain"]);
        req.project = Some("demo".to_owned());
        req.requirements.inventory = Some(InventoryRequirements {
            required: RequirementSet {
                mcp_integration_ids: vec!["example:repo-reader".to_owned()],
                ..RequirementSet::default()
            },
            preferred: RequirementSet {
                information_source_ids: vec!["project:repo-controller".to_owned()],
                min_offered_ram_mib: Some(1024),
                ..RequirementSet::default()
            },
        });
        let with = req.query("q-1", 3000, true);
        assert_eq!(with.requirements, req.requirements);
        with.validate().unwrap();
        let without = req.query("q-2", 1000, false);
        without.validate().unwrap();
        // Nor the project: a candidate without a grant for it is not told.
        assert_eq!(with.project.as_deref(), Some("demo"));
        assert_eq!(without.project, None);
        let json = String::from_utf8(without.to_jcs()).unwrap();
        assert!(!json.contains("example:repo-reader"), "{json}");
        assert!(!json.contains("project:repo-controller"), "{json}");
        // What is not an id still goes.
        assert!(json.contains("min_offered_ram_mib"), "{json}");
        assert_eq!(without.reply_timeout_ms, Some(1000));
        // With nothing left, no inventory at all: one spelling.
        req.requirements
            .inventory
            .as_mut()
            .unwrap()
            .preferred
            .min_offered_ram_mib = None;
        let bare = req.query("q-3", 1000, false);
        assert_eq!(bare.requirements.inventory, None);
        bare.validate().unwrap();
        assert_eq!(
            req.mandatory(),
            [
                "input_media_type:text/plain",
                "mcp_integration:example:repo-reader"
            ]
        );
    }
}
