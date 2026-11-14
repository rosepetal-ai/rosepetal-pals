#!/usr/bin/env bash
# Runs the official A2A TCK against a PALS agent, through the local gateway
# of another PALS agent, as any stock A2A client would reach it.
#
# Two daemons on this machine, in temporary directories, both with Iroh on
# 127.0.0.1 and no relay:
#   - the provider, with the `tck` worker adapter, which plays the TCK's
#     scenarios;
#   - the requester, whose gateway is what the TCK talks to.
# They exchange contacts and a grant with the `pals` CLI. The TCK
# (a2aproject/a2a-tck at TCK_COMMIT, cloned into target/tck and installed
# with uv) then runs with
#   --sut-host http://127.0.0.1:<requester gateway>/peers/<provider id>
#   --transport jsonrpc,http_json
# once per level in LEVELS. The reports of the `must` run are copied to
# docs/conformance/hito-1/compatibility.{json,html}, those of any other
# level to compatibility-<level>.{json,html}; the logs of the run stay in
# target/tck-run/<UTC time>/.
#
# Exit code: that of the `must` run (0 when no MUST requirement fails).
#
# Environment:
#   TCK_STREAMING_TIMEOUT  seconds, for both the TCK and the provider
#                          (default 2.0)
#   LEVELS                 levels to run, in order (default "must should")
#   PALS_BIN               a pals binary to use instead of building one

set -euo pipefail

TCK_REPO=https://github.com/a2aproject/a2a-tck
TCK_COMMIT=104a7b4ae0d8
UV_VERSION=0.13.8
TCK_PYTHON=3.15

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TCK_DIR=$ROOT/target/tck
TOOLS_VENV=$ROOT/target/tck-venv
REPORTS_OUT=$ROOT/docs/conformance/hito-1
RUN_DIR=$ROOT/target/tck-run/$(date -u +%Y%m%dT%H%M%SZ)
export TCK_STREAMING_TIMEOUT=${TCK_STREAMING_TIMEOUT:-2.0}
LEVELS=${LEVELS:-must should}

log() { printf '[run_tck] %s\n' "$*" >&2; }
fail() { log "error: $*"; exit 1; }

mkdir -p "$RUN_DIR"
log "logs in $RUN_DIR"

# --- The pals binary -------------------------------------------------------
if [[ -n ${PALS_BIN:-} ]]; then
    PALS=$PALS_BIN
else
    log "building pals"
    cargo build --locked --quiet --manifest-path "$ROOT/Cargo.toml" -p pals --bin pals
    PALS=$ROOT/target/debug/pals
fi
[[ -x $PALS ]] || fail "no pals binary at $PALS"

# --- The TCK, pinned, and uv ----------------------------------------------
if ! command -v uv >/dev/null 2>&1 && [[ ! -x $TOOLS_VENV/bin/uv ]]; then
    log "installing uv $UV_VERSION into $TOOLS_VENV"
    python3 -m venv "$TOOLS_VENV"
    "$TOOLS_VENV/bin/pip" install --quiet "uv==$UV_VERSION"
fi
UV=$(command -v uv 2>/dev/null || echo "$TOOLS_VENV/bin/uv")

if [[ ! -d $TCK_DIR/.git ]]; then
    log "cloning $TCK_REPO into $TCK_DIR"
    git clone --quiet "$TCK_REPO" "$TCK_DIR"
fi
if ! git -C "$TCK_DIR" cat-file -e "$TCK_COMMIT^{commit}" 2>/dev/null; then
    git -C "$TCK_DIR" fetch --quiet origin
fi
git -C "$TCK_DIR" checkout --quiet --detach "$TCK_COMMIT"
[[ -z $(git -C "$TCK_DIR" status --porcelain --untracked-files=no) ]] \
    || fail "$TCK_DIR has local changes to tracked files"
log "TCK at $(git -C "$TCK_DIR" rev-parse HEAD)"
(cd "$TCK_DIR" && "$UV" sync --quiet --frozen --python "$TCK_PYTHON")

# --- Two daemons ------------------------------------------------------------
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pals-tck.XXXXXX")
PIDS=()
cleanup() {
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    for pid in "${PIDS[@]}"; do
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$WORK"
}
trap cleanup EXIT

free_port() {
    python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}

# write_config HOME NAME ADAPTER PORT
write_config() {
    mkdir -p "$1"
    cat >"$1/config.toml" <<EOF
[agent]
name = "$2"
description = "PALS agent for the A2A TCK run ($3 adapter)"

[gateway]
listen = "127.0.0.1:$4"

[worker]
adapter = "$3"

[net]
relay = false
EOF
    "$PALS" init --home "$1" >/dev/null
}

# start NAME HOME: runs the daemon, waits until its gateway answers.
start() {
    "$PALS" daemon --home "$2" >"$RUN_DIR/$1.log" 2>&1 &
    PIDS+=("$!")
    for _ in $(seq 1 120); do
        if "$PALS" id --ticket --home "$2" >"$WORK/$1.ticket" 2>/dev/null; then
            return
        fi
        kill -0 "${PIDS[-1]}" 2>/dev/null || fail "the $1 daemon exited, see $RUN_DIR/$1.log"
        sleep 0.5
    done
    fail "the $1 daemon did not start within 60 s"
}

PROVIDER=$WORK/provider
REQUESTER=$WORK/requester
GW_PORT=$(free_port)
write_config "$PROVIDER" pals-tck-provider tck "$(free_port)"
write_config "$REQUESTER" pals-tck-requester echo "$GW_PORT"
start provider "$PROVIDER"
start requester "$REQUESTER"
PROVIDER_ID=$("$PALS" id --home "$PROVIDER")
log "provider $PROVIDER_ID, requester gateway on port $GW_PORT"

# Contacts both ways, and the grant that lets the requester use the
# provider's tasks.
"$PALS" contact add "$(cat "$WORK/provider.ticket")" --name provider --home "$REQUESTER" >&2
"$PALS" contact add "$(cat "$WORK/requester.ticket")" --name requester --home "$PROVIDER" >&2
TOKEN=$("$PALS" grant issue --to requester --project tck \
    --ops task.send,task.read,task.cancel --ttl 2h --home "$PROVIDER")
"$PALS" grant import "$TOKEN" --home "$REQUESTER" >&2

# --- The TCK, through the requester's gateway -------------------------------
SUT_HOST=http://127.0.0.1:$GW_PORT/peers/$PROVIDER_ID
mkdir -p "$REPORTS_OUT"
MUST_STATUS=
for level in $LEVELS; do
    log "TCK --level $level against $SUT_HOST (TCK_STREAMING_TIMEOUT=$TCK_STREAMING_TIMEOUT)"
    rm -rf "$TCK_DIR/reports"
    status=0
    (cd "$TCK_DIR" && .venv/bin/python run_tck.py --sut-host "$SUT_HOST" \
        --transport jsonrpc,http_json --level "$level") \
        2>&1 | tee "$RUN_DIR/tck-$level.log" || status=$?
    log "TCK --level $level exited $status"
    suffix=$([[ $level == must ]] && echo "" || echo "-$level")
    for ext in json html; do
        cp "$TCK_DIR/reports/compatibility.$ext" "$REPORTS_OUT/compatibility$suffix.$ext"
    done
    cp -r "$TCK_DIR/reports" "$RUN_DIR/reports-$level"
    [[ $level == must ]] && MUST_STATUS=$status
done
log "reports copied to $REPORTS_OUT"
exit "${MUST_STATUS:-0}"
