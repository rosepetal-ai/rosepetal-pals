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
use pals_core::discovery::query::{BenchmarkRef, QUERY_SCHEMA, Requirements, ServiceQuery};
use pals_core::discovery::wire::{self, ErrorCode, Request, request, response};
use pals_core::grant::Operation::TaskRead;
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
    (offer.doc.skill_id, offer.doc.benchmark_refs)
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
