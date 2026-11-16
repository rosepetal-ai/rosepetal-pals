-- Upgrades a PALS store from schema version 1 to 2: adds the tables of
-- milestone 2, empty, and changes nothing that exists. It runs inside the
-- transaction that then sets `user_version` to 2, so it either applies whole
-- or not at all. `schema.sql` creates the same tables in a new database.

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
