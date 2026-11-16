//! The store's schema versions: a hito 1 database (version 1) upgrades to
//! version 2 in one transaction and keeps all its data, ends with the same
//! schema as a new database, and a newer version is refused.

use std::collections::BTreeMap;
use std::path::Path;

use a2a::{Message, Part, Role, Task, TaskState, TaskStatus};
use pals_core::grant::{GRANT_SCHEMA, Grant, Operation, SignedGrant, format_timestamp};
use pals_core::identity::{AgentId, AgentKey};
use pals_core::store::{Contact, OUTBOX_PENDING, OutboxRow, Store};
use rusqlite::Connection;
use rusqlite::types::Value;
use serde_json::json;

/// The schema as hito 1 released it, at version 1.
const SCHEMA_V1: &str = include_str!("fixtures/schema-v1.sql");
/// 2026-11-14T12:00:00Z.
const NOW: i64 = 1_794_657_600;
const HOUR: i64 = 3600;

/// The tables of version 1.
const V1_TABLES: [&str; 9] = [
    "contacts",
    "dedup",
    "events",
    "grants_held",
    "grants_issued",
    "outbox",
    "revocations",
    "task_owners",
    "tasks",
];
/// The tables version 2 adds.
const V2_TABLES: [&str; 10] = [
    "a2a_contacts",
    "benchmark_reports",
    "inventory_cache",
    "inventory_observations",
    "offers_issued",
    "offers_received",
    "own_records",
    "peer_cards",
    "peer_records",
    "view_revisions",
];

/// A version 1 database at `path`, in WAL mode, with no rows.
fn create_v1(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    conn.execute_batch(SCHEMA_V1).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    conn
}

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

/// Every schema object as `(type, name, table, SQL)`, the SQL without
/// comments and with its whitespace collapsed, by type and name.
fn schema_of(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
    let normalize = |sql: String| {
        sql.lines()
            .map(|line| line.split("--").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, Option<String>>(3)?.map(normalize),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The names of the tables in `schema`, sorted.
fn tables_in(schema: &[(String, String, String, Option<String>)]) -> Vec<&str> {
    schema
        .iter()
        .filter(|(kind, ..)| kind == "table")
        .map(|(_, name, ..)| name.as_str())
        .collect()
}

/// Every row of each of `tables`, every column, in rowid order.
fn rows_of(conn: &Connection, tables: &[&str]) -> BTreeMap<String, Vec<Vec<Value>>> {
    tables
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..columns).map(|i| row.get::<_, Value>(i)).collect()
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            ((*table).to_owned(), rows)
        })
        .collect()
}

fn grant(issuer: &AgentKey, grant_id: &str, subject: AgentId, audience: AgentId) -> SignedGrant {
    let grant = Grant {
        schema: GRANT_SCHEMA.to_owned(),
        grant_id: grant_id.to_owned(),
        issuer: issuer.id(),
        subject,
        audience,
        project: "demo".to_owned(),
        resources: vec!["skill:ask".to_owned()],
        operations: vec![Operation::TaskSend],
        issued_at: format_timestamp(NOW - HOUR).unwrap(),
        expires_at: format_timestamp(NOW + HOUR).unwrap(),
    };
    SignedGrant::issue(issuer, grant).unwrap()
}

#[test]
fn version_1_database_upgrades_to_2_keeping_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let source = dir.path().join("source.db");
    let me = AgentKey::generate();
    let peer = AgentKey::generate();

    let contact = Contact {
        name: "peer".to_owned(),
        agent_id: peer.id(),
        ticket: Some("endpoint-ticket".to_owned()),
        added_at: NOW,
    };
    let issued = grant(&me, "g-issued", peer.id(), me.id());
    let held = grant(&peer, "g-held", me.id(), peer.id());
    let task = Task {
        id: "t-1".to_owned(),
        context_id: "ctx-1".to_owned(),
        status: TaskStatus {
            state: TaskState::Completed,
            message: Some(Message::new(Role::Agent, vec![Part::text("done")])),
            timestamp: None,
        },
        artifacts: None,
        history: Some(vec![Message::new(Role::User, vec![Part::text("hi")])]),
        metadata: None,
    };
    let outbox = OutboxRow {
        message_id: "m-1".to_owned(),
        peer: peer.id().to_string(),
        request_json: r#"{"message":{"messageId":"m-1"}}"#.to_owned(),
        task_id: None,
        state: OUTBOX_PENDING.to_owned(),
        attempts: 1,
        created_at: NOW,
        updated_at: NOW,
    };

    // The rows, written by the store's own methods into a scratch database,
    // then copied as they are into a version 1 database: the version 1
    // tables are the same in version 2.
    let events = {
        let store = Store::open(&source, me.id()).unwrap();
        store.contact_put(&contact).unwrap();
        store.grant_issued_put(&issued).unwrap();
        store.grant_held_put(&held).unwrap();
        assert!(store.revoke("g-issued", NOW).unwrap());
        store.task_create(&task, NOW).unwrap();
        store.task_owner_put("t-1", &peer.id()).unwrap();
        let events: Vec<_> = (0..3)
            .map(|i| {
                store
                    .event_append(&me, "task.accepted", &json!({"task_id": "t-1"}), NOW + i)
                    .unwrap()
            })
            .collect();
        store.outbox_insert(&outbox).unwrap();
        events
    };
    let conn = create_v1(&path);
    conn.execute("ATTACH DATABASE ?1 AS source", [source.to_str().unwrap()])
        .unwrap();
    for table in V1_TABLES {
        conn.execute(
            &format!("INSERT INTO main.{table} SELECT * FROM source.{table}"),
            [],
        )
        .unwrap();
    }
    conn.execute_batch("DETACH DATABASE source").unwrap();
    let v1_rows = rows_of(&conn, &V1_TABLES);
    let count = |table: &str| v1_rows[table].len();
    assert_eq!(
        V1_TABLES.map(count),
        [1, 0, 3, 1, 1, 1, 1, 1, 1],
        "{V1_TABLES:?}"
    );
    let v1_schema = schema_of(&conn);
    assert_eq!(tables_in(&v1_schema), V1_TABLES);
    drop(conn);

    let mut snapshots = Vec::new();
    for _ in 0..2 {
        let store = Store::open(&path, me.id()).unwrap();
        assert_eq!(store.contacts().unwrap(), vec![contact.clone()]);
        assert_eq!(store.grants_issued().unwrap(), vec![issued.clone()]);
        assert!(store.is_revoked("g-issued").unwrap());
        assert_eq!(store.grants_held().unwrap(), vec![held.clone()]);
        assert_eq!(store.task_get("t-1").unwrap(), Some(task.clone()));
        assert_eq!(store.task_owner("t-1").unwrap(), Some(peer.id()));
        assert_eq!(store.events_verify(&me.id()), Ok(3));
        assert_eq!(store.events_since(&me.id(), 0).unwrap(), events);
        assert_eq!(store.outbox_all().unwrap(), vec![outbox.clone()]);
        drop(store);

        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), 2);
        // The version 1 rows, column by column, as they were.
        assert_eq!(rows_of(&conn, &V1_TABLES), v1_rows);
        // The new tables, empty.
        assert!(
            rows_of(&conn, &V2_TABLES).values().all(Vec::is_empty),
            "{:?}",
            rows_of(&conn, &V2_TABLES)
        );
        // Every version 1 object, unchanged, and the new tables.
        let schema = schema_of(&conn);
        for object in &v1_schema {
            assert!(schema.contains(object), "{object:?}");
        }
        let mut tables = [V1_TABLES.as_slice(), V2_TABLES.as_slice()].concat();
        tables.sort_unstable();
        assert_eq!(tables_in(&schema), tables);
        snapshots.push((
            schema,
            rows_of(&conn, &[V1_TABLES.as_slice(), &V2_TABLES].concat()),
        ));
    }
    // Opening the upgraded database again changes nothing.
    assert_eq!(snapshots[0], snapshots[1]);
}

#[test]
fn migrated_schema_equals_fresh_schema() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("fresh.db");
    let migrated = dir.path().join("migrated.db");
    let owner = AgentKey::generate().id();

    drop(Store::open(&fresh, owner).unwrap());
    drop(create_v1(&migrated));
    drop(Store::open(&migrated, owner).unwrap());

    let fresh = Connection::open(&fresh).unwrap();
    let migrated = Connection::open(&migrated).unwrap();
    assert_eq!(user_version(&fresh), 2);
    assert_eq!(user_version(&migrated), 2);
    let schema = schema_of(&fresh);
    assert_eq!(schema_of(&migrated), schema);

    // Version 2 is version 1 and the new tables, all STRICT, and the index
    // the purge of issued offers uses.
    let mut tables = [V1_TABLES.as_slice(), V2_TABLES.as_slice()].concat();
    tables.sort_unstable();
    assert_eq!(tables_in(&schema), tables);
    for (kind, name, _, sql) in &schema {
        if kind == "table" {
            let sql = sql.as_deref().unwrap();
            assert!(sql.ends_with(") STRICT"), "{name}: {sql}");
        }
    }
    assert!(
        schema.iter().any(|(kind, name, table, sql)| kind == "index"
            && name == "offers_issued_by_expires_at"
            && table == "offers_issued"
            && sql
                .as_deref()
                .is_some_and(|sql| sql.ends_with("(expires_at)"))),
        "{schema:#?}"
    );
}

#[test]
fn upgrade_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let owner = AgentKey::generate().id();
    let peer = AgentKey::generate().id();

    let conn = create_v1(&path);
    conn.execute(
        "INSERT INTO contacts (name, agent_id, ticket, added_at) VALUES ('peer', ?1, NULL, ?2)",
        rusqlite::params![peer.to_string(), NOW],
    )
    .unwrap();
    // A table the upgrade creates, already there with another shape. The
    // upgrade creates other tables before it.
    conn.execute_batch(
        "CREATE TABLE peer_records (note TEXT);
         INSERT INTO peer_records VALUES ('left by hand');",
    )
    .unwrap();
    let schema = schema_of(&conn);
    let rows = rows_of(&conn, &[V1_TABLES.as_slice(), &["peer_records"]].concat());
    drop(conn);

    let err = Store::open(&path, owner)
        .err()
        .expect("the conflicting table makes the upgrade fail");
    assert!(
        format!("{err:#}").contains("peer_records already exists"),
        "{err:#}"
    );

    // Still version 1, with nothing created and nothing lost.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(user_version(&conn), 1);
    assert_eq!(schema_of(&conn), schema);
    assert!(!tables_in(&schema).contains(&"a2a_contacts"));
    assert_eq!(
        rows_of(&conn, &[V1_TABLES.as_slice(), &["peer_records"]].concat()),
        rows
    );

    // Without the conflict, the same database upgrades.
    conn.execute_batch("DROP TABLE peer_records").unwrap();
    drop(conn);
    let store = Store::open(&path, owner).unwrap();
    assert_eq!(
        store.contacts().unwrap(),
        vec![Contact {
            name: "peer".to_owned(),
            agent_id: peer,
            ticket: None,
            added_at: NOW,
        }]
    );
    drop(store);
    assert_eq!(user_version(&Connection::open(&path).unwrap()), 2);
}

#[test]
fn version_3_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pals.db");
    let owner = AgentKey::generate().id();
    drop(Store::open(&path, owner).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    let schema = schema_of(&conn);
    drop(conn);

    let err = Store::open(&path, owner)
        .err()
        .expect("a newer schema version is refused");
    assert!(
        format!("{err:#}").contains("unsupported schema version 3"),
        "{err:#}"
    );
    // And left as it was.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(user_version(&conn), 3);
    assert_eq!(schema_of(&conn), schema);
}
