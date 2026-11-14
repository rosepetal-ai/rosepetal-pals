//! Service queries between real nodes: a `ServiceQuery` over
//! `agentnet-discovery/0` gets a `ServiceOffer` signed for its requester and
//! bound to its query, matched on the provider's inventory and load only;
//! both sides keep it; it never runs work; it never reveals what the
//! requester may not see, and a project never widens what a grant shows.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    CountingExecutor, StubAsked, StubMode, TestNode, grant, now_unix, spawn_node, spawn_node_in,
    spawn_node_in_with_load, spawn_node_in_with_options, stub_discovery_peer, stub_response,
};
use pals::config::{INVENTORY_FILE, WorkerCfg};
use pals::discovery::{DiscoveryError, Reply};
use pals::node::NodeOptions;
use pals::worker::echo::EchoExecutor;
use pals_core::discovery::query::{
    Conditions, InventoryRequirements, OfferError, OfferStatus, QUERY_SCHEMA, RequirementSet,
    Requirements, ServiceOffer, ServiceQuery, accept_offer, query_hash,
};
use pals_core::discovery::wire::{self, ErrorCode, request, response};
use pals_core::discovery::{DISCOVERY_VERSION, Load};
use pals_core::grant::Operation::{BlobFetch, TaskCancel, TaskRead, TaskSend};
use pals_core::grant::{SignedGrant, format_timestamp, parse_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::signed::Signed;
use serde_json::{Value, json};

/// An OCR service anyone may know of, a review service of the project
/// `demo`, and a transcription service never offered.
const INVENTORY: &str = r#"
[[services]]
id = "ocr-metal-v1"
name = "Industrial OCR"
description = "Reads serial numbers stamped on metal parts"
capabilities = ["vision.ocr"]
input_modes = ["image/png"]
output_modes = ["application/json"]
languages = ["es"]
processing_region = "EU"
external_cloud_used = false
estimated_start_delay_ms = 500
estimated_duration_ms = 1200
limitations = ["Quality depends on contrast and resolution"]
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

/// A node whose inventory is `inventory`, whose work `executor` does, with
/// `opts`.
async fn provider_with(
    inventory: &str,
    executor: impl a2a_server::AgentExecutor,
    opts: NodeOptions,
) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    spawn_node_in_with_options(dir, executor, opts).await
}

async fn provider() -> TestNode {
    provider_with(INVENTORY, EchoExecutor, NodeOptions::default()).await
}

/// The query of design §7.1.
fn spec_query() -> ServiceQuery {
    ServiceQuery {
        schema: QUERY_SCHEMA.to_owned(),
        query_id: "q-ocr-001".to_owned(),
        capability: "vision.ocr".to_owned(),
        input_media_types: vec!["image/png".to_owned()],
        output_media_types: vec!["application/json".to_owned()],
        requirements: Requirements {
            language: Some("es".to_owned()),
            external_cloud_allowed: Some(false),
            required_region: Some("EU".to_owned()),
            ..Requirements::default()
        },
        summary: Some("Texto pequeño grabado sobre una superficie metálica".to_owned()),
        reply_timeout_ms: Some(3000),
        requested_inventory_sections: Vec::new(),
        project: None,
        execution_authorized: false,
    }
}

/// A query for `capability` with no requirements.
fn query_for(capability: &str) -> ServiceQuery {
    ServiceQuery {
        query_id: "q-1".to_owned(),
        capability: capability.to_owned(),
        input_media_types: Vec::new(),
        output_media_types: Vec::new(),
        requirements: Requirements::default(),
        summary: None,
        ..spec_query()
    }
}

/// `query` from `from` to `to`, presenting `grant`, over `from`'s discovery
/// client.
async fn ask(
    from: &TestNode,
    to: &TestNode,
    query: &ServiceQuery,
    grant: Option<&SignedGrant>,
) -> Result<(Signed<ServiceOffer>, Reply), DiscoveryError> {
    from.node
        .discovery()
        .query(&to.id(), to.node.addr(), query, grant, CALL)
        .await
}

async fn offer_of(
    from: &TestNode,
    to: &TestNode,
    query: &ServiceQuery,
    grant: Option<&SignedGrant>,
) -> ServiceOffer {
    ask(from, to, query, grant).await.unwrap().0.doc
}

/// `offer` without what changes from one offer to the next: its id, its
/// times, and the hash of the query it answers.
fn shape(offer: &ServiceOffer) -> ServiceOffer {
    ServiceOffer {
        offer_id: String::new(),
        issued_at: String::new(),
        expires_at: String::new(),
        query_hash: String::new(),
        ..offer.clone()
    }
}

#[tokio::test]
async fn query_over_the_wire_returns_a_signed_offer_for_the_requester() {
    let b = provider().await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let query = spec_query();
    let before = now_unix();
    let (signed, reply) = ask(&a, &b, &query, None).await.unwrap();
    let offer = &signed.doc;
    assert_eq!(offer.agent_id, b.id());
    assert_eq!(offer.requester, a.id());
    assert_eq!(offer.query_id, "q-ocr-001");
    assert_eq!(offer.query_hash, query_hash(&query.to_jcs()));
    assert_eq!(offer.skill_id.as_deref(), Some("ocr-metal-v1"));
    assert_eq!(offer.status, OfferStatus::Available);
    assert!(offer.compatible);
    assert_eq!(
        offer.conditions,
        Conditions {
            external_cloud_used: Some(false),
            processing_region: Some("EU".to_owned()),
            authorization_required: true,
        }
    );
    assert_eq!(offer.estimated_start_delay_ms, Some(500));
    assert_eq!(offer.estimated_duration_ms, Some(1200));
    assert_eq!(offer.scope, "public");
    assert_eq!(offer.agent_card_hash, b.node.cards().public().hash);
    assert!(offer.inventory_revision >= 1);
    assert!(offer.benchmark_refs.is_empty());
    // Valid 15 s, in whole seconds of b's clock.
    assert_eq!(offer.valid_for_ms, 15_000);
    let issued_at = parse_timestamp(&offer.issued_at).unwrap();
    assert_eq!(parse_timestamp(&offer.expires_at).unwrap(), issued_at + 15);
    assert!(issued_at >= before - 1);
    // As b signed it: the bytes of the reply, which carries b's records.
    let Some(response::Result::ServiceOffer(bytes)) = &reply.response.result else {
        panic!("an offer, not {:?}", reply.response.result);
    };
    assert_eq!(bytes, &signed.to_json());
    assert_eq!(reply.announce.doc.agent_card_hash, offer.agent_card_hash);
    // It is a's, and nobody else's.
    let c = AgentKey::generate().id();
    assert_eq!(
        accept_offer(bytes, &b.id(), &c, &query, now_unix()),
        Err(OfferError::WrongRequester)
    );

    // The provider sets how long its offers last.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), INVENTORY).unwrap();
    let brief = spawn_node_in(dir, WorkerCfg::Echo, |cfg| {
        cfg.discovery.offer_valid_for_ms = 1500;
    })
    .await;
    let offer = offer_of(&a, &brief, &query, None).await;
    assert_eq!(offer.valid_for_ms, 1500);
    let issued_at = parse_timestamp(&offer.issued_at).unwrap();
    assert_eq!(parse_timestamp(&offer.expires_at).unwrap(), issued_at + 2);
}

#[tokio::test]
async fn offers_are_kept_by_both_sides() {
    let b = provider().await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let query = spec_query();
    let (first, _) = ask(&a, &b, &query, None).await.unwrap();
    let (second, _) = ask(&a, &b, &query, None).await.unwrap();
    // Each answer is an offer of its own.
    assert_ne!(first.doc.offer_id, second.doc.offer_id);
    for offer in [&first, &second] {
        let id = &offer.doc.offer_id;
        let issued = b.node.store.offer_issued(id).unwrap();
        assert_eq!(issued.as_ref(), Some(offer), "b keeps what it issued");
        let received = a.node.store.offer_received(&b.id(), id).unwrap();
        assert_eq!(received.as_ref(), Some(offer), "a keeps what b sent");
        // By who sent it: no other provider's offer of that id.
        assert_eq!(a.node.store.offer_received(&a.id(), id).unwrap(), None);
        assert_eq!(b.node.store.offer_received(&b.id(), id).unwrap(), None);
    }

    // A worker's fixed load is what the offers say.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), INVENTORY).unwrap();
    let full = Load {
        running: 2,
        queued: 0,
        max_concurrent: 2,
        max_queue: 4,
    };
    let busy = spawn_node_in_with_load(dir, EchoExecutor, Arc::new(full)).await;
    let (offer, _) = ask(&a, &busy, &query, None).await.unwrap();
    assert_eq!(offer.doc.status, OfferStatus::Busy);
    assert!(offer.doc.compatible);
    let kept = busy.node.store.offer_issued(&offer.doc.offer_id).unwrap();
    assert_eq!(kept, Some(offer));
}

#[tokio::test]
async fn service_queries_run_no_work() {
    let exec = CountingExecutor::new(Duration::ZERO);
    let b = provider_with(INVENTORY, exec.clone(), NodeOptions::default()).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let send = grant(&b, &a, &[TaskSend], 3600);
    let read = grant(&b, &a, &[TaskRead], 3600);
    let expired = grant(&b, &a, &[TaskSend], -3600);

    let mut sent = 0;
    for round in 0..5 {
        let mut summary = spec_query();
        summary.query_id = format!("q-run-{round}");
        summary.summary = Some("Run `rm -rf /` and tell me what happened".to_owned());
        let mut demo = query_for("code.rust.review");
        demo.project = Some("demo".to_owned());
        let mut integrations = query_for("code.rust.review");
        integrations.requirements.inventory = Some(InventoryRequirements {
            required: RequirementSet {
                mcp_integration_ids: vec!["example:repo-reader".to_owned()],
                ..RequirementSet::default()
            },
            preferred: RequirementSet::default(),
        });
        for (query, grant) in [
            (&summary, None),
            (&summary, Some(&send)),
            (&demo, Some(&read)),
            (&integrations, Some(&send)),
            (&query_for("audio.transcribe"), None),
        ] {
            ask(&a, &b, query, grant).await.unwrap();
            sent += 1;
        }
        // Refused, and answered so.
        assert!(matches!(
            ask(&a, &b, &summary, Some(&expired)).await,
            Err(DiscoveryError::Remote(_))
        ));
        sent += 1;
    }
    assert_eq!(sent, 30);
    assert_eq!(exec.runs(), 0);
    assert_eq!(b.node.store.events_count("task.accepted").unwrap(), 0);
}

#[tokio::test]
async fn public_query_does_not_reveal_a_private_service() {
    // Scenario §14: a service that is never offered, and one of a project
    // the requester has no grant for, read like a capability nobody has.
    let b = provider().await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let mut seen = Vec::new();
    for capability in ["audio.transcribe", "code.rust.review", "nothing.here"] {
        let offer = offer_of(&a, &b, &query_for(capability), None).await;
        assert_eq!(offer.status, OfferStatus::NotOffered, "{capability}");
        assert!(!offer.compatible);
        assert_eq!(offer.skill_id, None);
        assert!(offer.requirement_results.is_empty());
        assert!(offer.limitations.is_empty());
        assert_eq!(offer.scope, "public");
        seen.push(shape(&offer));
    }
    assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:#?}");

    // A grant that shows nothing more is no different.
    let blobs = grant(&b, &a, &[BlobFetch], 3600);
    let offer = offer_of(&a, &b, &query_for("code.rust.review"), Some(&blobs)).await;
    assert_eq!(shape(&offer), seen[0]);

    // The project's grant shows its service; never the private one.
    let read = grant(&b, &a, &[TaskRead], 3600);
    let shown = offer_of(&a, &b, &query_for("code.rust.review"), Some(&read)).await;
    assert_eq!(shown.scope, "project:demo");
    assert_eq!(shown.skill_id.as_deref(), Some("repo-review"));
    assert_eq!(shown.status, OfferStatus::Available);
    let hidden = offer_of(&a, &b, &query_for("audio.transcribe"), Some(&read)).await;
    assert_eq!(hidden.status, OfferStatus::NotOffered);
    assert_eq!(hidden.skill_id, None);
}

/// A signed offer of `asked`'s stub for the query it was asked, numbered
/// `offer_id`, for `requester`, signed by `signer` and naming it as the
/// provider, bound to `query_bytes`.
fn stub_offer(
    signer: &AgentKey,
    requester: AgentId,
    query_bytes: &[u8],
    offer_id: &str,
    skill_id: &str,
) -> Vec<u8> {
    let query = ServiceQuery::from_jcs(query_bytes).expect("a query");
    let now = now_unix();
    let offer = ServiceOffer {
        schema: "agentnet.service-offer/1".to_owned(),
        query_id: query.query_id.clone(),
        offer_id: offer_id.to_owned(),
        agent_id: signer.id(),
        requester,
        query_hash: query_hash(query_bytes),
        skill_id: Some(skill_id.to_owned()),
        agent_card_hash: format!("blake3:{}", "0".repeat(64)).parse().unwrap(),
        scope: "public".to_owned(),
        inventory_revision: 1,
        status: OfferStatus::Available,
        compatible: true,
        conditions: Conditions {
            external_cloud_used: None,
            processing_region: None,
            authorization_required: true,
        },
        requirement_results: Vec::new(),
        unknown_requirements: Vec::new(),
        estimated_start_delay_ms: None,
        estimated_duration_ms: None,
        valid_for_ms: 15_000,
        issued_at: format_timestamp(now).unwrap(),
        expires_at: format_timestamp(now + 15).unwrap(),
        limitations: Vec::new(),
        benchmark_refs: Vec::new(),
    };
    Signed::issue(signer, offer).unwrap().to_json()
}

/// The query bytes of `asked`'s request.
fn query_bytes(asked: &StubAsked<'_>) -> Vec<u8> {
    match asked.request.and_then(|request| request.op.as_ref()) {
        Some(request::Op::ServiceQuery(query)) => query.query.clone(),
        other => panic!("a service query, not {other:?}"),
    }
}

#[tokio::test]
async fn offer_for_another_requester_is_refused_by_the_client() {
    let a = spawn_node(WorkerCfg::Echo).await;
    // What the stub answers depends on the query's id.
    let answer = Arc::new(|asked: &StubAsked<'_>| {
        let bytes = query_bytes(asked);
        let query = ServiceQuery::from_jcs(&bytes).unwrap();
        let stranger = AgentKey::generate();
        let offer = match query.query_id.as_str() {
            "q-stranger" => stub_offer(asked.key, stranger.id(), &bytes, "offer-1", "ocr"),
            "q-impostor" => stub_offer(&stranger, asked.requester, &bytes, "offer-1", "ocr"),
            "q-other" => {
                let other = query_for("audio.transcribe").to_jcs();
                stub_offer(asked.key, asked.requester, &other, "offer-1", "ocr")
            }
            _ => stub_offer(asked.key, asked.requester, &bytes, "offer-1", "ocr"),
        };
        stub_response(asked, response::Result::ServiceOffer(offer))
    });
    let stub = stub_discovery_peer(&a, "stub", StubMode::Answer(answer)).await;
    let ask_stub = |query_id: &str| {
        let client = a.node.discovery().clone();
        let (id, addr) = (stub.id, stub.addr());
        let mut query = spec_query();
        query.query_id = query_id.to_owned();
        async move { client.query(&id, addr, &query, None, CALL).await }
    };
    for (query_id, refusal) in [
        ("q-stranger", OfferError::WrongRequester),
        ("q-impostor", OfferError::WrongProvider),
        ("q-other", OfferError::QueryMismatch),
    ] {
        let error = ask_stub(query_id).await.unwrap_err();
        assert_eq!(error, DiscoveryError::Offer(refusal), "{query_id}");
        // Nothing refused is kept.
        let kept = a.node.store.offer_received(&stub.id, "offer-1").unwrap();
        assert_eq!(kept, None, "{query_id}");
    }
    // The stub's own offer for a's query is taken, and kept.
    let (offer, _) = ask_stub("q-fine").await.unwrap();
    assert_eq!(offer.doc.requester, a.id());
    let kept = a.node.store.offer_received(&stub.id, "offer-1").unwrap();
    assert_eq!(kept, Some(offer));
}

#[tokio::test]
async fn two_providers_may_use_the_same_offer_id() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let provider = |skill: &'static str| {
        StubMode::Answer(Arc::new(move |asked: &StubAsked<'_>| {
            let offer = stub_offer(
                asked.key,
                asked.requester,
                &query_bytes(asked),
                "offer-001",
                skill,
            );
            stub_response(asked, response::Result::ServiceOffer(offer))
        }))
    };
    let b = stub_discovery_peer(&a, "b", provider("ocr-b")).await;
    let c = stub_discovery_peer(&a, "c", provider("ocr-c")).await;
    let query = spec_query();
    let mut offers = Vec::new();
    for stub in [&b, &c] {
        let (offer, _) = a
            .node
            .discovery()
            .query(&stub.id, stub.addr(), &query, None, CALL)
            .await
            .unwrap();
        assert_eq!(offer.doc.offer_id, "offer-001");
        offers.push(offer);
    }
    // Both kept, each under its provider.
    for (stub, offer, skill) in [(&b, &offers[0], "ocr-b"), (&c, &offers[1], "ocr-c")] {
        let kept = a
            .node
            .store
            .offer_received(&stub.id, "offer-001")
            .unwrap()
            .expect("kept by provider and offer id");
        assert_eq!(&kept, offer);
        assert_eq!(kept.doc.agent_id, stub.id);
        assert_eq!(kept.doc.skill_id.as_deref(), Some(skill));
    }
}

#[tokio::test]
async fn project_never_widens_the_scope() {
    let b = provider().await;
    let a = spawn_node(WorkerCfg::Echo).await;
    // A grant of `demo` to send tasks.
    let send = grant(&b, &a, &[TaskSend], 3600);
    let review = |project: Option<&str>| {
        let mut query = query_for("code.rust.review");
        query.project = project.map(str::to_owned);
        query
    };
    // No project: the grant's scope. Its own project: the same.
    for project in [None, Some("demo")] {
        let offer = offer_of(&a, &b, &review(project), Some(&send)).await;
        assert_eq!(offer.scope, "task:demo", "{project:?}");
        assert_eq!(offer.status, OfferStatus::Available);
        assert_eq!(offer.skill_id.as_deref(), Some("repo-review"));
        // The grant presented already lets a send tasks; it is checked
        // again when one arrives.
        assert!(!offer.conditions.authorization_required);
    }
    // Another project: answered as public, never as `other`'s.
    let offer = offer_of(&a, &b, &review(Some("other")), Some(&send)).await;
    assert_eq!(offer.scope, "public");
    assert_eq!(offer.status, OfferStatus::NotOffered);
    assert_eq!(offer.agent_card_hash, b.node.cards().public().hash);
    // Without a grant, a project is no use either.
    let offer = offer_of(&a, &b, &review(Some("demo")), None).await;
    assert_eq!(offer.scope, "public");
    assert_eq!(offer.status, OfferStatus::NotOffered);
}

/// Runs `pals --home <home> <args>` off the async threads; its exit code,
/// stdout and stderr.
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
async fn query_cli_prints_the_offer() {
    let b = provider().await;
    let a = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    // The CLI finds the running daemon by its configuration.
    let home = a.dir.path().to_owned();
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    let requirements = home.join("requirements.json");
    std::fs::write(
        &requirements,
        r#"{"language": "es", "external_cloud_allowed": false, "required_region": "EU"}"#,
    )
    .unwrap();

    let (code, stdout, stderr) = pals(
        &home,
        &[
            "query",
            "bob",
            "--capability",
            "vision.ocr",
            "--input",
            "image/png",
            "--output",
            "application/json",
            "--requirements-file",
            requirements.to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (json, verified) = stdout
        .trim_end()
        .rsplit_once('\n')
        .expect("the offer, then the verified line");
    let printed: Value = serde_json::from_str(json).unwrap();
    // As b signed it.
    let signed = Signed::<ServiceOffer>::from_json(&pals_core::jcs::canonical(&printed)).unwrap();
    let offer = &signed.doc;
    assert_eq!(offer.agent_id, b.id());
    assert_eq!(offer.requester, a.id());
    assert_eq!(offer.status, OfferStatus::Available);
    assert_eq!(offer.skill_id.as_deref(), Some("ocr-metal-v1"));
    assert_eq!(
        verified,
        format!("verified: {} {} available", b.id(), offer.offer_id)
    );
    // a's daemon asked, so a keeps it.
    let kept = a
        .node
        .store
        .offer_received(&b.id(), &offer.offer_id)
        .unwrap();
    assert_eq!(kept, Some(signed.clone()));

    // A project b's grant does not cover is answered as public.
    let (code, stdout, stderr) = pals(
        &home,
        &[
            "query",
            "bob",
            "--capability",
            "code.rust.review",
            "--project",
            "demo",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("\"scope\": \"public\""), "{stdout}");
    assert!(stdout.trim_end().ends_with("not_offered"), "{stdout}");

    // An unknown peer, an invalid capability or requirements are said so,
    // before anything is asked.
    let (code, _, stderr) = pals(&home, &["query", "nobody", "--capability", "vision.ocr"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown contact"), "{stderr}");
    let (code, _, stderr) = pals(&home, &["query", "bob", "--capability", "vision ocr"]).await;
    assert_ne!(code, 0);
    assert!(stderr.contains("invalid capability"), "{stderr}");
    std::fs::write(&requirements, "[1, 2]").unwrap();
    let (code, _, stderr) = pals(
        &home,
        &[
            "query",
            "bob",
            "--capability",
            "vision.ocr",
            "--requirements-file",
            requirements.to_str().unwrap(),
        ],
    )
    .await;
    assert_ne!(code, 0);
    assert!(stderr.contains("requirements"), "{stderr}");
}

#[tokio::test]
async fn invalid_query_is_refused_by_the_provider_and_by_the_gateway() {
    let b = provider().await;
    let a = spawn_node(WorkerCfg::Echo).await;
    a.add_contact("bob", &b);
    // A query that authorizes execution, sent as it is.
    let mut bad = spec_query();
    bad.execution_authorized = true;
    let req = wire::Request {
        version: DISCOVERY_VERSION,
        max_response_bytes: 0,
        grant_token: String::new(),
        op: Some(request::Op::ServiceQuery(wire::ServiceQuery {
            query: bad.to_jcs(),
        })),
    };
    let error = a
        .node
        .discovery()
        .call(&b.id(), b.node.addr(), req, CALL)
        .await
        .unwrap_err();
    let DiscoveryError::Remote(error) = error else {
        panic!("a refusal, not {error}");
    };
    assert_eq!(error.code(), ErrorCode::BadRequest);

    // The gateway refuses it before asking.
    let gateway = a.gateway_url();
    let res = common::gateway_request(
        &gateway,
        "POST",
        "/discovery/query/bob",
        &[],
        Some(&serde_json::to_value(&bad).unwrap()),
    )
    .await;
    assert_eq!(res.status, 400, "{}", res.json);
    assert!(
        res.json["error"]
            .as_str()
            .unwrap()
            .contains("never authorizes execution")
    );
    // And answers a good one with the offer as b signed it.
    let res = common::gateway_request(
        &gateway,
        "POST",
        "/discovery/query/bob",
        &[],
        Some(&serde_json::to_value(spec_query()).unwrap()),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let signed = Signed::<ServiceOffer>::from_json(&pals_core::jcs::canonical(&res.json)).unwrap();
    assert_eq!(signed.doc.requester, a.id());
    assert_eq!(res.json["offer"]["status"], json!("available"));
    // An unknown peer is said so.
    let res = common::gateway_request(
        &gateway,
        "POST",
        "/discovery/query/nobody",
        &[],
        Some(&serde_json::to_value(spec_query()).unwrap()),
    )
    .await;
    assert_eq!(res.status, 404, "{}", res.json);
}

/// The busy worker of milestone 1, seen from a query: a provider that runs
/// `exec` with one task at a time says it is busy while one runs.
#[cfg(unix)]
#[tokio::test]
async fn offers_follow_the_worker_load() {
    use pals::config::{ExecCfg, ProjectCfg};

    let project = tempfile::tempdir().unwrap();
    let path = project.path().to_path_buf();
    let exec = ExecCfg {
        max_concurrent: 1,
        max_queue: 1,
        ..ExecCfg::new(
            "sh",
            &[
                "-c",
                "read -r x; if [ \"$x\" = wait ]; then sleep 600; fi; echo \"$x\"",
            ],
        )
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), INVENTORY).unwrap();
    let b = spawn_node_in(dir, WorkerCfg::Exec(exec), |cfg| {
        cfg.projects.insert("demo".to_owned(), ProjectCfg { path });
    })
    .await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant(&b, &a, &[TaskSend, TaskRead, TaskCancel], 3600);
    let query = spec_query();
    assert_eq!(
        offer_of(&a, &b, &query, None).await.status,
        OfferStatus::Available
    );

    // One task running: its working directory exists once it has its turn.
    let mut params = common::message_params("wait", None);
    params["configuration"] = json!({"returnImmediately": true});
    let res = a.rpc(&b, Some(&g), "SendMessage", params).await;
    let task_id = res.json["result"]["task"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a task: {}", res.json))
        .to_owned();
    let task_dir = b.dir.path().join(pals::worker::TASKS_DIR).join(&task_id);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !task_dir.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the run never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let busy = offer_of(&a, &b, &query, None).await;
    assert_eq!(busy.status, OfferStatus::Busy);
    assert!(busy.compatible);

    // Canceled, its place is free again.
    let res = a
        .rpc(&b, Some(&g), "CancelTask", json!({"id": task_id}))
        .await;
    assert_eq!(
        res.json["result"]["status"]["state"], "TASK_STATE_CANCELED",
        "{}",
        res.json
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let offer = offer_of(&a, &b, &query, None).await;
        if offer.status == OfferStatus::Available {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the place was never freed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
