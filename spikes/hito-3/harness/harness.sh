#!/usr/bin/env bash
# The harness of the hito 3 backend comparison
# (docs/plans/2026-11-16-hito-3-descubrimiento-kademlia.md, "Topología y
# escenarios comunes"): one Docker container per node of the common
# topology, on two internal networks, and scenarios S0 to S7 in order, the
# same for every candidate, with the parameters of params.env.
#
# Usage:
#   spikes/hito-3/harness/harness.sh <id>              # a run of spike-<id>
#   spikes/hito-3/harness/harness.sh --teardown <id>   # what an interrupted
#                                                      # run of <id> left
#
# A run builds spike-<id> (`cargo build --release --locked` in spikes/hito-3/)
# and the image pals-spike:latest (Dockerfile), creates the two networks
# with --internal and checks that Docker made them so, starts the nodes of
# the topology and runs S0 to S7. It drives every node through its control
# API, `docker exec <container> curl` on 127.0.0.1:7600; scenarios.py reads
# and writes all the JSON. It writes
# docs/decisions/hito-3-backend/results/<id>.json and checks it with
# check_results.py, whose exit code is the run's: 1 when a scenario ended in
# `error`. Logs go to spikes/hito-3/harness/logs/<id>/, which git ignores:
# harness.log, <node>.log for every container (`docker logs -t`), and raw/
# with every observation, one JSON line each.
#
# Nothing here knows the candidate: what the harness needs from it comes
# from its binary (spike_common::run_node over its Backend), namely how
# another node names it in --bootstrap (`bootstrap_addr` of GET /stats), its
# agent id and the scenarios it declares it cannot run (GET /healthz).
#
# Between scenarios the topology is put back: the nodes a scenario stops
# start again (same container, key and IPs), the nodes that start only for
# their scenario (c0, c5, rogue) stop, and the harness waits until what the
# routers store stops changing ("settle").
#
# Safety, as tests/exit/hito-2.sh: every container and network is named
# pals-spike-* and carries the label pals-spike.dir=<this run's logs
# directory>. A run refuses to start while any pals-spike-* container or
# network exists, and tears down, on exit and on Ctrl-C, only what has both
# its prefix and its label; the image stays. Nothing runs on the host's
# network (no iptables, nft, tc or sysctl, no prune), and only the
# candidate's binary is copied into a container, never a credential.

set -uo pipefail

HARNESS=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SPIKES=$(dirname "$HARNESS")
REPO=$(cd "$SPIKES/../.." && pwd)
PARAMS=$HARNESS/params.env

usage() { echo "usage: $0 [--teardown] <id>" >&2; exit 2; }
ONLY_TEARDOWN=0
if [ "${1:-}" = --teardown ]; then
    ONLY_TEARDOWN=1
    shift
fi
[ $# -eq 1 ] || usage
ID=$1
if ! [[ $ID =~ ^[a-z0-9][a-z0-9-]*$ ]] || [ ! -f "$SPIKES/$ID/Cargo.toml" ]; then
    echo "harness.sh: no candidate $ID in spikes/hito-3/" >&2
    exit 2
fi

# shellcheck source-path=SCRIPTDIR source=params.env
. "$PARAMS"

P=pals-spike
IMAGE=$P:latest
LABEL=pals-spike.dir
WORK=$HARNESS/logs/$ID
RAW=$WORK/raw
RESULTS_REL=docs/decisions/hito-3-backend/results/$ID.json
BIN=$SPIKES/target/release/spike-$ID
read -r -a ROUTER_LIST <<< "$ROUTERS"
CLIENTS="$CLIENTS_A $CLIENTS_B"
read -r -a TOPOLOGY <<< "$ROUTERS $PROVIDERS_A $PROVIDERS_B $CLIENTS"

# --- helpers ---------------------------------------------------------------

ts() { date -u +%T; }
say() { printf '\n[%s] == %s\n' "$(ts)" "$*"; }
note() { printf '[%s] %s\n' "$(ts)" "$*"; }
die() { note "ERROR: $*"; exit 1; }
# A wait that a trapped signal interrupts at once.
pause() { sleep "$1" & wait $!; }
# The JSON side: observations, waits and results (scenarios.py).
sc() { python3 "$HARNESS/scenarios.py" --raw "$RAW" --params "$PARAMS" --prefix "$P" "$@"; }
ctr() { echo "$P-$1"; }
net_name() { case $1 in a) echo "$NET_A" ;; b) echo "$NET_B" ;; *) return 1 ;; esac; }
caps_of() { local v="CAPS_$1"; echo "${!v}"; }
# announces NODE: ANN, the --announce flags of NODE's capabilities.
announces() {
    local c
    ANN=()
    for c in $(caps_of "$1"); do ANN+=(--announce "$CAPABILITY_PREFIX$c"); done
}

# Ours: named $P-*, labelled with this run's directory.
ours_containers() { docker ps -a --filter "label=$LABEL=$WORK" --format '{{.Names}}' | grep "^$P-" || true; }
ours_networks() { docker network ls --filter "label=$LABEL=$WORK" --format '{{.Name}}' | grep "^$P-" || true; }
any_containers() { docker ps -a --format '{{.Names}}' | grep "^$P-" || true; }
any_networks() { docker network ls --format '{{.Name}}' | grep "^$P-" || true; }
container_label() { docker inspect -f "{{ index .Config.Labels \"$LABEL\" }}" "$1" 2> /dev/null; }
network_label() { docker network inspect -f "{{ index .Labels \"$LABEL\" }}" "$1" 2> /dev/null; }

TORN=0
teardown() {
    TORN=1
    say "Teardown"
    local c n left
    mkdir -p "$WORK"
    for c in $(ours_containers); do
        [ "$(container_label "$c")" = "$WORK" ] || continue
        docker logs -t "$c" > "$WORK/${c#"$P-"}.log" 2>&1
        docker rm -f "$c" > /dev/null && note "container $c removed"
    done
    for n in $(ours_networks); do
        [ "$(network_label "$n")" = "$WORK" ] || continue
        docker network rm "$n" > /dev/null && note "network $n removed"
    done
    left=$(any_containers; any_networks)
    [ -z "$left" ] || note "WARNING: $P-* resources of another run remain, untouched: ${left//$'\n'/ }"
}

# create NODE ROLE NETS [ARG...]: NODE's container, with the binary in it,
# not started; NETS is "a", "b" or "a b"; the ARGs go to the binary. The key
# derives from the node's name, so that it is the same on every run.
create() {
    local node=$1 role=$2 nets=$3 first seed n
    shift 3
    first=${nets%% *}
    seed=$(printf 'pals-spike/%s' "$node" | sha256sum | cut -c1-64)
    docker create --init --name "$(ctr "$node")" --hostname "$node" --label "$LABEL=$WORK" \
        --cpus "$CPUS" --memory "$MEMORY" --network "$(net_name "$first")" "$IMAGE" \
        /usr/local/bin/spike-node --name "$node" --role "$role" --listen "0.0.0.0:$LISTEN_PORT" \
        --ttl-s "$TTL_S" --republish-s "$REPUBLISH_S" --key-seed "$seed" "$@" > /dev/null || return 1
    for n in ${nets#"$first"}; do
        docker network connect "$(net_name "$n")" "$(ctr "$node")" || return 1
    done
    docker cp "$BIN" "$(ctr "$node"):/usr/local/bin/spike-node" > /dev/null
}

# healthy NODE: waits until NODE answers GET /healthz, and keeps the answer
# in raw/healthz-NODE.json; fails if its process ends or after
# HEALTH_TIMEOUT_S.
healthy() {
    local node=$1 deadline=$((SECONDS + HEALTH_TIMEOUT_S)) body
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ "$(docker inspect -f '{{.State.Running}}' "$(ctr "$node")" 2> /dev/null)" != true ]; then
            note "$node is not running; its last lines:"
            docker logs --tail 20 "$(ctr "$node")" 2>&1 | sed 's/^/    /'
            return 1
        fi
        if body=$(docker exec "$(ctr "$node")" curl -sf -m 10 http://127.0.0.1:7600/healthz 2> /dev/null); then
            printf '%s\n' "$body" > "$RAW/healthz-$node.json"
            return 0
        fi
        pause 1
    done
    note "$node gave no /healthz in $HEALTH_TIMEOUT_S s"
    return 1
}

# up NODE...: starts the nodes and waits until every one is healthy.
up() {
    local node
    for node in "$@"; do docker start "$(ctr "$node")" > /dev/null || return 1; done
    for node in "$@"; do healthy "$node" || return 1; done
}

# settle TAG: waits until what the routers store has not changed for
# SETTLE_STABLE_POLLS polls in a row, SETTLE_MAX_S at most.
settle() {
    local start=$SECONDS deadline=$((SECONDS + SETTLE_MAX_S))
    while :; do
        sc stats settle "$1" "${ROUTER_LIST[@]}" || note "settle: a router gave no stats"
        if sc settled "$1"; then
            note "settled ($1) in $((SECONDS - start)) s"
            return 0
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            note "not settled ($1) after $SETTLE_MAX_S s: going on"
            return 0
        fi
        pause "$POLL_S"
    done
}

# Set when the topology could not be put back: the scenarios left end in
# `error`.
BROKEN=
restore() { up "$@" || BROKEN="$* did not start again"; }

# begin S: whether to run S, and its first line.
begin() {
    say "$1"
    if [ -n "$BROKEN" ]; then
        sc error "$1" "the topology is broken: $BROKEN"
        return 1
    fi
    if [ "$(sc declared "$1")" = not-applicable ]; then
        note "$1: declared not-applicable by the backend, not run"
        return 1
    fi
    sc event "$1" begin
}

# --- the run ---------------------------------------------------------------

preflight() {
    say "Run of $ID"
    local tool
    for tool in docker python3 cargo git; do
        command -v "$tool" > /dev/null || die "$tool is required"
    done
    note "date: $(date -u '+%F %T') UTC"
    note "commit: $(git -C "$REPO" rev-parse --short HEAD)"
    note "host: $(sed -n 's/^PRETTY_NAME="\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' /etc/os-release), kernel $(uname -r), $(nproc) CPU, load $(cut -d' ' -f1-3 /proc/loadavg)"
    note "docker: $(docker version --format '{{.Server.Version}}'); rustc: $(rustc --version)"
}

build() {
    say "Build"
    (cd "$SPIKES" && cargo build --quiet --release --locked -p "spike-$ID") || die "cargo build failed"
    note "spike-$ID: $(stat -c %s "$BIN") bytes"
    docker build -q -t "$IMAGE" - < "$HARNESS/Dockerfile" > /dev/null || die "docker build failed"
    note "image: $IMAGE $(docker image inspect -f '{{.Id}}' "$IMAGE" | cut -c1-19)…"
}

networks() {
    say "Networks"
    local n
    for n in "$NET_A" "$NET_B"; do
        [[ $n == "$P-"* ]] || die "network $n is not named $P-*"
        docker network create --internal --label "$LABEL=$WORK" "$n" > /dev/null || die "cannot create network $n"
        [ "$(docker network inspect -f '{{.Internal}}' "$n")" = true ] || die "network $n is not internal"
        note "$n: internal, $(docker network inspect -f '{{range .IPAM.Config}}{{.Subnet}}{{end}}' "$n")"
    done
}

setup() {
    say "Topology"
    local node others=()
    if ! create "$BOOTSTRAP" router "a b" || ! up "$BOOTSTRAP"; then die "$BOOTSTRAP does not start"; fi
    R1=$(sc get "$BOOTSTRAP" /stats bootstrap_addr) || die "$BOOTSTRAP gives no bootstrap_addr"
    note "$BOOTSTRAP is --bootstrap $R1"
    for node in "${ROUTER_LIST[@]}"; do
        [ "$node" = "$BOOTSTRAP" ] && continue
        create "$node" router "a b" --bootstrap "$R1" || die "cannot create $node"
        others+=("$node")
    done
    up "${others[@]}" || die "a router does not start"
    SAVED=$(sc get "$SAVED_CONTACT" /stats bootstrap_addr) || die "$SAVED_CONTACT gives no bootstrap_addr"
    note "$SAVED_CONTACT is --bootstrap $SAVED"
    for node in $PROVIDERS_A $PROVIDERS_B; do
        announces "$node"
        create "$node" provider "$([[ " $PROVIDERS_A " == *" $node "* ]] && echo a || echo b)" \
            --bootstrap "$R1" "${ANN[@]}" || die "cannot create $node"
    done
    for node in $CLIENTS_A $CLIENTS_B; do
        create "$node" client "$([[ " $CLIENTS_A " == *" $node "* ]] && echo a || echo b)" \
            --bootstrap "$R1" --bootstrap "$SAVED" || die "cannot create $node"
    done
    # shellcheck disable=SC2086 # lists of node names
    up $PROVIDERS_A $PROVIDERS_B $CLIENTS || die "a node does not start"
    local error
    for node in "${TOPOLOGY[@]}"; do
        note "$node: agent $(sc get "$node" /healthz agent_id)"
        if error=$(sc get "$node" /healthz announce_error); then
            note "WARNING: $node did not announce at start: $error"
        fi
    done
}

s0() {
    begin S0 || return 0
    local node=$S0_NODE
    if create "$node" client "$S0_NET" && sc event S0 start node="$node" && up "$node"; then
        pause "$S0_GRACE_S"
        sc search S0 grace --nodes "$node" --caps "$CAPABILITIES"
        sc stats S0 end "$node"
    else
        sc error S0 "$node did not start"
    fi
    docker stop -t 5 "$(ctr "$node")" > /dev/null 2>&1
}

s1() {
    begin S1 || return 0
    local round
    for ((round = 1; round <= SEARCHES; round++)); do
        sc search S1 "round=$round" --nodes "$CLIENTS" --caps "$CAPABILITIES" || note "S1: a search got no answer"
    done
}

s2() {
    begin S2 || return 0
    local node=$S2_NODE deadline
    sc event S2 stop node="$BOOTSTRAP"
    docker kill "$(ctr "$BOOTSTRAP")" > /dev/null || { sc error S2 "cannot stop $BOOTSTRAP"; return 0; }
    sc search S2 connected --nodes "$CLIENTS" --caps "$CAPABILITIES"
    if create "$node" client "$S2_NET" --bootstrap "$R1" --bootstrap "$SAVED" \
        && sc event S2 start node="$node" && up "$node"; then
        deadline=$((SECONDS + 2 * REPUBLISH_S))
        until sc reached S2 || [ "$SECONDS" -ge "$deadline" ]; do
            sc search S2 newcomer --nodes "$node" --caps "$CAPABILITIES"
        done
    else
        sc error S2 "$node did not start"
    fi
    docker stop -t 5 "$(ctr "$node")" > /dev/null 2>&1
    restore "$BOOTSTRAP" && settle after-S2
}

s3() {
    begin S3 || return 0
    local node=$S3_NODE caps i=0 deadline
    read -r -a caps <<< "$(caps_of "$node")"
    sc stats S3 before "${ROUTER_LIST[@]}"
    sc event S3 stop node="$node"
    docker kill "$(ctr "$node")" > /dev/null || { sc error S3 "cannot stop $node"; return 0; }
    deadline=$((SECONDS + TTL_S + 2 * REPUBLISH_S))
    until sc reached S3 || [ "$SECONDS" -ge "$deadline" ]; do
        pause "$POLL_S"
        sc stats S3 poll "${ROUTER_LIST[@]}"
        sc search S3 "poll=$i" --nodes "$CLIENTS" --caps "${caps[i % ${#caps[@]}]}"
        i=$((i + 1))
    done
    restore "$node" && settle after-S3
}

s4() {
    begin S4 || return 0
    local node i=0 deadline
    sc event S4 stop nodes="$S4_NODES"
    for node in $S4_NODES; do
        docker kill "$(ctr "$node")" > /dev/null || { sc error S4 "cannot stop $node"; return 0; }
    done
    deadline=$((SECONDS + 3 * REPUBLISH_S))
    until sc reached S4 || [ "$SECONDS" -ge "$deadline" ]; do
        sc search S4 "round=$i" --nodes "$CLIENTS" --caps "$CAPABILITIES"
        i=$((i + 1))
    done
    # shellcheck disable=SC2086 # a list of node names
    restore $S4_NODES && settle after-S4
}

s5() {
    begin S5 || return 0
    local node=$S5_NODE deadline
    sc stats S5 before "${ROUTER_LIST[@]}"
    announces "$node"
    if create "$node" provider "$S5_NET" --rogue --bootstrap "$R1" "${ANN[@]}" \
        && sc event S5 start node="$node" && up "$node"; then
        deadline=$((SECONDS + 2 * REPUBLISH_S))
        while :; do
            sc stats S5 poll "${ROUTER_LIST[@]}"
            if sc reached S5 || [ "$SECONDS" -ge "$deadline" ]; then break; fi
            pause "$POLL_S"
        done
        sc search S5 clients --nodes "$CLIENTS" --caps "$CAPS_rogue"
        sc stats S5 after "${ROUTER_LIST[@]}"
    else
        sc error S5 "$node did not start"
    fi
    docker stop -t 5 "$(ctr "$node")" > /dev/null 2>&1
}

s6() {
    begin S6 || return 0
    local end next
    settle before-S6
    sc counters S6 start "${TOPOLOGY[@]}"
    sc stats S6 sample "${TOPOLOGY[@]}"
    end=$((SECONDS + IDLE_S))
    while [ "$SECONDS" -lt "$end" ]; do
        next=$((end - SECONDS))
        [ "$next" -le "$IDLE_SAMPLE_S" ] || next=$IDLE_SAMPLE_S
        pause "$next"
        sc stats S6 sample "${TOPOLOGY[@]}"
    done
    sc counters S6 end "${TOPOLOGY[@]}"
}

s7() {
    begin S7 || return 0
    local node=$S7_NODE caps i=0 deadline ip
    read -r -a caps <<< "$(caps_of "$node")"
    sc event S7 move node="$node"
    if ! docker network disconnect "$NET_B" "$(ctr "$node")" \
        || ! docker network connect "$NET_A" "$(ctr "$node")"; then
        sc error S7 "cannot move $node from $NET_B to $NET_A"
        return 0
    fi
    ip=$(docker inspect -f "{{ (index .NetworkSettings.Networks \"$NET_A\").IPAddress }}" "$(ctr "$node")")
    sc event S7 moved node="$node" ip="$ip"
    sc announce S7 "$node" "${caps[@]}" || note "S7: the new announce failed"
    deadline=$((SECONDS + 3 * REPUBLISH_S))
    until sc reached S7 || [ "$SECONDS" -ge "$deadline" ]; do
        sc search S7 "poll=$i" --nodes "$CLIENTS" --caps "${caps[i % ${#caps[@]}]}"
        i=$((i + 1))
        pause "$POLL_S"
    done
}

results() {
    say "Results"
    local commit date out=$REPO/$RESULTS_REL
    commit=$(git -C "$REPO" rev-parse --short HEAD)
    if ! git -C "$REPO" diff --quiet HEAD -- Cargo.toml Cargo.lock crates spikes/hito-3 \
        || [ -n "$(git -C "$REPO" ls-files --others --exclude-standard -- crates spikes/hito-3)" ]; then
        commit=$commit-dirty
    fi
    date=$(date -u +%FT%TZ)
    mkdir -p "$(dirname "$out")"
    sc results --backend "$ID" --commit "$commit" --date "$date" --out "$out" || die "cannot write the results"
    note "results in $RESULTS_REL"
    python3 "$HARNESS/check_results.py" "$out"
}

if [ "$ONLY_TEARDOWN" = 1 ]; then
    teardown
    exit 0
fi

existing=$(any_containers; any_networks)
if [ -n "$existing" ]; then
    echo "harness.sh: $P-* resources exist, of another run: ${existing//$'\n'/ }." >&2
    echo "Run \`$0 --teardown <id>\` for the run that made them (their $LABEL label:" >&2
    echo "docker inspect -f '{{ index .Config.Labels \"$LABEL\" }}' <container>), or, if its directory is gone, remove them by hand after checking that label." >&2
    exit 1
fi
rm -rf -- "$WORK"
mkdir -p "$RAW" || exit 1
# The log's tee ignores SIGINT and SIGTERM, so that Ctrl-C, which reaches
# the whole process group, leaves it to log the teardown.
exec > >(trap '' INT TERM; exec tee -a "$WORK/harness.log") 2>&1
TEE_PID=$!

# shellcheck disable=SC2329 # run by the trap below
on_interrupt() {
    trap - INT TERM
    trap '' PIPE
    kill -0 "$TEE_PID" 2> /dev/null || exec >> "$WORK/harness.log" 2>&1
    note "interrupted: tearing down"
    [ "$TORN" = 1 ] || teardown
    exit 130
}
trap on_interrupt INT TERM
trap '[ "$TORN" = 1 ] || teardown' EXIT

preflight
build
networks
setup
settle initial
s0
s1
s2
s3
s4
s5
s6
s7
results
status=$?
teardown
exit "$status"
