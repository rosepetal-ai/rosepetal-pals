//! The SQLite store: contacts, issued and held grants, revocations, and the
//! schema version.

use pals_core::grant::{
    CLOCK_SKEW_S, GRANT_SCHEMA, Grant, Operation, SignedGrant, format_timestamp,
};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::{Contact, Store};

/// 2026-11-14T12:00:00Z.
const NOW: i64 = 1_794_657_600;
const HOUR: i64 = 3600;

fn ts(unix_s: i64) -> String {
    format_timestamp(unix_s).unwrap()
}

/// A grant of `issuer` valid from an hour before [`NOW`] to an hour after.
fn grant(issuer: &AgentKey, grant_id: &str, subject: AgentId, audience: AgentId) -> Grant {
    Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: grant_id.to_owned(),
        issuer: issuer.id(),
        subject,
        audience,
        project: "demo".to_owned(),
        resources: vec!["skill:ask".to_owned()],
        operations: vec![Operation::TaskSend],
        issued_at: ts(NOW - HOUR),
        expires_at: ts(NOW + HOUR),
    }
}

fn sign(issuer: &AgentKey, grant: Grant) -> SignedGrant {
    SignedGrant::issue(issuer, grant).unwrap()
}

fn ids(grants: &[SignedGrant]) -> Vec<&str> {
    grants.iter().map(|g| g.grant.grant_id.as_str()).collect()
}

#[test]
fn contact_put_get_and_replace_by_name() {
    let store = Store::open_in_memory().unwrap();
    let first = AgentKey::generate().id();
    let second = AgentKey::generate().id();

    let alice = Contact {
        name: "alice".to_owned(),
        agent_id: first,
        ticket: Some("endpoint-ticket".to_owned()),
        added_at: NOW,
    };
    store.contact_put(&alice).unwrap();
    assert_eq!(store.contact_by_name("alice").unwrap(), Some(alice.clone()));
    assert_eq!(store.contact_by_id(&first).unwrap(), Some(alice.clone()));
    assert_eq!(store.contact_by_name("bob").unwrap(), None);
    assert_eq!(store.contact_by_id(&second).unwrap(), None);

    // The same name replaces the whole contact.
    let replaced = Contact {
        agent_id: second,
        ticket: None,
        added_at: NOW + 1,
        ..alice.clone()
    };
    store.contact_put(&replaced).unwrap();
    assert_eq!(
        store.contact_by_name("alice").unwrap(),
        Some(replaced.clone())
    );
    assert_eq!(
        store.contact_by_id(&second).unwrap(),
        Some(replaced.clone())
    );
    assert_eq!(store.contact_by_id(&first).unwrap(), None);

    let bob = Contact {
        name: "bob".to_owned(),
        agent_id: first,
        ticket: None,
        added_at: NOW + 2,
    };
    store.contact_put(&bob).unwrap();
    assert_eq!(store.contact_by_id(&first).unwrap(), Some(bob.clone()));
    assert_eq!(store.contacts().unwrap(), vec![replaced, bob]);
}

#[test]
fn held_grants_exclude_expired_and_sort_newest_first() {
    let store = Store::open_in_memory().unwrap();
    let me = AgentKey::generate().id();
    let peer = AgentKey::generate();
    let other_peer = AgentKey::generate();
    // Grants a peer issued to this agent, to be used against that peer.
    let held = |issuer: &AgentKey, grant_id: &str, issued_at: i64, expires_at: i64| {
        let g = sign(
            issuer,
            Grant {
                issued_at: ts(issued_at),
                expires_at: ts(expires_at),
                ..grant(issuer, grant_id, me, issuer.id())
            },
        );
        store.grant_held_put(&g).unwrap();
        g
    };

    let older = held(&peer, "g-older", NOW - 3 * HOUR, NOW + HOUR);
    let newest = held(&peer, "g-newest", NOW - HOUR, NOW + HOUR);
    // Expiry uses the same clock skew as SignedGrant::check.
    let in_skew = held(&peer, "g-in-skew", NOW - 2 * HOUR, NOW - CLOCK_SKEW_S + 1);
    held(&peer, "g-expired", NOW - 4 * HOUR, NOW - CLOCK_SKEW_S);
    held(&peer, "g-long-expired", NOW - 48 * HOUR, NOW - 24 * HOUR);
    held(&other_peer, "g-other-audience", NOW - HOUR / 2, NOW + HOUR);
    // Storing the same grant again keeps one copy.
    store.grant_held_put(&newest).unwrap();

    let got = store.grants_held_for(&peer.id(), NOW).unwrap();
    assert_eq!(ids(&got), ["g-newest", "g-in-skew", "g-older"]);
    assert_eq!(got, vec![newest, in_skew, older]);
    assert_eq!(
        ids(&store.grants_held_for(&other_peer.id(), NOW).unwrap()),
        ["g-other-audience"]
    );
    assert!(store.grants_held_for(&me, NOW).unwrap().is_empty());
    // Held grants are not grants this agent issued.
    assert!(store.grants_issued().unwrap().is_empty());
    assert_eq!(store.grant_issued("g-newest").unwrap(), None);
}

#[test]
fn covering_grant_matches_subject_operation_and_resource() {
    let store = Store::open_in_memory().unwrap();
    let me = AgentKey::generate();
    let alice = AgentKey::generate().id();
    let bob = AgentKey::generate().id();
    let stranger = AgentKey::generate().id();
    let issue = |grant_id: &str, subject: AgentId, operations: Vec<Operation>, resource: &str| {
        let g = sign(
            &me,
            Grant {
                operations,
                resources: vec![resource.to_owned()],
                ..grant(&me, grant_id, subject, me.id())
            },
        );
        store.grant_issued_put(&g).unwrap();
        g
    };
    let covering = |subject: &AgentId, op: Operation, resource: &str| {
        store
            .grant_issued_covering(subject, op, resource, NOW)
            .unwrap()
    };

    let blob = issue("g-blob", alice, vec![Operation::BlobFetch], "blob:ab12");
    let ask = issue("g-ask", alice, vec![Operation::TaskSend], "skill:ask");
    let any_blob = issue(
        "g-any-blob",
        bob,
        vec![Operation::BlobFetch, Operation::TaskRead],
        "blob:*",
    );

    assert_eq!(
        covering(&alice, Operation::BlobFetch, "blob:ab12"),
        Some(blob.clone())
    );
    assert_eq!(
        covering(&alice, Operation::TaskSend, "skill:ask"),
        Some(ask)
    );
    // A resource ending in `*` covers every resource with that prefix.
    assert_eq!(
        covering(&bob, Operation::BlobFetch, "blob:ff00"),
        Some(any_blob.clone())
    );
    assert_eq!(covering(&bob, Operation::TaskRead, "blob:"), Some(any_blob));

    // Another resource.
    assert_eq!(covering(&alice, Operation::BlobFetch, "blob:ab123"), None);
    assert_eq!(covering(&alice, Operation::BlobFetch, "blob:ab1"), None);
    assert_eq!(covering(&bob, Operation::BlobFetch, "skill:ask"), None);
    // Another operation.
    assert_eq!(covering(&alice, Operation::TaskSend, "blob:ab12"), None);
    assert_eq!(covering(&alice, Operation::TaskRead, "blob:ab12"), None);
    assert_eq!(covering(&bob, Operation::TaskCancel, "blob:ab12"), None);
    // Another subject.
    assert_eq!(covering(&stranger, Operation::BlobFetch, "blob:ab12"), None);
    assert_eq!(covering(&bob, Operation::TaskSend, "skill:ask"), None);

    assert_eq!(store.grant_issued("g-blob").unwrap(), Some(blob));
    assert_eq!(store.grant_issued("g-unknown").unwrap(), None);
    let mut all = store.grants_issued().unwrap();
    all.sort_by(|a, b| a.grant.grant_id.cmp(&b.grant.grant_id));
    assert_eq!(ids(&all), ["g-any-blob", "g-ask", "g-blob"]);
}

#[test]
fn covering_grant_ignores_revoked_and_expired() {
    let store = Store::open_in_memory().unwrap();
    let me = AgentKey::generate();
    let alice = AgentKey::generate().id();
    let blob_grant = |grant_id: &str, resource: &str, expires_at: i64| {
        let g = sign(
            &me,
            Grant {
                operations: vec![Operation::BlobFetch],
                resources: vec![resource.to_owned()],
                expires_at: ts(expires_at),
                ..grant(&me, grant_id, alice, me.id())
            },
        );
        store.grant_issued_put(&g).unwrap();
        g
    };
    let covering = |resource: &str| {
        store
            .grant_issued_covering(&alice, Operation::BlobFetch, resource, NOW)
            .unwrap()
    };

    let live = blob_grant("g-live", "blob:01", NOW + HOUR);
    assert_eq!(covering("blob:01"), Some(live.clone()));
    assert!(!store.is_revoked("g-live").unwrap());
    assert!(store.revoke("g-live", NOW).unwrap());
    assert!(store.is_revoked("g-live").unwrap());
    assert_eq!(covering("blob:01"), None);
    // Revoking again is harmless, and the revoked grant is still listed.
    assert!(store.revoke("g-live", NOW + 1).unwrap());
    assert_eq!(store.grant_issued("g-live").unwrap(), Some(live));

    // Another grant for the same resource still covers it.
    let renewed = blob_grant("g-renewed", "blob:01", NOW + HOUR);
    assert_eq!(covering("blob:01"), Some(renewed));

    // Expiry uses the same clock skew as SignedGrant::check.
    blob_grant("g-expired", "blob:02", NOW - CLOCK_SKEW_S);
    assert_eq!(covering("blob:02"), None);
    let in_skew = blob_grant("g-in-skew", "blob:03", NOW - CLOCK_SKEW_S + 1);
    assert_eq!(covering("blob:03"), Some(in_skew));
    blob_grant("g-long-expired", "blob:04", NOW - 24 * HOUR);
    assert_eq!(covering("blob:04"), None);
}

#[test]
fn revoke_unknown_grant_returns_false() {
    let store = Store::open_in_memory().unwrap();
    assert!(!store.revoke("g-unknown", NOW).unwrap());
    assert!(!store.is_revoked("g-unknown").unwrap());

    // Only grants this agent issued can be revoked here.
    let peer = AgentKey::generate();
    let me = AgentKey::generate().id();
    store
        .grant_held_put(&sign(&peer, grant(&peer, "g-held", me, peer.id())))
        .unwrap();
    assert!(!store.revoke("g-held", NOW).unwrap());
    assert!(!store.is_revoked("g-held").unwrap());
}

#[test]
fn reopen_keeps_data_and_schema_version_is_1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let me = AgentKey::generate();
    let peer = AgentKey::generate();
    let contact = Contact {
        name: "peer".to_owned(),
        agent_id: peer.id(),
        ticket: Some("endpoint-ticket".to_owned()),
        added_at: NOW,
    };
    let issued = sign(&me, grant(&me, "g-issued", peer.id(), me.id()));
    let held = sign(&peer, grant(&peer, "g-held", me.id(), peer.id()));
    {
        let store = Store::open(&path).unwrap();
        store.contact_put(&contact).unwrap();
        store.grant_issued_put(&issued).unwrap();
        store.grant_held_put(&held).unwrap();
        assert!(store.revoke("g-issued", NOW).unwrap());
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(store.contacts().unwrap(), vec![contact]);
    assert_eq!(store.grants_issued().unwrap(), vec![issued]);
    assert!(store.is_revoked("g-issued").unwrap());
    assert_eq!(store.grants_held_for(&peer.id(), NOW).unwrap(), vec![held]);
    drop(store);

    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let journal_mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        [
            "contacts",
            "dedup",
            "events",
            "grants_held",
            "grants_issued",
            "outbox",
            "revocations",
            "task_owners",
            "tasks",
        ]
    );
}

#[test]
fn opening_a_newer_schema_version_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    drop(Store::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();

    let err = Store::open(&path)
        .err()
        .expect("a newer schema version is refused");
    assert!(
        format!("{err:#}").contains("unsupported schema version"),
        "{err:#}"
    );
}
