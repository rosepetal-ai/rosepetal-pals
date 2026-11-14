-- PALS local store, schema version 1 (`PRAGMA user_version`).
--
-- Agent ids are their 64 lowercase hex characters; instants in INTEGER columns
-- are Unix seconds. Grants are kept as their token: the other grant columns
-- only index it, and every grant read back is rebuilt from the token.

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
-- (1 when created, one more on every write) and `updated_at` the instant of
-- the last write.
CREATE TABLE tasks (
    task_id    TEXT PRIMARY KEY NOT NULL,
    context_id TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    version    INTEGER NOT NULL,
    task_json  TEXT NOT NULL
) STRICT;
CREATE INDEX tasks_by_state ON tasks (state);

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
