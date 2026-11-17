#!/usr/bin/env bash
# The hito 3 exit test (docs/runbooks/hito-3-descubrimiento-kademlia.md) on
# one host: the DHT of two operators on two internal Docker networks, one
# container per agent, every daemon with `relay = false`, `[net] listen =
# "0.0.0.0:4433"`, `[dht] record_ttl_s = 120` and `republish_s = 40`, and the
# echo worker:
#
#   pals-exit-h3-red-a  10.213.73.0/24, internal (network A)
#   pals-exit-h3-red-b  10.213.74.0/24, internal (network B)
#   pals-exit-h3-r1 … -r4  the routers, on both networks, both addresses in
#                       their external_addrs: r1 (no bootstrap, the original
#                       entry point) and r2 of operator 1; r3 (through r1)
#                       and r4 (through r3) of operator 2
#   pals-exit-h3-p1, -p2   providers of operator 1 on A, through r1
#   pals-exit-h3-p3, -p4   providers of operator 2 on B, through r3
#   pals-exit-h3-c1, -c2   clients of operator 1 on A, through r1
#   pals-exit-h3-c3, -c4   clients of operator 2 on B, through r3
#   pals-exit-h3-c0, -cx, -c5, -rogue   only for their case (1, 1, 3, 6)
#
# Each agent's config.toml comes from tests/exit/hito-3/operador-1/ or
# operador-2/, its bootstrap tickets filled in when the routers run; the
# providers' inventories, from <agent>-inventario.toml there. Tickets and
# grant tokens pass from one container to another through shell variables,
# never printed.
#
# Usage:
#   PALS_EXIT_DIR=<new or empty dir> tests/exit/hito-3.sh [phase...]
# Phases, in order: preparar caso1 … caso9 desmontar; `todo` (the default)
# runs them all. Each case prints `RESULTADO <case>: superado|FALLIDO`,
# collected in $PALS_EXIT_DIR/registro/resultados.txt; the full log is
# registro/run.log. A case that stops agents starts them again at its end,
# so the next one finds the whole topology (cases 3, 4 and 5). See
# tests/exit/README.md.
#
# Safety, as tests/exit/hito-2.sh: `preparar` only accepts a new or empty
# PALS_EXIT_DIR and marks it with .pals-exit; every other phase refuses a
# directory without that mark. `preparar` refuses to start while any
# pals-exit-h3-* container or network exists, and when a Docker network
# overlaps the two subnets. Every container and network is named
# pals-exit-h3-* and carries the label pals-exit.dir=<PALS_EXIT_DIR>;
# `desmontar` removes only those whose label is this directory, and the image
# only with PALS_EXIT_KEEP_IMAGE=0 and its pals-exit.image label. Nothing is
# run on the host's network: no iptables, nft, tc or sysctl, and no prune.
# Only the pals binary, the dht_rogue example, metricas.py and the agents'
# config files go into a container, never a credential. An interrupted run
# (Ctrl-C, SIGTERM) tears itself down, and so does `todo` on any exit; after
# any other end, run `desmontar` with the same PALS_EXIT_DIR.
#
# Environment:
#   PALS_EXIT_DIR         required: the logs (registro/) go here
#   PALS_EXIT_BIN         the pals binary [target/release/pals]
#   PALS_EXIT_ROGUE       case 6: the dht_rogue example
#                         [target/release/examples/dht_rogue]
#   PALS_EXIT_REPOSO_S    the idle measurement of `preparar`, in seconds [300];
#                         shorter only for a rehearsal, never for the record
#   PALS_EXIT_KEEP_IMAGE  1 (default) keeps the image on `desmontar`; 0
#                         removes it

set -uo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HERE=$REPO/tests/exit/hito-3
WORK=${PALS_EXIT_DIR:?set PALS_EXIT_DIR to a new or empty directory for the logs}
WORK=$(realpath -m -- "$WORK")
MARKER=$WORK/.pals-exit
PALS=${PALS_EXIT_BIN:-$REPO/target/release/pals}
ROGUE=${PALS_EXIT_ROGUE:-$REPO/target/release/examples/dht_rogue}
KEEP_IMAGE=${PALS_EXIT_KEEP_IMAGE:-1}
P=pals-exit-h3
LABEL=pals-exit.dir
NET_A=$P-red-a
NET_B=$P-red-b
SUBNET_A=10.213.73.0/24
SUBNET_B=10.213.74.0/24
IMAGE=$P:latest
LOGS=$WORK/registro
# Every agent's PALS_HOME is $H/<agent> in its container.
H=/root/pals-h3
# The DHT's times, as the operators' config files have them.
TTL_S=120
REPUBLISH_S=40
# The idle measurement: how long, and a sample every so often.
IDLE_S=${PALS_EXIT_REPOSO_S:-300}
IDLE_SAMPLE_S=60
# The pause between two polls of the routers.
POLL_S=3
# Ceilings: a wait that reaches one fails; none is a bound of the cases.
SETTLE_MAX_S=300
FIND_MAX_S=240
# Case 7: the burst, and how many at once (above the daemon's 4).
BURST=200
BURST_AT_ONCE=8

ROUTERS=(r1 r2 r3 r4)
PROVIDERS=(p1 p2 p3 p4)
CLIENTS=(c1 c2 c3 c4)
TOPOLOGY=("${ROUTERS[@]}" "${PROVIDERS[@]}" "${CLIENTS[@]}")
CAPABILITIES=(vision.ocr code.rust.review knowledge.documentation vision.defect-detection)
declare -A CAPS=(
    [p1]="vision.ocr knowledge.documentation"
    [p2]="code.rust.review vision.defect-detection"
    [p3]="vision.ocr vision.defect-detection"
    [p4]="code.rust.review knowledge.documentation"
)
# p2's private and project capabilities (p2-inventario.toml), never in the DHT.
HIDDEN_P2="audio.transcribe data.extraction"
# Where each agent is: <network>=<ip>, the first one its container starts on.
declare -A ADDRS=(
    [r1]="a=10.213.73.11 b=10.213.74.11" [r2]="a=10.213.73.12 b=10.213.74.12"
    [r3]="a=10.213.73.13 b=10.213.74.13" [r4]="a=10.213.73.14 b=10.213.74.14"
    [p1]="a=10.213.73.21" [p2]="a=10.213.73.22" [p3]="b=10.213.74.23" [p4]="b=10.213.74.24"
    [c1]="a=10.213.73.31" [c2]="a=10.213.73.32" [c3]="b=10.213.74.33" [c4]="b=10.213.74.34"
    [c0]="a=10.213.73.40" [c5]="a=10.213.73.45" [cx]="a=10.213.73.46" [rogue]="b=10.213.74.50"
)
# Case 9: p3's address once it moves to network A.
P3_MOVED=10.213.73.23
declare -A OPERATOR=(
    [r1]=1 [r2]=1 [p1]=1 [p2]=1 [c1]=1 [c2]=1 [c0]=1 [c5]=1 [cx]=1
    [r3]=2 [r4]=2 [p3]=2 [p4]=2 [c3]=2 [c4]=2
)
# The same topology for the checks, which read it from H3_TOPO: each
# provider's capabilities, each agent's networks ("a", "b" or "ab") and the
# routers.
topologia_json() {
    local node spec letters caps= nets= routers=
    for node in "${!CAPS[@]}"; do caps+="\"$node\": [\"${CAPS[$node]// /\", \"}\"], "; done
    for node in "${!ADDRS[@]}"; do
        letters=
        for spec in ${ADDRS[$node]}; do letters+=${spec%%=*}; done
        nets+="\"$node\": \"$letters\", "
    done
    for node in "${ROUTERS[@]}"; do routers+="\"$node\", "; done
    printf '{"caps": {%s}, "nets": {%s}, "routers": [%s]}' "${caps%, }" "${nets%, }" "${routers%, }"
}
TOPO_JSON=$(topologia_json)

phases=("$@")
[ ${#phases[@]} -gt 0 ] || phases=(todo)
TODO=0
if [ "${phases[0]}" = todo ]; then
    TODO=1
    phases=(preparar caso1 caso2 caso3 caso4 caso5 caso6 caso7 caso8 caso9 desmontar)
fi
for phase in "${phases[@]}"; do
    case $phase in
        preparar | caso[1-9] | desmontar) ;;
        *) echo "hito-3.sh: unknown phase $phase" >&2; exit 2 ;;
    esac
done
if [[ " ${phases[*]:1} " == *" preparar "* ]]; then
    echo "hito-3.sh: preparar must be the first phase" >&2
    exit 2
fi
if [ "${phases[0]}" = preparar ]; then
    if [ -e "$WORK" ] && { [ ! -d "$WORK" ] || [ -n "$(ls -A -- "$WORK")" ]; }; then
        echo "hito-3.sh: $WORK exists and is not an empty directory: preparar needs a new or empty PALS_EXIT_DIR" >&2
        exit 2
    fi
    mkdir -p -- "$WORK" && printf 'pals-exit harness, hito 3\n' > "$MARKER" || exit 2
elif [ ! -f "$MARKER" ]; then
    echo "hito-3.sh: $WORK has no .pals-exit mark: it was not made by \`preparar\`, and no phase but preparar touches it" >&2
    exit 2
fi

mkdir -p "$LOGS/busquedas" "$LOGS/casos"
# The log's tee ignores SIGINT and SIGTERM, so that Ctrl-C, which reaches
# the whole process group, leaves it to log the teardown.
exec > >(trap '' INT TERM; exec tee -a "$LOGS/run.log") 2>&1
TEE_PID=$!

# --- helpers ---------------------------------------------------------------

ts() { date -u +%T; }
now() { date +%s.%N; }
say() { printf '\n[%s] == %s\n' "$(ts)" "$*"; }
note() { printf '[%s] %s\n' "$(ts)" "$*"; }
die() {
    note "ERROR: $*"
    [ "$TODO" = 1 ] || note "para limpiar: PALS_EXIT_DIR=$WORK $0 desmontar"
    exit 1
}
result() {   # result CASE ok|fail [DETAIL]
    local verdict=superado
    [ "$2" = ok ] || verdict=FALLIDO
    printf '[%s] RESULTADO %s: %s%s\n' "$(ts)" "$1" "$verdict" "${3:+ ($3)}" \
        | tee -a "$LOGS/resultados.txt"
    [ "$2" = ok ]
}
# check WHAT CONDITION...: runs the condition; on failure, notes WHAT and
# counts it in FAILS.
FAILS=0
check() {
    local what=$1
    shift
    if "$@"; then note "  ok: $what"; else note "  NO: $what"; FAILS=$((FAILS + 1)); fi
}
has_line() { grep -qxF -- "$2" <<< "$1"; }
# A wait that a trapped signal interrupts at once.
pausa() { sleep "$1" & wait $!; }
load() { cut -d' ' -f1-3 /proc/loadavg; }

ctr() { echo "$P-$1"; }
net_name() { case $1 in a) echo "$NET_A" ;; b) echo "$NET_B" ;; *) return 1 ;; esac; }
# en X: runs the bash script on stdin in X's container, with PALS_HOME set
# to X's. The script is read whole first, so that the commands it runs never
# read it as their input.
en() { docker exec -i -e "PALS_HOME=$H/$1" "$(ctr "$1")" bash -c 'cd; eval "$(cat)"'; }
# en_wait FILE X: en X, its output in FILE, in the background and waited
# for, so that a trapped signal interrupts the wait at once.
en_wait() { en "$2" <&0 > "$1" 2>&1 & wait $!; }
# put X PATH: copies stdin to PATH in X's container; the path is an argument
# of the remote shell, never part of its script.
put() { docker exec -i "$(ctr "$1")" sh -c 'cat > "$1"' sh "$2"; }

# Ours: named $P-*, labelled with this PALS_EXIT_DIR.
ours_containers() { docker ps -a --filter "label=$LABEL=$WORK" --format '{{.Names}}' | grep "^$P-" || true; }
ours_networks() { docker network ls --filter "label=$LABEL=$WORK" --format '{{.Name}}' | grep "^$P-" || true; }
any_containers() { docker ps -a --format '{{.Names}}' | grep "^$P-" || true; }
any_networks() { docker network ls --format '{{.Name}}' | grep "^$P-" || true; }
container_label() { docker inspect -f "{{ index .Config.Labels \"$LABEL\" }}" "$1" 2> /dev/null; }
network_label() { docker network inspect -f "{{ index .Labels \"$LABEL\" }}" "$1" 2> /dev/null; }
ours() { [ "$(container_label "$(ctr "$1")")" = "$WORK" ]; }
running() { [ "$(docker inspect -f '{{.State.Running}}' "$(ctr "$1")" 2> /dev/null)" = true ]; }

# where X: "<network>=<ip> …" as Docker has X now, sorted.
where() {
    docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}}={{$v.IPAddress}} {{end}}' \
        "$(ctr "$1")" 2> /dev/null | tr ' ' '\n' | grep . | sort | tr '\n' ' '
}
# planned X [ADDRS]: the same, as ADDRS pins it.
planned() {
    local spec
    for spec in ${2:-${ADDRS[$1]}}; do echo "$(net_name "${spec%%=*}")=${spec#*=}"; done | sort | tr '\n' ' '
}
# placed X [ADDRS]: whether X is at its pinned addresses.
placed() {
    if [ "$(where "$1")" != "$(planned "$1" "${2:-}")" ]; then
        note "$1 is at '$(where "$1")', not at its pinned '$(planned "$1" "${2:-}")'"
        return 1
    fi
}

# crea X: X's container on its networks at its pinned addresses, started,
# with the pals binary and metricas.py in it; one CPU and 512 MiB. One of
# ours left by an earlier run of the same case goes first.
crea() {
    local node=$1 first=1 spec
    if docker inspect "$(ctr "$node")" > /dev/null 2>&1; then
        ours "$node" || { note "$(ctr "$node") exists and is not ours"; return 1; }
        docker rm -f "$(ctr "$node")" > /dev/null || return 1
    fi
    # A new container is a new agent: its id is learnt again.
    unset 'ID[$node]'
    ids_set "$node"
    for spec in ${ADDRS[$node]}; do
        if [ "$first" = 1 ]; then
            docker create --init --name "$(ctr "$node")" --hostname "$node" --label "$LABEL=$WORK" \
                --cpus 1 --memory 512m --network "$(net_name "${spec%%=*}")" --ip "${spec#*=}" \
                "$IMAGE" > /dev/null || return 1
            first=0
        else
            docker network connect --ip "${spec#*=}" "$(net_name "${spec%%=*}")" "$(ctr "$node")" || return 1
        fi
    done
    docker cp "$PALS" "$(ctr "$node"):/usr/local/bin/pals" > /dev/null || return 1
    docker start "$(ctr "$node")" > /dev/null || return 1
    docker exec "$(ctr "$node")" mkdir -p "$H/$node" || return 1
    docker cp "$HERE/metricas.py" "$(ctr "$node"):$H/metricas.py" > /dev/null || return 1
    placed "$node"
}

# The routers' tickets, which the configs of those that enter through them
# carry: filled in when needed, never printed.
T1=
T3=
ticket() { en "$1" <<< 'pals id --ticket'; }
# configura X: X's config.toml from its operator's directory, with the
# tickets filled in, and its inventory, if it has one.
configura() {
    local node=$1 dir=$HERE/operador-${OPERATOR[$1]} text
    if { grep -q '@TICKET_R1@' "$dir/$node.toml" && [ -z "$T1" ]; } \
        || { grep -q '@TICKET_R3@' "$dir/$node.toml" && [ -z "$T3" ]; }; then
        note "$node's config needs a router's ticket not known yet"
        return 1
    fi
    text=$(sed -e "s|@TICKET_R1@|$T1|g" -e "s|@TICKET_R3@|$T3|g" "$dir/$node.toml") || return 1
    put "$node" "$H/$node/config.toml" <<< "$text" || return 1
    if [ -f "$dir/$node-inventario.toml" ]; then
        put "$node" "$H/$node/inventory.toml" < "$dir/$node-inventario.toml" || return 1
    fi
}
# arranca X: `pals init` the first time, and X's daemon in the background;
# waits until its gateway gives its ticket.
arranca() {
    en "$1" <<'EOS'
[ -f "$PALS_HOME/agent.key" ] || pals init > /dev/null || exit 1
nohup pals daemon >> "$PALS_HOME/daemon.log" 2>&1 < /dev/null &
echo $! > "$PALS_HOME/daemon.pid"
for i in $(seq 1 240); do
    pals id --ticket > /dev/null 2>&1 && exit 0
    kill -0 "$(cat "$PALS_HOME/daemon.pid")" 2> /dev/null || break
    sleep 0.5
done
echo "the daemon did not start; its last lines:"
tail -n 20 "$PALS_HOME/daemon.log"
exit 1
EOS
}
# para X: stops X with no notice, its whole container (docker kill).
para() { docker kill "$(ctr "$1")" > /dev/null; }
# restaura X...: starts X's container again, at the same addresses, and its
# daemon, with the same data directory.
restaura() {
    local node
    for node in "$@"; do
        docker start "$(ctr "$node")" > /dev/null || return 1
        placed "$node" || return 1
        arranca "$node" || return 1
        note "$node arrancado otra vez"
    done
}
# vivo X: "<pid>:<start time>" of X's daemon while it runs, nothing once it
# does not: a daemon that died, or another one, never reads the same.
vivo() {
    en "$1" <<< 'p=$(cat "$PALS_HOME/daemon.pid") && kill -0 "$p" 2> /dev/null && echo "$p:$(cut -d" " -f22 "/proc/$p/stat")"'
}

# Agent ids, in registro/ids.json, which the checks read.
declare -A ID
# ids_set X [ID]: X's agent id in registro/ids.json; without one, X's goes.
ids_set() {
    python3 - "$LOGS/ids.json" "$1" "${2:-}" <<'PY'
import json, sys
from pathlib import Path
path, node, agent = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
ids = json.loads(path.read_text()) if path.exists() else {}
if agent:
    ids[node] = agent
else:
    ids.pop(node, None)
path.write_text(json.dumps(ids, indent=1, sort_keys=True) + "\n")
PY
}
agent_id() {
    if [ -z "${ID[$1]:-}" ]; then
        ID[$1]=$(en "$1" <<< 'pals id') || return 1
        ids_set "$1" "${ID[$1]}"
    fi
    echo "${ID[$1]}"
}
if [ -f "$LOGS/ids.json" ]; then
    while read -r node id; do ID[$node]=$id; done < <(python3 -c '
import json, sys
for k, v in json.load(open(sys.argv[1])).items():
    print(k, v)' "$LOGS/ids.json")
fi

# estados TAG X...: a sample of each X (metricas.py muestra: its interfaces,
# its memory and its GET /dht), appended to registro/estados.jsonl and kept
# alone in $WORK/ultimo.jsonl for the next check; null for one that does not
# answer.
estados() {
    local tag=$1 node line
    shift
    : > "$WORK/ultimo.jsonl"
    for node in "$@"; do
        line=$(docker exec "$(ctr "$node")" python3 "$H/metricas.py" muestra "$H/$node" 2> /dev/null) || line=
        [ -n "$line" ] || line=null
        printf '{"etiqueta": "%s", "nodo": "%s", "muestra": %s}\n' "$tag" "$node" "$line" \
            | tee -a "$LOGS/estados.jsonl" >> "$WORK/ultimo.jsonl"
    done
}

# cuerpo CAPABILITY [MORE]: the body of POST /discovery/search for it, with
# MORE fields of the search, if any.
cuerpo() { printf '{"search": {"capability": "%s"%s}}' "$1" "${2:+, $2}"; }
# busca X NAME BODY: X's search, with metricas.py busca, kept in
# registro/busquedas/NAME.json.
busca() {
    docker exec "$(ctr "$1")" python3 "$H/metricas.py" busca "$2" "$3" > "$LOGS/busquedas/$2.json" \
        2>> "$LOGS/busquedas/errores.txt"
}
# limpia CASE: the searches a run of CASE before this one kept
# (registro/busquedas/CASE-* and CASEb-*) go, so that a case run again in
# the same PALS_EXIT_DIR judges only its own.
limpia() { rm -f -- "$LOGS/busquedas/$1"-* "$LOGS/busquedas/$1"[a-z]-*; }
# ronda TAG "X…" "CAPABILITY…": every X searches every capability, one
# after another; the X at the same time. Searches named TAG-X-CAPABILITY.
ronda() {
    local tag=$1 node cap pids=()
    for node in $2; do
        (for cap in $3; do busca "$node" "$tag-$node-$cap" "$(cuerpo "$cap")"; done) &
        pids+=($!)
    done
    wait "${pids[@]}"
}

# The checks over what was kept: Python, the shared part first (PY_COMUN),
# then the script on stdin, with the arguments after.
PY_COMUN=$(cat <<'PY'
import json
import os
import sys
from pathlib import Path

LOGS = Path(os.environ["H3_LOGS"])
WORK = Path(os.environ["H3_WORK"])
IDS = json.loads((LOGS / "ids.json").read_text()) if (LOGS / "ids.json").exists() else {}
NODE_OF = {v: k for k, v in IDS.items()}
# The topology, as the harness's maps define it (topologia_json).
TOPO = json.loads(os.environ["H3_TOPO"])
CAPS = TOPO["caps"]
NETS = dict(TOPO["nets"])
# Case 9: p3 on network A once it moved.
if os.environ.get("H3_P3") == "a":
    NETS["p3"] = "a"
ROUTERS = TOPO["routers"]
NODES = set(TOPO["nets"])
CAPS_ALL = sorted({c for caps in CAPS.values() for c in caps})
# The reasons of a candidate discarded at the card because it cannot be
# reached: no answer in its time, or no path to it.
UNREACHABLE = ("timeout", "card: peer unreachable")


def name(agent):
    return NODE_OF.get(agent, agent[:8])


def search(path):
    """A kept search. One whose file is missing, empty or not a search
    stands as a placeholder that every check reports as a problem: its node
    from its name, no answer, and a t1 that sorts it last."""
    path = Path(path)
    try:
        doc = json.loads(path.read_text())
        if isinstance(doc, dict) and "nodo" in doc and "t1" in doc:
            return doc
    except (OSError, ValueError):
        pass
    node = next((t for t in path.stem.split("-") if t in NODES), None)
    return {"nombre": path.stem, "nodo": node, "cuerpo": None, "http": None, "respuesta": None,
            "error": "no es una búsqueda legible", "t0": float("inf"), "t1": float("inf")}


def searches(prefix):
    return [search(f) for f in sorted((LOGS / "busquedas").glob(prefix + "*.json"))]


def lines(path):
    path = Path(path)
    if not path.exists():
        return []
    return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]


def latest():
    """The last sample of each node (estados), None for one that did not
    answer."""
    return {l["nodo"]: l["muestra"] for l in lines(WORK / "ultimo.jsonl")}


def dht(sample):
    return (sample or {}).get("dht")


def tagged(tag):
    return {l["nodo"]: l["muestra"] for l in lines(LOGS / "estados.jsonl") if l["etiqueta"] == tag}


def outcome(s):
    o = s.get("respuesta")
    return o if s.get("http") == 200 and isinstance(o, dict) and "ranked" in o else None


def unanswered(ss):
    """Of the searches `ss`, those with no answer of the daemon's: a file
    that is not a search, or no HTTP 200 with a search's outcome."""
    return [s["nombre"] for s in ss if outcome(s) is None]


def capability(s):
    return ((s.get("cuerpo") or {}).get("search") or {}).get("capability", "?")


def expected(cap):
    return [p for p, caps in CAPS.items() if cap in caps]


def parts(o):
    ranked = {c.get("agent_id") or c["peer"]: c for c in o["ranked"]}
    discarded = {d["peer"]: d for d in o["discarded"]}
    return ranked, discarded


def verified(c, agent, requester):
    """Ranked as a provider the DHT found, its card and its offer verified:
    the card signed by the agent, the offer the agent's, for the requester."""
    offer = c.get("offer") or {}
    return (c.get("from_dht") is True and c.get("agent_id") == agent
            and c.get("card_signature") == {"status": "verified", "kid": agent}
            and offer.get("agent_id") == agent and offer.get("requester") == requester)


def unreachable(d):
    return d is not None and d["stage"] == "card" and d["reason"] in UNREACHABLE


def problems(s, tolerated=(), contacts=()):
    """The ruling of Q1 on one search: of each provider of the capability,
    the one on the searcher's network ranked, from the DHT, with its card
    and its offer verified; the one on the other network discarded at the
    card as unreachable, which says the DHT gave it. Anything else found is
    a problem, but for the agents in `tolerated` discarded with a reason of
    the DHT's (the rogue, until its records expire), and for the searcher's
    own `contacts`, by name, discarded at the card for not having the
    capability (c5's r3)."""
    o = outcome(s)
    if o is None:
        return [f"HTTP {s.get('http')} {s.get('error') or ''}".strip()]
    client, cap = s["nodo"], capability(s)
    ranked, discarded = parts(o)
    out = []
    for p in expected(cap):
        agent = IDS[p]
        if NETS[p] == NETS[client]:
            c = ranked.get(agent)
            if c is None:
                d = discarded.get(agent)
                out.append(f"{p} fuera del ranking ({d['stage']}: {d['reason']})" if d else f"{p} no encontrado")
            elif not verified(c, agent, IDS[client]):
                out.append(f"{p} en el ranking sin from_dht, card u oferta verificadas")
        else:
            d = discarded.get(agent)
            if not unreachable(d):
                out.append(f"{p} " + ("en el ranking" if agent in ranked else
                                      f"descartado {d['stage']}: {d['reason']}" if d else "no encontrado"))
    known = {IDS[p] for p in expected(cap)}
    for agent in ranked:
        if agent not in known:
            out.append(f"{name(agent)} en el ranking")
    for agent, d in discarded.items():
        if agent in known or (agent in tolerated and d["reason"].startswith("dht: ")):
            continue
        if agent in contacts and d["stage"] == "card" and d["reason"] == f"no skill of {cap}":
            continue
        out.append(f"{name(agent)} descartado {d['stage']}: {d['reason']}")
    return out


def found(s, p):
    """Whether the DHT gave provider p in this search: ranked as found
    there, or discarded at the card as unreachable."""
    o = outcome(s)
    if o is None:
        return False
    ranked, discarded = parts(o)
    agent = IDS[p]
    return (agent in ranked and ranked[agent].get("from_dht") is True) or unreachable(discarded.get(agent))


def both_found(s):
    want = expected(capability(s))
    return outcome(s) is not None and bool(want) and all(found(s, p) for p in want)


def mentions(s, agent):
    o = outcome(s)
    if o is None:
        return False
    ranked, discarded = parts(o)
    return agent in ranked or agent in discarded


def coverage(o):
    c = o["coverage"]
    return "complete" if c == "complete" else "partial (" + ", ".join(c["partial"]["reasons"]) + ")"


def rounds_seen(prefix, after):
    """Of each provider, the rounds of publication its samples tagged
    `prefix…` show that ended after `after` (Unix seconds), each once: when
    it ended, its error and what failed. A round that ended before is no
    round of what came after: its samples only show the last one."""
    out = {}
    for l in lines(LOGS / "estados.jsonl"):
        if not l["etiqueta"].startswith(prefix) or not dht(l["muestra"]):
            continue
        pub = dht(l["muestra"]).get("publication")
        if pub and pub.get("last_round_unix") and pub["last_round_unix"] > after:
            seen = out.setdefault(l["nodo"], {})
            seen[pub["last_round_unix"]] = (pub.get("last_error"), pub.get("failed_capabilities"))
    return {node: [(t, *v) for t, v in sorted(r.items())] for node, r in sorted(out.items())}


def save(case, data):
    (LOGS / "casos" / f"{case}.json").write_text(json.dumps(data, indent=1, sort_keys=True, ensure_ascii=False) + "\n")
PY
)
py() {
    { printf '%s\n' "$PY_COMUN"; cat; } \
        | H3_LOGS=$LOGS H3_WORK=$WORK H3_TOPO=$TOPO_JSON H3_P3=${H3_P3:-b} python3 - "$@"
}
# passed STATUS OUT: whether a check script ran to its end, with status 0,
# and found nothing wrong: its line OK. A script that crashes never passes.
passed() { [ "$1" = 0 ] && has_line "$2" OK; }

# espera_tiendas TAG MAX "R…" "P…": polls the routers R until each holds, of
# each provider P, its contact, its announce and a provider record of each
# of its capabilities; fails after MAX seconds. Prints the seconds it took.
espera_tiendas() {
    local tag=$1 max=$2 routers=$3 providers=$4 start=$SECONDS i=0
    while :; do
        # shellcheck disable=SC2086 # a list of node names
        estados "$tag-$i" $routers
        if py "$routers" "$providers" <<'PY'
routers, providers = sys.argv[1].split(), sys.argv[2].split()
st = latest()
ok = True
for r in routers:
    d = dht(st.get(r))
    held = (d or {}).get("stored_by_agent", {})
    for p in providers:
        if held.get(IDS[p], 0) != 2 + len(CAPS[p]):
            ok = False
sys.exit(0 if ok else 1)
PY
        then
            note "tiendas completas ($tag) en $((SECONDS - start)) s"
            return 0
        fi
        if [ $((SECONDS - start)) -ge "$max" ]; then
            note "tiendas incompletas ($tag) tras $max s:"
            py "$routers" "$providers" <<'PY'
st = latest()
for r in sys.argv[1].split():
    d = dht(st.get(r))
    held = (d or {}).get("stored_by_agent", {})
    print(f"  {r}: " + (", ".join(f"{p} {held.get(IDS[p], 0)}" for p in sys.argv[2].split()) if d else "sin estado"))
PY
            return 1
        fi
        i=$((i + 1))
        pausa "$POLL_S"
    done
}
# estado_red TAG: one line per node of the topology: its DHT mode, routing
# peers, connections and what it stores.
estado_red() {
    estados "$1" "${TOPOLOGY[@]}"
    py <<'PY'
for node, sample in latest().items():
    d = dht(sample)
    if d is None:
        print(f"  {node}: sin estado")
        continue
    held = {name(a): n for a, n in d["stored_by_agent"].items()}
    pub = d.get("publication") or {}
    print(f"  {node}: {d['mode']}, routing_peers {d['routing_peers']}, connections {d['connections']}, "
          f"records {d['records_stored']}, provider records {d['provider_records_stored']}, por agente {held}"
          + (f", publicación {pub.get('capabilities')} error {pub.get('last_error')}" if pub else ""))
PY
}

# --- phases ----------------------------------------------------------------

preparar() {
    say "Preparación: imagen, redes y contenedores; los dos operadores"
    [ -x "$PALS" ] || die "no pals binary at $PALS: cargo build --release --locked -p pals"
    [ -x "$ROGUE" ] || die "no dht_rogue at $ROGUE: cargo build --release --locked -p pals --examples"
    local tool
    for tool in docker python3 git; do command -v "$tool" > /dev/null || die "$tool is required"; done
    local existing
    existing=$(any_containers; any_networks)
    if [ -n "$existing" ]; then
        die "$P-* resources exist, of another run: $(echo $existing); run \`desmontar\` with its PALS_EXIT_DIR (their $LABEL label: docker inspect -f '{{ index .Config.Labels \"$LABEL\" }}' <container>), or, if that directory is gone, remove them by hand after checking that label"
    fi
    # Read-only: every subnet Docker has, against the two of the test.
    local overlaps
    overlaps=$(docker network ls -q | xargs docker network inspect -f '{{.Name}} {{range .IPAM.Config}}{{.Subnet}} {{end}}' \
        | python3 -c '
import ipaddress, sys
ours = [ipaddress.ip_network(s) for s in sys.argv[1:]]
bad = []
for line in sys.stdin:
    name, *subnets = line.split()
    for s in subnets:
        try:
            net = ipaddress.ip_network(s, strict=False)
        except ValueError:
            continue
        if any(net.version == o.version and net.overlaps(o) for o in ours):
            bad.append(f"{name} {s}")
print("; ".join(bad))
sys.exit(1 if bad else 0)' "$SUBNET_A" "$SUBNET_B") \
        || die "a Docker network overlaps $SUBNET_A or $SUBNET_B: $overlaps"

    note "fecha: $(date -u '+%F %T') UTC"
    note "commit: $(git -C "$REPO" rev-parse --short HEAD)$([ -z "$(git -C "$REPO" status --porcelain)" ] || echo ' (con cambios sin commit)')"
    note "pals: $("$PALS" --version), sha256 $(sha256sum "$PALS" | cut -c1-16)…; dht_rogue: sha256 $(sha256sum "$ROGUE" | cut -c1-16)…"
    note "anfitrión: $(. /etc/os-release && echo "$PRETTY_NAME"), kernel $(uname -r), $(nproc) CPU, carga $(load)"
    note "docker: $(docker version --format '{{.Server.Version}}'); rustc: $(rustc --version 2> /dev/null || echo 'no instalado')"

    docker build -q -t "$IMAGE" - < "$HERE/Dockerfile" > /dev/null 2> "$LOGS/imagen.log" \
        || die "docker build failed: $(tail -n 5 "$LOGS/imagen.log")"
    note "imagen: $IMAGE $(docker image inspect -f '{{.Id}}' "$IMAGE" | cut -c1-19)…, de debian:trixie-slim@sha256:a72b4433…"
    local net subnet out
    for net in "$NET_A" "$NET_B"; do
        subnet=$SUBNET_A
        [ "$net" = "$NET_B" ] && subnet=$SUBNET_B
        out=$(docker network create --internal --subnet "$subnet" --label "$LABEL=$WORK" "$net" 2>&1) \
            || die "cannot create network $net on $subnet: $out"
        [ "$(docker network inspect -f '{{.Internal}}' "$net")" = true ] || die "network $net is not internal"
        note "red $net: interna, $subnet"
    done
    local node
    for node in "${TOPOLOGY[@]}"; do crea "$node" || die "cannot create $node's container"; done
    note "contenedores: $(docker exec "$(ctr r1)" sh -c '. /etc/os-release; echo "$PRETTY_NAME, $(pals --version), $(python3 --version)"')"
    for node in "${TOPOLOGY[@]}"; do note "  $node: $(where "$node")"; done

    say "Arranque: operador 1 (r1, r2), operador 2 (r3 por r1, r4 por r3); proveedores y clientes"
    configura r1 && arranca r1 || die "r1 did not start"
    T1=$(ticket r1) || die "no ticket from r1"
    for node in r2 r3; do configura "$node" && arranca "$node" || die "$node did not start"; done
    T3=$(ticket r3) || die "no ticket from r3"
    configura r4 && arranca r4 || die "r4 did not start"
    for node in "${PROVIDERS[@]}" "${CLIENTS[@]}"; do
        configura "$node" && arranca "$node" || die "$node did not start"
    done
    for node in "${TOPOLOGY[@]}"; do note "id de $node: $(agent_id "$node")"; done
    for node in "${PROVIDERS[@]}"; do
        note "$node: $(en "$node" <<< 'grep -c "issued a signed record" "$PALS_HOME/daemon.log"') registros emitidos; $(en "$node" <<< 'grep "pals daemon:" "$PALS_HOME/daemon.log" | tail -n 1')"
    done

    say "Asentamiento: cada router guarda lo de cada proveedor"
    espera_tiendas asentado "$SETTLE_MAX_S" "${ROUTERS[*]}" "${PROVIDERS[*]}" || die "the routers did not get every provider's records"
    estado_red red-inicial

    say "Reposo: $IDLE_S s sin búsquedas, una muestra de cada agente cada $IDLE_SAMPLE_S s"
    local end next
    note "carga al empezar el reposo: $(load)"
    : > "$LOGS/reposo.jsonl"
    reposo_muestra() {
        local node line
        for node in "${TOPOLOGY[@]}"; do
            line=$(docker exec "$(ctr "$node")" python3 "$H/metricas.py" muestra "$H/$node") || continue
            printf '{"nodo": "%s", "muestra": %s}\n' "$node" "$line" >> "$LOGS/reposo.jsonl"
        done
    }
    reposo_muestra
    end=$((SECONDS + IDLE_S))
    while [ "$SECONDS" -lt "$end" ]; do
        next=$((end - SECONDS))
        [ "$next" -le "$IDLE_SAMPLE_S" ] || next=$IDLE_SAMPLE_S
        pausa "$next"
        reposo_muestra
    done
    note "carga al acabar el reposo: $(load)"
    python3 "$HERE/metricas.py" informe "$LOGS" | grep '^reposo' || true
}

caso1() {
    say "Caso 1. Sin fuga"
    limpia caso1
    local ok=ok net code out st
    FAILS=0
    note "carga: $(load)"
    for net in "$NET_A" "$NET_B"; do
        check "$net: Internal=true" test "$(docker network inspect -f '{{.Internal}}' "$net")" = true
    done

    note "c0: la DHT encendida, sin arranque ni contactos"
    crea c0 && configura c0 && arranca c0 || die "c0 did not start"
    agent_id c0 > /dev/null
    # As phase 1 (S0): time to find anyone, had it a way.
    pausa 10
    local cap
    for cap in "${CAPABILITIES[@]}"; do busca c0 "caso1-c0-$cap" "$(cuerpo "$cap")"; done
    estados caso1-c0 c0

    note "cx: sin [dht]; su único contacto, c0"
    local t0
    t0=$(ticket c0) || die "no ticket from c0"
    crea cx && configura cx || die "cx's container did not start"
    en cx <<< "pals init > /dev/null && pals contact add '$t0' --name c0" || die "cx cannot save c0"
    arranca cx || die "cx did not start"
    agent_id cx > /dev/null
    code=$(en cx <<< 'curl -sS -o /dev/null -w "%{http_code}" http://127.0.0.1:7420/dht')
    note "GET /dht de cx -> HTTP $code"
    # Time for cx to dial its saved contact, had it the DHT on.
    pausa 10
    busca cx caso1-cx-vision.ocr "$(cuerpo vision.ocr)"
    estados caso1-c0-despues c0
    out=$(py "$code" <<'PY'
cx_dht = sys.argv[1]
problems = []
for s in searches("caso1-c0-"):
    o = outcome(s)
    if o is None:
        problems.append(f"c0 {capability(s)}: HTTP {s['http']}")
        continue
    print(f"c0 {capability(s)}: ranked {len(o['ranked'])}, discarded {len(o['discarded'])}, "
          f"{coverage(o)}, dht_requests {o['metrics'].get('dht_requests', 0)}")
    if o["ranked"] or o["discarded"]:
        problems.append(f"c0 encuentra a alguien buscando {capability(s)}")
for tag in ("caso1-c0", "caso1-c0-despues"):
    d = dht(tagged(tag).get("c0"))
    if d is None:
        problems.append(f"c0 sin GET /dht ({tag})")
        continue
    print(f"c0 GET /dht ({tag}): routing_peers {d['routing_peers']}, connections {d['connections']}, "
          f"connections_accepted {d['connections_accepted']}")
    if d["routing_peers"] or d["connections"] or d["connections_accepted"]:
        problems.append(f"c0 con pares o conexiones de agentnet-kad/0 ({tag})")
s = search(LOGS / "busquedas" / "caso1-cx-vision.ocr.json")
o = outcome(s)
print(f"cx GET /dht: HTTP {cx_dht}")
if cx_dht != "404":
    problems.append(f"cx GET /dht {cx_dht}, no 404")
if o is None:
    problems.append(f"cx: HTTP {s['http']}")
else:
    print(f"cx vision.ocr: ranked {[c['peer'] for c in o['ranked']]}, discarded "
          f"{[(d['peer'], d['stage'], d['reason']) for d in o['discarded']]}, {coverage(o)}, "
          f"dht_requests {o['metrics'].get('dht_requests', 0)}, contacted {o['metrics']['contacted']}")
    if o["metrics"].get("dht_requests", 0) != 0:
        problems.append("cx hace peticiones a la DHT")
    if any(c.get("from_dht") for c in o["ranked"]):
        problems.append("cx tiene un candidato de la DHT")
    if not any(d["peer"] == "c0" for d in o["discarded"]) and not any(c["peer"] == "c0" for c in o["ranked"]):
        problems.append("cx no llegó a c0: la búsqueda no lo considera")
for p in problems:
    print("PROBLEMA:", p)
save("caso1", {"problemas": problems})
print("OK" if not problems else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "c0 no encuentra a nadie, 0 pares y 0 conexiones; cx: GET /dht 404, ninguna petición a la DHT, y c0 sin ninguna conexión de agentnet-kad/0 tras la búsqueda de cx" \
        passed "$st" "$out"
    docker stop -t 5 "$(ctr c0)" "$(ctr cx)" > /dev/null
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 1" "$ok" "$(grep -E '^(c0 GET|cx GET|cx vision)' <<< "$out" | tr '\n' ';' | cut -c1-400)"
}

caso2() {
    say "Caso 2. Encontrar un proveedor sin contacto previo"
    limpia caso2
    local ok=ok out st node r cap pids=()
    FAILS=0
    note "carga: $(load)"
    # Each client, 10 rounds of every capability, one after another; the
    # four clients at the same time.
    for node in "${CLIENTS[@]}"; do
        (for r in $(seq -w 1 10); do
            for cap in "${CAPABILITIES[@]}"; do busca "$node" "caso2-$node-$r-$cap" "$(cuerpo "$cap")"; done
        done) &
        pids+=($!)
    done
    wait "${pids[@]}"
    note "carga: $(load)"
    out=$(py <<'PY'
from collections import Counter
all_s = searches("caso2-")
bad = {}
labels = Counter()
for s in all_s:
    p = problems(s)
    if p:
        bad[s["nombre"]] = p
    o = outcome(s)
    if o:
        labels[coverage(o)] += 1
        for d in o["discarded"]:
            labels[f"descarte {d['reason']}"] += 1
print(f"búsquedas {len(all_s)}, conformes {len(all_s) - len(bad)}, coberturas y descartes {dict(labels)}")
for n, p in list(bad.items())[:20]:
    print(f"PROBLEMA {n}: {p}")
save("caso2", {"busquedas": len(all_s), "conformes": len(all_s) - len(bad), "etiquetas": dict(labels),
               "problemas": {k: v for k, v in list(bad.items())[:50]}})
print("OK" if len(all_s) == 160 and not bad else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    local summary
    summary=$(head -n 1 <<< "$out")
    check "160 búsquedas: el proveedor de la red del cliente, en el ranking desde la DHT con card y oferta verificadas; el de la otra red, descartado en la card por inalcanzable" \
        passed "$st" "$out"

    # What the routers' logs, at their default level, say of the searches:
    # any line that names a client, by its agent id or its PeerId.
    local who node peers=() named=
    for node in "${CLIENTS[@]}"; do
        who=$(en "$node" <<< 'sed -nE "s/.*the DHT started peer_id=([^ ]+).*/\1/p" "$PALS_HOME/daemon.log" | tail -n 1')
        # An empty pattern would match every line.
        check "el PeerId de $node, en su registro" test -n "$who"
        [ -z "$who" ] || peers+=("$who")
        peers+=("$(agent_id "$node")")
    done
    for node in "${ROUTERS[@]}"; do
        named="$named $node $(en "$node" <<< 'cat "$PALS_HOME/daemon.log"' | grep -cFf <(printf '%s\n' "${peers[@]}"))"
    done
    note "líneas del registro de cada router que nombran a un cliente (id de agente o PeerId), tras 160 búsquedas:$named"
    printf '{"lineas_que_nombran_clientes": "%s"}\n' "${named# }" > "$LOGS/casos/caso2-registros.json"

    note "p1 da un grant a c1, y c1 pregunta con pals discover --ask"
    local token c1 p1
    c1=$(agent_id c1); p1=$(agent_id p1)
    token=$(en p1 <<< "pals grant issue --to $c1 --project demo --ttl 1h") || die "p1 cannot issue a grant"
    en c1 <<< "pals grant import '$token'" || die "c1 cannot import p1's grant"
    en_wait "$LOGS/caso2-ask.out" c1 <<< 'pals discover --capability vision.ocr --ask "hola"; echo "código $?"; pals task ls | tail -n 2'
    out=$(cat "$LOGS/caso2-ask.out")
    echo "$out"
    check "código 0" has_line "$out" "código 0"
    check "p1 primero, con su grant, encontrado en la DHT y con su oferta" \
        grep -qE "^  1\. $p1 \(agent $p1\): available, .*, grant held, found in the DHT, card verified $p1, offer offer-[0-9a-f]+ until " <<< "$out"
    check "pregunta a p1" has_line "$out" "asked: $p1"
    check "echo: hola" has_line "$out" "echo: hola"
    check "la tarea, completada en c1" grep -q 'TASK_STATE_COMPLETED' <<< "$(tail -n 2 <<< "$out")"
    local accepted
    accepted=$(en p1 <<'EOS'
python3 - "$PALS_HOME/pals.db" <<'PY'
import sqlite3, sys
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
print(db.execute("SELECT COUNT(*) FROM events WHERE kind = 'task.accepted'").fetchone()[0])
PY
EOS
)
    note "p1: $accepted task.accepted en su registro de eventos"
    check "p1 acepta una tarea, la de c1" test "$accepted" = 1
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 2" "$ok" "${summary%%, coberturas*}; pregunta a p1: $(grep -x 'echo: hola' <<< "$out")"
}

caso3() {
    say "Caso 3. Sin el arranque original"
    local ok=ok out st t_stop t_start i start
    FAILS=0
    limpia caso3
    note "carga: $(load)"
    T1=$(ticket r1) || die "no ticket from r1"
    T3=$(ticket r3) || die "no ticket from r3"
    t_stop=$(now)
    para r1 || die "cannot stop r1"
    note "r1 parado (docker kill)"
    ronda caso3-conectados "${CLIENTS[*]}" "${CAPABILITIES[*]}"

    note "c5: arranque [r1], parado, y el ticket de r3 guardado como contacto"
    crea c5 && configura c5 || die "c5's container did not start"
    en c5 <<< "pals init > /dev/null && pals contact add '$T3' --name r3" || die "c5 cannot save r3"
    t_start=$(now)
    arranca c5 || die "c5 did not start"
    agent_id c5 > /dev/null
    start=$SECONDS
    i=0
    while :; do
        i=$((i + 1))
        estados "caso3-proveedores-$(printf %02d "$i")" "${PROVIDERS[@]}"
        ronda "caso3-c5-$(printf %02d "$i")" c5 "${CAPABILITIES[*]}"
        if py <<'PY'
done = set()
for s in searches("caso3-c5-"):
    if not problems(s, contacts={"r3"}):
        done.add(capability(s))
sys.exit(0 if len(done) == 4 else 1)
PY
        then break; fi
        if [ $((SECONDS - start)) -ge "$FIND_MAX_S" ]; then note "c5 no los encuentra en $FIND_MAX_S s"; break; fi
        pausa "$POLL_S"
    done
    # Each provider's rounds that ended with r1 stopped: until each shows
    # one with no error, within SETTLE_MAX_S.
    start=$SECONDS
    until py "$t_stop" <<'PY'
pubs = rounds_seen("caso3-proveedores-", float(sys.argv[1]))
sys.exit(0 if all(any(not r[1] for r in pubs.get(p, [])) for p in CAPS) else 1)
PY
    do
        if [ $((SECONDS - start)) -ge "$SETTLE_MAX_S" ]; then
            note "no todos los proveedores muestran una vuelta sin error con r1 parado en $SETTLE_MAX_S s"
            break
        fi
        i=$((i + 1))
        estados "caso3-proveedores-$(printf %02d "$i")" "${PROVIDERS[@]}"
        pausa "$POLL_S"
    done
    estados caso3-c5 c5
    note "registro de c5: $(en c5 <<< 'grep -E "no DHT bootstrap router answered|dialing the saved contacts|a saved contact is a DHT router" "$PALS_HOME/daemon.log" | sed -E "s/^[^ ]+ +//" | tr "\n" ";"')"
    local r3_peer entered=0
    r3_peer=$(en r3 <<< 'sed -nE "s/.*the DHT started peer_id=([^ ]+).*/\1/p" "$PALS_HOME/daemon.log" | tail -n 1')
    # With no PeerId of r3, any entry would match: none counts.
    [ -z "$r3_peer" ] \
        || entered=$(en c5 <<< 'cat "$PALS_HOME/daemon.log"' | grep -cF "a saved contact is a DHT router: entering through it peer=$r3_peer")
    out=$(py "$t_stop" "$t_start" "$r3_peer" "${entered:-0}" <<'PY'
t_stop, t_start, r3_peer, entered = float(sys.argv[1]), float(sys.argv[2]), sys.argv[3], int(sys.argv[4])
problems_ = []
if not r3_peer:
    problems_.append("sin el PeerId de r3 en su registro")
connected = searches("caso3-conectados-")
bad = {s["nombre"]: problems(s) for s in connected if problems(s)}
if len(connected) != 16 or bad:
    problems_.append(f"conectados: {len(connected)} búsquedas, {len(bad)} no conformes")
labels = sorted({coverage(outcome(s)) for s in connected if outcome(s)})
print(f"conectados: {len(connected)} búsquedas, conformes {len(connected) - len(bad)}, coberturas {labels}")
for n, p in bad.items():
    print(f"PROBLEMA {n}: {p}")
c5 = searches("caso3-c5-")
first, complete_at, done = None, None, set()
for s in sorted(c5, key=lambda s: s["t1"]):
    o = outcome(s)
    if o and o["ranked"] and first is None:
        first = s["t1"] - t_start
    if not problems(s, contacts={"r3"}):
        done.add(capability(s))
        if len(done) == 4 and complete_at is None:
            complete_at = s["t1"] - t_start
print(f"c5: {len(c5)} búsquedas; primer candidato a los {first and round(first, 1)} s de arrancar; "
      f"los dos proveedores de cada capacidad a los {complete_at and round(complete_at, 1)} s; "
      f"entra por r3: {'sí' if entered else 'no'}")
if len(done) < 4:
    problems_.append(f"c5 no encuentra los de {sorted(set(CAPS_ALL) - done)}")
if unanswered(c5):
    problems_.append(f"búsquedas de c5 sin respuesta: {unanswered(c5)}")
if not entered:
    problems_.append("c5 no dice que entra por r3")
pubs = rounds_seen("caso3-proveedores-", t_stop)
short = {node: [r for r in rs if r[1]] for node, rs in pubs.items()}
clean = {node: sum(1 for r in rs if not r[1]) for node, rs in pubs.items()}
without = sorted(p for p in CAPS if not clean.get(p))
print(f"vueltas de publicación terminadas con r1 parado: {sum(len(r) for r in pubs.values())}, "
      f"sin error por proveedor: {dict(sorted(clean.items()))}, "
      f"con error: {sum(len(r) for r in short.values())} {dict((k, v) for k, v in short.items() if v) or ''}")
if without or any(short.values()):
    problems_.append(f"vueltas de publicación con error con r1 parado, o proveedores sin una sin error: {without}")
for p in problems_:
    print("PROBLEMA:", p)
save("caso3", {"vueltas_proveedores": pubs, "conectados": len(connected), "conectados_conformes": len(connected) - len(bad),
               "coberturas_conectados": labels, "c5_busquedas": len(c5),
               "c5_primer_candidato_s": first and round(first, 1),
               "c5_completo_s": complete_at and round(complete_at, 1), "c5_entra_por_r3": bool(entered),
               "problemas": bad, "problemas_del_caso": problems_})
print("OK" if not problems_ else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "con r1 parado, los clientes conectados encuentran los dos proveedores de cada capacidad; c5 entra por r3 y los encuentra; cada proveedor termina con r1 parado al menos una vuelta de publicación, y ninguna falla" \
        passed "$st" "$out"
    docker stop -t 5 "$(ctr c5)" > /dev/null
    say "Restauración: r1"
    restaura r1 || die "r1 did not start again"
    check "restauración: los routers vuelven a guardar lo de cada proveedor" \
        espera_tiendas restaurado-r1 "$SETTLE_MAX_S" "${ROUTERS[*]}" "${PROVIDERS[*]}"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 3" "$ok" "$(grep -E '^(conectados|c5):' <<< "$out" | tr '\n' ';')"
}

caso4() {
    say "Caso 4. Caducar anuncios"
    limpia caso4
    local ok=ok out st t_stop i start p1 limit=$((TTL_S + REPUBLISH_S))
    FAILS=0
    p1=$(agent_id p1)
    note "carga: $(load)"
    estados caso4-antes "${ROUTERS[@]}"
    t_stop=$(now)
    para p1 || die "cannot stop p1"
    note "p1 parado sin aviso (docker kill)"
    start=$SECONDS
    i=0
    while :; do
        i=$((i + 1))
        estados "caso4-sondeo-$(printf %03d "$i")" "${ROUTERS[@]}"
        local cap=vision.ocr
        [ $((i % 2)) = 0 ] && cap=knowledge.documentation
        ronda "caso4-$(printf %03d "$i")" "${CLIENTS[*]}" "$cap"
        if py "$p1" "$i" <<'PY'
p1, i = sys.argv[1], int(sys.argv[2])
st = latest()
held = sum((dht(st.get(r)) or {}).get("stored_by_agent", {}).get(p1, 0) for r in ROUTERS)
last = searches(f"caso4-{i:03d}-")
clean = len(last) == 4 and all(outcome(s) and not mentions(s, p1) for s in last)
sys.exit(0 if held == 0 and clean else 1)
PY
        then break; fi
        if [ $((SECONDS - start)) -ge $((limit + 2 * REPUBLISH_S)) ]; then note "p1 sigue en la DHT tras $((SECONDS - start)) s"; break; fi
        pausa "$POLL_S"
    done
    note "carga: $(load)"
    out=$(py "$p1" "$t_stop" "$limit" "$(load)" <<'PY'
p1, t_stop, limit, load = sys.argv[1], float(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
before = tagged("caso4-antes")
held_before = {r: (dht(before.get(r)) or {}).get("stored_by_agent", {}).get(p1, 0) for r in ROUTERS}
polls = []
for l in lines(LOGS / "estados.jsonl"):
    if l["etiqueta"].startswith("caso4-sondeo-") and l["muestra"]:
        polls.append((l["etiqueta"], l["muestra"]["t"], l["nodo"], (dht(l["muestra"]) or {}).get("stored_by_agent", {}).get(p1, 0)))
by_poll = {}
for tag, t, node, n in polls:
    by_poll.setdefault(tag, []).append((t, n))
series = sorted((max(t for t, _ in v), sum(n for _, n in v)) for v in by_poll.values())
last_held = max((t for t, n in series if n > 0), default=None)
gone = [t for t, n in series if n == 0 and (last_held is None or t > last_held)]
routers_s = gone[0] - t_stop if gone and series[-1][1] == 0 else None
ss = sorted((s for s in searches("caso4-") if not s["nombre"].startswith("caso4-sondeo")), key=lambda s: s["t1"])
last_seen = max((s["t1"] for s in ss if mentions(s, p1)), default=None)
after = [s["t1"] for s in ss if outcome(s) and (last_seen is None or s["t1"] > last_seen)]
searches_s = after[0] - t_stop if after else None
print(f"antes: los routers guardaban de p1 {held_before} (contacto, anuncio y dos provider records cada uno)")
print(f"los routers lo olvidan a los {routers_s and round(routers_s, 1)} s; las búsquedas dejan de devolverlo a los "
      f"{searches_s and round(searches_s, 1)} s; límite {limit} s; {len(series)} sondeos, {len(ss)} búsquedas; carga {load}")
expired = sum(held_before.values())
broken = unanswered(ss)
if broken:
    print(f"PROBLEMA: búsquedas sin respuesta: {broken}")
ok = bool(routers_s is not None and searches_s is not None and routers_s < limit and searches_s < limit
          and all(n == 4 for n in held_before.values()) and not broken)
save("caso4", {"guardado_antes": held_before, "registros_caducados": expired, "routers_olvidan_s": routers_s and round(routers_s, 1),
               "busquedas_olvidan_s": searches_s and round(searches_s, 1), "limite_s": limit, "sondeos": len(series),
               "busquedas": len(ss), "carga": load})
print("OK" if ok else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "los routers olvidan a p1 y las búsquedas dejan de devolverlo antes de $limit s" passed "$st" "$out"
    say "Restauración: p1"
    restaura p1 || die "p1 did not start again"
    check "restauración: los routers vuelven a guardar lo de cada proveedor" \
        espera_tiendas restaurado-p1 "$SETTLE_MAX_S" "${ROUTERS[*]}" "${PROVIDERS[*]}"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 4" "$ok" "$(grep -E '^los routers' <<< "$out")"
}

caso5() {
    say "Caso 5. Caída de routers y renovación"
    local ok=ok out st t_stop i start limit=$((2 * REPUBLISH_S)) renewed=$((TTL_S + REPUBLISH_S))
    FAILS=0
    limpia caso5
    note "carga: $(load)"
    t_stop=$(now)
    para r2 && para r4 || die "cannot stop r2 and r4"
    note "r2 (operador 1) y r4 (operador 2) parados (docker kill)"
    start=$SECONDS
    i=0
    while :; do
        i=$((i + 1))
        estados "caso5-proveedores-$(printf %03d "$i")" "${PROVIDERS[@]}"
        ronda "caso5-$(printf %03d "$i")" "${CLIENTS[*]}" "${CAPABILITIES[*]}"
        if py "$i" <<'PY'
last = searches(f"caso5-{int(sys.argv[1]):03d}-")
sys.exit(0 if len(last) == 16 and all(both_found(s) for s in last) else 1)
PY
        then break; fi
        if [ $((SECONDS - start)) -ge $((3 * REPUBLISH_S + FIND_MAX_S / 2)) ]; then note "sin cobertura completa tras $((SECONDS - start)) s"; break; fi
    done
    note "carga: $(load)"
    # Renewal: once everything the routers held before r2 and r4 stopped
    # has expired (record_ttl_s + republish_s after), what they hold came
    # from the rounds made with them down, and a round must still find
    # every provider.
    note "renovación: hasta $renewed s después de pararlos, con r2 y r4 parados"
    while [ "$(python3 -c "import time, sys; print(int(time.time() - float(sys.argv[1])))" "$t_stop")" -lt "$renewed" ]; do
        i=$((i + 1))
        estados "caso5-proveedores-$(printf %03d "$i")" "${PROVIDERS[@]}"
        pausa $((5 * POLL_S))
    done
    estados caso5-final r1 r3
    ronda caso5b-renovada "${CLIENTS[*]}" "${CAPABILITIES[*]}"
    out=$(py "$t_stop" "$limit" "$(load)" "$renewed" <<'PY'
from collections import Counter
t_stop, limit, load, renewed = float(sys.argv[1]), int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
ss = searches("caso5-")
rounds = {}
for s in ss:
    rounds.setdefault(s["nombre"][:9], []).append(s)
order = sorted(rounds)
cov = [sum(1 for s in rounds[r] if both_found(s)) / len(rounds[r]) for r in order]
reached = bool(order) and cov[-1] == 1 and len(rounds[order[-1]]) == 16
full = None
if reached:
    short = [s for s in ss if not both_found(s)]
    if not short:
        full = 0.0
    else:
        last = max(short, key=lambda s: s["t1"])
        key = (last["nodo"], capability(last))
        end = min((s["t1"] for s in ss if s["t1"] > last["t1"] and both_found(s)
                   and (s["nodo"], capability(s)) == key), default=None)
        full = None if end is None else end - t_stop
labels = Counter(coverage(outcome(s)) for s in ss if outcome(s))
print(f"rondas {len(order)}, cobertura por ronda {[round(c, 3) for c in cov]}")
print(f"cobertura inmediata {cov[0] if cov else None}; cobertura completa a los {full if full is None else round(full, 1)} s "
      f"(límite {limit} s); etiquetas {dict(labels)}; carga {load}")
late = searches("caso5b-renovada-")
late_cov = sum(1 for s in late if both_found(s)) / len(late) if late else None
late_at = min((s["t0"] for s in late if s["t0"] != float("inf")), default=t_stop) - t_stop
print(f"renovación: a los {round(late_at)} s de pararlos (todo lo de antes ya caducado a los {renewed} s), "
      f"cobertura {late_cov} en {len(late)} búsquedas; etiquetas {dict(Counter(coverage(outcome(s)) for s in late if outcome(s)))}")
pubs = rounds_seen("caso5-proveedores-", t_stop)
short = {node: [r for r in rs if r[1]] for node, rs in pubs.items()}
print(f"vueltas de publicación terminadas con r2 y r4 parados: {sum(len(r) for r in pubs.values())}, "
      f"con error: {sum(len(r) for r in short.values())} {dict((k, v) for k, v in short.items() if v) or ''}")
broken = unanswered(ss + late)
if broken:
    print(f"PROBLEMA: búsquedas sin respuesta: {broken}")
ok = (reached and full is not None and full < limit and late_cov == 1 and late_at >= renewed and len(late) == 16
      and not broken)
save("caso5", {"rondas": len(order), "cobertura_por_ronda": [round(c, 3) for c in cov],
               "cobertura_inmediata": cov[0] if cov else None, "cobertura_completa_s": full if full is None else round(full, 1),
               "limite_s": limit, "etiquetas": dict(labels), "carga": load,
               "renovacion_s": round(late_at), "renovacion_cobertura": late_cov, "vueltas_proveedores": pubs})
print("OK" if ok else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "cobertura completa otra vez antes de $limit s, y entera aún después de $renewed s, con lo renovado sin r2 ni r4" \
        passed "$st" "$out"
    say "Restauración: r2 y r4"
    restaura r2 r4 || die "r2 and r4 did not start again"
    check "restauración: los routers vuelven a guardar lo de cada proveedor" \
        espera_tiendas restaurado-r2-r4 "$SETTLE_MAX_S" "${ROUTERS[*]}" "${PROVIDERS[*]}"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 5" "$ok" "$(grep -E '^(cobertura|renovación)' <<< "$out" | tr '\n' ';')"
}

caso6() {
    say "Caso 6. Contacto con firma inválida"
    limpia caso6
    local ok=ok out st rogue start
    FAILS=0
    note "carga: $(load)"
    T3=$(ticket r3) || die "no ticket from r3"
    estados caso6-antes "${ROUTERS[@]}"
    crea rogue || die "rogue's container did not start"
    docker cp "$ROGUE" "$(ctr rogue):/usr/local/bin/dht_rogue" > /dev/null || die "cannot copy dht_rogue"
    en rogue <<< "nohup dht_rogue --bootstrap '$T3' --addr ${ADDRS[rogue]#b=}:4433 --listen 0.0.0.0:4433 --capability vision.ocr --record-ttl-s $TTL_S --republish-s $REPUBLISH_S > \$PALS_HOME/rogue.log 2>&1 < /dev/null &"
    start=$SECONDS
    rogue=
    while [ -z "$rogue" ] && [ $((SECONDS - start)) -lt 60 ]; do
        rogue=$(en rogue <<< 'sed -nE "s/^dht_rogue: agent ([0-9a-f]{64}),.*/\1/p" "$PALS_HOME/rogue.log"')
        [ -n "$rogue" ] || pausa 1
    done
    [ -n "$rogue" ] || die "dht_rogue did not start: $(en rogue <<< 'cat "$PALS_HOME/rogue.log"')"
    ID[rogue]=$rogue
    ids_set rogue "$rogue"
    note "rogue: agent $rogue"
    local i=0
    while :; do
        i=$((i + 1))
        estados "caso6-sondeo-$i" "${ROUTERS[@]}"
        if py "$rogue" <<'PY'
rogue = sys.argv[1]
before, st = tagged("caso6-antes"), latest()
ok = all(dht(st.get(r)) and dht(st[r])["rejected"]["bad_contact"] > dht(before[r])["rejected"]["bad_contact"]
         and dht(st[r])["stored_by_agent"].get(rogue, 0) == 2 for r in ROUTERS)
sys.exit(0 if ok else 1)
PY
        then break; fi
        if [ $((SECONDS - start)) -ge "$FIND_MAX_S" ]; then note "los routers no cuentan el contacto del rogue en $FIND_MAX_S s"; break; fi
        pausa "$POLL_S"
    done
    note "rogue: $(en rogue <<< 'tr "\n" ";" < "$PALS_HOME/rogue.log"')"
    ronda caso6 "${CLIENTS[*]}" vision.ocr
    out=$(py "$rogue" <<'PY'
rogue = sys.argv[1]
before, after = tagged("caso6-antes"), latest()
problems_ = []
for r in ROUTERS:
    b, a = dht(before.get(r)), dht(after.get(r))
    if a is None or b is None:
        problems_.append(f"{r} sin estado")
        continue
    grew = a["rejected"]["bad_contact"] - b["rejected"]["bad_contact"]
    held = a["stored_by_agent"].get(rogue, 0)
    print(f"{r}: bad_contact +{grew}, guarda del rogue {held} (su anuncio y su provider record, sin contacto)")
    if grew < 1 or held != 2:
        problems_.append(f"{r}: bad_contact +{grew}, guarda {held}")
six = searches("caso6-")
if len(six) != 4:
    problems_.append(f"{len(six)} búsquedas de los clientes, no 4")
for s in six:
    o = outcome(s)
    if o is None:
        problems_.append(f"{s['nodo']}: HTTP {s['http']}")
        continue
    ranked, discarded = parts(o)
    d = discarded.get(rogue)
    print(f"{s['nodo']}: rogue {'EN EL RANKING' if rogue in ranked else 'fuera del ranking'}, "
          f"descartado {d and (d['stage'] + ': ' + d['reason'])}; el resto {problems(s, tolerated={rogue}) or 'conforme'}")
    if rogue in ranked or d is None or d["reason"] != "dht: no valid contact":
        problems_.append(f"{s['nodo']}: el rogue no queda descartado por su contacto")
    if problems(s, tolerated={rogue}):
        problems_.append(f"{s['nodo']}: {problems(s, tolerated={rogue})}")
for p in problems_:
    print("PROBLEMA:", p)
save("caso6", {"problemas": problems_})
print("OK" if not problems_ else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "los routers cuentan bad_contact y ninguno guarda su contacto; ningún cliente lo pone en su ranking y su discarded lo nombra" \
        passed "$st" "$out"
    docker stop -t 5 "$(ctr rogue)" > /dev/null
    note "rogue parado: sus registros caducan solos"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 6" "$ok" "$(grep -E '^r[1-4]:' <<< "$out" | sed -E 's/ \(.*//' | tr '\n' ';')"
}

caso7() {
    say "Caso 7. Búsquedas repetidas o maliciosamente amplias"
    limpia caso7
    local ok=ok out st caps=() n node serie start
    FAILS=0
    note "carga: $(load)"
    caps=("${CAPABILITIES[@]}")
    for n in $(seq -w 1 20); do caps+=("example:inexistente-$n"); done
    estados caso7-antes "${ROUTERS[@]}"
    local alive_before=
    for node in "${ROUTERS[@]}"; do alive_before="$alive_before $node=$(vivo "$node")"; done
    docker exec "$(ctr c3)" rm -f /tmp/parar-serie
    docker exec "$(ctr c3)" python3 "$H/metricas.py" serie /tmp/parar-serie "${CAPABILITIES[@]}" \
        > "$LOGS/busquedas/caso7-serie-c3.jsonl" 2>> "$LOGS/busquedas/errores.txt" &
    serie=$!
    # The burst starts once c3's series runs: its first search answered,
    # the next under way.
    start=$SECONDS
    until [ -s "$LOGS/busquedas/caso7-serie-c3.jsonl" ] || [ $((SECONDS - start)) -ge "$FIND_MAX_S" ]; do
        pausa 0.2
    done
    local t0 t1
    t0=$(now)
    docker exec "$(ctr c1)" python3 "$H/metricas.py" rafaga "$BURST" "$BURST_AT_ONCE" "${caps[@]}" \
        > "$LOGS/busquedas/caso7-rafaga-c1.jsonl" 2>> "$LOGS/busquedas/errores.txt" &
    wait $!
    t1=$(now)
    docker exec "$(ctr c3)" touch /tmp/parar-serie
    wait "$serie"
    note "carga: $(load)"
    estados caso7-despues "${ROUTERS[@]}"
    local alive_after= health=
    for node in "${ROUTERS[@]}"; do
        alive_after="$alive_after $node=$(vivo "$node")"
        health="$health $node=$(en "$node" <<< 'curl -sS -o /dev/null -w "%{http_code}" http://127.0.0.1:7420/healthz')"
    done
    note "daemons (PID:arranque) antes:$alive_before; después:$alive_after; healthz:$health"
    out=$(py "$t0" "$t1" <<'PY'
from collections import Counter
t0, t1 = float(sys.argv[1]), float(sys.argv[2])
burst = lines(LOGS / "busquedas" / "caso7-rafaga-c1.jsonl")
codes = Counter(str(l["http"]) for l in burst)
bad = []
for l in burst:
    if l["http"] == 429:
        if l["respuesta"] != {"error": "too many searches"}:
            bad.append(f"{l['nombre']}: 429 {l['respuesta']}")
    elif l["http"] != 200 or not isinstance(l["respuesta"], dict) or "ranked" not in l["respuesta"]:
        bad.append(f"{l['nombre']}: {l['http']} {l.get('error') or ''}")
real = [l for l in burst if l["http"] == 200 and not l["capacidad"].startswith("example:")]
fake = [l for l in burst if l["http"] == 200 and l["capacidad"].startswith("example:")]
print(f"ráfaga de c1: {len(burst)} búsquedas, 8 a la vez, en {t1 - t0:.1f} s; códigos {dict(sorted(codes.items()))}; "
      f"200 de capacidades que existen {len(real)}, con candidato {sum(1 for l in real if l['respuesta']['ranked'])}; "
      f"200 de las que no existen {len(fake)}, con candidato {sum(1 for l in fake if l['respuesta']['ranked'])}; anómalas {bad or 'ninguna'}")
serie = lines(LOGS / "busquedas" / "caso7-serie-c3.jsonl")
during = [l for l in serie if l["t0"] < t1 and l["t1"] > t0]
print(f"c3 durante la ráfaga: {len(during)} búsquedas, códigos {dict(Counter(str(l['http']) for l in during))}; "
      f"en total {len(serie)}")
before, after = tagged("caso7-antes"), tagged("caso7-despues")
refused = {}
for r in ROUTERS:
    b, a = dht(before.get(r)), dht(after.get(r))
    refused[r] = None if a is None or b is None else a["refused"]["streams"] - b["refused"]["streams"]
print(f"refused.streams de cada router durante el caso: {refused}")
ok = (codes.get("429", 0) >= 1 and not bad and len(burst) == 200 and during
      and all(l["http"] == 200 for l in during) and all(v is not None for v in refused.values()))
save("caso7", {"codigos": dict(codes), "anomalas": bad, "c3_durante": len(during),
               "c3_codigos": dict(Counter(str(l["http"]) for l in during)), "refused_streams": refused,
               "segundos": round(t1 - t0, 1)})
print("OK" if ok else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "al menos un too many searches, el resto 200, ninguna anómala; c3 termina sus búsquedas" passed "$st" "$out"
    # Each daemon running before and after, the same process: a daemon that
    # died, or one started again, has no PID or another start time.
    check "ningún router se cae: el mismo proceso en marcha y healthz 200" \
        test "$alive_before" = "$alive_after" -a -z "$(tr ' ' '\n' <<< "$alive_after$health" | grep . | grep -vE '=[0-9]+:[0-9]+$|=200$')"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 7" "$ok" "$(grep -E '^(ráfaga|refused)' <<< "$out" | sed -E 's/; 200 de .*//' | tr '\n' ';')"
}

caso8() {
    say "Caso 8. Política"
    limpia caso8
    local ok=ok out st start t0 node cap
    FAILS=0
    note "carga: $(load)"
    estados caso8-antes "${ROUTERS[@]}" p2
    t0=$(date +%s)
    start=$SECONDS
    # A whole round of p2 inside the case.
    while ! py "$t0" <<'PY'
pub = (dht(latest().get("p2")) or {}).get("publication") or {}
sys.exit(0 if (pub.get("last_round_unix") or 0) > int(sys.argv[1]) else 1)
PY
    do
        if [ $((SECONDS - start)) -ge $((3 * REPUBLISH_S)) ]; then note "p2 no termina ninguna vuelta en $((3 * REPUBLISH_S)) s"; break; fi
        pausa "$POLL_S"
        estados caso8-sondeo p2
    done
    estados caso8-despues "${ROUTERS[@]}" p2
    for node in "${CLIENTS[@]}"; do
        for cap in $HIDDEN_P2; do busca "$node" "caso8-$node-$cap" "$(cuerpo "$cap")"; done
    done
    busca c1 caso8-c1-nodht "$(cuerpo vision.ocr '"exclude_dht": true')"
    busca c1 caso8-c1-condht "$(cuerpo vision.ocr)"
    en c1 <<< 'pals discover --capability vision.ocr --no-dht; echo "código $?"' > "$LOGS/caso8-no-dht.out" 2>&1
    cat "$LOGS/caso8-no-dht.out"
    out=$(py "$t0" <<'PY'
t0 = int(sys.argv[1])
p2 = IDS["p2"]
before, after = tagged("caso8-antes"), tagged("caso8-despues")
problems_ = []
pub = (dht(after.get("p2")) or {}).get("publication") or {}
print(f"p2 publica {pub.get('capabilities')}; última vuelta {pub.get('last_round_unix', 0) - t0} s después de empezar el caso, error {pub.get('last_error')}")
if pub.get("capabilities") != CAPS["p2"] or (pub.get("last_round_unix") or 0) <= t0:
    problems_.append("p2 no publica solo sus capacidades públicas en una vuelta del caso")
for r in ROUTERS:
    b, a = dht(before.get(r)), dht(after.get(r))
    if a is None or b is None:
        problems_.append(f"{r} sin estado")
        continue
    held = (b["stored_by_agent"].get(p2, 0), a["stored_by_agent"].get(p2, 0))
    cna = (b["rejected"]["capability_not_announced"], a["rejected"]["capability_not_announced"])
    print(f"{r}: guarda de p2 {held[0]} -> {held[1]} (2 + sus {len(CAPS['p2'])} públicas); capability_not_announced {cna[0]} -> {cna[1]}")
    if held != (2 + len(CAPS["p2"]),) * 2 or cna[1] != cna[0]:
        problems_.append(f"{r}: guarda {held}, capability_not_announced {cna}")
hidden = [s for s in searches("caso8-c") if not s["nombre"].endswith("dht")]
if len(hidden) != 8:
    problems_.append(f"{len(hidden)} búsquedas de las capacidades ocultas, no 8")
for s in hidden:
    o = outcome(s)
    if o is None:
        problems_.append(f"{s['nombre']}: HTTP {s['http']}")
        continue
    seen = mentions(s, p2)
    print(f"{s['nodo']} busca {capability(s)}: p2 {'ENCONTRADO' if seen else 'no'}, dht_requests {o['metrics'].get('dht_requests', 0)}, "
          f"ranked {len(o['ranked'])}, discarded {len(o['discarded'])}")
    if seen or o["metrics"].get("dht_requests", 0) == 0:
        problems_.append(f"{s['nombre']}: p2 {seen}, dht_requests {o['metrics'].get('dht_requests', 0)}")
for which in ("nodht", "condht"):
    s = search(LOGS / "busquedas" / f"caso8-c1-{which}.json")
    o = outcome(s)
    if o is None:
        problems_.append(f"{which}: HTTP {s['http']}")
        continue
    print(f"c1 vision.ocr {'con exclude_dht (--no-dht)' if which == 'nodht' else 'sin exclude_dht'}: dht_requests {o['metrics'].get('dht_requests', 0)}, "
          f"de la DHT {sum(1 for c in o['ranked'] if c.get('from_dht'))} en el ranking, {len(o['discarded'])} descartados")
    if which == "nodht" and (o["metrics"].get("dht_requests", 0) != 0 or any(c.get("from_dht") for c in o["ranked"]) or o["discarded"]):
        problems_.append("con exclude_dht la búsqueda va a la DHT")
    if which == "condht" and o["metrics"].get("dht_requests", 0) == 0:
        problems_.append("sin exclude_dht la búsqueda no va a la DHT")
for p in problems_:
    print("PROBLEMA:", p)
save("caso8", {"problemas": problems_})
print("OK" if not problems_ else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "lo privado y de proyecto de p2 en ningún router; capability_not_announced igual; nadie encuentra a p2 por ello; con --no-dht, dht_requests 0" \
        passed "$st" "$out"
    check "pals discover --no-dht: nada encontrado en la DHT, código 0" \
        test "$(grep -c 'found in the DHT' "$LOGS/caso8-no-dht.out")" = 0 -a "$(grep -c '^código 0$' "$LOGS/caso8-no-dht.out")" = 1
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 8" "$ok" "$(grep -E '^r[1-4]:' <<< "$out" | sed -E 's/ \(2 \+ [^)]*\)//' | tr '\n' ';')"
}

caso9() {
    say "Caso 9. Cambio de red"
    limpia caso9
    local ok=ok out st t_move i start limit=$((2 * REPUBLISH_S))
    FAILS=0
    note "carga: $(load)"
    ronda caso9-antes "c1 c2" "${CAPS[p3]}"
    local seq_before
    seq_before=$(docker exec "$(ctr p3)" python3 "$H/metricas.py" contacto "$H/p3")
    note "contacto de p3 antes: seq y direcciones $seq_before"
    estados caso9-antes p3
    # The change starts with the disconnect: p3's daemon may see its new
    # address, and publish it, before `docker network connect` returns.
    t_move=$(now)
    docker network disconnect "$NET_B" "$(ctr p3)" || die "cannot take p3 off network B"
    docker network connect --ip "$P3_MOVED" "$NET_A" "$(ctr p3)" || die "cannot put p3 on network A"
    note "p3 conectado a la red A $(python3 -c 'import sys, time; print(round(time.time() - float(sys.argv[1]), 1))' "$t_move") s después de empezar a moverlo"
    placed p3 "a=$P3_MOVED" || die "p3 is not where it should be after the move"
    note "p3 pasa de la red B a la A: $(where p3)"
    start=$SECONDS
    i=0
    while :; do
        i=$((i + 1))
        local cap=vision.ocr
        [ $((i % 2)) = 0 ] && cap=vision.defect-detection
        ronda "caso9-$(printf %03d "$i")" "c1 c2" "$cap"
        if H3_P3=a py <<'PY'
ok = {n: False for n in ("c1", "c2")}
for s in searches("caso9-0"):
    o = outcome(s)
    if o:
        ranked, _ = parts(o)
        c = ranked.get(IDS["p3"])
        if c and verified(c, IDS["p3"], IDS[s["nodo"]]):
            ok[s["nodo"]] = True
sys.exit(0 if all(ok.values()) else 1)
PY
        then break; fi
        if [ $((SECONDS - start)) -ge $((3 * REPUBLISH_S)) ]; then note "los clientes de A no llegan a p3 en $((SECONDS - start)) s"; break; fi
        pausa "$POLL_S"
    done
    note "carga: $(load)"
    estados caso9-despues p3
    local seq_after renewed peer_rec
    seq_after=$(docker exec "$(ctr p3)" python3 "$H/metricas.py" contacto "$H/p3")
    renewed=$(en p3 <<< 'grep -E "renewed this agent.s records|issued a signed record kind=.contact." "$PALS_HOME/daemon.log" | tail -n 2 | sed -E "s/^([^ ]+) +[A-Z]+ +[^ ]+ /\1 /" | tr "\n" ";"')
    peer_rec=$(docker exec "$(ctr c1)" python3 "$H/metricas.py" contacto "$H/c1" "$(agent_id p3)")
    note "contacto de p3 después: $seq_after; en su registro: $renewed"
    note "el contacto de p3 que guarda c1: $peer_rec"
    out=$(H3_P3=b py "$t_move" "$limit" "$(load)" "$seq_before" "$seq_after" "$P3_MOVED" "$peer_rec" <<'PY'
t_move, limit, load = float(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
seq_before, seq_after, moved, at_c1 = sys.argv[4], sys.argv[5], sys.argv[6], json.loads(sys.argv[7])
p3 = IDS["p3"]
problems_ = []
before_s = searches("caso9-antes-")
if len(before_s) != 4:
    problems_.append(f"{len(before_s)} búsquedas antes del cambio, no 4")
for s in before_s:
    pr = problems(s, tolerated={IDS.get("rogue")})
    print(f"antes, {s['nodo']} {capability(s)}: {pr or 'p3 descartado por inalcanzable, conforme'}")
    if pr:
        problems_.append(f"antes {s['nombre']}: {pr}")
polls = searches("caso9-0")
if unanswered(polls):
    problems_.append(f"búsquedas sin respuesta tras el cambio: {unanswered(polls)}")
first = {}
for s in sorted(polls, key=lambda s: s["t1"]):
    o = outcome(s)
    if not o:
        continue
    ranked, _ = parts(o)
    c = ranked.get(p3)
    if c and verified(c, p3, IDS[s["nodo"]]) and s["nodo"] not in first:
        first[s["nodo"]] = round(s["t1"] - t_move, 1)
pub = (dht(tagged("caso9-despues").get("p3")) or {}).get("publication") or {}
# last_round_unix is in whole seconds.
round_after = (pub.get("last_round_unix") or 0) - int(t_move)
print(f"p3: contacto antes {json.loads(seq_before)}, después {json.loads(seq_after)}; última vuelta de publicación {round(round_after, 1)} s después del cambio, error {pub.get('last_error')}")
print(f"c1 y c2 verifican la card y la oferta de p3 en su dirección nueva a los {first} s del cambio (límite {limit} s); carga {load}")
before_c, after_c = json.loads(seq_before), json.loads(seq_after)
if not before_c or not after_c or after_c["seq"] <= before_c["seq"] or f"{moved}:4433" not in after_c["direct_addrs"]:
    problems_.append("p3 no emite un contacto nuevo con su dirección de la red A")
import datetime
issued = (datetime.datetime.fromisoformat(after_c["issued_at"].replace("Z", "+00:00")).timestamp() - int(t_move)
          if after_c else None)
print(f"p3 emite su contacto nuevo {issued} s después de empezar a moverlo (en segundos enteros)")
print(f"el contacto de p3 que guarda c1, de la DHT: {at_c1}")
if round_after < 0 or pub.get("last_error"):
    problems_.append("p3 no termina ninguna vuelta de publicación después del cambio")
if not at_c1 or not after_c or at_c1["seq"] != after_c["seq"] or at_c1["direct_addrs"] != after_c["direct_addrs"]:
    problems_.append("c1 no recibe de la DHT el contacto nuevo de p3")
if set(first) != {"c1", "c2"} or any(v >= limit for v in first.values()):
    problems_.append(f"los clientes de A no verifican a p3 antes de {limit} s: {first}")
for p in problems_:
    print("PROBLEMA:", p)
save("caso9", {"contacto_antes": json.loads(seq_before), "contacto_despues": json.loads(seq_after), "vuelta_tras_cambio_s": round(round_after, 1),
               "contacto_emitido_s": issued, "verificado_s": first, "limite_s": limit, "carga": load,
               "problemas": problems_})
print("OK" if not problems_ else "PROBLEMA")
PY
)
    st=$?
    echo "$out"
    check "p3 emite un contacto nuevo y lo publica; c1 y c2 verifican su card y su oferta en su dirección nueva antes de $limit s" \
        passed "$st" "$out"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 9" "$ok" "$(grep -E '^c1 y c2' <<< "$out")"
}

DESMONTADO=0
desmontar() {
    DESMONTADO=1
    say "Desmontaje"
    local c node left
    # The daemons' logs, of every container of ours, running or stopped.
    for c in $(ours_containers); do
        [ "$(container_label "$c")" = "$WORK" ] || continue
        node=${c#"$P-"}
        mkdir -p "$LOGS/nodos"
        docker cp "$c:$H/$node/daemon.log" "$LOGS/nodos/$node-daemon.log" > /dev/null 2>&1 \
            || docker cp "$c:$H/$node/rogue.log" "$LOGS/nodos/$node.log" > /dev/null 2>&1 || true
    done
    if ls "$LOGS/busquedas/"caso2-*.json > /dev/null 2>&1; then
        python3 "$HERE/metricas.py" informe "$LOGS" > "$LOGS/metricas.txt" 2>&1
        note "métricas en $LOGS/metricas.txt y metricas.json:"
        cat "$LOGS/metricas.txt"
    fi
    for c in $(ours_containers); do
        if [ "$(container_label "$c")" = "$WORK" ]; then
            docker rm -f "$c" > /dev/null && note "contenedor $c eliminado"
        fi
    done
    for c in $(ours_networks); do
        if [ "$(network_label "$c")" = "$WORK" ]; then
            docker network rm "$c" > /dev/null && note "red $c eliminada"
        fi
    done
    left=$(any_containers; any_networks)
    [ -z "$left" ] || note "AVISO: quedan recursos $P-* de otra ejecución, que no se tocan: $(echo $left)"
    note "con la etiqueta $LABEL=$WORK quedan: $(docker ps -a -q --filter "label=$LABEL=$WORK" | wc -l) contenedores, $(docker network ls -q --filter "label=$LABEL=$WORK" | wc -l) redes"
    if [ "$KEEP_IMAGE" = 0 ] \
        && [ -n "$(docker image inspect -f '{{ index .Config.Labels "pals-exit.image" }}' "$IMAGE" 2> /dev/null)" ]; then
        docker image rm "$IMAGE" > /dev/null && note "imagen $IMAGE eliminada"
    else
        note "la imagen $IMAGE se conserva (PALS_EXIT_KEEP_IMAGE=0 la elimina)"
    fi
    rm -f "$WORK/ultimo.jsonl"
    note "quedan los registros en $LOGS"
}

# --- main ------------------------------------------------------------------

on_interrupt() {
    trap - INT TERM
    trap '' PIPE
    kill -0 "$TEE_PID" 2> /dev/null || exec >> "$LOGS/run.log" 2>&1
    note "interrumpido: se desmonta"
    [ "$DESMONTADO" = 1 ] || desmontar
    exit 130
}
trap on_interrupt INT TERM
if [ "$TODO" = 1 ]; then
    trap '[ "$DESMONTADO" = 1 ] || desmontar' EXIT
fi

for phase in "${phases[@]}"; do
    case $phase in
        preparar) preparar ;;
        caso[1-9]) "$phase" ;;
        desmontar) desmontar ;;
        *) die "unknown phase $phase" ;;
    esac
done
if [ -f "$LOGS/resultados.txt" ]; then
    say "Resultados"
    cat "$LOGS/resultados.txt"
    ! grep -q FALLIDO "$LOGS/resultados.txt"
fi
