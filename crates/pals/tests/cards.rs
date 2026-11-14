//! The agent's signed public card, the extended card each grant's scope
//! gets, the contact and announce records the node issues, and peers' cards
//! as the gateway checks them.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::routing::{get, post};
use common::{
    TestNode, gateway_request, grant, now_unix, rpc_request, shut_down, spawn_node, spawn_node_in,
    stub_peer, test_config,
};
use pals::a2a::auth::SCOPE_HEADER;
use pals::a2a::{CardSet, IDEMPOTENCY_EXTENSION, SECURITY_SCHEME, gateway_view};
use pals::config::{INVENTORY_FILE, WorkerCfg};
use pals::discovery::{CardFetchError, PeerCards};
use pals::node::Node;
use pals_core::card_jws::{CanonicalForm, CardError, CardSigner, parse_and_verify, verify_card};
use pals_core::grant::{
    GRANT_SCHEMA, Grant, Operation, Operation::BlobFetch, Operation::TaskRead, Operation::TaskSend,
    SignedGrant, format_timestamp, parse_timestamp,
};
use pals_core::identity::AgentKey;
use pals_core::inventory::{Inventory, Scope};
use pals_core::records::{AnnounceRecord, ContactRecord, RECORD_RENEW_BEFORE_S, RECORD_TTL_S};
use pals_core::signed::Signed;
use serde_json::{Value, json};

/// A public service, one for the project `demo`'s requesters and one for
/// those that may send it tasks.
const INVENTORY: &str = r#"
[[services]]
id = "ocr-metal-v1"
name = "Metal OCR"
description = "Reads serial numbers stamped on metal parts"
capabilities = ["vision.ocr"]
tags = ["metal"]
examples = ["Read the plate in plate.jpg"]
input_modes = ["image/jpeg"]
output_modes = ["text/plain"]
visibility = "public"

[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["code.review"]
visibility = { project = ["demo"] }

[[services]]
id = "repo-fix"
name = "Repository fix"
description = "Fixes issues of the demo repository"
capabilities = ["code.fix"]
visibility = { task = ["demo"] }
"#;

const WELL_KNOWN: &str = "/.well-known/agent-card.json";

/// A node whose inventory is `inventory`, on a fresh data directory.
async fn spawn_with_inventory(inventory: &str) -> TestNode {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
    spawn_node_in(dir, WorkerCfg::Echo, |_| {}).await
}

/// Stops `node` and starts it again on its data directory.
async fn restart(node: TestNode) -> TestNode {
    let TestNode { node, dir, .. } = node;
    shut_down(node).await;
    spawn_node_in(dir, WorkerCfg::Echo, |_| {}).await
}

/// `count` public services, `service-<n>`, each with `description`.
fn public_services(count: usize, description: &str) -> String {
    (0..count)
        .map(|n| {
            format!(
                "[[services]]\nid = \"service-{n}\"\nname = \"Service {n}\"\n\
                 description = \"{description}\"\ncapabilities = [\"cap-{n}\"]\n\
                 visibility = \"public\"\n"
            )
        })
        .collect()
}

/// A grant of `issuer` that lets `subject` perform `ops` on `resources` of
/// `project` for an hour, recorded as issued.
fn grant_in(
    issuer: &TestNode,
    subject: &TestNode,
    project: &str,
    ops: &[Operation],
    resources: &[&str],
) -> SignedGrant {
    static NEXT_GRANT: AtomicU64 = AtomicU64::new(1);
    let now = now_unix();
    let grant = Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: format!("g-cards-{}", NEXT_GRANT.fetch_add(1, Ordering::Relaxed)),
        issuer: issuer.id(),
        subject: subject.id(),
        audience: issuer.id(),
        project: project.to_owned(),
        resources: resources.iter().map(|r| (*r).to_owned()).collect(),
        operations: ops.to_vec(),
        issued_at: format_timestamp(now).unwrap(),
        expires_at: format_timestamp(now + 3600).unwrap(),
    };
    let key = common::key_in(issuer.dir.path());
    let signed = SignedGrant::issue(&key, grant).unwrap();
    issuer.node.store.grant_issued_put(&signed).unwrap();
    signed
}

/// The ids of the skills of `card`, in order.
fn skill_ids(card: &Value) -> Vec<&str> {
    card["skills"]
        .as_array()
        .unwrap_or_else(|| panic!("no skills: {card}"))
        .iter()
        .map(|skill| skill["id"].as_str().unwrap())
        .collect()
}

/// The signed public card of a fresh agent, with an empty inventory.
fn signed_card_of(key: &AgentKey) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let cfg = test_config(dir.path(), WorkerCfg::Echo);
    let signer = CardSigner::new(Arc::new(AgentKey::from_bytes(&key.to_bytes())));
    let cards = CardSet::build(&cfg, &signer, &Inventory::default()).unwrap();
    cards.public().json.clone()
}

/// An app that serves `card` as its public card, answers
/// `GetExtendedAgentCard` with it on both bindings, and claims a card hash
/// of its own in a PALS header on every answer.
fn card_app(card: Value) -> axum::Router {
    let forged = [("x-agentnet-card-hash", "blake3:forged")];
    let public = card.clone();
    let rest = card.clone();
    axum::Router::new()
        .route(
            WELL_KNOWN,
            get(move || async move { (forged, axum::Json(public)) }),
        )
        .route(
            "/jsonrpc",
            post(move || async move {
                let answer = json!({"jsonrpc": "2.0", "id": 1, "result": card});
                (forged, axum::Json(answer))
            }),
        )
        .route(
            "/rest/extendedAgentCard",
            get(move || async move { (forged, axum::Json(rest)) }),
        )
}

/// What `a`'s gateway answers for the card of `peer`, on the view route,
/// the signed route and both bindings of the extended card.
async fn every_card_route(a: &TestNode, peer: &str) -> Vec<common::TestResponse> {
    let gw = a.gateway_url();
    let call = rpc_request("GetExtendedAgentCard", json!({}));
    let headers = [("a2a-version", "1.0")];
    vec![
        gateway_request(
            &gw,
            "GET",
            &format!("/peers/{peer}{WELL_KNOWN}"),
            &headers,
            None,
        )
        .await,
        gateway_request(
            &gw,
            "GET",
            &format!("/peers/{peer}/signed-agent-card.json"),
            &headers,
            None,
        )
        .await,
        gateway_request(
            &gw,
            "POST",
            &format!("/peers/{peer}/jsonrpc"),
            &headers,
            Some(&call),
        )
        .await,
        gateway_request(
            &gw,
            "GET",
            &format!("/peers/{peer}/rest/extendedAgentCard"),
            &headers,
            None,
        )
        .await,
    ]
}

/// Asserts that the four answers of [`every_card_route`] refuse the peer's
/// card: the two public routes because it is invalid for `reason`, the
/// extended card as a protocol error, none with the peer's own PALS header.
fn assert_refused(answers: &[common::TestResponse], reason: &str) {
    let invalid = json!({ "error": format!("peer card invalid: {reason}") });
    for res in &answers[..2] {
        assert_eq!(res.status, 502, "{}", res.json);
        assert_eq!(res.json, invalid);
    }
    for res in &answers[2..] {
        assert_eq!(res.status, 502, "{}", res.json);
        assert_eq!(res.json, json!({"error": "peer protocol error"}));
    }
    for res in answers {
        assert!(
            res.headers.get("x-agentnet-card-hash").is_none(),
            "{:?}",
            res.headers
        );
    }
}

fn parse(text: &str) -> i64 {
    parse_timestamp(text).unwrap()
}

#[tokio::test]
async fn served_card_is_signed_by_the_agent_and_verifies() {
    let b = spawn_with_inventory(INVENTORY).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let res = a.get(&b, WELL_KNOWN, None).await;
    assert_eq!(res.status, 200, "{}", res.json);

    let verified = parse_and_verify(&res.body, &b.id()).unwrap();
    // Signed over the card as served, as every SDK checks it.
    assert_eq!(verified.form, CanonicalForm::Raw);
    let public = b.node.cards().public();
    assert_eq!(verified.hash, public.hash);
    assert_eq!(verified.card, public.card);
    assert_eq!(res.json, public.json);
    assert_eq!(
        parse_and_verify(&res.body, &a.id()).unwrap_err(),
        CardError::WrongSigner
    );

    // `ask` and the public service only, with its capability as a tag.
    assert_eq!(skill_ids(&res.json), ["ask", "ocr-metal-v1"]);
    assert_eq!(
        res.json["skills"][1],
        json!({
            "id": "ocr-metal-v1",
            "name": "Metal OCR",
            "description": "Reads serial numbers stamped on metal parts",
            "tags": ["agentnet:capability:v1:vision.ocr", "metal"],
            "examples": ["Read the plate in plate.jpg"],
            "inputModes": ["image/jpeg"],
            "outputModes": ["text/plain"],
        })
    );
}

#[tokio::test]
async fn card_declares_the_extended_card() {
    let b = spawn_node(WorkerCfg::Echo).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let card = a.get(&b, WELL_KNOWN, None).await.json;

    assert_eq!(card["capabilities"]["extendedAgentCard"], true);
    assert_eq!(card["capabilities"]["streaming"], true);
    assert_eq!(card["capabilities"]["pushNotifications"], false);
    // Only the idempotency extension, which no request has to activate.
    let extensions = card["capabilities"]["extensions"].as_array().unwrap();
    assert_eq!(extensions.len(), 1, "{card}");
    assert_eq!(extensions[0]["uri"], IDEMPOTENCY_EXTENSION);
    assert!(extensions[0].get("required").is_none(), "{card}");
    // The grant must cover `skill:ask`.
    assert_eq!(
        card["securityRequirements"],
        json!([{"schemes": {SECURITY_SCHEME: {"list": ["skill:ask"]}}}])
    );
    assert_eq!(card["description"], "A PALS agent");

    // An empty description in the configuration says the same.
    let blank = common::spawn_node_with(WorkerCfg::Echo, |cfg| {
        cfg.agent.description = String::new();
    })
    .await;
    let card = a.get(&blank, WELL_KNOWN, None).await.json;
    assert_eq!(card["description"], "A PALS agent");
    parse_and_verify(&serde_json::to_vec(&card).unwrap(), &blank.id()).unwrap();
}

#[tokio::test]
async fn inventory_whose_cards_exceed_the_limits_is_refused_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let port = {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut cfg = test_config(dir.path(), WorkerCfg::Echo);
    cfg.gateway.listen = listen;
    let refused = async |inventory: String| {
        std::fs::write(dir.path().join(INVENTORY_FILE), inventory).unwrap();
        let err = Node::spawn(cfg.clone()).await.err().expect("refused");
        // Nothing was left listening: the gateway was never bound.
        assert!(tokio::net::TcpStream::connect(listen).await.is_err());
        format!("{err:#}")
    };

    // 65 public services and `ask`: more skills than a card holds.
    let err = refused(public_services(65, "Does one thing")).await;
    assert!(err.contains("the public Agent Card"), "{err}");
    assert!(err.contains("more than 64 skills"), "{err}");
    // Few services, but too much text for 32 KiB.
    let err = refused(public_services(10, &"x".repeat(4000))).await;
    assert!(err.contains("the public Agent Card"), "{err}");
    assert!(err.contains("card larger than 32768 bytes"), "{err}");
    // A scope's extended card is checked too, and named.
    let project = |n: usize| {
        format!(
            "[[services]]\nid = \"project-{n}\"\nname = \"Project {n}\"\n\
             description = \"For demo\"\ntags = [\"demo\"]\n\
             visibility = {{ project = [\"demo\"] }}\n"
        )
    };
    let inventory =
        public_services(40, "Does one thing") + &(0..30).map(project).collect::<String>();
    let err = refused(inventory).await;
    assert!(
        err.contains("the extended Agent Card of scope project:demo"),
        "{err}"
    );
    assert!(err.contains("more than 64 skills"), "{err}");
    // An announce record lists at most 32 public capabilities.
    let err = refused(public_services(33, "Does one thing")).await;
    assert!(err.contains("32 public capabilities"), "{err}");

    // With an inventory that fits, the same agent starts: it held nothing.
    std::fs::write(
        dir.path().join(INVENTORY_FILE),
        public_services(32, "Does one thing"),
    )
    .unwrap();
    let node = Node::spawn(cfg.clone()).await.expect("starts");
    assert_eq!(node.cards().public().card.skills.len(), 33);
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn records_are_renewed_before_they_expire() {
    let b = spawn_node(WorkerCfg::Echo).await;
    let records = b.node.records();
    let (contact_json, announce_json) = records.current();
    let contact = Signed::<ContactRecord>::from_json(&contact_json)
        .unwrap()
        .doc;
    let announce = Signed::<AnnounceRecord>::from_json(&announce_json)
        .unwrap()
        .doc;
    assert_eq!(contact.agent_id, b.id());
    assert_eq!(contact.iroh.endpoint_id, b.id());
    assert!(!contact.iroh.direct_addrs.is_empty(), "{contact:?}");
    assert_eq!(announce.agent_id, b.id());
    // Each issued with the whole lifetime.
    for (issued_at, expires_at) in [
        (&contact.issued_at, &contact.expires_at),
        (&announce.issued_at, &announce.expires_at),
    ] {
        assert_eq!(parse(expires_at) - parse(issued_at), RECORD_TTL_S);
    }

    // Now nothing changed, and nothing is issued again.
    assert!(!records.renew(now_unix()).unwrap());
    assert_eq!(
        records.current(),
        (contact_json.clone(), announce_json.clone())
    );

    // With less than RECORD_RENEW_BEFORE_S left, both are issued again, with
    // the next seq and the whole lifetime from then.
    let later =
        parse(&contact.expires_at).min(parse(&announce.expires_at)) - RECORD_RENEW_BEFORE_S + 1;
    assert!(records.renew(later).unwrap());
    let (renewed_contact, renewed_announce) = records.current();
    let renewed_contact = Signed::<ContactRecord>::from_json(&renewed_contact)
        .unwrap()
        .doc;
    let renewed_announce = Signed::<AnnounceRecord>::from_json(&renewed_announce)
        .unwrap()
        .doc;
    assert_eq!(renewed_contact.seq, contact.seq + 1);
    assert_eq!(renewed_announce.seq, announce.seq + 1);
    for record in [
        (&renewed_contact.issued_at, &renewed_contact.expires_at),
        (&renewed_announce.issued_at, &renewed_announce.expires_at),
    ] {
        assert_eq!(parse(record.0), later);
        assert_eq!(parse(record.1), later + RECORD_TTL_S);
    }
    assert_eq!(renewed_contact.iroh, contact.iroh);
    assert_eq!(renewed_announce.agent_card_hash, announce.agent_card_hash);
    // The store has them too, for the next start.
    let stored = b.node.store.own_record("contact").unwrap().unwrap();
    assert_eq!(stored.0, renewed_contact.seq);
    // And then nothing is due again.
    assert!(!records.renew(later + 60).unwrap());
}

#[tokio::test]
async fn card_hash_is_stable_across_restarts() {
    let b = spawn_with_inventory(INVENTORY).await;
    let public = b.node.cards().public().hash.clone();
    let task = Scope::Task("demo".to_owned());
    let extended = b.node.cards().for_scope(&task).hash.clone();
    assert_ne!(public, extended);

    let b = restart(b).await;
    assert_eq!(b.node.cards().public().hash, public);
    assert_eq!(b.node.cards().for_scope(&task).hash, extended);
    let a = spawn_node(WorkerCfg::Echo).await;
    let res = a.get(&b, WELL_KNOWN, None).await;
    assert_eq!(parse_and_verify(&res.body, &b.id()).unwrap().hash, public);
}

#[tokio::test]
async fn extended_card_follows_the_grant() {
    let b = spawn_with_inventory(INVENTORY).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let ask = ["skill:ask"];
    for (ops, project, scope, skills) in [
        (
            [TaskSend],
            "demo",
            Scope::Task("demo".to_owned()),
            vec!["ask", "ocr-metal-v1", "repo-review", "repo-fix"],
        ),
        (
            [TaskRead],
            "demo",
            Scope::Project("demo".to_owned()),
            vec!["ask", "ocr-metal-v1", "repo-review"],
        ),
        (
            [TaskSend],
            "other",
            Scope::Task("other".to_owned()),
            vec!["ask", "ocr-metal-v1"],
        ),
    ] {
        let g = grant_in(&b, &a, project, &ops, &ask);
        let expected = b.node.cards().for_scope(&scope);
        let res = a.rpc(&b, Some(&g), "GetExtendedAgentCard", json!({})).await;
        assert_eq!(res.status, 200, "{}", res.json);
        let card = &res.json["result"];
        assert_eq!(skill_ids(card), skills, "{scope:?}");
        let verified = verify_card(card, &b.id()).unwrap();
        assert_eq!(verified.form, CanonicalForm::Raw, "{scope:?}");
        assert_eq!(verified.hash, expected.hash, "{scope:?}");

        // The same card over HTTP+JSON, at both of its paths.
        for path in ["/rest/extendedAgentCard", "/rest/agent-card/extended"] {
            let res = a.get(&b, path, Some(&g)).await;
            assert_eq!(res.status, 200, "{path}: {}", res.json);
            assert_eq!(verify_card(&res.json, &b.id()).unwrap().hash, expected.hash);
        }
    }
    // Without services of its own, a scope gets the public card.
    let other = b.node.cards().for_scope(&Scope::Task("other".to_owned()));
    assert_eq!(other.hash, b.node.cards().public().hash);

    // The client discovery reads peers' cards with gets the same, and keeps
    // each card in its own context.
    let peer_cards = PeerCards::new(a.node.peer_http.clone(), Arc::clone(&a.node.store));
    let g = grant_in(&b, &a, "demo", &[TaskSend], &ask);
    let extended = peer_cards
        .extended(&b.id(), b.node.addr(), &g)
        .await
        .unwrap();
    let expected = b.node.cards().for_scope(&Scope::Task("demo".to_owned()));
    assert_eq!(extended.hash, expected.hash);
    let context = format!("grant:{}/{}", g.grant.issuer, g.grant.grant_id);
    let peer = b.id().to_string();
    assert_eq!(
        a.node.store.peer_card(&peer, &context).unwrap(),
        Some((expected.json.clone(), expected.hash.clone()))
    );
    let public = peer_cards.public(&b.id(), b.node.addr()).await.unwrap();
    assert_eq!(public.hash, b.node.cards().public().hash);
    assert_eq!(
        a.node
            .store
            .peer_card(&peer, "public")
            .unwrap()
            .map(|(_, hash)| hash),
        Some(public.hash)
    );
    // A grant the peer does not take gets no card.
    let foreign = grant_in(&a, &a, "demo", &[TaskSend], &ask);
    assert_eq!(
        peer_cards
            .extended(&b.id(), b.node.addr(), &foreign)
            .await
            .unwrap_err(),
        CardFetchError::Refused(401)
    );
}

#[tokio::test]
async fn extended_card_without_grant_is_401() {
    let b = spawn_with_inventory(INVENTORY).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let res = a.rpc(&b, None, "GetExtendedAgentCard", json!({})).await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert!(res.header("www-authenticate").starts_with("Bearer"));
    let res = a.get(&b, "/rest/extendedAgentCard", None).await;
    assert_eq!(res.status, 401, "{}", res.json);

    // An expired grant is no grant.
    let expired = grant(&b, &a, &[TaskSend], -3600);
    let res = a
        .rpc(&b, Some(&expired), "GetExtendedAgentCard", json!({}))
        .await;
    assert_eq!(res.status, 401, "{}", res.json);
    assert!(res.header("www-authenticate").contains("expired"));
}

#[tokio::test]
async fn blob_only_grant_sees_only_the_public_skills() {
    let b = spawn_with_inventory(INVENTORY).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let public = b.node.cards().public();
    for (ops, resources) in [
        // Valid, but no task operation.
        (vec![BlobFetch], vec!["skill:ask"]),
        // Task operations, on something other than `skill:ask`.
        (vec![TaskSend, TaskRead], vec!["blob:*"]),
    ] {
        let g = grant_in(&b, &a, "demo", &ops, &resources);
        let res = a.rpc(&b, Some(&g), "GetExtendedAgentCard", json!({})).await;
        assert_eq!(res.status, 200, "{}", res.json);
        let card = &res.json["result"];
        assert_eq!(skill_ids(card), ["ask", "ocr-metal-v1"], "{ops:?}");
        assert_eq!(verify_card(card, &b.id()).unwrap().hash, public.hash);
        // And it still cannot ask anything.
        let res = a
            .rpc(
                &b,
                Some(&g),
                "SendMessage",
                common::message_params("hola", None),
            )
            .await;
        assert_eq!(res.status, 403, "{}", res.json);
    }
}

#[tokio::test]
async fn scope_header_sent_by_a_client_is_replaced() {
    let b = spawn_with_inventory(INVENTORY).await;
    let a = spawn_node(WorkerCfg::Echo).await;
    let g = grant_in(&b, &a, "other", &[TaskSend], &["skill:ask"]);
    let public = b.node.cards().public();
    let demo = b.node.cards().for_scope(&Scope::Task("demo".to_owned()));
    assert_ne!(demo.hash, public.hash);

    let forged = [(SCOPE_HEADER, "task:demo")];
    let res = a
        .rpc_with(&b, Some(&g), "GetExtendedAgentCard", json!({}), &forged)
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    let card = &res.json["result"];
    assert_eq!(skill_ids(card), ["ask", "ocr-metal-v1"]);
    assert_eq!(verify_card(card, &b.id()).unwrap().hash, public.hash);

    let bearer = common::bearer(&g);
    let res = a
        .request(
            &b,
            "GET",
            "/rest/extendedAgentCard",
            &[
                ("a2a-version", "1.0"),
                ("authorization", &bearer),
                (SCOPE_HEADER, "task:demo"),
            ],
            None,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(verify_card(&res.json, &b.id()).unwrap().hash, public.hash);
}

#[tokio::test]
async fn gateway_refuses_a_tampered_peer_card() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let key = AgentKey::generate();
    let mut card = signed_card_of(&key);
    card["description"] = json!("A better agent than it was");
    let _stub = stub_peer(&a, "tampered", &key, card_app(card)).await;

    assert_refused(
        &every_card_route(&a, "tampered").await,
        "bad card signature",
    );
    // Nothing of it is kept.
    assert_eq!(
        a.node
            .store
            .peer_card(&key.id().to_string(), "public")
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn gateway_refuses_a_card_signed_by_another_agent() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let key = AgentKey::generate();
    let card = signed_card_of(&AgentKey::generate());
    let _stub = stub_peer(&a, "impostor", &key, card_app(card)).await;

    assert_refused(
        &every_card_route(&a, "impostor").await,
        "card signed by another key",
    );
}

#[tokio::test]
async fn gateway_refuses_a_card_over_32_kib() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let key = AgentKey::generate();
    // A card the peer signed, padded past 32 KiB, still valid JSON.
    let mut card = signed_card_of(&key);
    card["skills"][0]["examples"] = json!(vec!["x".repeat(4000); 9]);
    assert!(serde_json::to_vec(&card).unwrap().len() > 32 * 1024);
    let _stub = stub_peer(&a, "large", &key, card_app(card)).await;
    assert_refused(
        &every_card_route(&a, "large").await[..2],
        "card larger than 32768 bytes",
    );

    // A card that never ends is cut at the limit, not read to its end.
    let endless_key = AgentKey::generate();
    let endless = axum::Router::new().route(
        WELL_KNOWN,
        get(|| async {
            let chunk = Bytes::from(vec![b' '; 1024]);
            let chunks =
                futures::stream::repeat_with(move || Ok::<_, std::io::Error>(chunk.clone()));
            Body::from_stream(chunks)
        }),
    );
    let _endless = stub_peer(&a, "endless", &endless_key, endless).await;
    let res = tokio::time::timeout(
        Duration::from_secs(60),
        gateway_request(
            &a.gateway_url(),
            "GET",
            &format!("/peers/endless{WELL_KNOWN}"),
            &[],
            None,
        ),
    )
    .await
    .expect("answered without reading it all");
    assert_eq!(res.status, 502, "{}", res.json);
    assert_eq!(
        res.json,
        json!({"error": "peer card invalid: card larger than 32768 bytes"})
    );
}

#[tokio::test]
async fn gateway_serves_the_signed_card_for_independent_tools() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_with_inventory(INVENTORY).await;
    a.add_contact("bob", &b);
    let gw = a.gateway_url();

    let res = gateway_request(&gw, "GET", "/peers/bob/signed-agent-card.json", &[], None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.header("content-type"), "application/json");
    assert!(res.body.is_ascii());
    let verified = parse_and_verify(&res.body, &b.id()).unwrap();
    let public = b.node.cards().public();
    assert_eq!(verified.hash, public.hash);
    // The peer's own card, signatures and Iroh interfaces included.
    assert_eq!(res.json, public.json);
    assert!(res.json["signatures"].is_array());
    let url = res.json["supportedInterfaces"][0]["url"].as_str().unwrap();
    assert!(url.starts_with("iroh://"), "{url}");
    // The gateway kept it, as the peer's public card.
    assert_eq!(
        a.node
            .store
            .peer_card(&b.id().to_string(), "public")
            .unwrap(),
        Some((public.json.clone(), public.hash.clone()))
    );

    // The view stays unsigned, through the gateway.
    let res = gateway_request(&gw, "GET", &format!("/peers/bob{WELL_KNOWN}"), &[], None).await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert!(res.json.get("signatures").is_none());
    assert_eq!(
        res.json,
        serde_json::to_value(gateway_view(&public.card, &gw, &b.id())).unwrap()
    );
}

#[tokio::test]
async fn extended_card_through_the_gateway_is_a_gateway_view() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_with_inventory(INVENTORY).await;
    a.add_contact("bob", &b);
    a.hold(&grant(&b, &a, &[TaskSend, TaskRead], 3600));
    let gw = a.gateway_url();
    let extended = b.node.cards().for_scope(&Scope::Task("demo".to_owned()));
    let view = serde_json::to_value(gateway_view(&extended.card, &gw, &b.id())).unwrap();

    let call = rpc_request("GetExtendedAgentCard", json!({}));
    let res = gateway_request(
        &gw,
        "POST",
        "/peers/bob/jsonrpc",
        &[("a2a-version", "1.0")],
        Some(&call),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.json);
    assert_eq!(res.header("x-agentnet-card-hash"), extended.hash.as_str());
    let card = &res.json["result"];
    assert_eq!(card, &view);
    assert!(card.get("signatures").is_none());
    assert!(card.get("securitySchemes").is_none());
    assert!(skill_ids(card).contains(&"repo-review"));
    assert_eq!(
        card["supportedInterfaces"][0]["url"],
        format!("{gw}/peers/{}/jsonrpc", b.id())
    );

    for path in ["extendedAgentCard", "agent-card/extended"] {
        let res = gateway_request(
            &gw,
            "GET",
            &format!("/peers/bob/rest/{path}"),
            &[("a2a-version", "1.0")],
            None,
        )
        .await;
        assert_eq!(res.status, 200, "{path}: {}", res.json);
        assert_eq!(res.header("x-agentnet-card-hash"), extended.hash.as_str());
        assert_eq!(res.json, view, "{path}");
        assert!(res.body.is_ascii());
    }
}

#[tokio::test]
async fn announce_names_the_public_card_hash_and_its_seq_moves_only_on_change() {
    let announce_of = |node: &TestNode| {
        let (_, announce) = node.node.records().current();
        (
            Signed::<AnnounceRecord>::from_json(&announce).unwrap().doc,
            announce,
        )
    };
    let b = spawn_with_inventory(INVENTORY).await;
    let (announce, first) = announce_of(&b);
    let public = b.node.cards().public().hash.clone();
    assert_eq!(announce.agent_id, b.id());
    assert_eq!(announce.agent_card_hash, public);
    // The capabilities of the public services only.
    assert_eq!(announce.capabilities, ["vision.ocr"]);
    assert_eq!(announce.discovery_alpn, "agentnet-discovery/0");
    assert_eq!(announce.seq, 1);

    // Started again with the same inventory: the same announce.
    let b = restart(b).await;
    let (again, bytes) = announce_of(&b);
    assert_eq!(bytes, first);
    assert_eq!(again.seq, 1);

    // A new public service: another card, and the next seq.
    let more = format!(
        "{INVENTORY}\n[[services]]\nid = \"translate\"\nname = \"Translation\"\n\
         description = \"Translates manuals\"\ncapabilities = [\"text.translation\"]\n\
         visibility = \"public\"\n"
    );
    std::fs::write(b.dir.path().join(INVENTORY_FILE), more).unwrap();
    let b = restart(b).await;
    let (changed, _) = announce_of(&b);
    assert_eq!(changed.seq, 2);
    assert_ne!(changed.agent_card_hash, public);
    assert_eq!(changed.agent_card_hash, b.node.cards().public().hash);
    assert_eq!(changed.capabilities, ["vision.ocr", "text.translation"]);
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

/// The JSON and the `verified:` line `pals card show` printed.
fn shown(stdout: &str) -> (Value, String) {
    let (json, verified) = stdout.trim_end().rsplit_once('\n').expect("two parts");
    (
        serde_json::from_str(json).expect("a JSON card"),
        verified.to_owned(),
    )
}

#[tokio::test]
async fn card_show_prints_a_verified_card() {
    let a = spawn_node(WorkerCfg::Echo).await;
    let b = spawn_with_inventory(INVENTORY).await;
    a.add_contact("bob", &b);
    a.hold(&grant(&b, &a, &[TaskSend, TaskRead], 3600));
    // The CLI finds the running daemon by its configuration.
    let home = a.dir.path().to_owned();
    let listen = a.gateway_url().replace("http://", "");
    std::fs::write(
        home.join("config.toml"),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();

    // This agent's own card.
    let (code, stdout, stderr) = pals(&home, &["card", "show"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (card, verified) = shown(&stdout);
    let own = a.node.cards().public();
    assert_eq!(card, own.json);
    assert_eq!(verified, format!("verified: {} {}", a.id(), own.hash));

    // A peer's, as it signed it.
    let (code, stdout, stderr) = pals(&home, &["card", "show", "bob"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (card, verified) = shown(&stdout);
    let public = b.node.cards().public();
    assert_eq!(card, public.json);
    assert_eq!(verified, format!("verified: {} {}", b.id(), public.hash));

    // A peer's extended card, as the gateway checked it.
    let (code, stdout, stderr) = pals(&home, &["card", "show", "bob", "--extended"]).await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (card, verified) = shown(&stdout);
    let extended = b.node.cards().for_scope(&Scope::Task("demo".to_owned()));
    assert_eq!(
        card,
        serde_json::to_value(gateway_view(&extended.card, &a.gateway_url(), &b.id())).unwrap()
    );
    assert_eq!(verified, format!("verified: {} {}", b.id(), extended.hash));
}
