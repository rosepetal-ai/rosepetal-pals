#!/usr/bin/env bash
# The hito 2 exit test (docs/runbooks/hito-2-consulta-de-servicios.md) on one
# host, with one Docker container per agent on a bridge network of its own:
#
#   pals-exit-h2-a   A, the requester (with NET_ADMIN, only for the byte
#                    counters that tell the network path)
#   pals-exit-h2-b … -e, -g
#                    the providers B, C, D, E and G, with the inventories of
#                    task 10 and a real MCP server (server-everything over
#                    stdio); H, the contact that goes down in case 5, is a
#                    second daemon in G's container
#   pals-exit-h2-f   F, a stock A2A agent on the official Python SDK, over
#                    HTTPS with a test CA that A trusts
#   pals-exit-h2-m   case 8: a milestone 1 data directory under milestone 2
#
# Every daemon runs with `net.relay = true` (Iroh's public relays): with
# `relay = false` a daemon binds to 127.0.0.1 only, and no other container
# could reach it. Between containers the path is direct UDP over the bridge.
#
# The runbook's scripts (funciones.sh, proveedor.sh, mcp-everything.sh,
# req.json, metricas.py, caso4.sh, vista.py, caso8.sh) are extracted from the
# runbook itself and copied to ~/pals-h2 of every container. The runbook's
# other commands are copied into this file, and run in each container as
# root, as a new terminal would: with funciones.sh loaded and `agente <x>`
# chosen. Tickets and grant tokens pass from one container to another
# through shell variables, never printed. A change to those blocks of the
# runbook has to be carried over here by hand.
#
# Usage:
#   PALS_EXIT_DIR=<new or empty dir> tests/exit/hito-2.sh [phase...]
# Phases, in order: preparar caso1 caso2 caso3 caso4 caso5 caso6 caso7 caso8
# desmontar; `todo` (the default) runs them all. Each case prints
# `RESULTADO <case>: superado|FALLIDO`, collected in
# $PALS_EXIT_DIR/registro/resultados.txt; the full log is
# $PALS_EXIT_DIR/registro/run.log. Case 9 (TCK and Python client) runs apart,
# as the runbook says. See tests/exit/README.md.
#
# Safety: `preparar` only accepts a new or empty PALS_EXIT_DIR and marks it
# with .pals-exit; every other phase refuses a directory without that mark.
# Every container and the network are named pals-exit-h2-* and carry the
# label pals-exit.dir=<PALS_EXIT_DIR>; `desmontar` removes only those whose
# label is this directory, and the image only with PALS_EXIT_KEEP_IMAGE=0
# and its pals-exit.image label. Nothing is run on the host's network: no
# iptables, nft, tc or sysctl. The daemons live in the containers, and the
# runbook's `para` signals one only after checking that its PID is that
# agent's `pals daemon`. An interrupted run (Ctrl-C, SIGTERM) tears itself
# down; after any other failure, run `desmontar` with the same PALS_EXIT_DIR.
#
# Environment:
#   PALS_EXIT_DIR            required: the logs (registro/) and the image's
#                            build context go here
#   PALS_EXIT_BIN            the pals binary [target/release/pals]
#   PALS_EXIT_BIN_HITO1      case 8: a milestone 1 pals, built from 71092a0
#                            (see the runbook, section 1); without it case 8
#                            fails
#   PALS_EXIT_EVENTS_VERIFY  case 8: the events_verify example
#                            [target/release/examples/events_verify]
#   PALS_EXIT_TESTS          cargo (default): case 3 runs the tests of the
#                            tampered test peers with cargo; omitir: not
#   PALS_EXIT_KEEP_IMAGE     1 (default) keeps the image on `desmontar`; 0
#                            removes it

set -uo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
RUNBOOK=$REPO/docs/runbooks/hito-2-consulta-de-servicios.md
WORK=${PALS_EXIT_DIR:?set PALS_EXIT_DIR to a new or empty directory for the logs}
WORK=$(realpath -m -- "$WORK")
MARKER=$WORK/.pals-exit
PALS=${PALS_EXIT_BIN:-$REPO/target/release/pals}
PALS_H1=${PALS_EXIT_BIN_HITO1:-}
EVENTS_VERIFY=${PALS_EXIT_EVENTS_VERIFY:-$REPO/target/release/examples/events_verify}
TESTS=${PALS_EXIT_TESTS:-cargo}
KEEP_IMAGE=${PALS_EXIT_KEEP_IMAGE:-1}
P=pals-exit-h2
NET=$P-net
IMAGE=$P:latest
CONTAINERS=(a b c d e g f m)
PROVIDERS=(b c d e g)
F_HOST=$P-f
LOGS=$WORK/registro
# The question of case 1, as the runbook asks it.
PREGUNTA1="Revisa src/lib.rs"
# Case 4: the 50 searches in a row with every candidate's answer, at least.
SEGUIDAS_COMPLETAS=45

phases=("$@")
[ ${#phases[@]} -gt 0 ] || phases=(todo)
TODO=0
if [ "${phases[0]}" = todo ]; then
    TODO=1
    phases=(preparar caso1 caso2 caso3 caso4 caso5 caso6 caso7 caso8 desmontar)
fi
for phase in "${phases[@]}"; do
    case $phase in
        preparar|caso[1-8]|desmontar) ;;
        *) echo "hito-2.sh: unknown phase $phase" >&2; exit 2 ;;
    esac
done
if [[ " ${phases[*]:1} " == *" preparar "* ]]; then
    echo "hito-2.sh: preparar must be the first phase" >&2; exit 2
fi
if [ "${phases[0]}" = preparar ]; then
    if [ -e "$WORK" ] && { [ ! -d "$WORK" ] || [ -n "$(ls -A -- "$WORK")" ]; }; then
        echo "hito-2.sh: $WORK exists and is not an empty directory: preparar needs a new or empty PALS_EXIT_DIR" >&2
        exit 2
    fi
    mkdir -p -- "$WORK" && printf 'pals-exit harness, hito 2\n' > "$MARKER" || exit 2
elif [ ! -f "$MARKER" ]; then
    echo "hito-2.sh: $WORK has no .pals-exit mark: it was not made by \`preparar\`, and no phase but preparar touches it" >&2
    exit 2
fi

mkdir -p "$LOGS"
# The log's tee ignores SIGINT and SIGTERM, so that Ctrl-C, which reaches
# the whole process group, leaves it to log the teardown.
exec > >(trap '' INT TERM; exec tee -a "$LOGS/run.log") 2>&1
TEE_PID=$!

# --- helpers ---------------------------------------------------------------

ts() { date -u +%T; }
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
has() { grep -qF -- "$2" <<< "$1"; }       # has TEXT NEEDLE
has_line() { grep -qxF -- "$2" <<< "$1"; } # has_line TEXT LINE

# The heredoc the runbook writes to ~/pals-h2/$1.
extract() {
    awk -v start="cat > ~/pals-h2/$1 <<'EOF'" '
        $0 == start { on = 1; found = 1; next }
        on && $0 == "EOF" { exit }
        on { print }
        END { if (!found) exit 1 }' "$RUNBOOK"
}

# The container of agent $1: H lives in G's.
ctr() { case $1 in h) echo "$P-g" ;; *) echo "$P-$1" ;; esac; }
# en X: runs the bash script on stdin in X's container as a new terminal of
# the runbook would: funciones.sh loaded and, for a PALS agent, `agente X`.
# The script is read whole first, so that the commands it runs never read
# it as their input.
en() {
    local x=$1 pick=:
    case $x in a|b|c|d|e|g|h) pick="agente $x" ;; esac
    docker exec -i "$(ctr "$x")" bash -c \
        "cd; [ ! -f ~/pals-h2/funciones.sh ] || . ~/pals-h2/funciones.sh; $pick; "'eval "$(cat)"'
}
# en_wait FILE X: en X, its output in FILE, in the background and waited
# for, so that a trapped signal interrupts the wait at once.
en_wait() { en "$2" <&0 > "$1" 2>&1 & wait $!; }
# put X PATH: copies stdin to PATH, relative to the home directory, in X's
# container. The path is an argument of the remote shell, never part of its
# script.
put() { docker exec -i "$(ctr "$1")" sh -c 'cat > "$HOME/$1"' sh "$2"; }
# put_exec X PATH: put, and makes the file executable.
put_exec() { docker exec -i "$(ctr "$1")" sh -c 'cat > "$HOME/$1" && chmod +x "$HOME/$1"' sh "$2"; }

# Bytes through A's eth0, from the accounting rules `preparar` appends to
# A's chains: UDP except DNS (the direct path between containers), and TCP
# 443 (the relays). Prints "udp_out udp_in tcp443_out tcp443_in".
counters() {
    docker exec "$P-a" sh -c 'iptables -nvxL OUTPUT; iptables -nvxL INPUT' | awk '
        /pals-exit-udp-out/ { uo = $2 } /pals-exit-udp-in/ { ui = $2 }
        /pals-exit-443-out/ { to = $2 } /pals-exit-443-in/ { ti = $2 }
        END { print uo + 0, ui + 0, to + 0, ti + 0 }'
}
# path LABEL "BEFORE": the bytes since BEFORE and the path they say.
path() {
    local label=$1 b a; read -r -a b <<< "$2"; read -r -a a <<< "$(counters)"
    local udp=$(( a[0] - b[0] + a[1] - b[1] )) tcp=$(( a[2] - b[2] + a[3] - b[3] )) which=mixto
    if [ "$udp" -gt $(( 4 * tcp )) ]; then which="directo (UDP)"
    elif [ "$tcp" -gt $(( 4 * udp )) ]; then which="relay (TCP 443)"; fi
    note "camino $label: $which; por eth0 de A, UDP sin DNS: $(( a[0] - b[0] )) B enviados, $(( a[1] - b[1] )) B recibidos; TCP 443: $(( a[2] - b[2] )) B enviados, $(( a[3] - b[3] )) B recibidos"
}

# Our containers and network: named $P-*, labelled with this PALS_EXIT_DIR.
ours_containers() { docker ps -a --filter "label=pals-exit.dir=$WORK" --format '{{.Names}}' | grep "^$P-" || true; }
ours_networks() { docker network ls --filter "label=pals-exit.dir=$WORK" --format '{{.Name}}' | grep "^$P-" || true; }
any_containers() { docker ps -a --format '{{.Names}}' | grep "^$P-" || true; }
any_networks() { docker network ls --format '{{.Name}}' | grep "^$P-" || true; }
label_of() { docker inspect -f '{{ index .Config.Labels "pals-exit.dir" }}' "$1" 2> /dev/null; }

# Agent ids, once known.
declare -A ID
agent_id() { [ -n "${ID[$1]:-}" ] || ID[$1]=$(en "$1" <<< 'pals id'); echo "${ID[$1]}"; }

# counts X: "task.accepted ejecuciones tools/list pid" of provider X.
counts() { en "$1" <<< 'echo "$(eventos task.accepted) $(ejecuciones) $(tools_list) $(cat "$PALS_HOME/daemon.pid")"'; }

# timer_ticks X T0 T1: how many times the 60 s timer of X's inventory
# adapter (config_proveedor) can fire between T0 and T1 (Unix seconds),
# counted from its first observation, at the daemon's start, the first line
# of its *-mcp-arranques.log; 3 s of slack on each side.
timer_ticks() {
    local first k n=0
    first=$(en "$1" <<< 'head -n 1 "$H2/$AGENTE-mcp-arranques.log"')
    first=$(date -u -d "$first" +%s) || { echo 0; return; }
    for (( k = 1; first + 60 * k <= $3 + 3; k++ )); do
        if [ $(( first + 60 * k )) -ge $(( $2 - 3 )) ]; then n=$((n + 1)); fi
    done
    echo "$n"
}
# tools_calls: "<x> <n>" for every provider and H: the tools/call its MCP
# server received, by the copy of its stdin.
tools_calls() {
    local x n out=
    for x in b c d e g h; do
        n=$(en "$x" <<< 'grep -c "\"tools/call\"" "$H2/$AGENTE-mcp-entrada.log" 2> /dev/null || true')
        out="$out$x ${n:-0} "
    done
    echo "${out% }"
}

# A host-side Python check over files of the log: the script on stdin, the
# arguments after.
py() { python3 - "$@"; }

# --- phases ----------------------------------------------------------------

preparar() {
    say "Preparación: imagen, red y contenedores; agentes, contactos y grants"
    [ -x "$PALS" ] || die "no pals binary at $PALS: cargo build --release --locked -p pals"
    command -v docker > /dev/null || die "docker is required"
    local existing
    existing=$(any_containers; any_networks)
    if [ -n "$existing" ]; then
        die "$P-* resources exist, of another run: $(echo $existing); run \`desmontar\` with its PALS_EXIT_DIR (their pals-exit.dir label: docker inspect -f '{{ index .Config.Labels \"pals-exit.dir\" }}' <container>), or, if that directory is gone, remove them by hand after checking that label"
    fi

    note "fecha: $(date -u '+%F %T') UTC"
    note "commit: $(git -C "$REPO" rev-parse --short HEAD)$(git -C "$REPO" diff --quiet HEAD -- . || echo ' (con cambios sin commit)')"
    note "pals: $("$PALS" --version), sha256 $(sha256sum "$PALS" | cut -c1-16)…"
    [ -z "$PALS_H1" ] || note "pals del hito 1: $("$PALS_H1" --version), sha256 $(sha256sum "$PALS_H1" | cut -c1-16)…"
    note "anfitrión: $(. /etc/os-release && echo "$PRETTY_NAME"), kernel $(uname -r), $(nproc) CPU, carga $(cut -d' ' -f1-3 /proc/loadavg)"
    note "docker: $(docker version --format '{{.Server.Version}}')"
    note "rustc: $(rustc --version 2> /dev/null || echo 'no instalado'); go: $(go version 2> /dev/null || echo 'no instalado'); node: $(node --version 2> /dev/null || echo 'no instalado')"

    # The image's context: the Dockerfile, the MCP server's lock and the
    # Python pins.
    local ctx=$WORK/imagen
    mkdir -p "$ctx/mcp"
    cp "$REPO/tests/exit/hito-2/Dockerfile" "$REPO/tests/exit/hito-2/requirements-f.txt" "$ctx/" \
        && cp "$REPO/tests/interop/requirements.txt" "$ctx/" \
        && cp "$REPO/tests/exit/hito-2/mcp/package.json" "$REPO/tests/exit/hito-2/mcp/package-lock.json" "$ctx/mcp/" \
        || die "cannot assemble the image's context"
    docker build -q -t "$IMAGE" "$ctx" > /dev/null || die "docker build failed"
    note "imagen: $IMAGE $(docker image inspect -f '{{.Id}}' "$IMAGE" | cut -c1-19)…, de debian:trixie-slim@sha256:a72b4433…"
    docker network create --label "pals-exit.dir=$WORK" "$NET" > /dev/null || die "cannot create network $NET"
    local x caps
    for x in "${CONTAINERS[@]}"; do
        caps=()
        [ "$x" = a ] && caps=(--cap-add NET_ADMIN)
        docker run -d --init --name "$P-$x" --hostname "$P-$x" --network "$NET" "${caps[@]}" \
            --label "pals-exit.dir=$WORK" "$IMAGE" > /dev/null || die "cannot start container $P-$x"
    done
    note "red: $(docker network inspect -f '{{range .IPAM.Config}}{{.Subnet}} gw {{.Gateway}}{{end}}' "$NET")"

    # Every container: the binary, the files git tracks as they are in this
    # working tree (never an untracked one), and the runbook's scripts in
    # ~/pals-h2, the venv and the MCP server where the runbook installs them.
    local tree=$WORK/repositorio.tar f
    git -C "$REPO" ls-files -z --cached \
        | tar -C "$REPO" --null -T - -c -f "$tree" 2> /dev/null || die "cannot pack the repository"
    for x in "${CONTAINERS[@]}"; do
        docker cp "$PALS" "$P-$x:/usr/local/bin/pals" > /dev/null || die "cannot copy pals into $x"
        docker exec -i "$P-$x" sh -c 'mkdir -p ~/rosepetal-pals ~/pals-h2/proyecto && tar -x -C ~/rosepetal-pals' < "$tree" \
            || die "cannot copy the repository into $x"
        docker exec "$P-$x" sh -c 'cd ~/rosepetal-pals && pwd > ~/pals-h2/repo && ln -s /opt/venv ~/pals-h2/venv && ln -s /opt/mcp ~/pals-h2/mcp'
        for f in funciones.sh proveedor.sh mcp-everything.sh req.json metricas.py caso4.sh vista.py caso8.sh; do
            extract "$f" | put "$x" "pals-h2/$f" || die "the runbook has no $f"
        done
    done
    rm -f "$tree"
    note "contenedores: $(docker exec "$P-a" sh -c '. /etc/os-release; echo "$PRETTY_NAME, $(pals --version), node $(node --version), $(/opt/venv/bin/python --version), a2a-sdk $(/opt/venv/bin/pip show a2a-sdk 2> /dev/null | sed -n "s/^Version: //p"), server-everything $(sed -n "s/^ *\"version\": \"\(.*\)\",/\1/p" /opt/mcp/node_modules/@modelcontextprotocol/server-everything/package.json | head -n 1)"')"
    for x in "${CONTAINERS[@]}"; do
        note "  $P-$x: $(docker exec "$P-$x" ip -br -4 addr show eth0 | awk '{print $3}')"
    done
    for relay in use1-1 euc1-1; do
        note "relay $relay desde A: $(docker exec "$P-a" curl -sS -o /dev/null -w '%{http_code} en %{time_total}s' "https://$relay.relay.n0.iroh.link/")"
    done
    # Harness only: byte counters at the end of A's chains, for `path`.
    en a <<'EOS' || die "cannot add the accounting rules in A"
iptables -A OUTPUT -o eth0 -p udp ! --dport 53 -m comment --comment pals-exit-udp-out
iptables -A INPUT -i eth0 -p udp ! --sport 53 -m comment --comment pals-exit-udp-in
iptables -A OUTPUT -o eth0 -p tcp --dport 443 -m comment --comment pals-exit-443-out
iptables -A INPUT -i eth0 -p tcp --sport 443 -m comment --comment pals-exit-443-in
EOS

    say "2.1. F y su CA de prueba"
    en f <<EOS || die "F did not start"
F_HOST=$F_HOST
cd ~/pals-h2
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 7 \\
    -subj "/CN=CA de prueba del hito 2 de PALS" -keyout ca.key -out ca.crt \\
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign 2> /dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=\$F_HOST" -keyout f.key -out f.csr 2> /dev/null
printf 'subjectAltName=DNS:%s\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' "\$F_HOST" > f.ext
openssl x509 -req -in f.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 7 -extfile f.ext -out f.crt 2> /dev/null
for p in 8443 8444; do
    extra=; [ "\$p" = 8444 ] && extra=--tamper
    nohup ~/pals-h2/venv/bin/python "\$REPO/tests/exit/hito-2/agente_f.py" --listen "0.0.0.0:\$p" \\
        --url "https://\$F_HOST:\$p" --tls-cert f.crt --tls-key f.key --sign-key f-firma.key \$extra \\
        >> "f-\$p.log" 2>&1 < /dev/null &
    echo \$! > "f-\$p.pid"
done
openssl x509 -in ca.crt -noout -subject -fingerprint -sha256
EOS
    # ca.crt, never ca.key, from F to A.
    en f <<< 'cat ~/pals-h2/ca.crt' > "$WORK/ca.crt" && put a pals-h2/ca.crt < "$WORK/ca.crt" \
        || die "cannot copy F's CA certificate to A"

    say "2.2. A"
    en a <<'EOS' || die "A did not start"
cd ~/pals-h2
$SUDO cp ca.crt /usr/local/share/ca-certificates/pals-h2-prueba.crt && $SUDO update-ca-certificates > /dev/null
config_a
pals init
arranca
EOS
    en a <<EOS || die "A does not reach F over HTTPS"
for p in 8443 8444; do
    for doc in .well-known/agent-card.json jwks.json; do
        espera 120 "F en el puerto \$p: \$doc" curl -sSf -o /dev/null "https://$F_HOST:\$p/\$doc" || exit 1
    done
done
echo "F responde por https en 8443 y 8444, su card y su JWK Set, con el certificado de la CA de prueba"
EOS

    say "2.3. Los proveedores"
    en b <<< 'config_proveedor;         inventario false 8192 100 1000 "echo get-sum"; publicas 120; pals init; arranca' || die "B did not start"
    en c <<< 'config_proveedor --lleno; inventario false 8192 10 100 "echo";                       pals init; arranca' || die "C did not start"
    en d <<< 'config_proveedor;         inventario true 8192 1 1 "echo";                           pals init; arranca' || die "D did not start"
    en e <<< 'config_proveedor;         inventario false 8192 1 1 "read_file";                     pals init; arranca' || die "E did not start"
    en g <<< 'config_proveedor;         inventario false 1024 1 1 "echo";                          pals init; arranca' || die "G did not start"
    en h <<< 'config_proveedor;         inventario false 8192 1 1 "echo";                          pals init; arranca' || die "H did not start"
    local out
    for x in b c d e g h; do
        out=$(en "$x" <<< 'pals inventory refresh')
        note "$x: pals inventory refresh -> $(tr '\n' ';' <<< "$out")"
    done
    note "B, respuesta de server-everything a initialize: $(en b <<< 'head -n 1 "$H2/b-mcp-salida.log"' | grep -o '"serverInfo":{[^}]*}')"
    for x in a b c d e g h; do ID[$x]=$(en "$x" <<< 'pals id'); note "id de $x: ${ID[$x]}"; done

    say "2.4. Contactos y grants"
    local ta tx token
    ta=$(en a <<< 'pals id --ticket') || die "no ticket from A"
    for x in "${PROVIDERS[@]}"; do
        en "$x" <<< "pals contact add '$ta' --name a" || die "$x cannot save A"
        tx=$(en "$x" <<< 'pals id --ticket') || die "no ticket from $x"
        token=$(en "$x" <<< 'pals grant issue --to a --project demo --ttl 24h') || die "$x cannot issue a grant"
        en a <<< "pals contact add '$tx' --name $x && pals grant import '$token'" || die "A cannot save $x"
    done
    tx=$(en b <<< 'pals id --ticket') || die "no ticket from B"
    en g <<< "pals contact add '$tx' --name b" || die "G cannot save B"
    en a <<< 'pals contact ls; pals grant ls --held'
    out=$(en a <<< 'for x in b c d e g; do pals card show $x | tail -n 1; done' 2>&1)
    echo "$out"
    [ "$(grep -cE '^verified: [0-9a-f]{64} blake3:[0-9a-f]{64}$' <<< "$out")" = 5 ] \
        || die "A does not see the signed card of every provider"

    say "2.5. C, lleno"
    en a <<< 'pals ask c "espera 3600" --wait 10s; echo "código $?"'
    out=$(en c <<< 'espera 120 "la tarea larga de C" grep -q "espera 3600" "$H2/c-ejecuciones.log" && ejecuciones') \
        || die "C's long task did not start"
    note "C: $out ejecución en marcha"
}

caso1() {
    say "Caso 1. Elegir entre al menos tres candidatos"
    local before x out runs=() ok=ok try
    for x in "${PROVIDERS[@]}"; do runs+=("$(en "$x" <<< ejecuciones)"); done
    # As the runbook says: a search where no candidate answered within its
    # time, with only timeouts, ran nothing, and is repeated, up to three
    # times.
    for try in 1 2 3; do
        before=$(counters)
        en_wait "$LOGS/caso1-$try.out" a <<EOS
cd ~/pals-h2
pals discover --capability code.rust.review --requirements-file req.json --ask "$PREGUNTA1"; echo "código \$?"
EOS
        out=$(cat "$LOGS/caso1-$try.out")
        echo "$out"
        path "caso 1, intento $try" "$before"
        if has_line "$out" "código 2" && has "$out" "pals: no candidate took the task" \
            && ! grep -E '^  [a-z]+ \((card|offer)\): ' <<< "$out" | grep -qv ': timeout$'; then
            note "intento $try: ningún candidato respondió a tiempo, solo descartes timeout, y nada se ejecutó: se repite"
            continue
        fi
        break
    done
    cp "$LOGS/caso1-$try.out" "$LOGS/caso1.out"
    FAILS=0
    local b c
    b=$(agent_id b); c=$(agent_id c)
    check "código 0" has_line "$out" "código 0"
    local offer=', offer offer-[0-9a-f]+ until [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:]{8}Z'
    check "1. b, available, con grant, card verificada y su oferta" \
        grep -qE "^  1\. b \(agent $b\): available, skill repo-review, time 1100 ms, .*grant held, card verified $b$offer\$" <<< "$out"
    check "2. c, busy, con su oferta" \
        grep -qE "^  2\. c \(agent $c\): busy, skill repo-review, time 110 ms, .*grant held, card verified $c$offer\$" <<< "$out"
    check "d descartado por la nube externa" has_line "$out" "  d (offer): external_cloud_allowed not met"
    check "e descartado por la herramienta MCP" has_line "$out" "  e (offer): mcp_integration:example:everything not operational"
    check "g descartado por la RAM ofrecida" has_line "$out" "  g (offer): min_offered_ram_mib not met"
    check "cobertura completa" has_line "$out" "coverage: complete"
    check "cinco contactados" grep -qE '^metrics: contacted 5, ' <<< "$out"
    check "pregunta a b y responde b" grep -qxF -e "asked: b" <<< "$out"
    check "respuesta de b" has_line "$out" "b: $PREGUNTA1"
    local i n detail=
    for i in "${!PROVIDERS[@]}"; do
        x=${PROVIDERS[$i]}
        n=$(en "$x" <<< ejecuciones)
        detail="$detail $x $((n - runs[i]))"
        if [ "$x" = b ]; then check "b ejecuta una vez" test "$((n - runs[i]))" = 1
        else check "$x no ejecuta nada" test "$((n - runs[i]))" = 0; fi
    done
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 1" "$ok" "intentos: $try; ejecuciones nuevas:$detail; $(grep '^metrics:' <<< "$out")"
}

caso2() {
    say "Caso 2. Requisitos sobre integraciones y recursos"
    local code ok=ok
    code=$(en a <<< 'busca caso2')
    note "busca caso2 -> HTTP $code"
    en a <<< 'cat ~/pals-h2/busquedas/caso2.json' > "$LOGS/caso2.json"
    en a <<< 'pals inventory get b --sections mcp' > "$LOGS/caso2-mcp-b.txt" 2>&1
    en a <<< 'pals inventory get e --sections mcp' > "$LOGS/caso2-mcp-e.txt" 2>&1
    tail -n 1 "$LOGS/caso2-mcp-b.txt" "$LOGS/caso2-mcp-e.txt"
    FAILS=0
    check "HTTP 200" test "$code" = 200
    local summary
    summary=$(py "$LOGS/caso2.json" "$LOGS/caso2-mcp-b.txt" "$LOGS/caso2-mcp-e.txt" <<'PY'
import json, sys
def page(path):
    text = open(path).read()
    return json.loads(text[:text.rindex('verified:')])['view']
o = json.load(open(sys.argv[1]))
ranked = [c['peer'] for c in o['ranked']]
print('ranking', ' '.join(ranked))
b = o['ranked'][0]
for r in b['offer']['requirement_results']:
    print('b', r['requirement'], r['result'], r.get('basis'))
for d in o['discarded']:
    print('descarte', d['peer'], d['stage'], d['reason'])
for name, path in (('b', sys.argv[2]), ('e', sys.argv[3])):
    for e in page(path)['entries']:
        if e['id'] == 'example:everything':
            op = e['dimensions'].get('operational') or {}
            tools = ' '.join(t if isinstance(t, str) else t.get('name', '') for t in e['body'].get('tools', []))
            print(f"mcp {name}: {op.get('status')}, {op.get('source', {}).get('method')}, {op.get('detail', '-')}, herramientas [{tools}]")
PY
)
    echo "$summary"
    check "ranking b c" has_line "$summary" "ranking b c"
    check "b cumple external_cloud_allowed por declaración" has_line "$summary" "b external_cloud_allowed met declared"
    check "b cumple el MCP por observación" has_line "$summary" "b mcp_integration:example:everything met observed"
    check "b cumple la RAM ofrecida" has_line "$summary" "b min_offered_ram_mib met declared"
    check "e descartado por el MCP" has_line "$summary" "descarte e offer mcp_integration:example:everything not operational"
    check "g descartado por la RAM" has_line "$summary" "descarte g offer min_offered_ram_mib not met"
    check "el MCP de b, ok por tools/list, con echo y get-sum" \
        has_line "$summary" "mcp b: ok, mcp tools/list, -, herramientas [example:everything/echo example:everything/get-sum]"
    check "el MCP de e, failed: falta read_file" has_line "$summary" "mcp e: failed, mcp tools/list, missing tools: read_file, herramientas []"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 2" "$ok" "$(grep '^mcp' <<< "$summary" | tr '\n' ';')"
}

caso3() {
    say "Caso 3. Validar cards"
    local b out ok=ok
    b=$(agent_id b)
    FAILS=0
    out=$(en a <<< "~/pals-h2/venv/bin/python \"\$REPO/tests/interop/card_signatures.py\" --gateway \"\$(gw)\" --peer $b" 2>&1)
    echo "$out"
    check "Python (a2a-sdk): 3 checks passed" has_line "$out" "3 checks passed"
    en a <<< "curl -sS \"\$(gw)/peers/$b/signed-agent-card.json\"" > "$LOGS/tarjeta-b.json"
    note "card de B por el gateway de A: $(wc -c < "$LOGS/tarjeta-b.json") bytes, sha256 $(sha256sum "$LOGS/tarjeta-b.json" | cut -c1-16)…"
    out=$(py "$LOGS/tarjeta-b.json" <<'PY'
import json, sys
raw = open(sys.argv[1]).read()
card = json.loads(raw)
print('skills', ' '.join(s['id'] for s in card['skills']))
print('menciones de proyecto', sorted(w for w in ('repo-review', 'example:everything', 'example:coder', 'example:host', 'demo') if w in raw) or 'ninguna')
PY
)
    echo "$out"
    check "la card pública de B: solo la skill ask" has_line "$out" "skills ask"
    check "la card pública de B: nada de lo que es de proyecto" has_line "$out" "menciones de proyecto ninguna"
    out=$(go -C "$REPO/tests/interop/card_signatures/go" run . "$b" "$LOGS/tarjeta-b.json" 2>&1)
    echo "Go (a2a-go): $out"
    check "Go (a2a-go): verified" has_line "$out" "verified"
    if [ ! -d "$REPO/tests/interop/card_signatures/js/node_modules" ]; then
        (cd "$REPO/tests/interop/card_signatures/js" && npm ci --no-audit --no-fund) > /dev/null
    fi
    out=$(node "$REPO/tests/interop/card_signatures/js/verify.mjs" "$b" "$LOGS/tarjeta-b.json" 2>&1)
    echo "JS (@a2a-js/sdk): $out"
    check "JS (@a2a-js/sdk): verified" has_line "$out" "verified"

    if [ "$TESTS" = cargo ]; then
        note "los pares de prueba con la card alterada (tests de integración, en el anfitrión)"
        local t
        for t in "cards gateway_refuses_a_tampered_peer_card" "selection cards_are_validated"; do
            set -- $t
            (cd "$REPO" && exec cargo test --locked -p pals --test "$1" -- --exact "$2") > "$LOGS/caso3-test-$2.out" 2>&1 &
            wait $!
            grep -E '^test |^test result' "$LOGS/caso3-test-$2.out"
            check "test $1::$2" grep -q '^test result: ok. 1 passed' "$LOGS/caso3-test-$2.out"
        done
    else
        note "tests de los pares de prueba omitidos (PALS_EXIT_TESTS=$TESTS)"
    fi

    out=$(en a <<EOS 2>&1
F_HOST=$F_HOST
pals contact add-a2a "https://\$F_HOST:8444" --name f-alterada
pals discover --capability example:eco --peer f-alterada
EOS
)
    echo "$out"
    check "F alterado: firma no verificable al guardarlo" has "$out" "card signature unverifiable"
    check "F alterado: descartado en la etapa de card" has_line "$out" "  f-alterada (card): card not verified"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 3" "$ok" "Python, Go y JS verifican la card de B; las alteradas, rechazadas"
}

caso4() {
    say "Caso 4. No ejecutar trabajo al consultar"
    local x t0 t1 ok=ok
    declare -A antes despues
    for x in "${PROVIDERS[@]}"; do antes[$x]=$(counts "$x"); note "antes, $x: task.accepted ejecuciones tools/list pid = ${antes[$x]}"; done
    t0=$(date +%s)
    en_wait "$LOGS/caso4.out" a <<'EOS'
bash ~/pals-h2/caso4.sh rafaga 50
bash ~/pals-h2/caso4.sh seguidas 50 4
EOS
    cat "$LOGS/caso4.out"
    en a <<< 'for m in rafaga seguidas; do echo "== $m"; python3 ~/pals-h2/metricas.py ~/pals-h2/busquedas caso4-$m- | grep -v "^caso4-"; done' \
        | tee "$LOGS/caso4-metricas.txt"
    local health=
    for x in a "${PROVIDERS[@]}"; do health="$health $x $(en "$x" <<< 'curl -sS -o /dev/null -w "%{http_code}" "$(gw)/healthz"')"; done
    note "healthz:$health"
    local after_code
    after_code=$(en a <<< 'busca caso4-despues b c d e g')
    note "búsqueda de después -> HTTP $after_code"
    t1=$(date +%s)
    for x in "${PROVIDERS[@]}"; do despues[$x]=$(counts "$x"); note "después, $x: ${despues[$x]}"; done
    mkdir -p "$LOGS/busquedas"
    en a <<< 'cd ~/pals-h2/busquedas && tar -c caso4-*' | tar -x -C "$LOGS/busquedas"

    FAILS=0
    local codes summary
    codes=$(for m in rafaga seguidas; do echo "$m: $(cat "$LOGS/busquedas/caso4-$m-"*.codigo | sort | uniq -c | tr -s ' ' | tr '\n' ',')"; done | tr '\n' ' ')
    summary=$(py "$LOGS/busquedas" <<'PY'
import json, sys
from collections import Counter
from pathlib import Path
d = Path(sys.argv[1])
expected = {'d': 'external_cloud_allowed not met', 'e': 'mcp_integration:example:everything not operational',
            'g': 'min_offered_ram_mib not met'}
# What the limits of a search and the providers' quotas discard.
LIMITS = {'refused: RateLimited', 'timeout', 'time budget'}
for mode in ('rafaga', 'seguidas'):
    codes, bad, limited, full = Counter(), [], Counter(), 0
    for c in sorted(d.glob(f'caso4-{mode}-*.codigo')):
        code = c.read_text().strip()
        codes[code] += 1
        body = json.loads((d / c.name.replace('.codigo', '.json')).read_text())
        if code == '429':
            if body != {'error': 'too many searches'}:
                bad.append(c.stem)
            continue
        if code != '200':
            bad.append(c.stem)
            continue
        ranked = [x['peer'] for x in body['ranked']]
        why = {x['peer']: x['reason'] for x in body['discarded']}
        for peer, reason in why.items():
            if reason in LIMITS:
                limited[f'{peer} {reason}'] += 1
            elif expected.get(peer) != reason:
                bad.append(f'{c.stem}: {peer} {reason}')
        if ranked != [p for p in ('b', 'c') if why.get(p) not in LIMITS] \
                or set(ranked) | set(why) != {'b', 'c', 'd', 'e', 'g'}:
            bad.append(f'{c.stem}: {ranked}')
        # Every candidate answered: b and c ranked, d, e and g discarded by
        # their own offers.
        if ranked == ['b', 'c'] and why == expected:
            full += 1
    print(f"{mode}: {sum(codes.values())} búsquedas, códigos {dict(sorted(codes.items()))}, "
          f"completas {full}, descartes por cuotas o plazos {dict(limited) or 'ninguno'}, anómalas {bad or 'ninguna'}")
PY
)
    echo "$summary"
    # The burst must be cut by the quota, and the searches in a row must
    # reach the providers: at least SEGUIDAS_COMPLETAS of the 50 with every
    # candidate's answer.
    local full
    full=$(sed -nE "s/^seguidas: .*, completas ([0-9]+), .*/\1/p" <<< "$summary")
    check "la ráfaga: 50 búsquedas, al menos un 200 y un 429 (too many searches), ninguna anómala" \
        grep -qE "^rafaga: 50 búsquedas, códigos \{'200': [0-9]+, '429': [0-9]+\}, .*anómalas ninguna\$" <<< "$summary"
    check "las seguidas: 50 búsquedas, todas 200, ninguna anómala" \
        grep -qE "^seguidas: 50 búsquedas, códigos \{'200': 50\}, .*anómalas ninguna\$" <<< "$summary"
    check "las seguidas llegan a los proveedores: $full de 50 con el ranking completo (se exigen $SEGUIDAS_COMPLETAS)" \
        test "${full:-0}" -ge "$SEGUIDAS_COMPLETAS"
    check "healthz 200 en A y en B a G" test -z "$(tr ' ' '\n' <<< "$health" | grep -E '^[0-9]+$' | grep -vx 200)"
    check "búsqueda de después, 200" test "$after_code" = 200
    local a d ticks detail= calls
    for x in "${PROVIDERS[@]}"; do
        read -r -a a <<< "${antes[$x]}"; read -r -a d <<< "${despues[$x]}"
        ticks=$(timer_ticks "$x" "$t0" "$t1")
        detail="$detail $x: task.accepted +$((d[0] - a[0])), ejecuciones +$((d[1] - a[1])), tools/list +$((d[2] - a[2])) (temporizador $ticks);"
        check "$x: ningún task.accepted nuevo" test "${d[0]}" = "${a[0]}"
        check "$x: ninguna ejecución nueva" test "${d[1]}" = "${a[1]}"
        check "$x: el mismo daemon (PID ${a[3]})" test "${d[3]}" = "${a[3]}"
        check "$x: tools/list solo por el temporizador (vencimientos en el caso: $ticks)" test "$((d[2] - a[2]))" -le "$ticks"
    done
    calls=$(tools_calls)
    note "tools/call recibidos hasta ahora por cada servidor MCP: $calls"
    check "ningún tools/call en ningún servidor MCP" test -z "$(tr ' ' '\n' <<< "$calls" | grep -E '^[0-9]+$' | grep -vx 0)"
    note "arranques del servidor MCP de B (uno por comprobación del adaptador: al arrancar, pals inventory refresh y el temporizador de 60 s); el caso 4 fue de $(date -u -d "@$t0" +%T) a $(date -u -d "@$t1" +%T):"
    en b <<< 'cat "$H2/b-mcp-arranques.log"'
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 4" "$ok" "$codes; seguidas con el ranking completo: ${full:-0} de 50 (se exigen $SEGUIDAS_COMPLETAS);$detail tools/call: $calls"
}

caso5() {
    say "Caso 5. Oferta vencida y proveedor ocupado"
    local ta th token out ok=ok
    FAILS=0
    note "H: guarda a A, A guarda a H con su grant; después H se para"
    ta=$(en a <<< 'pals id --ticket') || die "no ticket from A"
    en h <<< "pals contact add '$ta' --name a" || die "H cannot save A"
    th=$(en h <<< 'pals id --ticket') || die "no ticket from H"
    token=$(en h <<< 'pals grant issue --to a --project demo --ttl 24h') || die "H cannot issue a grant"
    en a <<< "pals contact add '$th' --name h && pals grant import '$token'" || die "A cannot save H"
    en h <<< 'para' || die "H does not stop"

    note "B vuelve a arrancar con ofertas de 2 s"
    en b <<< 'para && config_proveedor --oferta-ms 2000 && arranca' || die "B did not restart"
    local runs_b runs_c
    runs_b=$(en b <<< ejecuciones); runs_c=$(en c <<< ejecuciones)

    say "5a. Una oferta vencida no se usa"
    en_wait "$LOGS/caso5a.out" a <<'EOS'
pals discover --capability code.rust.review --requirements-file ~/pals-h2/req.json --peer b | tee ~/pals-h2/oferta-b.txt
read -r OFERTA HASTA < <(sed -nE 's/^  1\. b .*, offer ([^ ]+) until ([^ ]+)$/\1 \2/p' ~/pals-h2/oferta-b.txt)
espera 30 "que venza la oferta" sh -c "[ \$(date +%s) -ge $(date -d "$HASTA" +%s) ]"
echo "la oferta $OFERTA vence $HASTA; ahora $(date -u +%FT%TZ)"
pals ask b "pregunta con la oferta vencida" --offer "$OFERTA"; echo "código $?"
EOS
    out=$(cat "$LOGS/caso5a.out")
    echo "$out"
    check "la oferta de b, available, con su id y su caducidad" \
        grep -qE "^  1\. b \(agent $(agent_id b)\): available, .*, offer offer-[0-9a-f]+ until [0-9T:-]+Z\$" <<< "$out"
    check "la pregunta con la oferta vencida: offer expired" has "$out" "offer expired: run pals discover again"
    check "código 2" has_line "$out" "código 2"
    check "b no ejecuta nada" test "$(en b <<< ejecuciones)" = "$runs_b"

    say "5b. La oferta vence entre la búsqueda y la pregunta"
    local try summary renewed=0
    for try in 1 2 3; do
        runs_b=$(en b <<< ejecuciones); runs_c=$(en c <<< ejecuciones)
        en_wait "$LOGS/caso5b-$try.out" a <<EOS
code=\$(busca caso5-$try --ask "pregunta del caso 5, intento $try" b c h); echo "busca -> HTTP \$code"
pals task ls | tail -n 3
EOS
        cat "$LOGS/caso5b-$try.out"
        en a <<< "cat ~/pals-h2/busquedas/caso5-$try.json" > "$LOGS/caso5-$try.json"
        summary=$(py "$LOGS/caso5-$try.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
s, c = r['search'], r.get('chosen')
by = {x['peer']: x for x in s['ranked']}
print('ranking', ' '.join(f"{x['peer']}:{x['offer']['status']}" for x in s['ranked']))
print('descartes', '; '.join(f"{d['peer']}: {d['reason']}" for d in s['discarded']))
print('busqueda', s['metrics']['elapsed_ms'], 'ms')
if c:
    first = by.get(c['candidate']['peer'], {}).get('offer', {})
    used = c['candidate']['offer']
    print('elegido', c['candidate']['peer'], c['outcome']['state'], c['outcome'].get('answer'))
    print('probados', c['tried'])
    print('oferta del ranking', first.get('offer_id'), first.get('expires_at'))
    print('oferta usada', used['offer_id'], used['expires_at'])
    print('renovada' if used['offer_id'] != first.get('offer_id') else 'la misma')
else:
    print('sin elegido', r.get('error'), r.get('tried'))
PY
)
        echo "$summary"
        local nb nc qb
        nb=$(en b <<< ejecuciones); nc=$(en c <<< ejecuciones)
        qb=$(en b <<< "grep -c 'pregunta del caso 5, intento $try' \"\$H2/b-ejecuciones.log\"")
        note "intento $try: b ejecuta la pregunta $qb veces (+$((nb - runs_b)) en total), c +$((nc - runs_c))"
        check "intento $try: ranking b:available c:busy" has_line "$summary" "ranking b:available c:busy"
        check "intento $try: h descartado" grep -qE '^descartes .*h: (timeout|card: peer unreachable)' <<< "$summary"
        check "intento $try: la hace b, completada" has_line "$summary" "elegido b TASK_STATE_COMPLETED b: pregunta del caso 5, intento $try"
        check "intento $try: b la ejecuta una vez" test "$qb" = 1
        check "intento $try: c no ejecuta nada" test "$nc" = "$runs_c"
        if has_line "$summary" renovada; then
            renewed=1
            note "intento $try: la oferta vencida se renovó"
            break
        fi
        note "intento $try: la oferta de b seguía en vigor al preguntar: se repite"
    done
    check "la oferta se renovó en algún intento" test "$renewed" = 1
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 5" "$ok" "intentos: $try; $(grep -E '^(oferta|renovada|la misma)' <<< "$summary" | tr '\n' ';')"
}

caso6() {
    say "Caso 6. Vistas y caché por permisos"
    local ok=ok out
    en a <<< 'export PRIVADAS; python3 ~/pals-h2/vista.py "$(gw)" b a-1; python3 ~/pals-h2/vista.py "$(gw)" b a-2 ~/pals-h2/vistas/a-1.cursor'
    en g <<< 'export PRIVADAS; python3 ~/pals-h2/vista.py "$(gw)" b g-1; python3 ~/pals-h2/vista.py "$(gw)" b g-2 ~/pals-h2/vistas/g-1.cursor'
    out=$(en g <<< 'pals query b --capability code.rust.review | tail -n 1; pals query b --capability example:no-existe | tail -n 1')
    echo "$out"
    # Each cursor to the other requester's container.
    en a <<< 'cat ~/pals-h2/vistas/a-1.cursor' | put g pals-h2/vistas/a-1.cursor
    en g <<< 'cat ~/pals-h2/vistas/g-1.cursor' | put a pals-h2/vistas/g-1.cursor
    en g <<< 'export PRIVADAS; python3 ~/pals-h2/vista.py "$(gw)" b g-con-cursor-de-a ~/pals-h2/vistas/a-1.cursor'
    en a <<< 'export PRIVADAS; python3 ~/pals-h2/vista.py "$(gw)" b a-con-cursor-de-g ~/pals-h2/vistas/g-1.cursor'
    mkdir -p "$LOGS/vistas"
    en a <<< 'cd ~/pals-h2/vistas && tar -c a-*.json' | tar -x -C "$LOGS/vistas"
    en g <<< 'cd ~/pals-h2/vistas && tar -c g-*.json' | tar -x -C "$LOGS/vistas"
    en b <<< 'pals inventory show --scope public' > "$LOGS/vistas/b-publica-local.json"
    FAILS=0
    local summary
    summary=$(py "$LOGS/vistas" "$(agent_id a)" <<'PY'
import json, sys
from pathlib import Path
d, a_id = Path(sys.argv[1]), sys.argv[2]
private = {'repo-review', 'example:everything', 'example:coder', 'example:host'}
def view(name):
    return json.loads((d / f'{name}.json').read_text())['view']
a1, a2, g1, g2 = (view(n) for n in ('a-1', 'a-2', 'g-1', 'g-2'))
a_ids = [e['id'] for e in a1['entries'] + a2['entries']]
g_ids = [e['id'] for e in g1['entries'] + g2['entries']]
print('a', a1['scope'], a1.get('audience') == a_id, len(a_ids), 'cursor' if a1.get('next_cursor') else '-', 'cursor' if a2.get('next_cursor') else '-', sorted(private - set(a_ids)) or 'todas')
print('g', g1['scope'], g1.get('audience', '-'), len(g_ids), 'cursor' if g1.get('next_cursor') else '-', 'cursor' if g2.get('next_cursor') else '-')
notes = {f'example:nota-{i:03d}' for i in range(1, 121)}
print('g solo notas', set(g_ids) == notes and len(g_ids) == 120)
raw = (d / 'g-1.json').read_text() + (d / 'g-2.json').read_text()
leaks = sorted(w for w in private | {'demo', 'project'} if w in raw)
print('g menciones de proyecto', leaks or 'ninguna')
schema = {'schema', 'agent_id', 'scope', 'audience', 'seq', 'issued_at', 'expires_at', 'agent_card_hash', 'sections', 'entries', 'next_cursor', 'truncated_by'}
print('g claves fuera del esquema', sorted((set(g1) | set(g2)) - schema) or 'ninguna')
local = json.loads((d / 'b-publica-local.json').read_text())
by_id = lambda e: e['id']
local_entries = sorted(local['entries'], key=by_id)
print('g igual a la vista pública de b', local_entries == sorted(g1['entries'] + g2['entries'], key=by_id))
cross = [json.loads((d / f'{n}.json').read_text()) for n in ('g-con-cursor-de-a', 'a-con-cursor-de-g')]
print('cursores cruzados', cross[0], cross[1], cross[0] == cross[1] and 'view' not in cross[0])
PY
)
    echo "$summary"
    check "A: task:demo, para A, 124 entradas con las de proyecto, cursor en la página 1" \
        has_line "$summary" "a task:demo True 124 cursor - todas"
    check "G: public, sin destinatario, 120 entradas, cursor en la página 1" has_line "$summary" "g public - 120 cursor -"
    check "G: solo las 120 notas" has_line "$summary" "g solo notas True"
    check "G: nada que mencione lo de proyecto" has_line "$summary" "g menciones de proyecto ninguna"
    check "G: ninguna clave fuera del esquema" has_line "$summary" "g claves fuera del esquema ninguna"
    check "G: la vista pública de B, entrada por entrada (cada entrada entera)" has_line "$summary" "g igual a la vista pública de b True"
    check "cursores cruzados: rechazados, con el mismo error" grep -qE '^cursores cruzados .* True$' <<< "$summary"
    check "pals query desde G: not_offered en los dos casos" \
        test "$(grep -c "^verified: $(agent_id b) [^ ]* not_offered\$" <<< "$out")" = 2
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 6" "$ok" "$(grep '^cursores' <<< "$summary")"
}

caso7() {
    say "Caso 7. Contacto A2A corriente"
    local out ok=ok
    en_wait "$LOGS/caso7.out" a <<EOS
F_HOST=$F_HOST
pals contact add-a2a "https://\$F_HOST:8443" --name f
pals ask f "hola, F"; echo "código \$?"
echo "== JSON-RPC"
curl -sS -X POST -H 'content-type: application/json' -H 'a2a-version: 1.0' "\$(gw)/peers/f/jsonrpc" \\
    -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"caso7-jsonrpc","role":"ROLE_USER","parts":[{"text":"por JSON-RPC"}]}}}'; echo
echo "== HTTP+JSON"
curl -sS -X POST -H 'content-type: application/json' -H 'a2a-version: 1.0' "\$(gw)/peers/f/rest/message:send" \\
    -d '{"message":{"messageId":"caso7-rest","role":"ROLE_USER","parts":[{"text":"por HTTP+JSON"}]}}'; echo
echo "== card"
curl -sS "\$(gw)/peers/f/.well-known/agent-card.json"; echo
echo "== búsqueda"
pals discover --capability example:eco --peer f --peer f-alterada
echo "== http"
pals contact add-a2a "http://\$F_HOST:8443" --name f-http; echo "código \$?"
EOS
    out=$(cat "$LOGS/caso7.out")
    echo "$out"
    FAILS=0
    check "F guardado, firma verificada" has "$out" "card signature verified f-1"
    check "pals ask f: F: hola, F" has_line "$out" "F: hola, F"
    check "código 0" has_line "$out" "código 0"
    local summary
    summary=$(python3 -c '
import json, sys
text = sys.stdin.read()
def after(tag):
    return text.split(tag + "\n", 1)[1].split("\n", 1)[0]
rpc = json.loads(after("== JSON-RPC"))["result"]["task"]
rest = json.loads(after("== HTTP+JSON"))["task"]
card = json.loads(after("== card"))
for name, t in (("jsonrpc", rpc), ("rest", rest)):
    print(name, t["status"]["state"], t["artifacts"][0]["parts"][0]["text"])
print("interfaces", " ".join(i["url"].split("/peers/", 1)[1] + ":" + i["protocolBinding"] for i in card["supportedInterfaces"]))
print("securitySchemes", "securitySchemes" in card)
' <<< "$out" 2>&1)
    echo "$summary"
    check "JSON-RPC: completada, F: por JSON-RPC" has_line "$summary" "jsonrpc TASK_STATE_COMPLETED F: por JSON-RPC"
    check "HTTP+JSON: completada, F: por HTTP+JSON" has_line "$summary" "rest TASK_STATE_COMPLETED F: por HTTP+JSON"
    check "la card de F por el gateway de A, sus interfaces en el gateway" has_line "$summary" "interfaces f/jsonrpc:JSONRPC f/rest:HTTP+JSON"
    check "sin securitySchemes" has_line "$summary" "securitySchemes False"
    check "búsqueda: f, card only y verificada" \
        grep -qE '^  1\. f \(plain A2A agent\): card only, skill eco, .*card verified f-1$' <<< "$out"
    check "búsqueda: f-alterada descartada" has_line "$out" "  f-alterada (card): card not verified"
    check "http:// a otra máquina, rechazado" has "$out" "http:// is only for 127.0.0.1, [::1] and localhost"
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 7" "$ok" "F por https con la CA de prueba; $(grep -E '^(jsonrpc|rest)' <<< "$summary" | tr '\n' ';')"
}

caso8() {
    say "Caso 8. Migración"
    local ok=ok
    if [ -z "$PALS_H1" ] || [ ! -x "$PALS_H1" ]; then
        note "falta el pals del hito 1: PALS_EXIT_BIN_HITO1 (runbook, sección 1)"
        result "caso 8" fail "sin el pals del hito 1"
        return 1
    fi
    [ -x "$EVENTS_VERIFY" ] || die "no events_verify at $EVENTS_VERIFY: cargo build --release --locked -p pals-core --example events_verify"
    put_exec m pals-h2/pals-hito1 < "$PALS_H1" || die "cannot copy the milestone 1 pals"
    put_exec m pals-h2/events_verify < "$EVENTS_VERIFY" || die "cannot copy events_verify"
    en_wait "$LOGS/caso8.out" m <<< 'bash ~/pals-h2/caso8.sh ~/pals-h2/pals-hito1 ~/pals-h2/events_verify'
    cat "$LOGS/caso8.out"
    FAILS=0
    check "caso8.sh: superado" grep -qx 'caso 8: superado' "$LOGS/caso8.out"
    check "events_verify de M1 y de M2" test "$(grep -c '^events_verify: [0-9]* events of [0-9a-f]* verified$' "$LOGS/caso8.out")" = 2
    [ "$FAILS" = 0 ] || ok=fail
    result "caso 8" "$ok" "$(grep -E '^events_verify|esquema 1 -> 2' "$LOGS/caso8.out" | tr '\n' ';')"
}

DESMONTADO=0
desmontar() {
    DESMONTADO=1
    say "Desmontaje"
    local c x
    # The metrics of every search A answered, with the runbook's metricas.py.
    if [ "$(label_of "$P-a")" = "$WORK" ] \
        && docker exec "$P-a" sh -c 'ls ~/pals-h2/busquedas/*.json' > /dev/null 2>&1; then
        en a <<< 'python3 ~/pals-h2/metricas.py' > "$LOGS/metricas.txt" 2>&1
        note "métricas de todas las búsquedas de A en $LOGS/metricas.txt:"
        grep -v '^[a-z0-9-]*	' "$LOGS/metricas.txt"
    fi
    # Over the whole run: no MCP server ever got a tools/call.
    if [ "$(label_of "$P-b")" = "$WORK" ]; then
        local calls
        calls=$(tools_calls)
        if [ -z "$(tr ' ' '\n' <<< "$calls" | grep -E '^[0-9]+$' | grep -vx 0)" ]; then
            result "sin tools/call en toda la prueba" ok "$calls"
        else
            result "sin tools/call en toda la prueba" fail "$calls"
        fi
    fi
    # Logs: daemons, workers' runs, MCP servers, F, A's searches.
    for c in $(ours_containers); do
        x=${c#"$P-"}
        mkdir -p "$LOGS/$x"
        docker exec "$c" sh -c 'cd ~/pals-h2 2> /dev/null && tar -c --ignore-failed-read */daemon.log *-ejecuciones.log *-mcp-*.log f-*.log busquedas migracion/*/daemon.log 2> /dev/null' \
            | tar -x -C "$LOGS/$x" 2> /dev/null
    done
    for c in $(ours_containers); do
        if [ "$(label_of "$c")" = "$WORK" ]; then
            docker rm -f "$c" > /dev/null && note "contenedor $c eliminado"
        fi
    done
    for c in $(ours_networks); do
        docker network rm "$c" > /dev/null && note "red $c eliminada"
    done
    local left
    left=$(any_containers; any_networks)
    [ -z "$left" ] || note "AVISO: quedan recursos $P-* de otra ejecución, que no se tocan: $(echo $left)"
    if [ "$KEEP_IMAGE" = 0 ] \
        && [ -n "$(docker image inspect -f '{{ index .Config.Labels "pals-exit.image" }}' "$IMAGE" 2> /dev/null)" ]; then
        docker image rm "$IMAGE" > /dev/null && note "imagen $IMAGE eliminada"
    else
        note "la imagen $IMAGE se conserva (PALS_EXIT_KEEP_IMAGE=0 la elimina)"
    fi
    rm -rf -- "$WORK/imagen" "$WORK/ca.crt"
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
        caso1) caso1 ;;
        caso2) caso2 ;;
        caso3) caso3 ;;
        caso4) caso4 ;;
        caso5) caso5 ;;
        caso6) caso6 ;;
        caso7) caso7 ;;
        caso8) caso8 ;;
        desmontar) desmontar ;;
        *) die "unknown phase $phase" ;;
    esac
done
if [ -f "$LOGS/resultados.txt" ]; then
    say "Resultados"
    cat "$LOGS/resultados.txt"
    ! grep -q FALLIDO "$LOGS/resultados.txt"
fi
