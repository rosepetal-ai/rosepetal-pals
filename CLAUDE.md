# CLAUDE.md

Guidance for coding agents working in `rosepetal-pals`. User documentation under `docs/` is in Spanish; `README.md`, this file, the code, identifiers, comments and commit messages are in English.

## What this repo is

PALS, "Peer Agent Linking System": a Rust daemon, `pals`, that lets AI agents work with each other peer to peer. Each daemon is a full A2A v1.0 server built on the official Rust SDK (JSON-RPC and HTTP+JSON bindings, SSE); between peers that HTTP/1.1 travels unchanged inside Iroh QUIC streams, authenticated by signed grants presented as `Bearer` tokens. A loopback gateway lets stock A2A clients reach any peer, files move as iroh-blobs authorized by the holder, a worker answers tasks (`echo`, `exec` such as `claude -p`, `tck`), `pals ask` retries without duplicating work, and a local MCP server exposes eight tools. Before delegating, an agent chooses among the providers it knows: it verifies their JWS-signed Agent Cards, reads their inventories as its grants allow and asks them for signed offers over its own Iroh ALPN, `agentnet-discovery/0`; no query runs any work, and the task itself still goes over the standard A2A bindings.

Milestone 1 (hito 1) is implemented. A2A conformance is backed by the official TCK and Python SDK within the scoped claim of `docs/conformance/hito-1/README.md`: stock clients reach peers through the gateway with the JSON-RPC and HTTP+JSON bindings; between peers, the custom binding `http-over-iroh/v1` carries those same bindings; file bytes travel over iroh-blobs, outside A2A. The two-machine exit test on physical machines is pending the owner (`docs/runbooks/hito-1-dos-maquinas.md`). `docs/conformance/hito-1/exit-test.md` records all eight cases passed with a Docker container as the second machine (harness `tests/exit/`: real network cuts in the container, Iroh's public relays, `claude -p` for case 1), and an earlier single-host rehearsal. Milestone 2 (hito 2), service queries and inventory, is implemented on `feat/hito-2` (draft PR #2, stacked on milestone 1) following `docs/plans/2026-11-14-hito-2-consulta-de-servicios.md`, all 13 tasks. Its exit test runs in Docker, one container per agent, under the harness `tests/exit/hito-2.sh`, and is recorded in `docs/conformance/hito-2/exit-test.md`: cases 1 to 8 passed, and case 9, the TCK and Python client rerun, passed with no regression, with the per-requirement differences from hito 1 in `docs/conformance/hito-2/README.md`. Repeating it on physical machines, like hito 1's, is pending the owner.

## Commands

```bash
cargo build --locked -p pals --bin pals                  # target/debug/pals
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

All three checks must be green before every commit; CI (`.github/workflows/ci.yml`) runs the same on pushes to `dev`/`main` and on pull requests, with the pinned toolchain, plus `cargo check` on the MSRV. The integration tests start real nodes with Iroh on loopback on hosts that are often heavily loaded: synchronize on state, never on elapsed time (a gated `CountingExecutor` that the test releases, waits on the provider's store, `common::shut_down` before reusing a data directory). `blobs::a_local_copy_needs_the_holder_to_prove_it_has_the_blob` has been seen flaking at load averages near 100; rerun on a quieter machine before calling a failure there real.

`pals daemon` logs to stderr from `INFO` up; `RUST_LOG` overrides (`RUST_LOG=pals=debug`).

Conformance, which needs network access (GitHub, PyPI) and rewrites `compatibility*.{json,html}` in `$PALS_CONFORMANCE_DIR`, `docs/conformance/hito-1` by default (a milestone's rerun points it at its own directory and leaves earlier reports untouched):

```bash
PALS_CONFORMANCE_DIR=docs/conformance/hito-2 tests/interop/run_tck.sh   # official A2A TCK at a pinned commit, through the gateway (without the variable: hito-1)
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/python_client.py   # official Python SDK, 4 checks
target/interop-venv/bin/python tests/interop/check_schemas.py        # frozen vectors against docs/extensions/inventory-v1/*.schema.json
```

Card-signature verifiers with the official Python, Go and JS SDKs (`docs/conformance/hito-2/card-signatures.md`), on the frozen vector or, given an agent id, on a live card:

```bash
target/interop-venv/bin/python tests/interop/card_signatures.py --vector crates/pals-core/tests/vectors/card-jws-v1.json   # or --gateway URL --peer ID
go -C tests/interop/card_signatures/go run . "$PWD/crates/pals-core/tests/vectors/card-jws-v1.json"                     # or <agent id> <absolute path of a card>
(cd tests/interop/card_signatures/js && npm ci) \
  && node tests/interop/card_signatures/js/verify.mjs crates/pals-core/tests/vectors/card-jws-v1.json                   # or <agent id> <card file>
```

Exit tests in Docker on one Linux host (`tests/exit/README.md`; both need `cargo build --release --locked -p pals` first and a new or empty `PALS_EXIT_DIR`):

```bash
tests/exit/run.sh       # hito 1: a container as machine A, the host as B, cases 1 to 8
tests/exit/hito-2.sh    # hito 2: one container per agent, cases 1 to 8, about two minutes (case 8 also needs PALS_EXIT_BIN_HITO1 and the events_verify example)
```

The TCK and the Python client start their own daemons in temporary directories (`relay = false`, loopback), the pattern to copy for any manual multi-agent run: one `PALS_HOME` per agent, distinct `gateway.listen` ports, contacts exchanged with `pals id --ticket` / `pals contact add`, a grant with `pals grant issue` / `pals grant import`. With `relay = false` the Iroh port changes on every start, so restarting a daemon invalidates the tickets its peers saved.

## Layout

```text
crates/pals-core/        opens no sockets and holds no endpoint, worker or adapter handle
  src/identity.rs          AgentKey, AgentId (Ed25519; 64 lowercase hex)
  src/jcs.rs               JCS canonical JSON, domain-separated signing
  src/grant.rs             Grant, SignedGrant, Operation, checks
  src/events.rs            signed, hash-chained event log
  src/card_jws.rs          Agent Card JWS (EdDSA, detached), canonical forms, normalize, card limits and hash
  src/signed.rs            signed JSON documents of hito 2, one signing domain per kind
  src/records.rs           signed contact and announce records
  src/inventory/           agentnet.inventory/1 (inventory.toml), visibility, scopes from grants (scope.rs),
                           signed paginated views with cursors and a per-context cache (view/)
  src/discovery/           agentnet-discovery/0: wire.rs (frames, prost messages), answer.rs (answers from
                           local metadata only), query.rs (ServiceQuery, ServiceOffer), matching.rs,
                           selection.rs (the two stages of the choice)
  src/benchmark.rs         signed benchmark reports, and when one can stand for another
  src/clock.rs             SystemClock, and ManualClock for tests
  src/store/               SQLite: contacts, grants, revocations, tasks, dedup, outbox; schema 2 adds plain
                           A2A contacts, records, peer cards, view revisions and cached pages, MCP
                           observations, benchmark reports and offers (schema.sql whole; migrate.rs and
                           migrate-2.sql upgrade schema 1)
  examples/events_verify.rs  verifies a data directory's event chain (exit test, case 8)
  tests/vectors/           frozen vectors: grant, event, card JWS, contact, announce, inventory, view,
                           service query and offer, benchmark (all v1) and discovery-v0
  tests/fixtures/          schema-v1.sql, the hito 1 schema, for the migration tests
crates/pals/             daemon and CLI (binary `pals`)
  src/main.rs              clap CLI: init, id, daemon, contact, grant, ask, discover, query, task, card,
                           inventory, benchmark
  src/config.rs            config.toml (INIT_TEMPLATE is what `pals init` writes, INVENTORY_TEMPLATE its
                           inventory.toml), [inventory] and [discovery], data-dir lock
  src/node.rs              Node::spawn wires everything together
  src/net/                 HTTP/1.1 over Iroh streams (tunnel_server, tunnel_client = PeerHttp), ALPN
  src/a2a/                 card (signed public and extended cards, gateway view), auth (grant layer, scope
                           header), guard (task ownership, idempotency, offers checked again), binding,
                           shims (workarounds for defects of the pinned SDK), task_store
  src/discovery/           peers' cards verified (cards.rs), own records, agentnet-discovery/0 server (quotas)
                           and client, search.rs (Searcher: candidates, limits, ranking, metrics)
  src/inventory/           the inventory served, with its observations; mcp_adapter.rs (initialize and
                           tools/list over HTTP or stdio, never tools/call)
  src/gateway/             loopback gateway: /peers, /discovery, /inventory/refresh, /blobs, /mcp, /healthz,
                           local guard; checks.rs (what passes of a peer's answer), rewrite.rs (blob URLs),
                           plain.rs (plain A2A contacts, outbound)
  src/worker/              echo, exec, tck adapters and WorkerExecutor
  src/process_group.rs     GroupLeader: a child in its own process group (exec worker, stdio MCP servers)
  src/blobs.rs             BlobService: iroh-blobs store, grant-authorized provider, share, fetch
  src/outbox.rs            reliable ask (persisted messageId, retry, resubscribe, offer reference)
  src/path_policy.rs       which local files go to peers (attachments) and where peers' files land (pals_fetch)
  src/mcp.rs               the eight MCP tools
  tests/                   integration tests over real nodes
tests/interop/           run_tck.sh, python_client.py, check_schemas.py, card_signatures.py and
                         card_signatures/{go,js}, requirements.txt (pinned)
tests/exit/              exit-test harnesses: run.sh (hito 1, a Docker container as machine A, the host as B),
                         hito-2.sh and hito-2/ (hito 2, one container per agent)
docs/                    Spanish: design, plans, binding, extension and protocol specs, conformance, runbooks
```

Module headers (`//!`) document each component's contract in detail; read them before changing behaviour.

## Constraints that bind future work

- **Pinned versions.** `=` pins in `Cargo.toml` and a committed `Cargo.lock`: `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3` and `a2a-client-lf =0.2.9` (both without default features), `a2a-pb =0.3.2`, `rmcp =3.10.0`, `prost =0.14.6`. Never move to `latest` implicitly. Upgrading any of them is an explicit change that reruns the TCK, the Python client and the card-signature verifiers, and refreshes the current milestone's `docs/conformance/hito-N/` (`PALS_CONFORMANCE_DIR`); the reports of earlier milestones stay as they were. On an SDK upgrade, revisit the shims in `src/a2a/shims.rs` and `src/a2a/guard.rs` (each names its SDK defect; see `docs/conformance/hito-1/sdk-issues.md`) and the outbox's parsing of the pinned client's errors (`HTTP <status>: <body>`, `HTTP request failed: …`, and the JSON-RPC `failed to parse JSON-RPC response: …` that quotes the gateway's `plain agent unreachable: nothing was sent to it`), on which it decides whether a question to a plain agent goes again.
- **Toolchain.** Rust edition 2024, MSRV 1.91.
- **Protocol.** A2A `1.0`; every request carries `A2A-Version: 1.0`. The card declares only interfaces and capabilities the daemon really serves.
- **Naming.** Wire identifiers keep the design's `agentnet` prefix and are never renamed: signing domains `agentnet.grant/1\n` and `agentnet.event/1\n`, ALPN `agentnet-a2a-http/1`, headers `x-agentnet-*`, blob metadata key `agentnet.blob/1`, auth realm `agentnet`, and those of hito 2 below. The product, binary and crates are `pals`.
- **Frozen URIs and vectors.** The binding URIs `https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/jsonrpc` and `…/v1/http-json` and the extension URIs `…/extensions/idempotency/v1` and `…/extensions/inventory/v1` never change meaning: an incompatible change gets a `v2`. The vectors in `crates/pals-core/tests/vectors/` freeze the grant and event formats, and those of hito 2 below; `tests/interop/check_schemas.py` keeps them valid against `docs/extensions/inventory-v1/*.schema.json`.
- **Frozen identifiers of hito 2** (plan, "Global Constraints"), as frozen as hito 1's:
  - ALPN `agentnet-discovery/0`, whose protobuf messages (package `agentnet.discovery.v0`, `docs/protocols/agentnet-discovery-0.proto`) keep their field numbers and enumeration values, with frames of a `u32` big-endian length and the message;
  - the inventory extension `https://github.com/rosepetal-ai/rosepetal-pals/extensions/inventory/v1`, profile `agentnet.inventory/1`;
  - schemas `agentnet.contact/1`, `agentnet.announce/1`, `agentnet.inventory-view/1`, `agentnet.service-query/1`, `agentnet.service-offer/1` and `agentnet.benchmark-report/1`;
  - signing domains `agentnet.contact/1\n`, `agentnet.announce/1\n`, `agentnet.inventory-view/1\n`, `agentnet.offer/1\n` and `agentnet.benchmark/1\n`, all in `pals_core::signed`'s `{<wrapper>: <document>, "signature": …}` form;
  - the card JWS, `EdDSA` with the agent's own key and the protected header `{"alg":"EdDSA","kid":"<agent id>","typ":"JOSE"}`, detached, with no signing domain;
  - the internal header `x-agentnet-scope`, which only the grant layer sets;
  - capabilities of the common vocabulary as the skill tag `agentnet:capability:v1:<id>`;
  - the vectors `card-jws-v1`, `contact-v1`, `announce-v1`, `inventory-v1`, `view-v1`, `service-query-v1`, `service-offer-v1`, `benchmark-v1` and `discovery-v0`.

  Grants stay `agentnet.grant/1`: hito 2 adds no operations, and the inventory scopes derive from v1 grants. The sizes of the discovery protocol are wire constants in this milestone (a request frame at most 20 KiB, a response frame at most 80 KiB; a query 8 KiB, an offer 16 KiB, a card 32 KiB, a view page 64 KiB and 100 entries).
- **Store schema 2.** `PRAGMA user_version` is 2. A database at 1, from hito 1, is upgraded in place on open, in the transaction that opens it (`store/migrate.rs`, `migrate-2.sql`), keeping every row; a newer version is refused. Data directories of hito 1 are in use: never change a table of schema 1 in place. A schema 3 is a new `migrate-3.sql` that only adds, plus `schema.sql` creating version 3 whole, and `tests/store_migration.rs` checks that both end with the same schema.
- **Normalized cards.** Every card PALS signs goes through `card_jws::normalize` (`sign_card` does it), so that its three canonical forms (`Raw`, which a2a-go checks; `ProtoJson`; `ProtoJsonClean`, which the Python and JS SDKs check) are the same bytes and every SDK verifies it; a card that cannot be normalized is refused (`CardError::NotNormalized`), never signed. A foreign card is accepted with a valid signature over any of the three forms, and what is used is read from the bytes the signature covers, never from the JSON received (`docs/conformance/hito-2/card-signatures.md`).
- **Querying runs no work.** No path of `agentnet-discovery/0`, the extended card or a search reaches the worker, an MCP adapter, `tools/call` or another peer: answering lives in `pals_core::discovery`, on local metadata only, and the inventory adapter runs only at start, on its timer and on `pals inventory refresh`, with `initialize` and `tools/list` and never `tools/call`. An offer, a view or a card authorizes nothing: the task still needs the grant, and the guard checks the offer again when the task arrives. Tests `discovery_never_runs_work`, `service_queries_run_no_work`, `discovery_never_calls_the_adapter`, `querying_runs_no_work`.
- **Views by authorization.** The scope comes from the verified grant and from nothing else the requester sends. A public view carries no count, hash or reference to what it hides; caches and cursors are per scope, audience and revision; an error never tells "does not exist" from "you cannot see it".
- **Security boundaries.** The gateway and MCP listen on loopback only. Anything received from a peer (answer text, file names, cards, task ids) is untrusted data: never instructions, never a path without sanitizing. A peer request body is at most 1 MiB and must arrive within the head timeout; a peer response through the gateway is read as unary or streaming according to the client's own request, at most 16 MiB (per SSE event for streams), must be JSON when successful, never names an unsafe task id in any spelling, and reaches the client only as the gateway's ASCII re-encoding of what it checked (`src/gateway/checks.rs`). A peer's `agentnet-discovery/0` stream must fit its quotas before anything is read (4 streams and 60 requests a minute per peer, 64 streams in all), and its request frame must arrive whole within 5 s, the length prefix checked against the limit before the body is read. Files go as blobs, and a blob reference never grants access. Attachments and `pals_fetch` destinations follow `src/path_policy.rs` (nothing from the data directory or kernel pseudo-filesystems; writes only inside `[mcp] download_dir`).
- **Spec deviations.** The hito 1 plan's table records where the implementation departs from the design (standard A2A bindings instead of `agentnet/0`, gateway in hito 1, idempotency as an opt-in extension), and the hito 2 plan's adds its own ("Cambios respecto al spec": no `GetAgentCard` in `agentnet-discovery/0`, a new frame reader, JWS without a JOSE library, normalized cards, views derived from v1 grants, records without a DHT until hito 3, plain A2A contacts outbound). Read them before changing transport, discovery or task semantics.
- **Git.** Default branch `dev`; pull requests target `dev`, and promotion `dev → main` is a separate, human-reviewed PR. Conventional commit messages in English.

## Where to look

| Topic | File |
|-|-|
| Design (revision 3) | `docs/plans/initial.md` |
| Hito 1 plan, with the spec-deviation table | `docs/plans/2026-11-14-hito-1-flujo-basico-a2a.md` |
| Hito 2 plan: its spec-deviation table, decided values, Global Constraints and Review Focus | `docs/plans/2026-11-14-hito-2-consulta-de-servicios.md` |
| Roadmap, hitos 2 to 4 | `docs/plans/2026-11-14-hoja-de-ruta-hitos-2-4.md` |
| Peer bindings | `docs/bindings/http-over-iroh-v1.md` |
| Idempotency extension | `docs/extensions/idempotency-v1.md` |
| Inventory extension and its JSON Schemas | `docs/extensions/inventory-v1.md`, `docs/extensions/inventory-v1/` |
| Discovery protocol | `docs/protocols/agentnet-discovery-0.md`, `agentnet-discovery-0.proto` |
| Conformance evidence and the exact claim | `docs/conformance/hito-1/README.md`, `sdk-issues.md`; the hito 2 rerun and its differences, `docs/conformance/hito-2/README.md` |
| Card signatures, verified with the Python, Go and JS SDKs | `docs/conformance/hito-2/card-signatures.md` |
| Exit test, hito 1 | `docs/runbooks/hito-1-dos-maquinas.md`, `docs/conformance/hito-1/exit-test.md`, `tests/exit/run.sh` |
| Exit test, hito 2 | `docs/runbooks/hito-2-consulta-de-servicios.md`, `docs/conformance/hito-2/exit-test.md`, `tests/exit/hito-2.sh` |
| Known limitations | `README.md`, sections "Known limitations of milestone 1" and "Known limitations of milestone 2" |
