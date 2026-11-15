//! Benchmark reports between real nodes: `GetBenchmarkReport` over
//! `agentnet-discovery/0` returns a report the provider already holds, as
//! its measurer signed it, and measures nothing; a report the requester may
//! not see is not found exactly as one that does not exist; offers refer to
//! the reports of their service the requester sees; and `pals benchmark
//! import|ls` keep a report as signed, with a visibility of their own.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{CountingExecutor, TestNode, grant, key_in, now_unix, spawn_node};
use pals::config::{CONFIG_FILE, Config, INVENTORY_FILE, WorkerCfg};
use pals::discovery::{DiscoveryError, Reply};
use pals::node::{NodeOptions, STORE_FILE};
use pals::worker::echo::EchoExecutor;
use pals_core::benchmark::{
    BENCHMARK_SCHEMA, BenchmarkReport, Environment, MeasurementKind, MetricResult, Procedure,
    Provenance, Subject,
};
use pals_core::discovery::DISCOVERY_VERSION;
use pals_core::discovery::query::{
    BenchmarkRef, QUERY_SCHEMA, Requirements, ServiceOffer, ServiceQuery,
};
use pals_core::discovery::wire::{self, ErrorCode, Request, request, response};
use pals_core::grant::Operation::{TaskRead, TaskSend};
use pals_core::grant::{SignedGrant, format_timestamp};
use pals_core::identity::AgentKey;
use pals_core::inventory::Visibility;
use pals_core::inventory::view::{ViewPage, verify_page};
use pals_core::signed::Signed;
use pals_core::store::Store;
use serde_json::json;

/// An OCR service anyone may know of, a review service of the project
/// `demo`, and a transcription service never offered.
const INVENTORY: &str = r#"
[[services]]
id = "ocr-metal-v1"
name = "Industrial OCR"
description = "Reads serial numbers stamped on metal parts"
capabilities = ["vision.ocr"]
visibility = "public"

[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["code.rust.review"]
visibility = { project = ["demo"] }

[[services]]
id = "transcriber"
name = "Transcriber"
description = "Transcribes meetings"
capabilities = ["audio.transcribe"]
"#;

/// How long a call may take: far more than any answer needs, even on a
/// loaded host; only a broken test waits it out.
const CALL: Duration = Duration::from_secs(30);
const DAY: i64 = 86_400;

/// A node whose inventory is [`INVENTORY`], whose work `executor` does.
async fn provider_with(executor: impl a2a_server::AgentExecutor) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), INVENTORY).unwrap();
    common::spawn_node_in_with_options(dir, executor, NodeOptions::default()).await
}

/// `measurer`'s report `id` of `kind` about `subject`, measured `age`
/// seconds ago and in force for 30 days from then.
fn report(
    measurer: &AgentKey,
    id: &str,
    subject: Subject,
    kind: MeasurementKind,
    age: i64,
) -> Signed<BenchmarkReport> {
    let measured_at = now_unix() - age;
    let doc = BenchmarkReport {
        schema: BENCHMARK_SCHEMA.to_owned(),
        benchmark_id: id.to_owned(),
        subject,
        measurement_kind: kind,
        procedure: Procedure {
            profile: "service.task".to_owned(),
            profile_version: "1".to_owned(),
            runner: None,
            config: json!({"task": "ocr-serial-v2"}),
            inputs_hash: None,
        },
        environment: Environment::default(),
        measured_at: format_timestamp(measured_at).unwrap(),
        duration_ms: 60_000,
        samples: 20,
        expires_at: format_timestamp(measured_at + 30 * DAY).unwrap(),
        results: vec![MetricResult {
            metric: "total_time".to_owned(),
            unit: "ms".to_owned(),
            statistic: "p50".to_owned(),
            value: 1180.0,
            errors: None,
            uncertainty: None,
        }],
        provenance: Provenance {
            measured_by: measurer.id(),
            method: "pals-bench".to_owned(),
        },
        evidence: None,
    };
    Signed::issue(measurer, doc).unwrap()
}

/// Keeps `report` in `node`'s store, seen by `visibility`.
fn keep(node: &TestNode, report: &Signed<BenchmarkReport>, visibility: Visibility) {
    node.node
        .store
        .benchmark_put(report, &visibility, now_unix())
        .unwrap();
}

fn ocr() -> Subject {
    Subject::Service("ocr-metal-v1".to_owned())
}

fn request_of(op: request::Op, grant: Option<&SignedGrant>) -> Request {
    Request {
        version: DISCOVERY_VERSION,
        max_response_bytes: 0,
        grant_token: grant.map(SignedGrant::to_token).unwrap_or_default(),
        op: Some(op),
    }
}

/// What `to` answers `from` asking for the report `id`.
async fn get_report(
    from: &TestNode,
    to: &TestNode,
    id: &str,
    grant: Option<&SignedGrant>,
) -> Result<Reply, DiscoveryError> {
    let op = request::Op::GetBenchmarkReport(wire::GetBenchmarkReport {
        benchmark_id: id.to_owned(),
    });
    from.node
        .discovery()
        .call(&to.id(), to.node.addr(), request_of(op, grant), CALL)
        .await
}

/// The report bytes of a reply.
fn report_of(reply: &Reply) -> &[u8] {
    match &reply.response.result {
        Some(response::Result::BenchmarkReport(bytes)) => bytes,
        other => panic!("a report, not {other:?}"),
    }
}

/// The refusal of a call that does not find its report.
fn refusal(result: Result<Reply, DiscoveryError>) -> wire::Error {
    match result {
        Err(DiscoveryError::Remote(error)) => error,
        other => panic!("a refusal, not {other:?}"),
    }
}

/// The page of the `benchmarks` section `to` signs for `from`.
async fn benchmarks_page(from: &TestNode, to: &TestNode, grant: Option<&SignedGrant>) -> ViewPage {
    let op = request::Op::GetInventory(wire::GetInventory {
        sections: vec!["benchmarks".to_owned()],
        cursor: String::new(),
        page_entries: 0,
    });
    let reply = from
        .node
        .discovery()
        .call(&to.id(), to.node.addr(), request_of(op, grant), CALL)
        .await
        .unwrap();
    let Some(response::Result::InventoryPage(page)) = &reply.response.result else {
        panic!("a page, not {:?}", reply.response.result);
    };
    verify_page(page, &to.id(), Some(&from.id()), now_unix()).unwrap()
}

fn ids(page: &ViewPage) -> Vec<&str> {
    page.entries.iter().map(|entry| entry.id.as_str()).collect()
}

/// The benchmark references of the offer `to` makes `from` for
/// `capability`, presenting `grant`.
async fn refs_offered(
    from: &TestNode,
    to: &TestNode,
    capability: &str,
    grant: Option<&SignedGrant>,
) -> (Option<String>, Vec<BenchmarkRef>) {
    let offer = offer_of(from, to, capability, grant).await;
    (offer.skill_id, offer.benchmark_refs)
}

/// The offer `to` makes `from` for `capability`, presenting `grant`.
async fn offer_of(
    from: &TestNode,
    to: &TestNode,
    capability: &str,
    grant: Option<&SignedGrant>,
) -> ServiceOffer {
    let query = ServiceQuery {
        schema: QUERY_SCHEMA.to_owned(),
        query_id: "q-bench".to_owned(),
        capability: capability.to_owned(),
        input_media_types: Vec::new(),
        output_media_types: Vec::new(),
        requirements: Requirements::default(),
        summary: None,
        reply_timeout_ms: None,
        requested_inventory_sections: Vec::new(),
        project: None,
        execution_authorized: false,
    };
    let (offer, _) = from
        .node
        .discovery()
        .query(&to.id(), to.node.addr(), &query, grant, CALL)
        .await
        .unwrap();
    offer.doc
}

/// The revision of the view of the scope `grant` gives `from` on `to`: the
/// `seq` of its pages.
async fn seq_of(from: &TestNode, to: &TestNode, grant: Option<&SignedGrant>) -> u64 {
    benchmarks_page(from, to, grant).await.seq
}

/// The ids of the reports `page` refers to, not those of subjects without
/// any.
fn references(page: &ViewPage) -> Vec<String> {
    page.entries
        .iter()
        .filter(|entry| entry.body.get("status").is_none())
        .map(|entry| entry.id.clone())
        .collect()
}

#[tokio::test]
async fn get_benchmark_report_returns_an_existing_report_and_runs_nothing() {
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_with(exec.clone()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    // b's own measurement of its OCR service, and a peer's observation of
    // it, both public.
    let own = report(
        &key_in(b.dir.path()),
        "ocr-own-1",
        ocr(),
        MeasurementKind::OwnMeasurement,
        DAY,
    );
    let observed = report(
        &AgentKey::generate(),
        "ocr-observed-1",
        ocr(),
        MeasurementKind::PeerObservation,
        2 * DAY,
    );
    keep(&b, &own, Visibility::Public);
    keep(&b, &observed, Visibility::Public);

    for _ in 0..3 {
        // The report as its measurer signed it, which verifies as theirs.
        for kept in [&own, &observed] {
            let reply = get_report(&a, &b, &kept.doc.benchmark_id, None)
                .await
                .unwrap();
            let bytes = report_of(&reply);
            assert_eq!(bytes, kept.to_json());
            let read = Signed::<BenchmarkReport>::from_json(bytes).unwrap();
            assert_eq!(&read, kept);
            assert_eq!(read.doc.measurement_kind, kept.doc.measurement_kind);
        }
        // A report b does not hold is not made: nothing is measured.
        let missing = refusal(get_report(&a, &b, "ocr-new-run", None).await);
        assert_eq!(missing.code(), ErrorCode::NotFound);
        // And the view refers to what b holds.
        let page = benchmarks_page(&a, &b, None).await;
        assert_eq!(ids(&page), ["ocr-observed-1", "ocr-own-1"]);
    }
    assert_eq!(exec.runs(), 0);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
    // Nor did b keep anything new: it holds the two reports it held.
    assert_eq!(b.node.store.benchmarks().unwrap().len(), 2);
}

#[tokio::test]
async fn private_report_is_not_found_like_a_missing_one() {
    let b = provider_with(EchoExecutor).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let measurer = AgentKey::generate();
    let kind = MeasurementKind::PeerObservation;
    let public = report(&measurer, "r-public", ocr(), kind, DAY);
    let private = report(&measurer, "r-private", ocr(), kind, DAY);
    let demo = report(&measurer, "r-demo", ocr(), kind, DAY);
    // Public, but about a service nobody is shown: it would name it.
    let hidden = report(
        &measurer,
        "r-hidden",
        Subject::Service("transcriber".to_owned()),
        kind,
        DAY,
    );
    keep(&b, &public, Visibility::Public);
    keep(&b, &private, Visibility::Private);
    keep(&b, &demo, Visibility::Project(vec!["demo".to_owned()]));
    keep(&b, &hidden, Visibility::Public);

    // Without a grant: only the public one, and every other answer is the
    // answer to a report that does not exist.
    let missing = refusal(get_report(&a, &b, "r-missing", None).await);
    assert_eq!(missing.code(), ErrorCode::NotFound);
    for id in ["r-private", "r-demo", "r-hidden", "", "not an id"] {
        assert_eq!(
            refusal(get_report(&a, &b, id, None).await),
            missing,
            "{id:?}"
        );
    }
    assert!(!missing.message.contains("r-missing"));
    get_report(&a, &b, "r-public", None).await.unwrap();
    // The public view refers to the public report only, and counts nothing
    // of the others.
    let page = benchmarks_page(&a, &b, None).await;
    assert_eq!(ids(&page), ["r-public"]);
    let text = String::from_utf8(serde_json::to_vec(&page).unwrap()).unwrap();
    for id in [
        "r-private",
        "r-demo",
        "r-hidden",
        "transcriber",
        "repo-review",
    ] {
        assert!(!text.contains(id), "{id}");
    }

    // With a grant for the project: its report too, and the private one is
    // still a report that does not exist.
    let read = grant(&b, &a, &[TaskRead], 3600);
    let reply = get_report(&a, &b, "r-demo", Some(&read)).await.unwrap();
    assert_eq!(report_of(&reply), demo.to_json());
    for id in ["r-private", "r-hidden"] {
        assert_eq!(
            refusal(get_report(&a, &b, id, Some(&read)).await),
            missing,
            "{id}"
        );
    }
    let page = benchmarks_page(&a, &b, Some(&read)).await;
    assert_eq!(page.scope, "project:demo");
    assert_eq!(ids(&page), ["r-demo", "r-public", "service:repo-review"]);
    assert_eq!(
        page.entries[2].body,
        json!({"subject": {"service": "repo-review"}, "status": "no_measurement"})
    );
}

#[tokio::test]
async fn every_scope_gets_exactly_the_reports_its_view_lists() {
    // One rule for what a scope sees of the reports (RF2): its view, its
    // offers and GetBenchmarkReport never disagree.
    let b = provider_with(EchoExecutor).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let measurer = AgentKey::generate();
    let kind = MeasurementKind::PeerObservation;
    let review = || Subject::Service("repo-review".to_owned());
    let demo = |visibility: fn(Vec<String>) -> Visibility| visibility(vec!["demo".to_owned()]);
    let kept = [
        ("r-public", ocr(), Visibility::Public),
        ("r-private", ocr(), Visibility::Private),
        ("r-demo", ocr(), demo(Visibility::Project)),
        (
            "r-other",
            ocr(),
            Visibility::Project(vec!["other".to_owned()]),
        ),
        ("r-review", review(), Visibility::Public),
        ("r-task", review(), demo(Visibility::Task)),
        (
            "r-hidden",
            Subject::Service("transcriber".to_owned()),
            Visibility::Public,
        ),
        (
            "r-pair",
            Subject::NodePair {
                a: measurer.id(),
                b: b.id(),
            },
            Visibility::Public,
        ),
    ];
    for (id, subject, visibility) in &kept {
        keep(
            &b,
            &report(&measurer, id, subject.clone(), kind, DAY),
            visibility.clone(),
        );
    }

    let read = grant(&b, &a, &[TaskRead], 3600);
    let send = grant(&b, &a, &[TaskSend], 3600);
    for (grant, expected) in [
        (None, vec!["r-public", "r-pair"]),
        (
            Some(&read),
            vec!["r-demo", "r-public", "r-review", "r-pair"],
        ),
        (
            Some(&send),
            vec!["r-demo", "r-public", "r-review", "r-task", "r-pair"],
        ),
    ] {
        let page = benchmarks_page(&a, &b, grant).await;
        let listed = references(&page);
        assert_eq!(listed, expected, "{}", page.scope);
        // GetBenchmarkReport serves exactly what the view lists.
        let mut served = Vec::new();
        for (id, _, _) in &kept {
            match get_report(&a, &b, id, grant).await {
                Ok(_) => served.push(id.to_string()),
                Err(DiscoveryError::Remote(error)) => {
                    assert_eq!(error.code(), ErrorCode::NotFound, "{id}")
                }
                Err(other) => panic!("{id}: {other}"),
            }
        }
        let mut sorted = listed.clone();
        sorted.sort();
        served.sort();
        assert_eq!(served, sorted, "{}", page.scope);
        // And each offer refers to what the view lists of its service.
        for capability in ["vision.ocr", "code.rust.review", "audio.transcribe"] {
            let offer = offer_of(&a, &b, capability, grant).await;
            let of_service: Vec<String> = match &offer.skill_id {
                Some(skill) => page
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry.body.get("status").is_none()
                            && entry.body["subject"] == json!({ "service": skill })
                    })
                    .map(|entry| entry.id.clone())
                    .collect(),
                None => Vec::new(),
            };
            let mut referred: Vec<String> = offer
                .benchmark_refs
                .iter()
                .map(|reference| reference.benchmark_id.clone())
                .collect();
            referred.sort();
            let mut of_service = of_service;
            of_service.sort();
            assert_eq!(referred, of_service, "{} {capability}", page.scope);
        }
    }
}

#[tokio::test]
async fn a_report_the_public_scope_does_not_see_leaves_its_revision_alone() {
    // RF2: what the public scope does not see moves nothing it sees, not
    // even the revision of its view or of its offers.
    let b = provider_with(EchoExecutor).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let measurer = AgentKey::generate();
    let kind = MeasurementKind::PeerObservation;
    let read = grant(&b, &a, &[TaskRead], 3600);
    let public = seq_of(&a, &b, None).await;
    let project = seq_of(&a, &b, Some(&read)).await;
    let offered = offer_of(&a, &b, "vision.ocr", None).await;
    assert_eq!(offered.inventory_revision, public);

    // A private report, one for the project, and a public one about a
    // service the public is not shown.
    let private = report(&measurer, "r-private", ocr(), kind, DAY);
    keep(&b, &private, Visibility::Private);
    keep(
        &b,
        &report(&measurer, "r-demo", ocr(), kind, DAY),
        Visibility::Project(vec!["demo".to_owned()]),
    );
    keep(
        &b,
        &report(
            &measurer,
            "r-hidden",
            Subject::Service("transcriber".to_owned()),
            kind,
            DAY,
        ),
        Visibility::Public,
    );
    assert_eq!(seq_of(&a, &b, None).await, public);
    let offered = offer_of(&a, &b, "vision.ocr", None).await;
    assert_eq!(offered.inventory_revision, public);
    assert!(offered.benchmark_refs.is_empty());
    // The project's scope does see one: its view moved.
    assert_eq!(seq_of(&a, &b, Some(&read)).await, project + 1);
    // The private report made the project's: still nothing public.
    keep(&b, &private, Visibility::Project(vec!["demo".to_owned()]));
    assert_eq!(seq_of(&a, &b, None).await, public);
    assert_eq!(
        offer_of(&a, &b, "vision.ocr", None)
            .await
            .inventory_revision,
        public
    );
}

#[tokio::test]
async fn a_report_the_public_scope_sees_moves_its_revision() {
    let b = provider_with(EchoExecutor).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let measurer = AgentKey::generate();
    let public = seq_of(&a, &b, None).await;

    let seen = report(
        &measurer,
        "r-public",
        ocr(),
        MeasurementKind::PeerObservation,
        DAY,
    );
    keep(&b, &seen, Visibility::Public);
    assert_eq!(seq_of(&a, &b, None).await, public + 1);
    let offered = offer_of(&a, &b, "vision.ocr", None).await;
    assert_eq!(offered.inventory_revision, public + 1);
    assert_eq!(offered.benchmark_refs, [seen.doc.reference()]);
    // Made private again, it leaves the view, which moves again.
    keep(&b, &seen, Visibility::Private);
    assert_eq!(seq_of(&a, &b, None).await, public + 2);
    assert!(
        offer_of(&a, &b, "vision.ocr", None)
            .await
            .benchmark_refs
            .is_empty()
    );
}

#[tokio::test]
async fn offer_references_reports_of_its_service_visible_in_scope() {
    let b = provider_with(EchoExecutor).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let measurer = AgentKey::generate();
    let kind = MeasurementKind::PeerObservation;
    let older = report(&measurer, "ocr-older", ocr(), kind, 2 * DAY);
    let newer = report(&measurer, "ocr-newer", ocr(), kind, DAY);
    let private = report(&measurer, "ocr-private", ocr(), kind, 0);
    let declared = report(
        &key_in(b.dir.path()),
        "review-spec",
        Subject::Service("repo-review".to_owned()),
        MeasurementKind::DeclaredSpec,
        DAY,
    );
    let transcriber = report(
        &measurer,
        "transcriber-1",
        Subject::Service("transcriber".to_owned()),
        kind,
        DAY,
    );
    keep(&b, &older, Visibility::Public);
    keep(&b, &newer, Visibility::Project(vec!["demo".to_owned()]));
    keep(&b, &private, Visibility::Private);
    keep(&b, &declared, Visibility::Project(vec!["demo".to_owned()]));
    keep(&b, &transcriber, Visibility::Public);

    // Public: the public report of the service offered.
    let (skill, refs) = refs_offered(&a, &b, "vision.ocr", None).await;
    assert_eq!(skill.as_deref(), Some("ocr-metal-v1"));
    assert_eq!(refs, [older.doc.reference()]);
    // The project's scope: its report too, the most recent first.
    let read = grant(&b, &a, &[TaskRead], 3600);
    let (_, refs) = refs_offered(&a, &b, "vision.ocr", Some(&read)).await;
    assert_eq!(refs, [newer.doc.reference(), older.doc.reference()]);
    // Each service its own: a declared specification, said so.
    let (skill, refs) = refs_offered(&a, &b, "code.rust.review", Some(&read)).await;
    assert_eq!(skill.as_deref(), Some("repo-review"));
    assert_eq!(refs, [declared.doc.reference()]);
    assert_eq!(refs[0].measurement_kind, "declared_spec");
    // A service the scope does not show: no offer, no reference.
    for grant in [None, Some(&read)] {
        let (skill, refs) = refs_offered(&a, &b, "audio.transcribe", grant).await;
        assert_eq!(skill, None);
        assert!(refs.is_empty());
    }
    let (skill, refs) = refs_offered(&a, &b, "code.rust.review", None).await;
    assert_eq!(skill, None);
    assert!(refs.is_empty());
}

#[tokio::test]
async fn client_takes_only_the_report_an_offer_refers_to_as_its_measurer_signed_it() {
    let b = provider_with(EchoExecutor).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let own = report(
        &key_in(b.dir.path()),
        "ocr-own",
        ocr(),
        MeasurementKind::OwnMeasurement,
        DAY,
    );
    keep(&b, &own, Visibility::Public);
    let (_, refs) = refs_offered(&a, &b, "vision.ocr", None).await;
    assert_eq!(refs, [own.doc.reference()]);
    let client = a.node.discovery();
    let (fetched, reply) = client
        .benchmark_report(&b.id(), b.node.addr(), &refs[0], None, CALL)
        .await
        .unwrap();
    assert_eq!(fetched, own);
    assert!(reply.bytes_received > 0);
    // Not the report referred to: another time than the offer said.
    let mut moved = refs[0].clone();
    moved.measured_at = format_timestamp(now_unix()).unwrap();
    let refused = client
        .benchmark_report(&b.id(), b.node.addr(), &moved, None, CALL)
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        DiscoveryError::Report("not the report the offer refers to")
    );

    // A peer that answers with what its measurer did not sign, or with
    // another agent's own measurement as its own.
    let other = AgentKey::generate();
    let foreign = report(
        &other,
        "ocr-foreign",
        ocr(),
        MeasurementKind::OwnMeasurement,
        DAY,
    );
    let mut forged = foreign.clone();
    forged.doc.results[0].value = 1.0;
    for (n, (answer, why)) in [
        (forged.to_json(), "not a report its measurer signed"),
        (
            foreign.to_json(),
            "an own measurement signed by another agent than the peer",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mode = common::StubMode::Answer(std::sync::Arc::new(move |asked| {
            common::stub_response(asked, response::Result::BenchmarkReport(answer.clone()))
        }));
        let stub = common::stub_discovery_peer(&a, &format!("stub{n}"), mode).await;
        let refused = client
            .benchmark_report(&stub.id, stub.addr(), &foreign.doc.reference(), None, CALL)
            .await
            .unwrap_err();
        assert_eq!(refused, DiscoveryError::Report(why));
    }
}

/// `pals --home <home> <args>`: its exit code, stdout and stderr.
async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .arg("--home")
        .arg(home)
        .args(args);
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .unwrap();
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[tokio::test]
async fn import_and_ls_cli() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (code, _, stderr) = pals(home, &["init"]).await;
    assert_eq!(code, 0, "{stderr}");
    let cfg = Config::load(&home.join(CONFIG_FILE)).unwrap();
    let own = AgentKey::load_or_create(&cfg.key_path()).unwrap();
    let store = || Store::open(&cfg.data_dir.join(STORE_FILE), own.id()).unwrap();

    // A peer's observation of this agent, as a file in any JSON spelling:
    // what is signed is its JCS.
    let measurer = AgentKey::generate();
    let observed = report(
        &measurer,
        "observed-1",
        Subject::NodePair {
            a: measurer.id(),
            b: own.id(),
        },
        MeasurementKind::PeerObservation,
        DAY,
    );
    let signed_json: serde_json::Value = serde_json::from_slice(&observed.to_json()).unwrap();
    let file = home.join("observed-1.json");
    std::fs::write(&file, serde_json::to_string_pretty(&signed_json).unwrap()).unwrap();
    let file = file.to_str().unwrap();

    // Without --visibility: private.
    let (code, stdout, stderr) = pals(home, &["benchmark", "import", file]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("observed-1"), "{stdout}");
    assert!(stdout.contains("private"), "{stdout}");
    let (kept, visibility) = store().benchmark("observed-1").unwrap().unwrap();
    assert_eq!(kept, observed);
    assert_eq!(visibility, Visibility::Private);
    let (code, stdout, stderr) = pals(home, &["benchmark", "ls"]).await;
    assert_eq!(code, 0, "{stderr}");
    let line = format!(
        "observed-1\tpeer_observation\tservice.task 1\tnode_pair:{}:{}\t{}\t{}\t{}\tprivate",
        measurer.id(),
        own.id(),
        measurer.id(),
        observed.doc.measured_at,
        observed.doc.expires_at
    );
    assert_eq!(stdout.lines().collect::<Vec<_>>(), [line.as_str()]);

    // With --visibility public: the column changes, the report does not,
    // and it still verifies as its measurer's.
    let (code, stdout, stderr) = pals(
        home,
        &["benchmark", "import", file, "--visibility", "public"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("public"), "{stdout}");
    let (kept, visibility) = store().benchmark("observed-1").unwrap().unwrap();
    assert_eq!(visibility, Visibility::Public);
    assert_eq!(kept.to_json(), observed.to_json());
    Signed::<BenchmarkReport>::from_json(&kept.to_json()).unwrap();
    let (_, stdout, _) = pals(home, &["benchmark", "ls"]).await;
    assert!(stdout.trim_end().ends_with("\tpublic"), "{stdout}");
    // A project's.
    let (code, _, stderr) = pals(
        home,
        &["benchmark", "import", file, "--visibility", "project:demo"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        store().benchmark("observed-1").unwrap().unwrap().1,
        Visibility::Project(vec!["demo".to_owned()])
    );

    // Refused: a visibility that is none, a report its measurer did not
    // sign, and another report under an id kept already.
    let (code, _, stderr) = pals(
        home,
        &["benchmark", "import", file, "--visibility", "friends"],
    )
    .await;
    assert_ne!(code, 0);
    assert!(stderr.contains("visibility"), "{stderr}");
    let mut forged = signed_json.clone();
    forged["report"]["results"][0]["value"] = json!(1.0);
    let forged_file = home.join("forged.json");
    std::fs::write(&forged_file, forged.to_string()).unwrap();
    let (code, _, stderr) = pals(
        home,
        &["benchmark", "import", forged_file.to_str().unwrap()],
    )
    .await;
    assert_ne!(code, 0);
    assert!(stderr.contains("not signed by its measurer"), "{stderr}");
    let other = report(
        &measurer,
        "observed-1",
        Subject::Service("ocr-metal-v1".to_owned()),
        MeasurementKind::PeerObservation,
        0,
    );
    let other_file = home.join("other.json");
    std::fs::write(&other_file, other.to_json()).unwrap();
    let (code, _, stderr) =
        pals(home, &["benchmark", "import", other_file.to_str().unwrap()]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("another benchmark report"), "{stderr}");
    // What was kept stays as it was.
    let (kept, _) = store().benchmark("observed-1").unwrap().unwrap();
    assert_eq!(kept, observed);
}
