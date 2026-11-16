-- PALS local store, schema version 2 (`PRAGMA user_version`), as a new
-- database gets it. A version 1 database gets the same schema from
-- `migrate-2.sql`, which repeats the tables version 2 added, at the end.
--
-- Agent ids are their 64 lowercase hex characters; instants in INTEGER columns
-- are Unix seconds. Grants are kept as their token: the other grant columns
-- only index it, and every grant read back is rebuilt from the token. Signed
-- documents are kept as the exact bytes their signature was verified over,
-- in BLOB columns.

CREATE TABLE contacts (
    name     TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL,
    ticket   TEXT,
    added_at INTEGER NOT NULL
) STRICT;
CREATE INDEX contacts_by_agent_id ON contacts (agent_id);

-- Grants this agent issued.
CREATE TABLE grants_issued (
    grant_id   TEXT PRIMARY KEY NOT NULL,
    issuer     TEXT NOT NULL,
    subject    TEXT NOT NULL,
    audience   TEXT NOT NULL,
    issued_at  INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    token      TEXT NOT NULL
) STRICT;
CREATE INDEX grants_issued_by_subject ON grants_issued (subject, expires_at);

-- Grants other agents issued to this agent. Grant ids are chosen by their
-- issuer, so they are unique per issuer only.
CREATE TABLE grants_held (
    issuer     TEXT NOT NULL,
    grant_id   TEXT NOT NULL,
    subject    TEXT NOT NULL,
    audience   TEXT NOT NULL,
    issued_at  INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    token      TEXT NOT NULL,
    PRIMARY KEY (issuer, grant_id)
) STRICT;
CREATE INDEX grants_held_by_audience ON grants_held (audience, issued_at);

-- Revocations of grants this agent issued.
CREATE TABLE revocations (
    grant_id   TEXT PRIMARY KEY NOT NULL REFERENCES grants_issued (grant_id),
    revoked_at INTEGER NOT NULL
) STRICT;

-- A2A tasks this agent serves. A task is kept as its A2A JSON, `task_json`;
-- the other columns index it and are written with it: `state` is the A2A
-- name of its state (`TASK_STATE_WORKING`), `version` its `TaskStore` version
-- (1 when created, one more on every write), `updated_at` the instant of
-- the last write, and `status_at` its status timestamp in nanoseconds since
-- the Unix epoch, for listing the most recently updated first
-- (-9223372036854775808 when it has none).
CREATE TABLE tasks (
    task_id    TEXT PRIMARY KEY NOT NULL,
    context_id TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    version    INTEGER NOT NULL,
    task_json  TEXT NOT NULL,
    status_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX tasks_by_state ON tasks (state);
CREATE INDEX tasks_by_status_at ON tasks (status_at, task_id);

-- The peer that owns each task, the one that created it. Set once, and
-- possibly before the task is stored.
CREATE TABLE task_owners (
    task_id TEXT PRIMARY KEY NOT NULL,
    owner   TEXT NOT NULL
) STRICT;
CREATE INDEX task_owners_by_owner ON task_owners (owner);

-- Idempotency of A2A messages, keyed by (peer, messageId). `request_hash`
-- is the BLAKE3 of the JCS form of the request; `task_id` or `response_json`
-- is the outcome of the first request with the key, set once (both NULL
-- while it is in flight); `created_at` is when that request arrived.
CREATE TABLE dedup (
    peer          TEXT NOT NULL,
    message_id    TEXT NOT NULL,
    request_hash  TEXT NOT NULL,
    task_id       TEXT,
    response_json TEXT,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (peer, message_id)
) STRICT;
CREATE INDEX dedup_by_created_at ON dedup (created_at);

-- The signed, hash-chained event log. `payload` is the JCS text of the
-- payload; the other columns are the signed header and its signature.
CREATE TABLE events (
    author       TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    prev_hash    TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    kind         TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    payload      TEXT NOT NULL,
    signature    TEXT NOT NULL,
    PRIMARY KEY (author, seq)
) STRICT;
CREATE INDEX events_by_kind ON events (kind);

-- Messages this agent sends to peers, persisted before the first attempt.
-- `request_json` is the A2A `SendMessageRequest`, sent unchanged on every
-- attempt; `task_id` the task the peer runs it as, once known; `state` is
-- `pending` until the outcome is known, then the A2A name of the state the
-- task ended in, or `refused` when the request was refused with no task;
-- `attempts` counts the exchanges with the peer.
CREATE TABLE outbox (
    message_id   TEXT PRIMARY KEY NOT NULL,
    peer         TEXT NOT NULL,
    request_json TEXT NOT NULL,
    task_id      TEXT,
    state        TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;

-- Added in version 2.

-- Contacts that are plain A2A agents, reached at their card's URL with no
-- PALS daemon. A name is unique across this table and `contacts`. `origin`
-- is the scheme, host and port the card was read from; `bearer_env` names
-- the environment variable that holds a bearer token for it, if any.
CREATE TABLE a2a_contacts (
    name       TEXT PRIMARY KEY NOT NULL,
    card_url   TEXT NOT NULL,
    origin     TEXT NOT NULL,
    bearer_env TEXT,
    added_at   INTEGER NOT NULL
) STRICT;

-- This agent's own signed records, one row per kind (`contact`,
-- `announce`): `seq` is the last sequence number handed out for the kind,
-- never handed out again; `record` is the signed record that carries it,
-- NULL from the moment `seq` is handed out until that record is stored,
-- and `updated_at` the instant it was stored (0 before the first).
CREATE TABLE own_records (
    kind       TEXT PRIMARY KEY NOT NULL,
    seq        INTEGER NOT NULL,
    record     BLOB,
    updated_at INTEGER NOT NULL
) STRICT;

-- The newest signed record of each kind received from each peer, the one
-- a later record is compared with to detect a regression until it expires.
CREATE TABLE peer_records (
    agent_id   TEXT NOT NULL,
    kind       TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    record     BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    seen_at    INTEGER NOT NULL,
    PRIMARY KEY (agent_id, kind)
) STRICT;

-- Verified Agent Cards of peers, by authorization context: `public`, or
-- `grant:<issuer>/<grant_id>` for the extended card a grant let this agent
-- see. `peer` is the peer's agent id, or `a2a:<name>` for a plain A2A
-- contact; `signer` is the agent whose signature was verified, NULL when
-- none is required (plain A2A contacts).
CREATE TABLE peer_cards (
    peer       TEXT NOT NULL,
    context    TEXT NOT NULL,
    card_json  TEXT NOT NULL,
    card_hash  TEXT NOT NULL,
    signer     TEXT,
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (peer, context)
) STRICT;

-- The revision of each scope of this agent's inventory view: `seq` moves
-- when `content_hash`, the hash of the scope's entries, changes.
CREATE TABLE view_revisions (
    scope_key    TEXT PRIMARY KEY NOT NULL,
    content_hash TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;

-- What the inventory adapters last observed of each entry; `detail_json` is
-- the whole observation.
CREATE TABLE inventory_observations (
    entry_id    TEXT PRIMARY KEY NOT NULL,
    source      TEXT NOT NULL,
    status      TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    detail_json TEXT NOT NULL
) STRICT;

-- Signed benchmark reports this agent holds, its own or imported; the other
-- columns index `report_json`. `visibility` is local metadata, never part of
-- what the measurer signed.
CREATE TABLE benchmark_reports (
    benchmark_id     TEXT PRIMARY KEY NOT NULL,
    measured_by      TEXT NOT NULL,
    measurement_kind TEXT NOT NULL,
    subject          TEXT NOT NULL,
    profile          TEXT NOT NULL,
    measured_at      INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    visibility       TEXT NOT NULL,
    report_json      BLOB NOT NULL,
    imported_at      INTEGER NOT NULL
) STRICT;

-- Service offers this agent issued, kept to check an offer again when the
-- task that names it arrives.
CREATE TABLE offers_issued (
    offer_id   TEXT PRIMARY KEY NOT NULL,
    requester  TEXT NOT NULL,
    query_hash TEXT NOT NULL,
    scope_key  TEXT NOT NULL,
    card_hash  TEXT NOT NULL,
    revision   INTEGER NOT NULL,
    issued_at  INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    offer_json BLOB NOT NULL
) STRICT;
CREATE INDEX offers_issued_by_expires_at ON offers_issued (expires_at);

-- Service offers peers sent to this agent. Offer ids are chosen by their
-- provider, so they are unique per provider only.
CREATE TABLE offers_received (
    provider    TEXT NOT NULL,
    offer_id    TEXT NOT NULL,
    query_id    TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    offer_json  BLOB NOT NULL,
    PRIMARY KEY (provider, offer_id)
) STRICT;

-- Signed inventory view pages of peers, by authorization context and
-- request (sections, cursor).
CREATE TABLE inventory_cache (
    peer        TEXT NOT NULL,
    context     TEXT NOT NULL,
    request_key TEXT NOT NULL,
    page_json   BLOB NOT NULL,
    expires_at  INTEGER NOT NULL,
    PRIMARY KEY (peer, context, request_key)
) STRICT;
