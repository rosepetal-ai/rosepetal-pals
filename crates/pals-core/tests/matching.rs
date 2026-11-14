//! Service queries and offers (design §7, §8): a query checked before it is
//! answered, matched against the inventory and the worker's load only, the
//! offer signed for its requester and bound to its query, the requester's
//! one acceptance of it, and the frozen vectors.

use std::collections::BTreeMap;
use std::sync::Arc;

use pals_core::card_jws::CardHash;
use pals_core::discovery::matching::{Evaluation, evaluate};
use pals_core::discovery::query::{
    Basis, Conditions, InventoryRequirements, MAX_OFFER_BYTES, MAX_QUERY_BYTES, Met, OFFER_SCHEMA,
    OFFER_SIG_DOMAIN, OfferError, OfferParts, OfferStatus, QUERY_SCHEMA, QueryError,
    RequirementResult, RequirementSet, Requirements, ServiceOffer, ServiceQuery, TRUNCATED,
    accept_offer, query_hash, sign_offer,
};
use pals_core::discovery::wire::{self, ErrorCode, request, response};
use pals_core::discovery::{DISCOVERY_VERSION, DiscoveryContext, Load, PUBLIC_CARD, answer};
use pals_core::grant::{CLOCK_SKEW_S, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::view::{CursorKey, VIEW_SIG_DOMAIN, ViewCache};
use pals_core::inventory::{
    Inventory, InventorySnapshot, Observation, ObservationSource, OperationalStatus, Scope,
    Service, Visibility,
};
use pals_core::jcs;
use pals_core::signed::{Signed, SignedError};
use pals_core::store::Store;
use serde_json::{Value, json};

/// 2026-11-14T00:00:00Z.
const NOW: i64 = 1_794_614_400;
/// The hash of the frozen card vector, `card-jws-v1.json`.
const CARD_HASH: &str = "blake3:856acc066c7304adbae9c444204c10a0c2e3d113491fca62c1a06da74fedd4fe";

/// The key whose secret is 32 bytes of `byte`.
fn fixed_key(byte: u8) -> AgentKey {
    AgentKey::from_bytes(&[byte; 32])
}

fn ts(unix_s: i64) -> String {
    format_timestamp(unix_s).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn vector(name: &str) -> Value {
    let path = format!("{}/tests/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
}

/// The industrial OCR agent of the inventory vector: the public service
/// `ocr-metal-v1` (EU, no external cloud, `image/png` and `image/jpeg` in,
/// `application/json` out), the service `repo-review` of the project
/// `demo`, the MCP connection `example:repo-reader` of that project, and
/// public hardware with 65536 MiB installed and 8192 offered.
fn ocr_inventory() -> Inventory {
    Inventory::from_toml(vector("inventory-v1.json")["toml"].as_str().unwrap()).unwrap()
}

fn snapshot(inventory: Inventory, observations: &[(&str, Observation)]) -> InventorySnapshot {
    InventorySnapshot {
        inventory,
        observations: observations
            .iter()
            .map(|(id, observation)| ((*id).to_owned(), observation.clone()))
            .collect(),
        generation: 1,
    }
}

/// An adapter's observation of `status`, made at `observed_at` and holding
/// until `expires_at`.
fn observed(status: OperationalStatus, observed_at: i64, expires_at: i64) -> Observation {
    Observation {
        source: ObservationSource::Adapter {
            method: "mcp tools/list".to_owned(),
        },
        status,
        observed_at: ts(observed_at),
        expires_at: ts(expires_at),
        detail: (status == OperationalStatus::Failed)
            .then(|| "missing tools: read_file".to_owned()),
        tools: Vec::new(),
    }
}

/// The query of design §7.1, as it is written there.
fn spec_query() -> ServiceQuery {
    let example = json!({
        "schema": "agentnet.service-query/1",
        "query_id": "q-ocr-001",
        "capability": "vision.ocr",
        "input_media_types": ["image/png"],
        "output_media_types": ["application/json"],
        "requirements": {
            "language": "es",
            "external_cloud_allowed": false,
            "required_region": "EU"
        },
        "summary": "Texto pequeño grabado sobre una superficie metálica",
        "reply_timeout_ms": 3000,
        "execution_authorized": false
    });
    ServiceQuery::from_jcs(&jcs::canonical(&example)).expect("the example of §7.1 is a query")
}

/// A query for `capability`, with no media types and no requirements.
fn query(capability: &str) -> ServiceQuery {
    ServiceQuery {
        schema: QUERY_SCHEMA.to_owned(),
        query_id: "q-1".to_owned(),
        capability: capability.to_owned(),
        input_media_types: Vec::new(),
        output_media_types: Vec::new(),
        requirements: Requirements::default(),
        summary: None,
        reply_timeout_ms: None,
        requested_inventory_sections: Vec::new(),
        project: None,
        execution_authorized: false,
    }
}

/// A query for `vision.ocr` that requires `required` and prefers
/// `preferred` of the inventory.
fn ocr_query_with(required: RequirementSet, preferred: RequirementSet) -> ServiceQuery {
    let mut query = query("vision.ocr");
    query.requirements.inventory = Some(InventoryRequirements {
        required,
        preferred,
    });
    query
}

/// A worker with nothing running, that runs two tasks at a time.
fn idle() -> Load {
    Load {
        running: 0,
        queued: 0,
        max_concurrent: 2,
        max_queue: 8,
    }
}

/// The result of `requirement` in `evaluation`.
fn result_of<'a>(evaluation: &'a Evaluation, requirement: &str) -> &'a RequirementResult {
    evaluation
        .requirement_results
        .iter()
        .find(|result| result.requirement == requirement)
        .unwrap_or_else(|| {
            panic!(
                "no result for {requirement} in {:?}",
                evaluation.requirement_results
            )
        })
}

fn met(evaluation: &Evaluation, requirement: &str) -> (Met, Option<Basis>, bool) {
    let result = result_of(evaluation, requirement);
    (result.result, result.basis, result.mandatory)
}

/// The OCR inventory with its OCR service changed by `edit`.
fn with_ocr_service(edit: impl FnOnce(&mut Service)) -> Inventory {
    let mut inventory = ocr_inventory();
    edit(&mut inventory.services[0]);
    inventory.validate().unwrap();
    inventory
}

/// The context `answer` answers with for the agent of `key`, whose
/// inventory is `inventory`: a store of its own, the card hash of the
/// vector for every scope, the worker's load `load`, offers valid 15 s.
fn context(key: &Arc<AgentKey>, inventory: Inventory, load: Load) -> DiscoveryContext {
    let snapshot = Arc::new(snapshot(inventory, &[]));
    DiscoveryContext {
        key: Arc::clone(key),
        store: Arc::new(Store::open_in_memory(key.id()).unwrap()),
        inventory: Arc::new(move || Arc::clone(&snapshot)),
        card_hashes: BTreeMap::from([(PUBLIC_CARD.to_owned(), CARD_HASH.parse().unwrap())]),
        records: Arc::new(|| (b"contact".to_vec(), b"announce".to_vec())),
        accepted_issuers: vec![key.id()],
        public: true,
        cursor_key: CursorKey::random(),
        views: ViewCache::new(),
        load: Arc::new(load),
        offer_valid_for_ms: 15_000,
    }
}

/// A `ServiceQuery` request of `bytes`, without a grant.
fn query_request(bytes: Vec<u8>) -> wire::Request {
    wire::Request {
        version: DISCOVERY_VERSION,
        max_response_bytes: 0,
        grant_token: String::new(),
        op: Some(request::Op::ServiceQuery(wire::ServiceQuery {
            query: bytes,
        })),
    }
}

/// What `ctx` answers `peer`'s query `bytes` with: the offer's signed JSON,
/// or the error.
fn ask(ctx: &DiscoveryContext, peer: &AgentId, bytes: Vec<u8>) -> Result<Vec<u8>, wire::Error> {
    match answer(ctx, peer, query_request(bytes), NOW).result {
        Some(response::Result::ServiceOffer(offer)) => Ok(offer),
        Some(response::Result::Error(error)) => Err(error),
        other => panic!("an offer or an error, not {other:?}"),
    }
}

/// The offer `ctx` makes `peer` for `query`, as `peer` accepts it.
fn offer_from(
    ctx: &DiscoveryContext,
    peer: &AgentId,
    query: &ServiceQuery,
) -> Signed<ServiceOffer> {
    let bytes = ask(ctx, peer, query.to_jcs()).expect("an offer");
    accept_offer(&bytes, &ctx.key.id(), peer, query, NOW).expect("a valid offer")
}

/// `offer` without what changes from one offer to the next for the same
/// query: its id and its times.
fn timeless(offer: &ServiceOffer) -> ServiceOffer {
    ServiceOffer {
        offer_id: String::new(),
        issued_at: String::new(),
        expires_at: String::new(),
        ..offer.clone()
    }
}

/// The parts of an offer from the agent of `key` to `requester` for
/// `query`, in the public scope, at revision 1, valid 15 s from `NOW`.
fn parts(key: &AgentKey, requester: AgentId, query: &ServiceQuery) -> OfferParts {
    OfferParts {
        offer_id: "offer-001".to_owned(),
        agent_id: key.id(),
        requester,
        query_id: query.query_id.clone(),
        query_hash: query.hash(),
        agent_card_hash: CARD_HASH.parse().unwrap(),
        scope: Scope::Public.key(),
        inventory_revision: 1,
        valid_for_ms: 15_000,
        now_unix: NOW,
    }
}

/// The OCR agent's signed offer to `requester` for the query of §7.1.
fn signed_spec_offer(key: &AgentKey, requester: AgentId) -> Signed<ServiceOffer> {
    let query = spec_query();
    let evaluation = evaluate(
        &query,
        &snapshot(ocr_inventory(), &[]),
        &Scope::Public,
        idle(),
        NOW,
    );
    let offer = ServiceOffer::new(parts(key, requester, &query), evaluation).unwrap();
    sign_offer(key, offer).unwrap()
}

#[test]
fn ocr_example_from_the_spec_is_available_and_compatible() {
    let query = spec_query();
    assert_eq!(query.query_id, "q-ocr-001");
    assert_eq!(query.reply_timeout_ms, Some(3000));
    let evaluation = evaluate(
        &query,
        &snapshot(ocr_inventory(), &[]),
        &Scope::Public,
        idle(),
        NOW,
    );
    assert_eq!(evaluation.status, OfferStatus::Available);
    assert!(evaluation.compatible);
    assert_eq!(evaluation.skill_id.as_deref(), Some("ocr-metal-v1"));
    assert_eq!(
        evaluation.conditions,
        Conditions {
            external_cloud_used: Some(false),
            processing_region: Some("EU".to_owned()),
            // A public requester holds no grant to run it.
            authorization_required: true,
        }
    );
    for requirement in [
        "input_media_type:image/png",
        "output_media_type:application/json",
        "language",
        "external_cloud_allowed",
        "required_region",
    ] {
        assert_eq!(
            met(&evaluation, requirement),
            (Met::Met, Some(Basis::Declared), true),
            "{requirement}"
        );
    }
    assert_eq!(evaluation.requirement_results.len(), 5);
    assert!(evaluation.unknown_requirements.is_empty());
    // The times are the service's declarations, never measurements.
    assert_eq!(evaluation.estimated_start_delay_ms, Some(500));
    assert_eq!(evaluation.estimated_duration_ms, Some(4000));
    assert_eq!(evaluation.limitations, ["Images up to 20 MiB"]);
}

#[test]
fn capability_not_offered_and_private_service_look_the_same() {
    let key = Arc::new(fixed_key(0x01));
    let requester = fixed_key(0x02).id();
    // One agent offers `audio.transcribe` privately and `code.rust.review`
    // to the project `demo` only; the other offers neither.
    let mut hidden = ocr_inventory();
    hidden.services.push(Service {
        id: "transcriber".to_owned(),
        name: "Transcriber".to_owned(),
        description: "Transcribes meetings".to_owned(),
        capabilities: vec!["audio.transcribe".to_owned()],
        visibility: Visibility::Private,
        ..Service::default()
    });
    hidden.validate().unwrap();
    let mut without = ocr_inventory();
    without
        .services
        .retain(|service| service.id == "ocr-metal-v1");
    let hidden = context(&key, hidden, idle());
    let without = context(&key, without, idle());

    let mut required = RequirementSet {
        plugin_ids: vec!["example:vision-pack".to_owned()],
        min_offered_ram_mib: Some(1024),
        ..RequirementSet::default()
    };
    required
        .other
        .insert("gpu_vendor".to_owned(), json!("example"));
    for capability in ["audio.transcribe", "code.rust.review"] {
        let mut asked = ocr_query_with(required.clone(), RequirementSet::default());
        asked.capability = capability.to_owned();
        asked.requirements.required_region = Some("EU".to_owned());
        let shown = offer_from(&hidden, &requester, &asked);
        let none = offer_from(&without, &requester, &asked);
        assert_eq!(timeless(&shown.doc), timeless(&none.doc), "{capability}");
        assert_ne!(shown.doc.offer_id, none.doc.offer_id);
        let offer = &shown.doc;
        assert_eq!(offer.status, OfferStatus::NotOffered, "{capability}");
        assert!(!offer.compatible);
        assert_eq!(offer.skill_id, None);
        assert!(offer.requirement_results.is_empty());
        assert!(offer.unknown_requirements.is_empty());
        assert_eq!(offer.estimated_start_delay_ms, None);
        assert!(offer.limitations.is_empty());
        assert_eq!(offer.scope, "public");
        assert_eq!(offer.inventory_revision, 1);
        // And the same as for a capability nobody offers.
        let mut nothing = asked.clone();
        nothing.capability = "nothing.here".to_owned();
        let nobody = offer_from(&hidden, &requester, &nothing);
        assert_eq!(
            ServiceOffer {
                query_hash: String::new(),
                ..timeless(&nobody.doc)
            },
            ServiceOffer {
                query_hash: String::new(),
                ..timeless(offer)
            }
        );
    }
}

#[test]
fn media_types_must_all_be_supported() {
    let inventory = snapshot(ocr_inventory(), &[]);
    let mut asked = query("vision.ocr");
    asked.input_media_types = vec!["image/png".to_owned(), "image/tiff".to_owned()];
    asked.output_media_types = vec!["application/json".to_owned()];
    let evaluation = evaluate(&asked, &inventory, &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "input_media_type:image/png"),
        (Met::Met, Some(Basis::Declared), true)
    );
    assert_eq!(
        met(&evaluation, "input_media_type:image/tiff"),
        (Met::NotMet, Some(Basis::Declared), true)
    );
    assert!(!evaluation.compatible);
    assert_eq!(evaluation.status, OfferStatus::NotOffered);
    // The service it is about stays named, with why.
    assert_eq!(evaluation.skill_id.as_deref(), Some("ocr-metal-v1"));

    // An output it does not produce, too.
    let mut asked = query("vision.ocr");
    asked.output_media_types = vec!["application/json".to_owned(), "text/plain".to_owned()];
    let evaluation = evaluate(&asked, &inventory, &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "output_media_type:text/plain").0,
        Met::NotMet
    );
    assert!(!evaluation.compatible);

    // `type/*` and `*/*` accept what they cover; parameters and case do
    // not change a type.
    let wide = with_ocr_service(|service| {
        service.input_modes = vec!["image/*".to_owned()];
        service.output_modes = vec!["*/*".to_owned()];
    });
    let mut asked = query("vision.ocr");
    asked.input_media_types = vec!["image/tiff".to_owned(), "IMAGE/PNG; q=1".to_owned()];
    asked.output_media_types = vec!["text/csv".to_owned()];
    let evaluation = evaluate(&asked, &snapshot(wide, &[]), &Scope::Public, idle(), NOW);
    assert!(
        evaluation.compatible,
        "{:?}",
        evaluation.requirement_results
    );
    let mut asked = query("vision.ocr");
    asked.input_media_types = vec!["text/plain".to_owned()];
    let wide = with_ocr_service(|service| service.input_modes = vec!["image/*".to_owned()]);
    let evaluation = evaluate(&asked, &snapshot(wide, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "input_media_type:text/plain").0,
        Met::NotMet
    );

    // A service that declares no input modes cannot show it takes one.
    let silent = with_ocr_service(|service| service.input_modes.clear());
    let mut asked = query("vision.ocr");
    asked.input_media_types = vec!["image/png".to_owned()];
    let evaluation = evaluate(&asked, &snapshot(silent, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "input_media_type:image/png"),
        (Met::Unknown, None, true)
    );
    assert_eq!(evaluation.status, OfferStatus::NeedsInformation);
}

#[test]
fn external_cloud_forbidden_but_used_is_incompatible() {
    let mut asked = query("vision.ocr");
    asked.requirements.external_cloud_allowed = Some(false);

    let cloud = with_ocr_service(|service| service.external_cloud_used = Some(true));
    let evaluation = evaluate(&asked, &snapshot(cloud, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "external_cloud_allowed"),
        (Met::NotMet, Some(Basis::Declared), true)
    );
    assert!(!evaluation.compatible);
    assert_eq!(evaluation.status, OfferStatus::NotOffered);
    assert_eq!(evaluation.conditions.external_cloud_used, Some(true));

    // Undeclared, it is unknown, never met.
    let unsaid = with_ocr_service(|service| service.external_cloud_used = None);
    let evaluation = evaluate(&asked, &snapshot(unsaid, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "external_cloud_allowed"),
        (Met::Unknown, None, true)
    );
    assert_eq!(evaluation.status, OfferStatus::NeedsInformation);
    assert_eq!(evaluation.conditions.external_cloud_used, None);

    // Allowed, it asks for nothing.
    asked.requirements.external_cloud_allowed = Some(true);
    let cloud = with_ocr_service(|service| service.external_cloud_used = Some(true));
    let evaluation = evaluate(&asked, &snapshot(cloud, &[]), &Scope::Public, idle(), NOW);
    assert!(evaluation.compatible);
    assert!(evaluation.requirement_results.is_empty());
}

#[test]
fn region_unknown_is_unknown() {
    let mut asked = query("vision.ocr");
    asked.requirements.required_region = Some("EU".to_owned());
    asked.requirements.language = Some("es".to_owned());
    let unsaid = with_ocr_service(|service| {
        service.processing_region = None;
        service.languages.clear();
    });
    let evaluation = evaluate(&asked, &snapshot(unsaid, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "required_region"),
        (Met::Unknown, None, true)
    );
    assert_eq!(met(&evaluation, "language"), (Met::Unknown, None, true));
    assert!(!evaluation.compatible);
    assert_eq!(evaluation.status, OfferStatus::NeedsInformation);
    assert_eq!(evaluation.conditions.processing_region, None);

    // Another region, or language, declared is not met.
    let elsewhere = with_ocr_service(|service| {
        service.processing_region = Some("US".to_owned());
        service.languages = vec!["en".to_owned()];
    });
    let evaluation = evaluate(
        &asked,
        &snapshot(elsewhere, &[]),
        &Scope::Public,
        idle(),
        NOW,
    );
    assert_eq!(met(&evaluation, "required_region").0, Met::NotMet);
    assert_eq!(met(&evaluation, "language").0, Met::NotMet);
    assert_eq!(evaluation.status, OfferStatus::NotOffered);
}

#[test]
fn mandatory_unknown_requirement_is_never_met() {
    let mut required = RequirementSet {
        plugin_ids: vec![
            "example:vision-pack".to_owned(),
            "example:absent".to_owned(),
        ],
        ..RequirementSet::default()
    };
    required.other.insert("tpu_generation".to_owned(), json!(5));
    let mut preferred = RequirementSet::default();
    preferred
        .other
        .insert("favourite_colour".to_owned(), json!("blue"));
    let mut asked = ocr_query_with(required, preferred);
    asked
        .requirements
        .other
        .insert("gpu_vendor".to_owned(), json!("example"));

    let evaluation = evaluate(
        &asked,
        &snapshot(ocr_inventory(), &[]),
        &Scope::Public,
        idle(),
        NOW,
    );
    assert_eq!(
        met(&evaluation, "plugin:example:vision-pack"),
        (Met::Met, Some(Basis::Declared), true)
    );
    assert_eq!(
        met(&evaluation, "plugin:example:absent"),
        (Met::Unknown, None, true)
    );
    assert_eq!(met(&evaluation, "gpu_vendor"), (Met::Unknown, None, true));
    assert_eq!(
        met(&evaluation, "inventory.tpu_generation"),
        (Met::Unknown, None, true)
    );
    // Only the mandatory ones are listed as unknown; a preferred one this
    // agent does not understand is ignored.
    assert_eq!(
        evaluation.unknown_requirements,
        ["gpu_vendor", "inventory.tpu_generation"]
    );
    assert!(
        evaluation
            .requirement_results
            .iter()
            .all(|result| !result.requirement.contains("favourite_colour"))
    );
    assert!(!evaluation.compatible);
    assert_eq!(evaluation.status, OfferStatus::NeedsInformation);
}

#[test]
fn mcp_configured_but_not_operational_is_not_met() {
    // Scenario §14: the connection is configured, and its adapter found it
    // does not serve the tool offered.
    let mut asked = query("code.rust.review");
    asked.requirements.inventory = Some(InventoryRequirements {
        required: RequirementSet {
            mcp_integration_ids: vec!["example:repo-reader".to_owned()],
            ..RequirementSet::default()
        },
        preferred: RequirementSet::default(),
    });
    let demo = Scope::Project("demo".to_owned());
    let failed = observed(OperationalStatus::Failed, NOW - 60, NOW + 1740);
    let inventory = snapshot(ocr_inventory(), &[("example:repo-reader", failed)]);
    let evaluation = evaluate(&asked, &inventory, &demo, idle(), NOW);
    assert_eq!(evaluation.skill_id.as_deref(), Some("repo-review"));
    assert_eq!(
        met(&evaluation, "mcp_integration:example:repo-reader"),
        (Met::NotMet, Some(Basis::Observed), true)
    );
    assert!(!evaluation.compatible);
    assert_eq!(evaluation.status, OfferStatus::NotOffered);

    // Observed operational, it is met, and on that basis.
    let ok = observed(OperationalStatus::Ok, NOW - 60, NOW + 1740);
    let inventory = snapshot(ocr_inventory(), &[("example:repo-reader", ok)]);
    let evaluation = evaluate(&asked, &inventory, &demo, idle(), NOW);
    assert_eq!(
        met(&evaluation, "mcp_integration:example:repo-reader"),
        (Met::Met, Some(Basis::Observed), true)
    );
    assert_eq!(evaluation.status, OfferStatus::Available);
    // A project scope's grant does not send tasks.
    assert!(evaluation.conditions.authorization_required);
    let task = Scope::Task("demo".to_owned());
    let evaluation = evaluate(&asked, &inventory, &task, idle(), NOW);
    assert!(!evaluation.conditions.authorization_required);
}

#[test]
fn hidden_and_missing_integrations_are_both_unknown() {
    // `example:repo-reader` exists, for the project `demo` only;
    // `example:nowhere` does not exist. Observed operational or not, a
    // public requester learns the same of both.
    let asked = ocr_query_with(
        RequirementSet {
            mcp_integration_ids: vec![
                "example:repo-reader".to_owned(),
                "example:nowhere".to_owned(),
            ],
            information_source_ids: vec!["project:repo-controller".to_owned()],
            ..RequirementSet::default()
        },
        RequirementSet::default(),
    );
    let ok = observed(OperationalStatus::Ok, NOW - 60, NOW + 1740);
    for observations in [vec![], vec![("example:repo-reader", ok)]] {
        let inventory = snapshot(ocr_inventory(), &observations);
        let evaluation = evaluate(&asked, &inventory, &Scope::Public, idle(), NOW);
        let hidden = result_of(&evaluation, "mcp_integration:example:repo-reader");
        let missing = result_of(&evaluation, "mcp_integration:example:nowhere");
        assert_eq!((hidden.result, hidden.basis), (Met::Unknown, None));
        assert_eq!(
            RequirementResult {
                requirement: String::new(),
                ..hidden.clone()
            },
            RequirementResult {
                requirement: String::new(),
                ..missing.clone()
            }
        );
        assert_eq!(
            met(&evaluation, "information_source:project:repo-controller"),
            (Met::Unknown, None, true)
        );
        assert_eq!(evaluation.status, OfferStatus::NeedsInformation);
    }
}

#[test]
fn unobserved_or_expired_integrations_are_unknown() {
    // Shown in the scope, but with no observation in force: what cannot be
    // shown operational is never met (design §7.2).
    let asked = ocr_query_with(
        RequirementSet {
            mcp_integration_ids: vec!["example:repo-reader".to_owned()],
            ..RequirementSet::default()
        },
        RequirementSet::default(),
    );
    let demo = Scope::Project("demo".to_owned());
    let at = |observations: &[(&str, Observation)]| {
        let evaluation = evaluate(
            &asked,
            &snapshot(ocr_inventory(), observations),
            &demo,
            idle(),
            NOW,
        );
        let (result, basis, _) = met(&evaluation, "mcp_integration:example:repo-reader");
        (result, basis)
    };
    // Never observed.
    assert_eq!(at(&[]), (Met::Unknown, None));
    // Observed operational, expired beyond the skew of this agent's own
    // clock; or failed, expired too.
    let expired = NOW - CLOCK_SKEW_S - 1;
    for status in [OperationalStatus::Ok, OperationalStatus::Failed] {
        let old = observed(status, expired - 1800, expired);
        assert_eq!(
            at(&[("example:repo-reader", old)]),
            (Met::Unknown, None),
            "{status:?}"
        );
    }
    // An adapter that could not tell.
    let unsure = observed(OperationalStatus::Unknown, NOW - 60, NOW + 1740);
    assert_eq!(at(&[("example:repo-reader", unsure)]), (Met::Unknown, None));
    // Within the skew, it still holds.
    let late = observed(OperationalStatus::Ok, NOW - 1800, NOW - CLOCK_SKEW_S + 1);
    assert_eq!(
        at(&[("example:repo-reader", late)]),
        (Met::Met, Some(Basis::Observed))
    );
}

#[test]
fn preferred_requirements_never_make_an_offer_incompatible() {
    let mut preferred = RequirementSet {
        mcp_integration_ids: vec!["example:repo-reader".to_owned()],
        plugin_ids: vec![
            "example:absent".to_owned(),
            "example:vision-pack".to_owned(),
        ],
        model_ids: vec!["example:ocr-model".to_owned()],
        min_offered_ram_mib: Some(1 << 20),
        accelerators: vec!["npu".to_owned()],
        ..RequirementSet::default()
    };
    preferred
        .other
        .insert("cooling".to_owned(), json!("liquid"));
    let asked = ocr_query_with(RequirementSet::default(), preferred);
    let evaluation = evaluate(
        &asked,
        &snapshot(ocr_inventory(), &[]),
        &Scope::Public,
        idle(),
        NOW,
    );
    assert!(
        evaluation.compatible,
        "{:?}",
        evaluation.requirement_results
    );
    assert_eq!(evaluation.status, OfferStatus::Available);
    assert!(evaluation.unknown_requirements.is_empty());
    assert!(
        evaluation
            .requirement_results
            .iter()
            .all(|result| !result.mandatory)
    );
    assert_eq!(
        met(&evaluation, "min_offered_ram_mib"),
        (Met::NotMet, Some(Basis::Declared), false)
    );
    assert_eq!(
        met(&evaluation, "plugin:example:absent"),
        (Met::Unknown, None, false)
    );
    assert_eq!(
        met(&evaluation, "model:example:ocr-model"),
        (Met::Met, Some(Basis::Declared), false)
    );
}

#[test]
fn resource_requirements_compare_offered_not_installed() {
    // Installed 65536 MiB of RAM, 8192 offered; a GPU of 24576 MiB
    // installed, 8192 offered.
    let required = RequirementSet {
        min_offered_ram_mib: Some(16384),
        min_offered_vram_mib: Some(8192),
        accelerators: vec!["gpu".to_owned(), "npu".to_owned()],
        ..RequirementSet::default()
    };
    let asked = ocr_query_with(required, RequirementSet::default());
    let inventory = snapshot(ocr_inventory(), &[]);
    let evaluation = evaluate(&asked, &inventory, &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "min_offered_ram_mib"),
        (Met::NotMet, Some(Basis::Declared), true)
    );
    assert_eq!(
        met(&evaluation, "min_offered_vram_mib"),
        (Met::Met, Some(Basis::Declared), true)
    );
    assert_eq!(met(&evaluation, "accelerator:gpu").0, Met::Met);
    assert_eq!(met(&evaluation, "accelerator:npu").0, Met::NotMet);
    assert!(!evaluation.compatible);

    let asked = ocr_query_with(
        RequirementSet {
            min_offered_ram_mib: Some(8192),
            min_offered_vram_mib: Some(16384),
            ..RequirementSet::default()
        },
        RequirementSet::default(),
    );
    let evaluation = evaluate(&asked, &inventory, &Scope::Public, idle(), NOW);
    assert_eq!(met(&evaluation, "min_offered_ram_mib").0, Met::Met);
    assert_eq!(met(&evaluation, "min_offered_vram_mib").0, Met::NotMet);

    // Hardware the scope does not show, or that offers nothing, is unknown.
    let mut hidden = ocr_inventory();
    hidden.hardware.as_mut().unwrap().visibility = Visibility::Private;
    let asked = ocr_query_with(
        RequirementSet {
            min_offered_ram_mib: Some(1),
            ..RequirementSet::default()
        },
        RequirementSet::default(),
    );
    let evaluation = evaluate(&asked, &snapshot(hidden, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(
        met(&evaluation, "min_offered_ram_mib"),
        (Met::Unknown, None, true)
    );
    let mut nothing_offered = ocr_inventory();
    nothing_offered.hardware.as_mut().unwrap().body.offered = None;
    let evaluation = evaluate(
        &asked,
        &snapshot(nothing_offered, &[]),
        &Scope::Public,
        idle(),
        NOW,
    );
    assert_eq!(met(&evaluation, "min_offered_ram_mib").0, Met::Unknown);
}

#[test]
fn busy_load_gives_busy_status() {
    let inventory = snapshot(ocr_inventory(), &[]);
    let full = Load {
        running: 2,
        queued: 3,
        max_concurrent: 2,
        max_queue: 8,
    };
    let evaluation = evaluate(&spec_query(), &inventory, &Scope::Public, full, NOW);
    assert!(evaluation.compatible);
    assert_eq!(evaluation.status, OfferStatus::Busy);
    let one_free = Load { running: 1, ..full };
    let evaluation = evaluate(&spec_query(), &inventory, &Scope::Public, one_free, NOW);
    assert_eq!(evaluation.status, OfferStatus::Available);
    // Load never makes an incompatible offer look available or busy.
    let mut asked = spec_query();
    asked.requirements.required_region = Some("US".to_owned());
    let evaluation = evaluate(&asked, &inventory, &Scope::Public, full, NOW);
    assert_eq!(evaluation.status, OfferStatus::NotOffered);
}

#[test]
fn unknown_estimates_are_absent_never_zero() {
    let unsaid = with_ocr_service(|service| {
        service.estimated_start_delay_ms = None;
        service.estimated_duration_ms = None;
    });
    let key = fixed_key(0x01);
    let query = spec_query();
    let evaluation = evaluate(&query, &snapshot(unsaid, &[]), &Scope::Public, idle(), NOW);
    assert_eq!(evaluation.estimated_start_delay_ms, None);
    assert_eq!(evaluation.estimated_duration_ms, None);
    let offer = ServiceOffer::new(parts(&key, fixed_key(0x02).id(), &query), evaluation).unwrap();
    let signed = sign_offer(&key, offer).unwrap();
    let json: Value = serde_json::from_slice(&signed.to_json()).unwrap();
    let offer = json["offer"].as_object().unwrap();
    assert!(!offer.contains_key("estimated_start_delay_ms"));
    assert!(!offer.contains_key("estimated_duration_ms"));
    assert_eq!(offer["status"], "available");
}

#[test]
fn query_over_8_kib_is_too_large() {
    let mut asked = query("vision.ocr");
    let base = asked.to_jcs().len();
    // `"a":"…"` in `requirements`, sized to one byte over the limit.
    let filler = MAX_QUERY_BYTES + 1 - base - r#""a":"""#.len();
    asked
        .requirements
        .other
        .insert("a".to_owned(), json!("x".repeat(filler)));
    let bytes = asked.to_jcs();
    assert_eq!(bytes.len(), MAX_QUERY_BYTES + 1);
    assert_eq!(ServiceQuery::from_jcs(&bytes), Err(QueryError::TooLarge));

    let key = Arc::new(fixed_key(0x01));
    let ctx = context(&key, ocr_inventory(), idle());
    let error = ask(&ctx, &fixed_key(0x02).id(), bytes).unwrap_err();
    assert_eq!(error.code(), ErrorCode::TooLarge);
    assert!(!error.message.contains("xxxx"));

    // At the limit, it is read.
    asked
        .requirements
        .other
        .insert("a".to_owned(), json!("x".repeat(filler - 1)));
    let bytes = asked.to_jcs();
    assert_eq!(bytes.len(), MAX_QUERY_BYTES);
    assert_eq!(ServiceQuery::from_jcs(&bytes).unwrap(), asked);
}

#[test]
fn offer_stays_within_16_kib() {
    // Forty limitations of 1 KiB.
    let wordy = with_ocr_service(|service| {
        service.limitations = (0..40)
            .map(|n| format!("{n:02} {}", "l".repeat(1021)))
            .collect();
    });
    let key = Arc::new(fixed_key(0x01));
    let requester = fixed_key(0x02).id();
    let ctx = context(&key, wordy, idle());
    let asked = spec_query();
    let bytes = ask(&ctx, &requester, asked.to_jcs()).unwrap();
    assert!(bytes.len() <= MAX_OFFER_BYTES, "{}", bytes.len());
    let offer = accept_offer(&bytes, &key.id(), &requester, &asked, NOW)
        .unwrap()
        .doc;
    // The limitations are cut at the end, and the offer says so; what the
    // query asked is all answered.
    assert_eq!(
        offer.limitations.last().map(String::as_str),
        Some(TRUNCATED)
    );
    assert!((2..40).contains(&offer.limitations.len()));
    for (n, limitation) in offer.limitations.iter().rev().skip(1).rev().enumerate() {
        assert!(limitation.starts_with(&format!("{n:02} ")));
    }
    assert_eq!(offer.requirement_results.len(), 5);
    assert_eq!(offer.status, OfferStatus::Available);

    // A query that asks for many things, too: then its results are cut at
    // the end as well, the preferred ones first.
    let required = RequirementSet {
        mcp_integration_ids: (0..32).map(|n| format!("example:mcp-{n:02}")).collect(),
        information_source_ids: (0..32).map(|n| format!("example:info-{n:02}")).collect(),
        plugin_ids: (0..32).map(|n| format!("example:plugin-{n:02}")).collect(),
        ..RequirementSet::default()
    };
    let asked = ocr_query_with(required.clone(), required);
    assert!(asked.to_jcs().len() <= MAX_QUERY_BYTES);
    let bytes = ask(&ctx, &requester, asked.to_jcs()).unwrap();
    assert!(bytes.len() <= MAX_OFFER_BYTES, "{}", bytes.len());
    let offer = accept_offer(&bytes, &key.id(), &requester, &asked, NOW)
        .unwrap()
        .doc;
    assert_eq!(offer.limitations, [TRUNCATED]);
    assert!((1..192).contains(&offer.requirement_results.len()));
    assert_eq!(
        offer.requirement_results[0].requirement,
        "mcp_integration:example:mcp-00"
    );
    assert_eq!(offer.status, OfferStatus::NeedsInformation);

    // An offer that fits is never cut.
    let offer = signed_spec_offer(&key, requester).doc;
    assert!(!offer.limitations.iter().any(|l| l == TRUNCATED));
}

#[test]
fn offer_over_16_kib_is_refused_by_the_client() {
    let key = fixed_key(0x01);
    let requester = fixed_key(0x02).id();
    let query = spec_query();
    let oversized = vec![b' '; 16 * 1024 + 1];
    assert_eq!(
        accept_offer(&oversized, &key.id(), &requester, &query, NOW),
        Err(OfferError::TooLarge)
    );
    // A real offer padded past the limit, too: refused before it is read.
    let mut padded = signed_spec_offer(&key, requester).to_json();
    padded.resize(MAX_OFFER_BYTES + 1, b' ');
    assert_eq!(
        accept_offer(&padded, &key.id(), &requester, &query, NOW),
        Err(OfferError::TooLarge)
    );
}

#[test]
fn execution_authorized_true_is_refused() {
    let mut asked = spec_query();
    asked.execution_authorized = true;
    let bytes = asked.to_jcs();
    assert!(matches!(
        ServiceQuery::from_jcs(&bytes),
        Err(QueryError::Invalid(_))
    ));
    let key = Arc::new(fixed_key(0x01));
    let ctx = context(&key, ocr_inventory(), idle());
    let error = ask(&ctx, &fixed_key(0x02).id(), bytes).unwrap_err();
    assert_eq!(error.code(), ErrorCode::BadRequest);
    assert_eq!(error.message, "a query never authorizes execution");
}

#[test]
fn invalid_queries_are_bad_requests_that_quote_nothing() {
    let key = Arc::new(fixed_key(0x01));
    let ctx = context(&key, ocr_inventory(), idle());
    let peer = fixed_key(0x02).id();
    let base = serde_json::to_value(spec_query()).unwrap();
    let with = |pointer: &str, value: Value| {
        let mut query = base.clone();
        *query.pointer_mut(pointer).unwrap() = value;
        jcs::canonical(&query)
    };
    let many = |n: usize, item: &str| json!(vec![item; n]);
    let mut not_canonical = jcs::canonical(&base);
    not_canonical.insert(1, b' ');
    let mut with_null = base.clone();
    with_null["project"] = Value::Null;
    let mut unknown = base.clone();
    unknown["peers"] = json!(["carol"]);
    let with_project = |project: &str| {
        let mut query = base.clone();
        query["project"] = json!(project);
        jcs::canonical(&query)
    };
    for (name, bytes) in [
        ("not JSON", b"{\"query".to_vec()),
        ("not canonical", not_canonical),
        ("a null", jcs::canonical(&with_null)),
        ("an unknown field", jcs::canonical(&unknown)),
        (
            "another schema",
            with("/schema", json!("agentnet.service-query/2")),
        ),
        ("an empty id", with("/query_id", json!(""))),
        ("a long id", with("/query_id", json!("q".repeat(65)))),
        ("an id with a slash", with("/query_id", json!("q/1"))),
        (
            "a capability with spaces",
            with("/capability", json!("vision ocr")),
        ),
        (
            "a reserved capability",
            with("/capability", json!("agentnet:ocr")),
        ),
        (
            "17 input types",
            with("/input_media_types", many(17, "image/png")),
        ),
        (
            "17 output types",
            with("/output_media_types", many(17, "text/plain")),
        ),
        (
            "a type without a slash",
            with("/input_media_types", json!(["png"])),
        ),
        ("a long summary", with("/summary", json!("s".repeat(1025)))),
        (
            "33 required ids",
            with(
                "/requirements",
                json!({"inventory": {"required": {"plugin_ids": many(33, "example:p")}}}),
            ),
        ),
        (
            "33 preferred accelerators",
            with(
                "/requirements",
                json!({"inventory": {"preferred": {"accelerators": many(33, "gpu")}}}),
            ),
        ),
        // Unknown requirements are echoed in signed offers: never as an
        // empty name, nor one that is not printable ASCII.
        (
            "an empty requirement",
            with("/requirements", json!({"": true})),
        ),
        (
            "a requirement with a control character",
            with("/requirements", json!({"tier\u{7}": true})),
        ),
        (
            "an empty required inventory requirement",
            with("/requirements", json!({"inventory": {"required": {"": 1}}})),
        ),
        (
            "an empty preferred inventory requirement",
            with(
                "/requirements",
                json!({"inventory": {"preferred": {"": 1}}}),
            ),
        ),
        (
            "a long requirement",
            with("/requirements", json!({"q".repeat(256): true})),
        ),
        // A project is a project name, as the schema says.
        ("an empty project", with_project("")),
        ("a project with spaces", with_project("two words")),
        ("a project not in ASCII", with_project("diseño")),
        // No inventory requirements are left out, never `{}`.
        (
            "empty inventory requirements",
            with("/requirements", json!({"inventory": {}})),
        ),
    ] {
        let error = ask(&ctx, &peer, bytes.clone()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::BadRequest, "{name}: {error:?}");
        assert!(error.message.len() <= 256, "{name}");
        for echo in [
            "carol",
            "qqqq",
            "ssss",
            "vision ocr",
            "png\"",
            "two words",
            "diseño",
            "tier",
        ] {
            assert!(!error.message.contains(echo), "{name}: {}", error.message);
        }
        assert!(ServiceQuery::from_jcs(&bytes).is_err(), "{name}");
    }
    // A long reply timeout is no error: it is cut to 3 s.
    let patient = with("/reply_timeout_ms", json!(60_000));
    let read = ServiceQuery::from_jcs(&patient).unwrap();
    assert_eq!(read.reply_timeout(), std::time::Duration::from_millis(3000));
    ask(&ctx, &peer, patient).unwrap();
    // Nor an empty summary of 1 KiB.
    ask(&ctx, &peer, with("/summary", json!("s".repeat(1024)))).unwrap();
}

#[test]
fn offer_signature_binds_query_and_requester() {
    let key = fixed_key(0x01);
    let requester = fixed_key(0x02).id();
    let stranger = fixed_key(0x03).id();
    let query = spec_query();
    let signed = signed_spec_offer(&key, requester);
    let bytes = signed.to_json();

    let accepted = accept_offer(&bytes, &key.id(), &requester, &query, NOW).unwrap();
    assert_eq!(accepted, signed);
    assert_eq!(accepted.doc.query_hash, query_hash(&query.to_jcs()));
    assert_eq!(accepted.doc.requester, requester);

    // For another requester, from another provider.
    assert_eq!(
        accept_offer(&bytes, &key.id(), &stranger, &query, NOW),
        Err(OfferError::WrongRequester)
    );
    assert_eq!(
        accept_offer(&bytes, &stranger, &requester, &query, NOW),
        Err(OfferError::WrongProvider)
    );
    // For another query: another capability, or the same with another id.
    let mut other = query.clone();
    other.capability = "vision.ocr.handwriting".to_owned();
    assert_eq!(
        accept_offer(&bytes, &key.id(), &requester, &other, NOW),
        Err(OfferError::QueryMismatch)
    );
    let mut renamed = query.clone();
    renamed.query_id = "q-ocr-002".to_owned();
    assert_eq!(
        accept_offer(&bytes, &key.id(), &requester, &renamed, NOW),
        Err(OfferError::QueryMismatch)
    );
    // Changed after it was signed: the requester, or the status.
    for (pointer, value) in [
        ("/offer/requester", json!(stranger.to_string())),
        ("/offer/status", json!("busy")),
    ] {
        let mut tampered: Value = serde_json::from_slice(&bytes).unwrap();
        *tampered.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            accept_offer(
                &jcs::canonical(&tampered),
                &key.id(),
                &requester,
                &query,
                NOW
            ),
            Err(OfferError::Signed(SignedError::BadSignature)),
            "{pointer}"
        );
    }
    // Signed under another domain: a view page's signature is no offer's.
    let doc = serde_json::to_value(&signed.doc).unwrap();
    let foreign = json!({
        "offer": doc,
        "signature": pals_core::jcs::sign_domain(&key, VIEW_SIG_DOMAIN, &doc),
    });
    assert_eq!(
        accept_offer(
            &jcs::canonical(&foreign),
            &key.id(),
            &requester,
            &query,
            NOW
        ),
        Err(OfferError::Signed(SignedError::BadSignature))
    );
    assert_eq!(OFFER_SIG_DOMAIN, b"agentnet.offer/1\n");

    // Valid for 15 s, and 60 s of skew beyond them, only to refuse.
    let expires_at = NOW + 15;
    accept_offer(
        &bytes,
        &key.id(),
        &requester,
        &query,
        expires_at + CLOCK_SKEW_S - 1,
    )
    .unwrap();
    assert_eq!(
        accept_offer(
            &bytes,
            &key.id(),
            &requester,
            &query,
            expires_at + CLOCK_SKEW_S
        ),
        Err(OfferError::Expired)
    );
    assert_eq!(
        accept_offer(
            &bytes,
            &key.id(),
            &requester,
            &query,
            NOW - CLOCK_SKEW_S - 1
        ),
        Err(OfferError::Signed(SignedError::NotYetValid))
    );
}

#[test]
fn offer_validity_is_whole_seconds_rounded_up() {
    let key = fixed_key(0x01);
    let requester = fixed_key(0x02).id();
    let query = spec_query();
    for (valid_for_ms, seconds) in [(1, 1), (1000, 1), (1001, 2), (15_000, 15), (300_000, 300)] {
        let evaluation = evaluate(
            &query,
            &snapshot(ocr_inventory(), &[]),
            &Scope::Public,
            idle(),
            NOW,
        );
        let offer = ServiceOffer::new(
            OfferParts {
                valid_for_ms,
                ..parts(&key, requester, &query)
            },
            evaluation,
        )
        .unwrap();
        assert_eq!(offer.issued_at, ts(NOW));
        assert_eq!(offer.expires_at, ts(NOW + seconds), "{valid_for_ms}");
        let signed = sign_offer(&key, offer).unwrap();
        accept_offer(&signed.to_json(), &key.id(), &requester, &query, NOW).unwrap();
    }
}

#[test]
fn query_fields_have_one_spelling() {
    let full = {
        let mut query = spec_query();
        query.project = Some("demo".to_owned());
        query.requested_inventory_sections = vec![pals_core::inventory::Section::Mcp];
        let mut required = RequirementSet {
            mcp_integration_ids: vec!["example:repo-reader".to_owned()],
            execution_external_cloud_allowed: Some(false),
            min_offered_ram_mib: Some(1024),
            min_offered_vram_mib: Some(2048),
            accelerators: vec!["gpu".to_owned()],
            ..RequirementSet::default()
        };
        required
            .other
            .insert("example:tier".to_owned(), json!({"n": 1}));
        query.requirements.inventory = Some(InventoryRequirements {
            required,
            preferred: RequirementSet {
                model_ids: vec!["example:ocr-model".to_owned()],
                ..RequirementSet::default()
            },
        });
        query
    };
    // Every optional field, there and left out.
    let mut variants = vec![full.clone(), query("vision.ocr")];
    let mut drop = |edit: &dyn Fn(&mut ServiceQuery)| {
        let mut query = full.clone();
        edit(&mut query);
        variants.push(query);
    };
    drop(&|q| q.summary = None);
    drop(&|q| q.reply_timeout_ms = None);
    drop(&|q| q.project = None);
    drop(&|q| q.requirements.language = None);
    drop(&|q| q.requirements.external_cloud_allowed = None);
    drop(&|q| q.requirements.required_region = None);
    drop(&|q| q.requirements.inventory = None);
    drop(&|q| {
        let inventory = q.requirements.inventory.as_mut().unwrap();
        inventory.required.execution_external_cloud_allowed = None;
        inventory.required.min_offered_ram_mib = None;
        inventory.required.min_offered_vram_mib = None;
    });
    drop(&|q| q.requirements.inventory.as_mut().unwrap().preferred = RequirementSet::default());
    for query in variants {
        let bytes = query.to_jcs();
        let read = ServiceQuery::from_jcs(&bytes).unwrap();
        assert_eq!(read, query);
        assert_eq!(read.to_jcs(), bytes);
        assert_eq!(read.hash(), query_hash(&bytes));
    }
    // Absent is never spelled `null`, nor an empty list `[]`.
    let base = serde_json::to_value(query("vision.ocr")).unwrap();
    for (key, value) in [
        ("summary", Value::Null),
        ("reply_timeout_ms", Value::Null),
        ("project", Value::Null),
        ("input_media_types", json!([])),
        ("requested_inventory_sections", json!([])),
    ] {
        let mut spelled = base.clone();
        spelled[key] = value;
        assert!(
            ServiceQuery::from_jcs(&jcs::canonical(&spelled)).is_err(),
            "{key}"
        );
    }
    let mut spelled = base.clone();
    spelled["requirements"]["language"] = Value::Null;
    assert!(ServiceQuery::from_jcs(&jcs::canonical(&spelled)).is_err());
    // No inventory requirements are left out: `{}` is no second spelling of
    // them, read or written.
    for inventory in [json!({}), json!({"required": {}}), json!({"preferred": {}})] {
        let mut spelled = base.clone();
        spelled["requirements"]["inventory"] = inventory.clone();
        assert!(
            ServiceQuery::from_jcs(&jcs::canonical(&spelled)).is_err(),
            "{inventory}"
        );
    }
    let mut empty = query("vision.ocr");
    empty.requirements.inventory = Some(InventoryRequirements::default());
    assert!(empty.validate().is_err());
}

#[test]
fn offer_fields_have_one_spelling() {
    let key = fixed_key(0x01);
    let requester = fixed_key(0x02).id();
    let query = spec_query();
    let full = signed_spec_offer(&key, requester).doc;
    assert!(full.skill_id.is_some() && full.estimated_duration_ms.is_some());
    assert!(full.requirement_results.iter().all(|r| r.basis.is_some()));
    let mut variants = vec![full.clone()];
    let mut drop = |edit: &dyn Fn(&mut ServiceOffer)| {
        let mut offer = full.clone();
        edit(&mut offer);
        variants.push(offer);
    };
    drop(&|o| o.skill_id = None);
    drop(&|o| o.estimated_start_delay_ms = None);
    drop(&|o| o.estimated_duration_ms = None);
    drop(&|o| o.conditions.external_cloud_used = None);
    drop(&|o| o.conditions.processing_region = None);
    drop(&|o| o.requirement_results[0].basis = None);
    for offer in variants {
        let signed = Signed::issue(&key, offer.clone()).unwrap();
        let bytes = signed.to_json();
        let read = Signed::<ServiceOffer>::from_json(&bytes).unwrap();
        assert_eq!(read.doc, offer);
        assert_eq!(read.to_json(), bytes);
        accept_offer(&bytes, &key.id(), &requester, &query, NOW).unwrap();
    }
    // Absent is never spelled `null`: such an offer, signed as it is, is
    // refused.
    let base = serde_json::to_value(&full).unwrap();
    for pointer in [
        "/skill_id",
        "/estimated_start_delay_ms",
        "/conditions/processing_region",
        "/requirement_results/0/basis",
    ] {
        let mut spelled = base.clone();
        *spelled.pointer_mut(pointer).unwrap() = Value::Null;
        let doc = json!({
            "offer": spelled,
            "signature": pals_core::jcs::sign_domain(&key, OFFER_SIG_DOMAIN, &spelled),
        });
        assert_eq!(
            accept_offer(&jcs::canonical(&doc), &key.id(), &requester, &query, NOW),
            Err(OfferError::Signed(SignedError::Malformed)),
            "{pointer}"
        );
    }
}

#[test]
fn query_and_offer_vectors_match() {
    let key = fixed_key(0x01);
    let requester = fixed_key(0x02).id();

    // The query: §7.1's, as it travels.
    let qv = vector("service-query-v1.json");
    let query = spec_query();
    assert_eq!(qv["query"], serde_json::to_value(&query).unwrap());
    let bytes = query.to_jcs();
    assert_eq!(qv["canonical_hex"], json!(hex(&bytes)));
    assert_eq!(qv["query_hash"], json!(query_hash(&bytes)));
    assert_eq!(
        qv["query_hash"],
        json!(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
    );
    assert_eq!(ServiceQuery::from_jcs(&bytes).unwrap(), query);
    assert_eq!(qv["query"]["schema"], QUERY_SCHEMA);

    // The offer: the OCR agent's to the agent of key 0x02, public scope,
    // revision 1, idle, at 2026-11-14T00:00:00Z for 15 s.
    let ov = vector("service-offer-v1.json");
    assert_eq!(ov["signing_key_hex"], json!(hex(&[0x01; 32])));
    assert_eq!(ov["agent_id"], json!(key.id().to_string()));
    assert_eq!(ov["requester_key_hex"], json!(hex(&[0x02; 32])));
    assert_eq!(ov["offer"]["requester"], json!(requester.to_string()));
    assert_eq!(ov["offer"]["query_hash"], qv["query_hash"]);
    assert_eq!(
        ov["offer"]["agent_card_hash"],
        vector("card-jws-v1.json")["hash"]
    );
    assert_eq!(ov["offer"]["schema"], OFFER_SCHEMA);
    let signed = signed_spec_offer(&key, requester);
    assert_eq!(serde_json::to_value(&signed.doc).unwrap(), ov["offer"]);
    assert_eq!(
        ov["canonical_hex"],
        json!(hex(&jcs::canonical(&ov["offer"])))
    );
    assert_eq!(json!(signed.signature), ov["signature"]);
    assert_eq!(
        ov["signed"],
        json!({"offer": ov["offer"], "signature": ov["signature"]})
    );
    let frozen = jcs::canonical(&ov["signed"]);
    assert_eq!(signed.to_json(), frozen);
    let accepted = accept_offer(&frozen, &key.id(), &requester, &query, NOW).unwrap();
    assert_eq!(accepted, signed);
    let card_hash: CardHash = CARD_HASH.parse().unwrap();
    assert_eq!(accepted.doc.agent_card_hash, card_hash);
}
