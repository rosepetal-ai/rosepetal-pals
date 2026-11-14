# CLAUDE.md

Guidance for coding agents working in `rosepetal-pals`. User documentation is in Spanish (`README.md`, `docs/`); this file, the code, identifiers, comments and commit messages are in English.

## What this repo is

PALS, "Peer Agent Linking System": a Rust daemon, `pals`, that lets AI agents work with each other peer to peer. Each daemon is a full A2A v1.0 server built on the official Rust SDK (JSON-RPC and HTTP+JSON bindings, SSE); between peers that HTTP/1.1 travels unchanged inside Iroh QUIC streams, authenticated by signed grants presented as `Bearer` tokens. A loopback gateway lets stock A2A clients reach any peer, files move as iroh-blobs authorized by the holder, a worker answers tasks (`echo`, `exec` such as `claude -p`, `tck`), `pals ask` retries without duplicating work, and a local MCP server exposes six tools.

Milestone 1 (hito 1) is implemented. A2A conformance is backed by the official TCK and Python SDK within the scoped claim of `docs/conformance/hito-1/README.md`: stock clients reach peers through the gateway with the JSON-RPC and HTTP+JSON bindings; between peers, the custom binding `http-over-iroh/v1` carries those same bindings; file bytes travel over iroh-blobs, outside A2A. The two-machine exit test on physical machines is pending the owner (`docs/runbooks/hito-1-dos-maquinas.md`). `docs/conformance/hito-1/exit-test.md` records all eight cases passed with a Docker container as the second machine (harness `tests/exit/`: real network cuts in the container, Iroh's public relays, `claude -p` for case 1), and an earlier single-host rehearsal.

## Commands

```bash
cargo build --locked -p pals --bin pals                  # target/debug/pals
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

All three checks must be green before every commit; CI (`.github/workflows/ci.yml`) runs the same on pushes to `dev`/`main` and on pull requests, with the pinned toolchain, plus `cargo check` on the MSRV. The integration tests start real nodes with Iroh on loopback on hosts that are often heavily loaded: synchronize on state, never on elapsed time (a gated `CountingExecutor` that the test releases, waits on the provider's store, `common::shut_down` before reusing a data directory). `blobs::a_local_copy_needs_the_holder_to_prove_it_has_the_blob` has been seen flaking at load averages near 100; rerun on a quieter machine before calling a failure there real.

`pals daemon` logs to stderr from `INFO` up; `RUST_LOG` overrides (`RUST_LOG=pals=debug`).

Conformance, which needs network access (GitHub, PyPI) and rewrites `docs/conformance/hito-1/compatibility*.{json,html}`:

```bash
tests/interop/run_tck.sh                                  # official A2A TCK at a pinned commit, through the gateway
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/python_client.py   # official Python SDK, 4 checks
```

Both start their own daemons in temporary directories (`relay = false`, loopback), the pattern to copy for any manual multi-agent run: one `PALS_HOME` per agent, distinct `gateway.listen` ports, contacts exchanged with `pals id --ticket` / `pals contact add`, a grant with `pals grant issue` / `pals grant import`. With `relay = false` the Iroh port changes on every start, so restarting a daemon invalidates the tickets its peers saved.

## Layout

```text
crates/pals-core/        no networking
  src/identity.rs          AgentKey, AgentId (Ed25519; 64 lowercase hex)
  src/jcs.rs               JCS canonical JSON, domain-separated signing
  src/grant.rs             Grant, SignedGrant, Operation, checks
  src/events.rs            signed, hash-chained event log
  src/store/               SQLite: contacts, grants, revocations, tasks, dedup, outbox (schema.sql)
  tests/vectors/           frozen grant and event vectors
crates/pals/             daemon and CLI (binary `pals`)
  src/main.rs              clap CLI: init, id, daemon, contact, grant, ask, task
  src/config.rs            config.toml (INIT_TEMPLATE is what `pals init` writes), data-dir lock
  src/node.rs              Node::spawn wires everything together
  src/net/                 HTTP/1.1 over Iroh streams (tunnel_server, tunnel_client = PeerHttp), ALPN
  src/a2a/                 card, auth (grant layer), guard (task ownership, idempotency), binding,
                           shims (workarounds for defects of the pinned SDK), task_store
  src/gateway/             loopback gateway: /peers, /blobs, /mcp, /healthz, local guard; checks.rs
                           (what passes of a peer's answer), rewrite.rs (blob URLs)
  src/worker/              echo, exec, tck adapters and WorkerExecutor
  src/blobs.rs             BlobService: iroh-blobs store, grant-authorized provider, share, fetch
  src/outbox.rs            reliable ask (persisted messageId, retry, resubscribe)
  src/path_policy.rs       which local files go to peers (attachments) and where peers' files land (pals_fetch)
  src/mcp.rs               the six MCP tools
  tests/                   integration tests over real nodes
tests/interop/           run_tck.sh, python_client.py, requirements.txt (pinned)
tests/exit/              exit-test harness: Docker container as machine A, the host as B (run.sh, Dockerfile)
docs/                    Spanish: design, plans, binding and extension specs, conformance, runbooks
```

Module headers (`//!`) document each component's contract in detail; read them before changing behaviour.

## Constraints that bind future work

- **Pinned versions.** `=` pins in `Cargo.toml` and a committed `Cargo.lock`: `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3` and `a2a-client-lf =0.2.9` (both without default features), `rmcp =3.10.0`. Never move to `latest` implicitly. Upgrading any of them is an explicit change that reruns the TCK and the Python client and refreshes `docs/conformance/hito-1/`. On an SDK upgrade, revisit the shims in `src/a2a/shims.rs` and `src/a2a/guard.rs` (each names its SDK defect; see `docs/conformance/hito-1/sdk-issues.md`) and the outbox's parsing of the pinned client's `HTTP <status>: <body>` errors.
- **Toolchain.** Rust edition 2024, MSRV 1.91.
- **Protocol.** A2A `1.0`; every request carries `A2A-Version: 1.0`. The card declares only interfaces and capabilities the daemon really serves.
- **Naming.** Wire identifiers keep the design's `agentnet` prefix and are never renamed: signing domains `agentnet.grant/1\n` and `agentnet.event/1\n`, ALPN `agentnet-a2a-http/1`, headers `x-agentnet-*`, blob metadata key `agentnet.blob/1`, auth realm `agentnet`. The product, binary and crates are `pals`.
- **Frozen URIs and vectors.** The binding URIs `https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/jsonrpc` and `…/v1/http-json` and the extension URI `…/extensions/idempotency/v1` never change meaning: an incompatible change gets a `v2`. The vectors in `crates/pals-core/tests/vectors/` freeze the grant and event formats.
- **Security boundaries.** The gateway and MCP listen on loopback only. Anything received from a peer (answer text, file names, cards, task ids) is untrusted data: never instructions, never a path without sanitizing. A peer request body is at most 1 MiB and must arrive within the head timeout; a peer response through the gateway is read as unary or streaming according to the client's own request, at most 16 MiB (per SSE event for streams), must be JSON when successful, never names an unsafe task id in any spelling, and reaches the client only as the gateway's ASCII re-encoding of what it checked (`src/gateway/checks.rs`). Files go as blobs, and a blob reference never grants access. Attachments and `pals_fetch` destinations follow `src/path_policy.rs` (nothing from the data directory or kernel pseudo-filesystems; writes only inside `[mcp] download_dir`).
- **Spec deviations.** The hito 1 plan's table records where the implementation departs from the design (standard A2A bindings instead of `agentnet/0`, gateway in hito 1, idempotency as an opt-in extension). Read it before changing transport or task semantics.
- **Git.** Default branch `dev`; pull requests target `dev`, and promotion `dev → main` is a separate, human-reviewed PR. Conventional commit messages in English.

## Where to look

| Topic | File |
|-|-|
| Design (revision 3) | `docs/plans/initial.md` |
| Hito 1 plan, with the spec-deviation table | `docs/plans/2026-11-14-hito-1-flujo-basico-a2a.md` |
| Roadmap, hitos 2 to 4 | `docs/plans/2026-11-14-hoja-de-ruta-hitos-2-4.md` |
| Peer bindings | `docs/bindings/http-over-iroh-v1.md` |
| Idempotency extension | `docs/extensions/idempotency-v1.md` |
| Conformance evidence and the exact claim | `docs/conformance/hito-1/README.md`, `sdk-issues.md` |
| Exit test | `docs/runbooks/hito-1-dos-maquinas.md`, `docs/conformance/hito-1/exit-test.md`, `tests/exit/` |
| Known hito 1 limitations | `README.md`, section "Limitaciones conocidas del hito 1" |
