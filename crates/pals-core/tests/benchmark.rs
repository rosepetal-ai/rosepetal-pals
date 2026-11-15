//! Benchmark reports (design §9.7): signed by whoever measured, kept with
//! their kind of measurement, compared only with like for like, and never
//! standing in for a measurement that does not exist: a subject without a
//! report shows as `no_measurement`, never as zero.

use std::collections::BTreeMap;
use std::sync::Arc;

use pals_core::benchmark::{
    BENCHMARK_SCHEMA, BENCHMARK_SIG_DOMAIN, BenchmarkReport, Comparability, Environment, Evidence,
    MAX_REPORT_BYTES, MAX_RESULTS, MeasurementKind, MetricResult, Procedure, Provenance, Route,
    Subject, Wanted, comparable,
};
use pals_core::discovery::query::{QUERY_SCHEMA, Requirements, ServiceQuery};
use pals_core::discovery::wire::{self, ErrorCode, request, response};
use pals_core::discovery::{DISCOVERY_VERSION, DiscoveryContext, Load, PUBLIC_CARD, answer};
use pals_core::grant::format_timestamp;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::inventory::view::{
    CursorKey, VIEW_SIG_DOMAIN, ViewCache, ViewEntry, render_benchmarks, verify_page,
};
use pals_core::inventory::{Inventory, InventorySnapshot, Section, Visibility};
use pals_core::jcs;
use pals_core::signed::{Signed, SignedError};
use pals_core::store::Store;
use serde_json::{Value, json};

/// 2026-11-14T00:00:00Z.
const NOW: i64 = 1_794_614_400;
/// A day, in seconds.
const DAY: i64 = 86_400;

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

/// The OCR agent of the inventory vector, whose public service is
/// `ocr-metal-v1`.
fn ocr_inventory() -> Inventory {
    Inventory::from_toml(vector("inventory-v1.json")["toml"].as_str().unwrap()).unwrap()
}

/// `measured_by`'s own measurement of the service `ocr-metal-v1` under
/// `service.task` 1, measured at [`NOW`] and in force for 30 days, with
/// every field it can have.
fn task_report(measured_by: AgentId) -> BenchmarkReport {
    BenchmarkReport {
        schema: BENCHMARK_SCHEMA.to_owned(),
        benchmark_id: "bench-ocr-task-001".to_owned(),
        subject: Subject::Service("ocr-metal-v1".to_owned()),
        measurement_kind: MeasurementKind::OwnMeasurement,
        procedure: Procedure {
            profile: "service.task".to_owned(),
            profile_version: "1".to_owned(),
            runner: Some("pals-bench 0.1".to_owned()),
            config: json!({"task": "ocr-serial-v2", "concurrency": 1, "image": {"width": 1024}}),
            inputs_hash: Some(format!("blake3:{}", "ab".repeat(32))),
        },
        environment: Environment {
            model_revision: Some("ocr-metal 2026.11".to_owned()),
            runtime: Some("onnxruntime 1.20".to_owned()),
            hardware_ref: Some("example:workstation".to_owned()),
            load: Some("idle".to_owned()),
            warmup: Some("3 runs".to_owned()),
            cache_state: Some("warm".to_owned()),
            route: Some(Route::Direct),
            peer: Some(fixed_key(0x02).id()),
        },
        measured_at: ts(NOW),
        duration_ms: 120_000,
        samples: 100,
        expires_at: ts(NOW + 30 * DAY),
        results: vec![
            MetricResult {
                metric: "total_time".to_owned(),
                unit: "ms".to_owned(),
                statistic: "p50".to_owned(),
                value: 1180.5,
                errors: Some(0),
                uncertainty: Some(12.25),
            },
            MetricResult {
                metric: "total_time".to_owned(),
                unit: "ms".to_owned(),
                statistic: "p95".to_owned(),
                value: 1730.0,
                errors: None,
                uncertainty: None,
            },
        ],
        provenance: Provenance {
            measured_by,
            method: "pals-bench run on the node itself".to_owned(),
        },
        evidence: Some(Evidence {
            hash: format!("blake3:{}", "cd".repeat(32)),
            blob: Some(format!("blob:{}", "ef".repeat(32))),
        }),
    }
}

/// `report` with only the fields a report must have.
fn bare(report: &BenchmarkReport) -> BenchmarkReport {
    BenchmarkReport {
        procedure: Procedure {
            runner: None,
            inputs_hash: None,
            ..report.procedure.clone()
        },
        environment: Environment::default(),
        results: vec![MetricResult {
            errors: None,
            uncertainty: None,
            ..report.results[0].clone()
        }],
        evidence: None,
        ..report.clone()
    }
}

/// What a requester wants that `task_report` gives.
fn wanted() -> Wanted {
    Wanted {
        profile: "service.task".to_owned(),
        profile_version: "1".to_owned(),
        config: json!({"image": {"width": 1024}, "task": "ocr-serial-v2", "concurrency": 1}),
        model_revision: None,
        route: None,
        peer_pair: None,
    }
}

/// `doc` signed by `key` under `domain`, as the signed JSON of a report,
/// whatever it says.
fn signed_as(key: &AgentKey, domain: &[u8], doc: &Value) -> Vec<u8> {
    jcs::canonical(&json!({
        "report": doc,
        "signature": jcs::sign_domain(key, domain, doc),
    }))
}

/// The discovery context of the OCR agent of `key`, showing its public view
/// to anybody, with `store`.
fn context(key: &Arc<AgentKey>, store: Arc<Store>) -> DiscoveryContext {
    let snapshot = Arc::new(InventorySnapshot {
        inventory: ocr_inventory(),
        observations: BTreeMap::new(),
        generation: 1,
    });
    let hash = format!("blake3:{}", "0".repeat(64)).parse().unwrap();
    DiscoveryContext {
        key: Arc::clone(key),
        store,
        inventory: Arc::new(move || Arc::clone(&snapshot)),
        card_hashes: BTreeMap::from([(PUBLIC_CARD.to_owned(), hash)]),
        records: Arc::new(|| (b"contact".to_vec(), b"announce".to_vec())),
        accepted_issuers: vec![key.id()],
        public: true,
        cursor_key: CursorKey::random(),
        views: ViewCache::new(),
        load: Arc::new(Load::unbounded()),
        offer_valid_for_ms: 15_000,
    }
}

fn request(op: request::Op) -> wire::Request {
    wire::Request {
        version: DISCOVERY_VERSION,
        max_response_bytes: 0,
        grant_token: String::new(),
        op: Some(op),
    }
}

/// What `ctx` answers `peer` asking for the report `id`, without a grant.
fn get_report(ctx: &DiscoveryContext, peer: &AgentId, id: &str) -> response::Result {
    let get = request::Op::GetBenchmarkReport(wire::GetBenchmarkReport {
        benchmark_id: id.to_owned(),
    });
    answer(ctx, peer, request(get), NOW).result.unwrap()
}

/// The `benchmarks` section of the public view `ctx` shows `peer`.
fn benchmarks_section(ctx: &DiscoveryContext, peer: &AgentId) -> Vec<ViewEntry> {
    let get = request::Op::GetInventory(wire::GetInventory {
        sections: vec!["benchmarks".to_owned()],
        cursor: String::new(),
        page_entries: 0,
    });
    let result = answer(ctx, peer, request(get), NOW).result;
    let Some(response::Result::InventoryPage(page)) = result else {
        panic!("a page, not {result:?}");
    };
    let page = verify_page(&page, &ctx.key.id(), Some(peer), NOW).unwrap();
    assert_eq!(page.sections, [Section::Benchmarks]);
    page.entries
}

/// The offer `ctx` makes `peer` for OCR, without a grant.
fn ocr_offer(ctx: &DiscoveryContext, peer: &AgentId) -> pals_core::discovery::ServiceOffer {
    let query = ServiceQuery {
        schema: QUERY_SCHEMA.to_owned(),
        query_id: "q-1".to_owned(),
        capability: "vision.ocr".to_owned(),
        input_media_types: Vec::new(),
        output_media_types: Vec::new(),
        requirements: Requirements::default(),
        summary: None,
        reply_timeout_ms: None,
        requested_inventory_sections: Vec::new(),
        project: None,
        execution_authorized: false,
    };
    let ask = request::Op::ServiceQuery(wire::ServiceQuery {
        query: query.to_jcs(),
    });
    let Some(response::Result::ServiceOffer(offer)) = answer(ctx, peer, request(ask), NOW).result
    else {
        panic!("an offer");
    };
    Signed::<pals_core::discovery::ServiceOffer>::from_json(&offer)
        .unwrap()
        .doc
}

#[test]
fn report_roundtrip_and_tamper() {
    let key = fixed_key(0x01);
    let report = task_report(key.id());
    let signed = Signed::issue(&key, report.clone()).unwrap();
    let bytes = signed.to_json();
    let read = Signed::<BenchmarkReport>::from_json(&bytes).unwrap();
    assert_eq!(read, signed);
    assert_eq!(read.to_json(), bytes);

    // What an offer says of it.
    let reference = report.reference();
    assert_eq!(reference.benchmark_id, "bench-ocr-task-001");
    assert_eq!(reference.profile, "service.task");
    assert_eq!(reference.measurement_kind, "own_measurement");
    assert_eq!(reference.measured_at, "2026-11-14T00:00:00Z");
    assert_eq!(reference.expires_at, "2026-12-14T00:00:00Z");

    // A result changed after signing: the signature no longer holds.
    let tampered = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("1180.5", "118.5");
    assert_ne!(tampered.as_bytes(), bytes);
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(tampered.as_bytes()),
        Err(SignedError::BadSignature)
    );
    // The kind changed after signing: neither.
    let declared = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("own_measurement", "peer_observation");
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(declared.as_bytes()),
        Err(SignedError::BadSignature)
    );
    // Signed for another kind of document, by the right key: not a report.
    let doc = serde_json::to_value(&report).unwrap();
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&signed_as(&key, VIEW_SIG_DOMAIN, &doc)),
        Err(SignedError::BadSignature)
    );
    assert!(
        Signed::<BenchmarkReport>::from_json(&signed_as(&key, BENCHMARK_SIG_DOMAIN, &doc)).is_ok()
    );
    // A field this schema does not know, signed as it is: refused.
    let mut extra = doc.clone();
    extra["visibility"] = json!("public");
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&signed_as(&key, BENCHMARK_SIG_DOMAIN, &extra)),
        Err(SignedError::Malformed)
    );
}

#[test]
fn declared_report_keeps_its_kind_after_import() {
    // The operator's declared figures, kept and served: never a measurement
    // (design §14).
    let key = Arc::new(fixed_key(0x01));
    let report = BenchmarkReport {
        benchmark_id: "spec-ocr-001".to_owned(),
        measurement_kind: MeasurementKind::DeclaredSpec,
        duration_ms: 0,
        samples: 0,
        ..bare(&task_report(key.id()))
    };
    let signed = Signed::issue(&key, report).unwrap();
    // Imported: read from its bytes, as `pals benchmark import` does.
    let imported = Signed::<BenchmarkReport>::from_json(&signed.to_json()).unwrap();
    assert_eq!(imported.doc.measurement_kind, MeasurementKind::DeclaredSpec);

    // Kept.
    let store = Arc::new(Store::open_in_memory(key.id()).unwrap());
    store
        .benchmark_put(&imported, &Visibility::Public, NOW)
        .unwrap();
    let (kept, visibility) = store.benchmark("spec-ocr-001").unwrap().unwrap();
    assert_eq!(kept, signed);
    assert_eq!(kept.doc.measurement_kind, MeasurementKind::DeclaredSpec);
    assert_eq!(visibility, Visibility::Public);

    // Served: the report as signed, its reference in the view and in an
    // offer, each saying it is declared.
    let ctx = context(&key, Arc::clone(&store));
    let peer = fixed_key(0x02).id();
    let response::Result::BenchmarkReport(served) = get_report(&ctx, &peer, "spec-ocr-001") else {
        panic!("the report");
    };
    assert_eq!(served, signed.to_json());
    let served = Signed::<BenchmarkReport>::from_json(&served).unwrap();
    assert_eq!(served.doc.measurement_kind, MeasurementKind::DeclaredSpec);

    let section = benchmarks_section(&ctx, &peer);
    let entry = section
        .iter()
        .find(|entry| entry.id == "spec-ocr-001")
        .unwrap();
    assert_eq!(entry.body["measurement_kind"], "declared_spec");
    assert!(
        entry.description.contains("declared"),
        "{}",
        entry.description
    );
    assert!(
        !entry.description.contains("measurement of"),
        "{}",
        entry.description
    );
    assert_eq!(entry.provenance, key.id().to_string());
    assert!(!entry.dimensions.authorized);

    let offer = ocr_offer(&ctx, &peer);
    assert_eq!(offer.skill_id.as_deref(), Some("ocr-metal-v1"));
    assert_eq!(offer.benchmark_refs.len(), 1);
    assert_eq!(offer.benchmark_refs[0].measurement_kind, "declared_spec");
    assert_eq!(offer.benchmark_refs[0], served.doc.reference());

    // A measurement without samples is no measurement: refused.
    let unsampled = BenchmarkReport {
        measurement_kind: MeasurementKind::OwnMeasurement,
        ..served.doc
    };
    assert_eq!(Signed::issue(&key, unsampled), Err(SignedError::Malformed));
}

#[test]
fn report_signed_by_someone_other_than_its_measurer_is_refused() {
    let measurer = fixed_key(0x01);
    let other = fixed_key(0x03);
    let report = task_report(measurer.id());
    // Nobody signs another's report.
    assert_eq!(
        Signed::issue(&other, report.clone()),
        Err(SignedError::BadSignature)
    );
    // One signed by somebody else is refused, whoever signed it.
    let doc = serde_json::to_value(&report).unwrap();
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&signed_as(&other, BENCHMARK_SIG_DOMAIN, &doc)),
        Err(SignedError::BadSignature)
    );
    // Naming the signer as its measurer makes it the signer's report, a
    // new one: the original measurer's signature does not carry over.
    let original = Signed::issue(&measurer, report.clone()).unwrap();
    let mut claimed = doc.clone();
    claimed["provenance"]["measured_by"] = json!(other.id().to_string());
    let forged = jcs::canonical(&json!({"report": claimed, "signature": original.signature}));
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&forged),
        Err(SignedError::BadSignature)
    );
    // Whoever keeps another's report keeps it as signed: it stays its
    // measurer's.
    let store = Store::open_in_memory(other.id()).unwrap();
    store
        .benchmark_put(&original, &Visibility::Private, NOW)
        .unwrap();
    let (kept, _) = store.benchmark("bench-ocr-task-001").unwrap().unwrap();
    assert_eq!(kept.doc.provenance.measured_by, measurer.id());
}

#[test]
fn expired_other_config_and_other_route_are_not_usable() {
    let (a, b) = (fixed_key(0x01).id(), fixed_key(0x02).id());
    let report = task_report(a);
    // The same configuration in another order is the same.
    assert_eq!(comparable(&report, &wanted(), NOW), Comparability::Usable);
    assert_eq!(
        comparable(&report, &wanted(), NOW + 30 * DAY - 1),
        Comparability::Usable
    );

    // Expired: from its `expires_at`, by the requester's clock.
    assert_eq!(
        comparable(&report, &wanted(), NOW + 30 * DAY),
        Comparability::Expired
    );
    // Another profile, or another version of it.
    let other_profile = Wanted {
        profile: "compute.inference".to_owned(),
        ..wanted()
    };
    assert_eq!(
        comparable(&report, &other_profile, NOW),
        Comparability::OtherProfile
    );
    let other_version = Wanted {
        profile_version: "2".to_owned(),
        ..wanted()
    };
    assert_eq!(
        comparable(&report, &other_version, NOW),
        Comparability::OtherProfile
    );
    // Another configuration.
    let other_config = Wanted {
        config: json!({"task": "ocr-serial-v2", "concurrency": 4, "image": {"width": 1024}}),
        ..wanted()
    };
    assert_eq!(
        comparable(&report, &other_config, NOW),
        Comparability::OtherConfig
    );
    // Another route, or one the report does not say.
    let relay = Wanted {
        route: Some(Route::Relay),
        ..wanted()
    };
    assert_eq!(comparable(&report, &relay, NOW), Comparability::OtherRoute);
    let direct = Wanted {
        route: Some(Route::Direct),
        ..wanted()
    };
    assert_eq!(comparable(&report, &direct, NOW), Comparability::Usable);
    let unsaid = bare(&report);
    assert_eq!(comparable(&unsaid, &direct, NOW), Comparability::OtherRoute);
    // Between other nodes; the same two in either order are the same.
    let c = fixed_key(0x03).id();
    let pair = |x, y| Wanted {
        peer_pair: Some((x, y)),
        ..wanted()
    };
    assert_eq!(comparable(&report, &pair(a, b), NOW), Comparability::Usable);
    assert_eq!(comparable(&report, &pair(b, a), NOW), Comparability::Usable);
    assert_eq!(
        comparable(&report, &pair(a, c), NOW),
        Comparability::OtherRoute
    );
    assert_eq!(
        comparable(&unsaid, &pair(a, b), NOW),
        Comparability::OtherRoute
    );
    let rtt = BenchmarkReport {
        subject: Subject::NodePair { a: b, b: c },
        procedure: Procedure {
            profile: "network.rtt".to_owned(),
            config: json!({"protocol": "quic"}),
            ..unsaid.procedure.clone()
        },
        ..unsaid.clone()
    };
    let rtt_wanted = |x, y| Wanted {
        profile: "network.rtt".to_owned(),
        config: json!({"protocol": "quic"}),
        peer_pair: Some((x, y)),
        ..wanted()
    };
    assert_eq!(
        comparable(&rtt, &rtt_wanted(c, b), NOW),
        Comparability::Usable
    );
    assert_eq!(
        comparable(&rtt, &rtt_wanted(a, b), NOW),
        Comparability::OtherRoute
    );
    // An expired report of another profile is expired first.
    assert_eq!(
        comparable(&report, &other_profile, NOW + 31 * DAY),
        Comparability::Expired
    );
}

#[test]
fn llm_reports_compare_only_with_the_same_model() {
    let a = fixed_key(0x01).id();
    let llm = |model: Option<&str>| BenchmarkReport {
        procedure: Procedure {
            profile: "compute.llm".to_owned(),
            config: json!({"prompt_tokens": 512, "output_tokens": 128}),
            ..task_report(a).procedure
        },
        environment: Environment {
            model_revision: model.map(str::to_owned),
            ..Environment::default()
        },
        ..task_report(a)
    };
    let wanted = |model: Option<&str>| Wanted {
        profile: "compute.llm".to_owned(),
        config: json!({"output_tokens": 128, "prompt_tokens": 512}),
        model_revision: model.map(str::to_owned),
        ..wanted()
    };
    let qwen = Some("qwen3-8b q4_k_m 2026-10");
    let report = llm(qwen);
    assert_eq!(
        comparable(&report, &wanted(qwen), NOW),
        Comparability::Usable
    );
    // Another model, or its tokenizer, is another measurement.
    assert_eq!(
        comparable(&report, &wanted(Some("llama-3.1-8b q4_k_m")), NOW),
        Comparability::OtherModel
    );
    // Without the model asked for, or said, nothing of the profile counts.
    assert_eq!(
        comparable(&report, &wanted(None), NOW),
        Comparability::OtherModel
    );
    assert_eq!(
        comparable(&llm(None), &wanted(qwen), NOW),
        Comparability::OtherModel
    );
    // Another configuration comes first.
    let longer = Wanted {
        config: json!({"prompt_tokens": 4096, "output_tokens": 128}),
        ..wanted(qwen)
    };
    assert_eq!(
        comparable(&report, &longer, NOW),
        Comparability::OtherConfig
    );
    // Elsewhere the model is compared only when asked for.
    let task = task_report(a);
    assert_eq!(
        comparable(&task, &wanted_task(None), NOW),
        Comparability::Usable
    );
    assert_eq!(
        comparable(&task, &wanted_task(Some("ocr-metal 2026.11")), NOW),
        Comparability::Usable
    );
    assert_eq!(
        comparable(&task, &wanted_task(Some("ocr-metal 2026.12")), NOW),
        Comparability::OtherModel
    );
    // As a search asks for it: JSON, the absent fields left out.
    let json = serde_json::to_value(wanted(qwen)).unwrap();
    assert_eq!(json["model_revision"], "qwen3-8b q4_k_m 2026-10");
    assert!(json.get("route").is_none(), "{json}");
    assert_eq!(
        serde_json::from_value::<Wanted>(json).unwrap(),
        wanted(qwen)
    );
}

/// [`wanted`], with the model `model` asked for.
fn wanted_task(model: Option<&str>) -> Wanted {
    Wanted {
        model_revision: model.map(str::to_owned),
        ..wanted()
    }
}

#[test]
fn missing_measurement_is_no_measurement_never_zero() {
    let key = fixed_key(0x01);
    let report = Signed::issue(&key, task_report(key.id())).unwrap();
    let ocr = Subject::Service("ocr-metal-v1".to_owned());
    let model = Subject::Model("example:ocr-model".to_owned());
    let hardware = Subject::Hardware("example:workstation".to_owned());

    let entries = render_benchmarks(
        std::slice::from_ref(&report),
        &[ocr.clone(), model.clone(), hardware.clone()],
    );
    let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "bench-ocr-task-001",
            "model:example:ocr-model",
            "hardware:example:workstation"
        ]
    );
    // The report's reference.
    assert_eq!(
        entries[0].body,
        json!({
            "subject": {"service": "ocr-metal-v1"},
            "measurement_kind": "own_measurement",
            "profile": "service.task",
            "profile_version": "1",
            "measured_at": "2026-11-14T00:00:00Z",
        })
    );
    assert_eq!(
        entries[0].expires_at.as_deref(),
        Some("2026-12-14T00:00:00Z")
    );
    // A subject without a report: that, and nothing that reads as a value.
    for (entry, subject) in entries[1..].iter().zip([&model, &hardware]) {
        assert_eq!(entry.section, Section::Benchmarks);
        assert_eq!(
            entry.body,
            json!({"subject": serde_json::to_value(subject).unwrap(), "status": "no_measurement"})
        );
        assert_eq!(entry.expires_at, None);
        assert_eq!(entry.version, None);
        assert!(!entry.dimensions.authorized);
        assert!(entry.dimensions.operational.is_none());
        let text = serde_json::to_string(entry).unwrap();
        assert!(!text.contains(":0"), "{text}");
    }
    // Nothing to show: each subject without one.
    let none = render_benchmarks(&[], std::slice::from_ref(&ocr));
    assert_eq!(
        none[0].body,
        json!({"subject": {"service": "ocr-metal-v1"}, "status": "no_measurement"})
    );
    assert!(render_benchmarks(&[], &[]).is_empty());

    // Through the protocol: the public view of the OCR agent, which has
    // none, says so of its public service, and its offer refers to none.
    let key = Arc::new(key);
    let ctx = context(&key, Arc::new(Store::open_in_memory(key.id()).unwrap()));
    let peer = fixed_key(0x02).id();
    let section = benchmarks_section(&ctx, &peer);
    assert!(!section.is_empty());
    assert!(
        section
            .iter()
            .all(|entry| entry.body["status"] == "no_measurement")
    );
    assert!(
        section
            .iter()
            .any(|entry| entry.body["subject"] == json!({"service": "ocr-metal-v1"}))
    );
    assert!(ocr_offer(&ctx, &peer).benchmark_refs.is_empty());
    // And a report it does not hold is not found: nothing is measured.
    let response::Result::Error(error) = get_report(&ctx, &peer, "bench-ocr-task-001") else {
        panic!("not found");
    };
    assert_eq!(error.code(), ErrorCode::NotFound);
}

#[test]
fn report_over_64_kib_is_refused() {
    let key = fixed_key(0x01);
    let report = task_report(key.id());
    // As many results as a report takes, and no more.
    let result = report.results[0].clone();
    let full = BenchmarkReport {
        results: vec![result.clone(); MAX_RESULTS],
        ..report.clone()
    };
    Signed::issue(&key, full.clone()).unwrap();
    let over = BenchmarkReport {
        results: vec![result; MAX_RESULTS + 1],
        ..report.clone()
    };
    assert_eq!(Signed::issue(&key, over), Err(SignedError::Malformed));

    // Raw results belong in evidence: a configuration that carries them
    // makes a report too large to sign, or to read.
    let bulky = BenchmarkReport {
        procedure: Procedure {
            config: json!({"raw": "x".repeat(MAX_REPORT_BYTES)}),
            ..report.procedure.clone()
        },
        ..report.clone()
    };
    assert_eq!(
        Signed::issue(&key, bulky.clone()),
        Err(SignedError::TooLarge)
    );
    let doc = serde_json::to_value(&bulky).unwrap();
    let bytes = signed_as(&key, BENCHMARK_SIG_DOMAIN, &doc);
    assert!(bytes.len() > MAX_REPORT_BYTES);
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&bytes),
        Err(SignedError::TooLarge)
    );
    // Refused before it is read: what is over is not even parsed.
    let mut garbage = vec![b'{'; MAX_REPORT_BYTES + 1];
    garbage[0] = b'[';
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&garbage),
        Err(SignedError::TooLarge)
    );
    // A configuration nested too deep is no configuration.
    let mut deep = json!(1);
    for _ in 0..17 {
        deep = json!({ "k": deep });
    }
    let nested = BenchmarkReport {
        procedure: Procedure {
            config: deep,
            ..report.procedure.clone()
        },
        ..report
    };
    assert_eq!(Signed::issue(&key, nested), Err(SignedError::Malformed));
}

#[test]
fn report_fields_have_one_spelling() {
    let key = fixed_key(0x01);
    let full = task_report(key.id());
    // Every optional field, there and left out.
    let mut variants = vec![full.clone(), bare(&full)];
    let mut drop = |edit: &dyn Fn(&mut BenchmarkReport)| {
        let mut report = full.clone();
        edit(&mut report);
        variants.push(report);
    };
    drop(&|r| r.procedure.runner = None);
    drop(&|r| r.procedure.inputs_hash = None);
    drop(&|r| r.environment.model_revision = None);
    drop(&|r| r.environment.runtime = None);
    drop(&|r| r.environment.hardware_ref = None);
    drop(&|r| r.environment.load = None);
    drop(&|r| r.environment.warmup = None);
    drop(&|r| r.environment.cache_state = None);
    drop(&|r| r.environment.route = None);
    drop(&|r| r.environment.peer = None);
    drop(&|r| r.results[0].errors = None);
    drop(&|r| r.results[0].uncertainty = None);
    drop(&|r| r.evidence = None);
    drop(&|r| r.evidence.as_mut().unwrap().blob = None);
    for report in variants {
        let signed = Signed::issue(&key, report.clone()).unwrap();
        let bytes = signed.to_json();
        let read = Signed::<BenchmarkReport>::from_json(&bytes).unwrap();
        assert_eq!(read.doc, report);
        assert_eq!(read.to_json(), bytes);
    }
    // Absent is never spelled `null`: such a report, signed as it is, is
    // refused.
    let base = serde_json::to_value(&full).unwrap();
    for pointer in [
        "/procedure/runner",
        "/procedure/inputs_hash",
        "/environment/model_revision",
        "/environment/route",
        "/environment/peer",
        "/results/0/errors",
        "/results/0/uncertainty",
        "/evidence",
        "/evidence/blob",
    ] {
        let mut spelled = base.clone();
        *spelled.pointer_mut(pointer).unwrap() = Value::Null;
        assert_eq!(
            Signed::<BenchmarkReport>::from_json(&signed_as(&key, BENCHMARK_SIG_DOMAIN, &spelled)),
            Err(SignedError::Malformed),
            "{pointer}"
        );
    }
    // A subject has one key.
    let mut two = base.clone();
    two["subject"] = json!({"service": "ocr-metal-v1", "model": "example:ocr-model"});
    assert_eq!(
        Signed::<BenchmarkReport>::from_json(&signed_as(&key, BENCHMARK_SIG_DOMAIN, &two)),
        Err(SignedError::Malformed)
    );
}

#[test]
fn benchmark_vector_matches() {
    let key = fixed_key(0x01);
    let v = vector("benchmark-v1.json");
    assert_eq!(v["signing_key_hex"], json!(hex(&[0x01; 32])));
    assert_eq!(v["agent_id"], json!(key.id().to_string()));
    assert_eq!(v["report"]["schema"], BENCHMARK_SCHEMA);
    assert_eq!(
        v["domain"],
        json!(String::from_utf8(BENCHMARK_SIG_DOMAIN.to_vec()).unwrap())
    );

    let report = task_report(key.id());
    let signed = Signed::issue(&key, report.clone()).unwrap();
    assert_eq!(serde_json::to_value(&report).unwrap(), v["report"]);
    assert_eq!(
        v["canonical_hex"],
        json!(hex(&jcs::canonical(&v["report"])))
    );
    assert_eq!(json!(signed.signature), v["signature"]);
    assert_eq!(
        v["signed"],
        json!({"report": v["report"], "signature": v["signature"]})
    );
    let frozen = jcs::canonical(&v["signed"]);
    assert_eq!(signed.to_json(), frozen);
    let read = Signed::<BenchmarkReport>::from_json(&frozen).unwrap();
    assert_eq!(read, signed);
    assert_eq!(
        serde_json::to_value(read.doc.reference()).unwrap(),
        v["reference"]
    );
    // The subject is the public service of the inventory vector.
    assert_eq!(v["report"]["subject"], json!({"service": "ocr-metal-v1"}));
    assert!(
        ocr_inventory()
            .services
            .iter()
            .any(|service| service.id == "ocr-metal-v1" && service.visibility == Visibility::Public)
    );
}

#[test]
fn offer_references_are_checked_like_reports() {
    // An offer refers to reports as they are: by a benchmark id, a profile
    // and a kind of measurement, at most 16 of them.
    let key = Arc::new(fixed_key(0x01));
    let store = Arc::new(Store::open_in_memory(key.id()).unwrap());
    let report = Signed::issue(&key, task_report(key.id())).unwrap();
    store
        .benchmark_put(&report, &Visibility::Public, NOW)
        .unwrap();
    let ctx = context(&key, store);
    let offer = ocr_offer(&ctx, &fixed_key(0x02).id());
    assert_eq!(offer.benchmark_refs, [report.doc.reference()]);
    Signed::issue(&key, offer.clone()).unwrap();

    let refused = |edit: &dyn Fn(&mut pals_core::discovery::query::BenchmarkRef)| {
        let mut offer = offer.clone();
        edit(&mut offer.benchmark_refs[0]);
        Signed::issue(&key, offer)
    };
    for (name, result) in [
        ("an empty id", refused(&|r| r.benchmark_id.clear())),
        (
            "an id with a colon",
            refused(&|r| r.benchmark_id = "service:ocr".to_owned()),
        ),
        (
            "an unknown profile",
            refused(&|r| r.profile = "compute.gpu".to_owned()),
        ),
        (
            "a reserved profile",
            refused(&|r| r.profile = "agentnet:bench".to_owned()),
        ),
        (
            "an unknown kind",
            refused(&|r| r.measurement_kind = "measured".to_owned()),
        ),
        (
            "a time",
            refused(&|r| r.measured_at = "yesterday".to_owned()),
        ),
    ] {
        assert_eq!(result, Err(SignedError::Malformed), "{name}");
    }
    let mut many = offer.clone();
    many.benchmark_refs = vec![report.doc.reference(); 16];
    Signed::issue(&key, many.clone()).unwrap();
    many.benchmark_refs.push(report.doc.reference());
    assert_eq!(Signed::issue(&key, many), Err(SignedError::Malformed));
    // A namespaced profile is its author's own.
    let mut own = offer;
    own.benchmark_refs[0].profile = "example:bench".to_owned();
    Signed::issue(&key, own).unwrap();
}

#[test]
fn a_page_with_a_benchmark_entry_no_view_makes_is_malformed() {
    // A peer's page is held to what a view makes: a reference to a report,
    // or a subject without one, and nothing that reads as a measurement.
    let key = fixed_key(0x01);
    let report = Signed::issue(&key, task_report(key.id())).unwrap();
    let model = Subject::Model("example:ocr-model".to_owned());
    let entries = render_benchmarks(
        std::slice::from_ref(&report),
        &[Subject::Service("ocr-metal-v1".to_owned()), model],
    );
    let scope = pals_core::inventory::Scope::Public;
    let sections = [Section::Benchmarks];
    let req = pals_core::inventory::view::PageRequest {
        scope: &scope,
        audience: None,
        sections: &sections,
        cursor: None,
        page_entries: None,
        revision: 1,
    };
    let card_hash = format!("blake3:{}", "0".repeat(64)).parse().unwrap();
    let sign = |entries: Vec<ViewEntry>| {
        pals_core::inventory::view::sign_page(&key, &card_hash, &req, (entries, None, None), NOW)
    };
    let page = sign(entries.clone()).unwrap();
    verify_page(&page.to_json(), &key.id(), None, NOW).unwrap();

    let malformed = |name: &str, index: usize, edit: &dyn Fn(&mut ViewEntry)| {
        let mut entries = entries.clone();
        edit(&mut entries[index]);
        assert_eq!(
            sign(entries).unwrap_err(),
            pals_core::inventory::view::ViewError::Malformed,
            "{name}"
        );
    };
    malformed("a zero for no measurement", 1, &|e| {
        e.body["value"] = json!(0);
    });
    malformed("a status that is not no_measurement", 1, &|e| {
        e.body["status"] = json!("measured");
    });
    malformed("an id that is not its subject's", 1, &|e| {
        e.id = "model:example:other".to_owned();
    });
    malformed("an unmeasured subject that expires", 1, &|e| {
        e.expires_at = Some(ts(NOW + DAY));
    });
    malformed("an authorized reference", 0, &|e| {
        e.dimensions.authorized = true;
    });
    malformed("a reference without its expiry", 0, &|e| {
        e.expires_at = None
    });
    malformed("a reference whose measurer is no agent", 0, &|e| {
        e.provenance = "operator".to_owned();
    });
    malformed("a reference with a result in it", 0, &|e| {
        e.body["value"] = json!(1180.5);
    });
    malformed("a reference of an unknown kind", 0, &|e| {
        e.body["measurement_kind"] = json!("measured");
    });
    malformed("a reference with a colon in its id", 0, &|e| {
        e.id = "service:ocr-metal-v1".to_owned();
    });
}

#[test]
fn an_unreadable_report_hides_only_itself() {
    // Rows that no longer read, as a damaged disk or another program might
    // leave them: a private one and a public one that no longer verify, and
    // one whose visibility does not parse. Each hides only itself.
    let key = Arc::new(fixed_key(0x01));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let store = Arc::new(Store::open(&path, key.id()).unwrap());
    let with_id = |id: &str| {
        let report = BenchmarkReport {
            benchmark_id: id.to_owned(),
            ..task_report(key.id())
        };
        Signed::issue(&key, report).unwrap()
    };
    let good = with_id("ocr-good");
    store
        .benchmark_put(&good, &Visibility::Public, NOW)
        .unwrap();
    for (id, visibility) in [
        ("ocr-private-bad", Visibility::Private),
        ("ocr-public-bad", Visibility::Public),
        ("ocr-visibility-bad", Visibility::Public),
    ] {
        store.benchmark_put(&with_id(id), &visibility, NOW).unwrap();
    }
    let raw = rusqlite::Connection::open(&path).unwrap();
    for id in ["ocr-private-bad", "ocr-public-bad"] {
        let tampered = String::from_utf8(with_id(id).to_json())
            .unwrap()
            .replace("1180.5", "118.5");
        let changed = raw
            .execute(
                "UPDATE benchmark_reports SET report_json = ?1 WHERE benchmark_id = ?2",
                rusqlite::params![tampered.as_bytes(), id],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }
    raw.execute(
        "UPDATE benchmark_reports SET visibility = 'everyone' WHERE benchmark_id = ?1",
        ["ocr-visibility-bad"],
    )
    .unwrap();

    let ctx = context(&key, Arc::clone(&store));
    let peer = fixed_key(0x02).id();
    // The public view and the offers still work, with the report that reads.
    let section = benchmarks_section(&ctx, &peer);
    let references: Vec<&str> = section
        .iter()
        .filter(|entry| entry.body.get("status").is_none())
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(references, ["ocr-good"]);
    assert_eq!(
        ocr_offer(&ctx, &peer).benchmark_refs,
        [good.doc.reference()]
    );
    // A report that does not read is not found, as one that does not exist:
    // the answer never says it is there.
    let missing = get_report(&ctx, &peer, "ocr-missing");
    let response::Result::Error(error) = &missing else {
        panic!("not found, not {missing:?}");
    };
    assert_eq!(error.code(), ErrorCode::NotFound);
    for id in ["ocr-private-bad", "ocr-public-bad", "ocr-visibility-bad"] {
        assert_eq!(get_report(&ctx, &peer, id), missing, "{id}");
    }
    let response::Result::BenchmarkReport(served) = get_report(&ctx, &peer, "ocr-good") else {
        panic!("the report that reads");
    };
    assert_eq!(served, good.to_json());

    // The store lists every row, those that do not read by their id.
    let kept: Vec<(String, bool)> = store
        .benchmarks()
        .unwrap()
        .into_iter()
        .map(|kept| (kept.benchmark_id, kept.report.is_ok()))
        .collect();
    assert_eq!(
        kept,
        [
            ("ocr-good".to_owned(), true),
            ("ocr-private-bad".to_owned(), false),
            ("ocr-public-bad".to_owned(), false),
            ("ocr-visibility-bad".to_owned(), false),
        ]
    );
    // Asked for by its id, a row that does not read is an error naming it.
    let err = store.benchmark("ocr-private-bad").unwrap_err();
    assert!(format!("{err:#}").contains("ocr-private-bad"), "{err:#}");
}
