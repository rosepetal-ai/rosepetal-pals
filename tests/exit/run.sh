#!/usr/bin/env bash
# The hito 1 exit test (docs/runbooks/hito-1-dos-maquinas.md) on one host,
# with a Docker container standing in for the second machine:
#
#   A: a container (tests/exit/Dockerfile) on its own bridge network, behind
#      Docker's NAT, with NET_ADMIN so that the network cuts of cases 3 and 4
#      are real packet drops (iptables) inside A's own network namespace. The
#      third identity of case 7 is a third daemon inside A.
#   B: this host, outside any container, with its own PALS_HOME under
#      $PALS_EXIT_DIR and its gateway on loopback. Case 1 runs the host's
#      `claude` against a project of the host.
#
# Both daemons run with `net.relay = true` (Iroh's public relays). The relay
# repetition of case 3 forces the relay by dropping A's outgoing UDP except
# DNS and loopback, as the runbook says.
#
# The six scripts the runbook writes to ~/pals-salida (funciones.sh,
# pals-prueba.sh, corte-red.sh, caso3.sh, caso4-eventos.py, caso8.py) are
# extracted from the runbook itself. The runbook's other commands are not:
# A's are copied into this file and run as root in A's container (~/.pals,
# ~/pals-salida, with funciones.sh loaded), and B's are reimplemented below
# (pb, arranca_b, para_b, write_b_config) with B's paths. A change to those
# blocks of the runbook has to be carried over here by hand.
#
# Usage:
#   PALS_EXIT_DIR=<new or empty dir> tests/exit/run.sh [phase...]
# Phases, in order: preparar caso1 cambio-worker caso2 caso3 caso4 caso5
# caso6 caso7 caso8 limpieza desmontar; `todo` (the default) runs them all.
# Each case prints `RESULTADO <case>: superado|FALLIDO`, collected in
# $PALS_EXIT_DIR/registro/resultados.txt; the full log is
# $PALS_EXIT_DIR/registro/run.log. See tests/exit/README.md.
#
# `preparar` only accepts a new or empty PALS_EXIT_DIR, and marks it with a
# .pals-exit file; every other phase refuses a directory without that mark,
# and `desmontar` deletes only what `preparar` made there. The container and
# the network carry the label pals-exit.dir=<PALS_EXIT_DIR>, and `desmontar`
# removes them only when the label is this directory. B's daemon is signalled
# only after checking that the PID in its pid file is this run's `pals
# daemon`. An interrupted run (Ctrl-C, SIGTERM) tears itself down; after any
# other failure, run `desmontar` with the same PALS_EXIT_DIR.
#
# Environment:
#   PALS_EXIT_DIR        required: B's PALS_HOME, B's ~/pals-salida, the
#                        project copy and the logs go here
#   PALS_EXIT_BIN        the pals binary [target/release/pals]
#   PALS_EXIT_CASO1      claude (default): case 1 with the `claude` on PATH,
#                        two `claude -p` calls; omitir: skip case 1
#   PALS_EXIT_PROYECTO   B's project for case 1 [a clone of this repo at HEAD]
#   PALS_EXIT_B_GATEWAY  B's gateway [127.0.0.1:17420]
#   PALS_EXIT_LIMITE     the sender's limit in case 3 when a transfer ends
#                        before the cut [50mbit]
#   PALS_EXIT_KEEP_IMAGE 1 (default) keeps the image on `desmontar`; 0 removes it

set -uo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
RUNBOOK=$REPO/docs/runbooks/hito-1-dos-maquinas.md
WORK=${PALS_EXIT_DIR:?set PALS_EXIT_DIR to a new or empty directory for B and the logs}
# Absolute: B's config names files in it, and its commands run elsewhere.
WORK=$(realpath -m -- "$WORK")
MARKER=$WORK/.pals-exit
PALS=${PALS_EXIT_BIN:-$REPO/target/release/pals}
CASO1=${PALS_EXIT_CASO1:-claude}
B_GATEWAY=${PALS_EXIT_B_GATEWAY:-127.0.0.1:17420}
KEEP_IMAGE=${PALS_EXIT_KEEP_IMAGE:-1}
A=pals-exit-a
NET=pals-exit-net
IMAGE=pals-exit-a:latest

B_HOME=$WORK/b          # B's PALS_HOME (the runbook's ~/.pals on B)
B_SAL=$WORK/b-salida    # B's ~/pals-salida
LOGS=$WORK/registro
PROYECTO=${PALS_EXIT_PROYECTO:-$WORK/proyecto}
# What B's daemon, and so its worker's commands, get: the runbook starts it
# from a terminal where `claude` is on PATH.
B_PATH=$PATH
B_LANG=${LANG:-C.UTF-8}
FULL=268435456          # 256 MiB

phases=("$@")
[ ${#phases[@]} -gt 0 ] || phases=(todo)
TODO=0
if [ "${phases[0]}" = todo ]; then
    TODO=1
    phases=(preparar caso1 cambio-worker caso2 caso3 caso4 caso5 caso6 caso7 caso8 limpieza desmontar)
fi
for phase in "${phases[@]}"; do
    case $phase in
        preparar|caso[1-8]|cambio-worker|limpieza|desmontar) ;;
        *) echo "run.sh: unknown phase $phase" >&2; exit 2 ;;
    esac
done
# Checked before anything is written: `preparar` makes and marks a new or
# empty directory; the other phases need that mark.
if [[ " ${phases[*]:1} " == *" preparar "* ]]; then
    echo "run.sh: preparar must be the first phase" >&2; exit 2
fi
if [ "${phases[0]}" = preparar ]; then
    if [ -e "$WORK" ] && { [ ! -d "$WORK" ] || [ -n "$(ls -A -- "$WORK")" ]; }; then
        echo "run.sh: $WORK exists and is not an empty directory: preparar needs a new or empty PALS_EXIT_DIR" >&2
        exit 2
    fi
    mkdir -p -- "$WORK" && printf 'pals-exit harness\n' > "$MARKER" || exit 2
elif [ ! -f "$MARKER" ]; then
    echo "run.sh: $WORK has no .pals-exit mark: it was not made by \`preparar\`, and no phase but preparar touches it" >&2
    exit 2
fi

mkdir -p "$LOGS"
exec > >(tee -a "$LOGS/run.log") 2>&1

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

# The heredoc the runbook writes to ~/pals-salida/$1.
extract() {
    awk -v start="cat > ~/pals-salida/$1 <<'EOF'" '
        $0 == start { on = 1; found = 1; next }
        on && $0 == "EOF" { exit }
        on { print }
        END { if (!found) exit 1 }' "$RUNBOOK"
}

# Runs the bash script on stdin in A, as a new terminal of the runbook would:
# with funciones.sh loaded. The script is read whole first, so that the
# commands it runs never read it as their input.
en_a() {
    docker exec -i "$A" bash -c \
        'cd; [ ! -f ~/pals-salida/funciones.sh ] || . ~/pals-salida/funciones.sh; eval "$(cat)"'
}
# Copies stdin to the file $1 in A; a leading ~ is A's home.
a_write() { docker exec -i "$A" sh -c "cat > $1"; }

pb() { PALS_HOME=$B_HOME "$PALS" "$@"; }

# is_b_daemon PID: whether PID is this run's B daemon: the command line
# `$PALS daemon` and PALS_HOME=$B_HOME in its environment. A pid file can
# outlive its process (a reboot, a crash) and name an unrelated one.
is_b_daemon() {
    local pid=$1
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    [ "$(tr '\0' ' ' 2> /dev/null < "/proc/$pid/cmdline")" = "$PALS daemon " ] || return 1
    tr '\0' '\n' 2> /dev/null < "/proc/$pid/environ" | grep -qxF "PALS_HOME=$B_HOME"
}
# b_daemon_pid: the PID in B's pid file, if it is this run's B daemon.
b_daemon_pid() {
    local pid
    pid=$(cat "$B_SAL/daemon.pid" 2> /dev/null) && is_b_daemon "$pid" && echo "$pid"
}

# Starts B's daemon in its own session, so that it outlives this script, with
# the environment of a clean terminal.
arranca_b() {
    if b_daemon_pid > /dev/null; then die "B's daemon is already running ($(b_daemon_pid))"; fi
    env -i PATH="$B_PATH" HOME="$HOME" LANG="$B_LANG" PALS_HOME="$B_HOME" \
        ${RUST_LOG:+RUST_LOG="$RUST_LOG"} \
        setsid "$PALS" daemon >> "$B_SAL/daemon.log" 2>&1 < /dev/null &
    local pid=$!
    echo "$pid" > "$B_SAL/daemon.pid"
    disown   # case 5 kills it: no job notice
    local _
    for _ in $(seq 1 120); do
        pb id --ticket > /dev/null 2>&1 && break
        kill -0 "$pid" 2>/dev/null || die "B's daemon exited, see $B_SAL/daemon.log"
        sleep 0.5
    done
    is_b_daemon "$pid" || die "B's daemon ($pid) is not running as expected"
    sleep 5   # the runbook: wait a few seconds, so that the ticket carries the relay
    grep 'pals daemon:' "$B_SAL/daemon.log" | tail -n 1
}
# Stops B's daemon, if its pid file names it: SIGTERM, then up to 60 s for it
# to end. Removes the pid file once it has.
para_b() {
    local pid _
    if ! pid=$(b_daemon_pid); then
        rm -f "$B_SAL/daemon.pid"
        return 0
    fi
    kill "$pid"
    for _ in $(seq 1 300); do
        is_b_daemon "$pid" || { rm -f "$B_SAL/daemon.pid"; return 0; }
        sleep 0.2
    done
    note "AVISO: el daemon de B ($pid) sigue vivo 60 s después de SIGTERM"
    return 1
}

# A string as a literal in an extended regular expression.
# (`]` first and `[` last in the bracket: `[.` would open a collating symbol.)
ere_quote() { printf '%s' "$1" | sed 's/[].^$*+?(){}|\\[]/\\&/g'; }
# The test worker's commands on B: the PIDs of the shells that run B's
# pals-prueba.sh, matched by the start of their command line, so that no
# other process that merely mentions the script counts (the runbook's
# `grep '[p]als-prueba'` would).
b_workers() { pgrep -f "^sh $(ere_quote "$B_SAL/pals-prueba.sh")( |\$)" || true; }
# b_worker_of TASK_ID: the test worker's shell that runs that task, by the
# PALS_TASK_ID the exec worker gives its command.
b_worker_of() {
    local p
    for p in $(b_workers); do
        if tr '\0' '\n' 2> /dev/null < "/proc/$p/environ" | grep -qxF "PALS_TASK_ID=$1"; then
            echo "$p"; return 0
        fi
    done
    return 1
}
# group_of PID: its process group; ps_groups PGID…: their processes, with a header.
group_of() { ps -o pgid= -p "$1" | tr -d ' '; }
ps_groups() { ps -eo pid,pgid,etime,args | awk -v gs=" $* " 'NR == 1 || index(gs, " " $2 " ")'; }

# Bytes through A's eth0, from the accounting rules `preparar` appends to A's
# chains: UDP except DNS (the direct path, and the relays' address discovery),
# and TCP 443 (the relays). Prints "udp_out udp_in tcp443_out tcp443_in".
counters() {
    docker exec "$A" sh -c 'iptables -nvxL OUTPUT; iptables -nvxL INPUT' | awk '
        /pals-exit-udp-out/ { uo = $2 } /pals-exit-udp-in/ { ui = $2 }
        /pals-exit-443-out/ { to = $2 } /pals-exit-443-in/ { ti = $2 }
        END { print uo + 0, ui + 0, to + 0, ti + 0 }'
}
# path LABEL "BEFORE": prints the bytes since BEFORE and which path they say,
# and leaves that path in CAMINO: directo, relay or mixto.
CAMINO=
path() {
    local label=$1 b; read -r -a b <<< "$2"
    local a; read -r -a a <<< "$(counters)"
    local udp=$(( a[0] - b[0] + a[1] - b[1] )) tcp=$(( a[2] - b[2] + a[3] - b[3] ))
    local which=mixto
    CAMINO=mixto
    if [ "$udp" -gt $(( 4 * tcp )) ]; then which="directo (UDP)"; CAMINO=directo
    elif [ "$tcp" -gt $(( 4 * udp )) ]; then which="relay (TCP 443)"; CAMINO=relay; fi
    note "camino $label: $which; por eth0 de A, UDP sin DNS: $(( a[0] - b[0] )) B enviados, $(( a[1] - b[1] )) B recibidos; TCP 443: $(( a[2] - b[2] )) B enviados, $(( a[3] - b[3] )) B recibidos"
}

elapsed() { echo $(( $(date +%s) - $1 )) s; }

# The PALS_EXIT_DIR of the run that made container $A or network $NET, from
# its label; empty when it does not exist.
container_dir() { docker inspect -f '{{ index .Config.Labels "pals-exit.dir" }}' "$A" 2> /dev/null; }
network_dir() { docker network inspect -f '{{ index .Labels "pals-exit.dir" }}' "$NET" 2> /dev/null; }
container_exists() { docker ps -a --format '{{.Names}}' | grep -qx "$A"; }
network_exists() { docker network ls --format '{{.Name}}' | grep -qx "$NET"; }

# --- phases ----------------------------------------------------------------

preparar() {
    say "Preparación: imagen, red y contenedor de A; directorios de B"
    [ -x "$PALS" ] || die "no pals binary at $PALS: cargo build --release --locked -p pals"
    command -v docker > /dev/null || die "docker is required"
    if [ "$CASO1" = claude ]; then
        command -v claude > /dev/null || die "case 1 needs claude on PATH (or PALS_EXIT_CASO1=omitir)"
    fi
    local owner
    if container_exists; then
        owner=$(container_dir)
        die "the container $A exists, made by the run in ${owner:-an unknown directory}: another run may be in progress; if not, run \`desmontar\` with that PALS_EXIT_DIR"
    fi
    if network_exists; then
        owner=$(network_dir)
        die "the network $NET exists, made by the run in ${owner:-an unknown directory}: another run may be in progress; if not, run \`desmontar\` with that PALS_EXIT_DIR"
    fi

    note "fecha: $(date -u '+%F %T') UTC"
    note "commit: $(git -C "$REPO" rev-parse --short HEAD)$(git -C "$REPO" diff --quiet HEAD -- . || echo ' (con cambios sin commit)')"
    note "pals: $("$PALS" --version), sha256 $(sha256sum "$PALS" | cut -c1-16)…"
    note "anfitrión: $(. /etc/os-release && echo "$PRETTY_NAME"), kernel $(uname -r), $(nproc) CPU, carga $(cut -d' ' -f1-3 /proc/loadavg)"
    note "docker: $(docker version --format '{{.Server.Version}}')"
    note "rustc: $(rustc --version 2>/dev/null || echo 'no instalado')"

    docker build -q -t "$IMAGE" "$REPO/tests/exit" > /dev/null || die "docker build failed"
    docker network create --label "pals-exit.dir=$WORK" "$NET" > /dev/null || die "cannot create network $NET"
    docker run -d --init --name "$A" --hostname "$A" --network "$NET" --cap-add NET_ADMIN \
        --label "pals-exit.dir=$WORK" "$IMAGE" > /dev/null || die "cannot start container $A"
    docker cp "$PALS" "$A:/usr/local/bin/pals" > /dev/null || die "cannot copy pals into A"
    # The repository, as in this working tree, at A's ~/rosepetal-pals.
    git -C "$REPO" ls-files -z --cached --others --exclude-standard \
        | tar -C "$REPO" --null -T - -c 2> /dev/null \
        | docker exec -i "$A" sh -c 'mkdir -p ~/rosepetal-pals && tar -x -C ~/rosepetal-pals' \
        || die "cannot copy the repository into A"
    note "A: $(docker exec "$A" sh -c '. /etc/os-release; echo "$PRETTY_NAME, $(pals --version), $(python3 --version)"'), $(docker exec "$A" ip -br -4 addr show eth0 | awk '{print $1, $3}')"
    note "red de A: $(docker network inspect -f '{{range .IPAM.Config}}{{.Subnet}} gw {{.Gateway}}{{end}}' "$NET")"
    for relay in use1-1 euc1-1; do
        note "relay $relay desde A: $(docker exec "$A" curl -sS -o /dev/null -w '%{http_code} en %{time_total}s' "https://$relay.relay.n0.iroh.link/")"
        note "relay $relay desde B: $(curl -sS -o /dev/null -w '%{http_code} en %{time_total}s' "https://$relay.relay.n0.iroh.link/")"
    done

    # The runbook's scripts: A's in A's ~/pals-salida, B's in $B_SAL.
    docker exec "$A" sh -c 'mkdir -p ~/pals-salida'
    local f
    for f in funciones.sh corte-red.sh caso3.sh caso8.py; do
        extract "$f" | a_write "~/pals-salida/$f" || die "the runbook has no $f"
    done
    en_a <<< 'cd ~/rosepetal-pals && pwd > ~/pals-salida/repo'
    mkdir -p "$B_SAL" "$B_HOME"
    for f in pals-prueba.sh caso4-eventos.py; do
        extract "$f" > "$B_SAL/$f" || die "the runbook has no $f"
    done

    # Harness only: accounting rules at the end of A's chains, for `path`.
    en_a <<'EOS' || die "cannot add the accounting rules in A"
iptables -A OUTPUT -o eth0 -p udp ! --dport 53 -m comment --comment pals-exit-udp-out
iptables -A INPUT -i eth0 -p udp ! --sport 53 -m comment --comment pals-exit-udp-in
iptables -A OUTPUT -o eth0 -p tcp --dport 443 -m comment --comment pals-exit-443-out
iptables -A INPUT -i eth0 -p tcp --sport 443 -m comment --comment pals-exit-443-in
EOS

    say "Paso 2. Configuración de A (runbook, tal cual)"
    en_a <<'EOS' || die "A did not start"
mkdir -p ~/.pals
cat > ~/.pals/config.toml <<'EOF'
[agent]
name = "pals-a"
description = "Máquina A de la prueba de salida del hito 1"

[net]
relay = true

[limits]
blob_fetch_timeout_s = 1800
EOF
pals init
arranca
EOS

    say "Paso 2. Configuración de B"
    if [ -z "${PALS_EXIT_PROYECTO:-}" ]; then
        echo proyecto >> "$MARKER"   # desmontar deletes the copy only with this line
        git clone -q --no-local "$REPO" "$PROYECTO" || die "cannot clone the project"
        git -C "$PROYECTO" checkout -q --detach "$(git -C "$REPO" rev-parse HEAD)"
        note "proyecto de B: una copia del repositorio en $(git -C "$PROYECTO" rev-parse --short HEAD)"
    fi
    if [ "$CASO1" = claude ]; then
        write_b_config claude
    else
        write_b_config prueba
    fi
    pb init
    if [ "$CASO1" = claude ]; then
        note "claude: $(claude --version)"
        note "comprobación del runbook: claude en el entorno del worker, en el proyecto (llamada 1 de 2)"
        (cd "$PROYECTO" && echo '¿Qué hace este proyecto? Responde en una frase.' \
            | env -i PATH="$B_PATH" HOME="$HOME" LANG="$B_LANG" claude -p --output-format text) \
            || die "claude does not answer in the worker's environment"
    fi
    arranca_b || die "B did not start"

    say "Paso 3. Contactos y grants, en los dos sentidos"
    local ticket_a ticket_b token
    ticket_b=$(pb id --ticket) || die "no ticket from B"
    ticket_a=$(en_a <<< 'pals id --ticket') || die "no ticket from A"
    en_a <<< "pals contact add '$ticket_b' --name b" || die "A cannot save B"
    pb contact add "$ticket_a" --name a || die "B cannot save A"
    token=$(pb grant issue --to a --project demo --ttl 24h) || die "B cannot issue a grant"
    en_a <<< "pals grant import '$token'" || die "A cannot import B's grant"
    token=$(en_a <<< 'pals grant issue --to b --project demo --ttl 24h') || die "A cannot issue a grant"
    pb grant import "$token" || die "B cannot import A's grant"
    local before out code=0
    before=$(counters)
    out=$(pb ask a hola) || code=$?
    note "En B: pals ask a hola -> $out (código $code)"
    path "B -> A" "$before"
    en_a <<< 'pals grant ls; pals contact ls'
    [ "$out" = "echo: hola" ] && [ "$code" = 0 ] || die "B cannot ask A"
}

# write_b_config claude|prueba: the runbook's config.toml of B, for case 1
# (claude) or for cases 2 to 8 (the test worker), with B's gateway.
write_b_config() {
    local worker
    if [ "$1" = claude ]; then
        worker='adapter = "exec"
command = "claude"
args = ["-p", "--output-format", "text"]'
    else
        worker="adapter = \"exec\"
command = \"sh\"
args = [\"$B_SAL/pals-prueba.sh\"]
timeout_s = 900"
    fi
    cat > "$B_HOME/config.toml" <<EOF
[agent]
name = "pals-b"
description = "Máquina B de la prueba de salida del hito 1"

[gateway]
listen = "$B_GATEWAY"

[worker]
$worker

[net]
relay = true

[limits]
blob_fetch_timeout_s = 1800

[projects.demo]
path = "$PROYECTO"
EOF
}

caso1() {
    say "Caso 1. Pregunta útil"
    if [ "$CASO1" != claude ]; then
        note "omitido (PALS_EXIT_CASO1=$CASO1)"
        return 0
    fi
    note "pregunta a B, que responde con claude -p en el proyecto (llamada 2 de 2)"
    local before t0 code=0
    before=$(counters); t0=$(date +%s)
    en_a > "$LOGS/caso1.out" 2>&1 <<'EOS' || code=$?
time pals ask b "Lee el README y los ficheros de configuración de este proyecto. ¿Cómo se arranca y qué dependencias principales tiene? Cita los ficheros."
EOS
    cat "$LOGS/caso1.out"
    note "código $code, $(elapsed "$t0")"
    path "caso 1" "$before"
    local cited
    cited=$(grep -o -E 'README\.md|Cargo\.toml|Cargo\.lock|CLAUDE\.md|crates/[a-z_/.-]+' "$LOGS/caso1.out" | sort -u | tr '\n' ' ')
    local ok=fail
    if [ "$code" = 0 ] && [ -n "$cited" ]; then ok=ok; fi
    result "caso 1" "$ok" "código $code; cita: ${cited:-nada}; juzga a mano si la respuesta es útil"
}

cambio_worker() {
    say "Entre el caso 1 y el 2: B cambia al worker de pruebas"
    if [ "$CASO1" != claude ]; then
        note "B ya usa el worker de pruebas"
        return 0
    fi
    para_b
    write_b_config prueba
    arranca_b || die "B did not start"
    local out code=0
    out=$(en_a <<< 'pals ask b hola') || code=$?
    note "En A: pals ask b hola -> $out (código $code)"
    if [ "$code" = 3 ]; then
        note "A no alcanza a B tras su rearranque: se vuelve a guardar el ticket de B y se retoma"
        en_a <<< "pals contact add '$(pb id --ticket)' --name b"
        code=0; out=$(en_a <<< 'pals task resume "$(pals task ls | tail -n 1 | cut -f1)"') || code=$?
        note "pals task resume -> $out (código $code)"
    fi
    local ok=fail
    if [ "$out" = "eco: hola" ] && [ "$code" = 0 ]; then ok=ok; fi
    result "cambio de worker" "$ok" "código $code"
}

caso2() {
    say "Caso 2. Transferencia autorizada"
    local before t0 out
    before=$(counters); t0=$(date +%s)
    out=$(en_a <<'EOS'
cd ~/pals-salida
head -c 256M /dev/urandom > grande.bin
sha256sum grande.bin
t0=$(date +%s%N)
pals ask b devuelve --attach grande.bin | tee caso2.out; echo "código ${PIPESTATUS[0]}"
echo "ask: $(( ($(date +%s%N) - t0) / 1000000 )) ms"
URL=$(sed -n 's/^file grande.bin: //p' caso2.out); echo "$URL"
curl -sS -o devuelto.bin -w '%{http_code} %{size_download}\n' "$URL"
sha256sum grande.bin devuelto.bin
EOS
)
    echo "$out"
    path "caso 2, de A a B (y la vuelta)" "$before"
    local sent b_got back
    sent=$(echo "$out" | awk '$2 == "grande.bin" {print $1; exit}')
    b_got=$(echo "$out" | sed -n '/^código/q; /grande.bin$/p' | awk 'NR == 2 {print $1}')
    back=$(echo "$out" | awk '$2 == "devuelto.bin" {print $1}')
    local ok=fail
    if echo "$out" | grep -qx 'código 0' && echo "$out" | grep -qx "200 $FULL" \
        && [ -n "$sent" ] && [ "$sent" = "$b_got" ] && [ "$sent" = "$back" ]; then ok=ok; fi
    result "caso 2 (A -> B -> A)" "$ok" "sha256 enviado ${sent:0:8}…, recibido por B ${b_got:0:8}…, devuelto ${back:0:8}…; $(elapsed "$t0")"

    before=$(counters); t0=$(date +%s)
    out=$(en_a <<'EOS'
cd ~/pals-salida
pals ask b "genera 256" | tee caso2b.out; echo "código ${PIPESTATUS[0]}"
URL=$(sed -n 's/^file generado.bin: //p' caso2b.out)
t0=$(date +%s%N)
curl -sS -o generado.bin -w '%{http_code} %{size_download}\n' "$URL"
echo "curl: $(( ($(date +%s%N) - t0) / 1000000 )) ms"
sha256sum generado.bin
EOS
)
    echo "$out"
    path "caso 2, de B a A" "$before"
    local made got
    made=$(echo "$out" | awk '$2 == "generado.bin" {print $1; exit}')
    got=$(echo "$out" | awk '$2 == "generado.bin" {h = $1} END {print h}')
    ok=fail
    if echo "$out" | grep -qx 'código 0' && echo "$out" | grep -qx "200 $FULL" \
        && [ -n "$made" ] && [ "$made" = "$got" ]; then ok=ok; fi
    result "caso 2 (B -> A)" "$ok" "sha256 de B ${made:0:8}…, descargado ${got:0:8}…; $(elapsed "$t0")"
}

# limit_a on|off bajada|subida: the runbook's way out when a transfer ends
# before the cut, "limit the upload of the machine that sends". In `subida` A
# sends: the runbook's own tbf on A's eth0. In `bajada` B sends, and B is the
# host, whose network this harness never touches: A polices what it receives
# instead, at the same rate.
LIMIT=${PALS_EXIT_LIMITE:-50mbit}
limit_a() {
    if [ "$1" = on ] && [ "$2" = subida ]; then
        en_a <<< "tc qdisc add dev eth0 root tbf rate $LIMIT burst 64kb latency 400ms"
    elif [ "$1" = on ]; then
        en_a <<< "tc qdisc add dev eth0 handle ffff: ingress && tc filter add dev eth0 parent ffff: protocol all matchall action police rate $LIMIT burst 256k drop"
    elif [ "$2" = subida ]; then
        en_a <<< 'tc qdisc del dev eth0 root'
    else
        en_a <<< 'tc qdisc del dev eth0 ingress'
    fi
    en_a <<< 'tc qdisc show dev eth0'
}

# caso3_try MODE: one run of the runbook's caso3.sh; sets `out`, `t0` and
# `before`, and prints the output and the path.
caso3_try() {
    before=$(counters); t0=$(date +%s)
    out=$(en_a <<< "bash ~/pals-salida/caso3.sh $1" 2>&1)
    echo "$out"
    path "caso 3 $label $1" "$before"
}

# caso3_run bajada|subida LABEL directo|relay: the runbook's caso3.sh,
# checked, on the path it must take; repeated with the sender limited when
# the transfer ends before the cut.
caso3_run() {
    local mode=$1 label=$2 expected=$3 before t0 out limited=no
    say "Caso 3, $label: caso3.sh $mode"
    caso3_try "$mode"
    local d2
    d2=$(echo "$out" | awk '/durante el corte/ {v = $(NF - 1)} END {print v}')
    if echo "$out" | grep -q 'terminó antes del corte' || [ -z "$d2" ] || [ "$d2" -ge "$FULL" ]; then
        note "la transferencia acabó antes del corte: se limita a $LIMIT lo que envía $( [ "$mode" = subida ] && echo A || echo 'B (en A, lo que recibe)') y se repite"
        limited=yes
        limit_a on "$mode"
        caso3_try "$mode"
        limit_a off "$mode"
    fi
    local d1 after ok=fail detail
    d1=$(echo "$out" | awk '/durante el corte/ {print $(NF - 1); exit}')
    d2=$(echo "$out" | awk '/durante el corte/ {v = $(NF - 1)} END {print v}')
    after=$(echo "$out" | awk '/tras el corte/ {print $(NF - 1)}')
    local stalled=no
    if [ -n "$d1" ] && [ -n "$d2" ] && [ "$d2" -lt "$FULL" ] && [ $(( d2 - d1 )) -lt 1048576 ] \
        && [ -n "$after" ] && [ "$after" -gt "$d2" ]; then stalled=yes; fi
    if [ "$mode" = bajada ]; then
        local made got
        made=$(echo "$out" | awk '$2 == "generado.bin" {print $1; exit}')
        got=$(echo "$out" | awk '$2 == "caso3.bin" {print $1}')
        if echo "$out" | grep -qx "curl: código 0, 200 $FULL" && [ -n "$made" ] && [ "$made" = "$got" ] \
            && [ "$stalled" = yes ]; then ok=ok; fi
        detail="sha256 de B ${made:0:8}…, descargado ${got:0:8}…"
    else
        local sent b_got
        sent=$(echo "$out" | awk '$2 == "caso3.bin" {print $1; exit}')
        b_got=$(echo "$out" | sed -n '/^pals ask: código/,$p' | awk '$2 == "caso3.bin" {print $1}')
        if echo "$out" | grep -qx 'pals ask: código 0' && [ -n "$sent" ] && [ "$sent" = "$b_got" ] \
            && [ "$stalled" = yes ]; then ok=ok; fi
        detail="sha256 enviado ${sent:0:8}…, recibido por B ${b_got:0:8}…"
    fi
    if [ "$CAMINO" != "$expected" ]; then
        ok=fail
        note "AVISO: el camino medido es $CAMINO, no $expected"
    fi
    result "caso 3 $label $mode" "$ok" "$detail; camino medido: $CAMINO; limitado: $limited; parada durante el corte: $stalled ($d1, $d2, tras el corte $after); $(elapsed "$t0")"
}

caso3() {
    say "Caso 3. Interrupción de transferencia"
    en_a <<'EOS'
$SUDO iptables-save | grep -- '^-' > ~/pals-salida/iptables.antes     # solo las reglas
$SUDO ip6tables-save | grep -- '^-' > ~/pals-salida/ip6tables.antes
EOS
    caso3_run bajada 3a directo
    caso3_run subida 3a directo
    en_a <<< reglas_como_antes
    result "caso 3a, reglas como antes" "$( en_a <<< reglas_como_antes > /dev/null 2>&1 && echo ok || echo fail)"

    say "Caso 3b. Por relay: A descarta su UDP saliente salvo DNS y loopback, y rearranca"
    en_a <<'EOS'
for t in iptables ip6tables; do
    $SUDO $t -I OUTPUT 1 -p udp -j DROP; $SUDO $t -I OUTPUT 1 -p udp --dport 53 -j ACCEPT
    $SUDO $t -I OUTPUT 1 -o lo -j ACCEPT
done
t=$(date +%s); para; echo "para: $(( $(date +%s) - t )) s"; arranca
EOS
    caso3_run bajada 3b relay
    caso3_run subida 3b relay
    say "Caso 3b: se deshacen las reglas y A rearranca"
    en_a <<'EOS'
for t in iptables ip6tables; do
    $SUDO $t -D OUTPUT -o lo -j ACCEPT
    $SUDO $t -D OUTPUT -p udp --dport 53 -j ACCEPT; $SUDO $t -D OUTPUT -p udp -j DROP
done
t=$(date +%s); para; echo "para: $(( $(date +%s) - t )) s"; arranca
reglas_como_antes
EOS
    result "caso 3b, reglas como antes" "$( en_a <<< reglas_como_antes > /dev/null 2>&1 && echo ok || echo fail)"
}

# accepted MESSAGE_ID: the runbook's caso4-eventos.py on B's store.
accepted() { python3 "$B_SAL/caso4-eventos.py" "$B_HOME/pals.db" "$1"; }

caso4() {
    say "Caso 4. Reintento sin duplicar: tarea de 120 s y, a los 15 s, corte de red de 45 s en A"
    local before t0 out msg code
    before=$(counters); t0=$(date +%s)
    out=$(en_a <<'EOS' 2>&1
[ -z "$SUDO" ] || sudo -v
echo "$(date -u +%T) pals ask b 'espera 120'"
pals ask b "espera 120" & ASK=$!
sleep 15; $SUDO setsid -f sh ~/pals-salida/corte-red.sh 45
wait $ASK; echo "código $?"
echo "$(date -u +%T) fin"
pals task ls | tail -n 1        # messageId, estado, par, tarea, intentos
EOS
)
    echo "$out"
    msg=$(echo "$out" | tail -n 1 | cut -f1)
    code=$(echo "$out" | sed -n 's/^código //p')
    if [ "$code" = 3 ]; then
        note "pals ask terminó con código 3: pals task resume $msg"
        out=$(en_a <<< "pals task resume $msg; echo \"código \$?\"; pals task ls | tail -n 1" 2>&1)
        echo "$out"
        code=$(echo "$out" | sed -n 's/^código //p')
    fi
    path "caso 4" "$before"
    local row ev
    row=$(echo "$out" | tail -n 1)
    ev=$(accepted "$msg")
    echo "$ev"
    local attempts state ok=fail
    state=$(echo "$row" | cut -f2); attempts=$(echo "$row" | cut -f5)
    if [ "$code" = 0 ] && echo "$out" | grep -qx 'he esperado 120 s' && [ "$state" = TASK_STATE_COMPLETED ] \
        && [ "${attempts:-0}" -ge 2 ] \
        && echo "$ev" | grep -q 'filas de deduplicación: 1 ' \
        && echo "$ev" | grep -q 'tareas que lo contienen: 1 ' \
        && echo "$ev" | grep -qx '  eventos task.accepted de esas tareas: 1'; then ok=ok; fi
    result "caso 4" "$ok" "código $code, $state con $attempts intentos; $(echo "$ev" | sed -n 's/^  eventos task.accepted de esas tareas: /task.accepted: /p'); $(elapsed "$t0")"
}

caso5() {
    say "Caso 5. Caída del proveedor: kill -9 del daemon de B a mitad de tarea"
    local t0 msg before
    before=$(counters); t0=$(date +%s)
    en_a > "$LOGS/caso5-ask.out" 2>&1 <<'EOS' &
echo "$(date -u +%T) pals ask b 'espera 300'"
pals ask b "espera 300"; echo "código $?"
echo "$(date -u +%T) fin"
EOS
    local ask=$!
    sleep 15
    local row task worker pgid daemon
    row=$(en_a <<< 'pals task ls | tail -n 1')
    echo "$row"
    msg=$(echo "$row" | cut -f1)
    task=$(echo "$row" | cut -f4)
    # The worker of this task, not one an earlier run may have left behind.
    worker=$(b_worker_of "$task") || die "no test worker on B runs task $task"
    pgid=$(group_of "$worker")
    note "En B: la orden de la tarea $task, grupo de procesos $pgid"
    ps_groups "$pgid"
    daemon=$(b_daemon_pid) || die "B's pid file does not name this run's B daemon"
    note "En B: kill -9 del daemon ($daemon)"
    kill -9 "$daemon"
    rm -f "$B_SAL/daemon.pid"
    sleep 1
    note "En B, tras el kill -9 (limitación conocida: la orden sigue viva)"
    ps_groups "$pgid"
    note "En A: espera a que pals ask termine"
    wait "$ask"
    cat "$LOGS/caso5-ask.out"
    note "En B: arranca"
    arranca_b || die "B did not restart"
    local out code=0 hello hcode=0
    out=$(en_a <<< "pals task resume $msg" 2>&1) || code=$?
    note "En A: pals task resume -> $out (código $code)"
    if [ "$code" = 3 ]; then
        note "A no alcanza a B tras el rearranque: se vuelve a guardar el ticket de B y se repite"
        en_a <<< "pals contact add '$(pb id --ticket)' --name b"
        code=0; out=$(en_a <<< "pals task resume $msg" 2>&1) || code=$?
        note "En A: pals task resume -> $out (código $code)"
    fi
    hello=$(en_a <<< 'pals ask b "hola tras el rearranque"') || hcode=$?
    note "En A: pals ask b 'hola tras el rearranque' -> $hello (código $hcode)"
    local ev
    ev=$(accepted "$msg"); echo "$ev"
    local first
    first=$(grep -E 'ended TASK_STATE_FAILED: interrupted: daemon restarted' "$LOGS/caso5-ask.out")
    local ok=fail
    if { [ "$code" = 2 ] && [[ "$out" == *"ended TASK_STATE_FAILED: interrupted: daemon restarted"* ]]; } \
        || [ -n "$first" ]; then
        if echo "$ev" | grep -qx '  eventos task.accepted de esas tareas: 1' \
            && [ "$hello" = "eco: hola tras el rearranque" ] && [ "$hcode" = 0 ]; then ok=ok; fi
    fi
    # Looked up again: the group seen 80 s ago may be gone, and its id reused.
    if worker=$(b_worker_of "$task"); then
        pgid=$(group_of "$worker")
        note "En B: se termina la orden que sobrevivió al kill -9 (tarea $task, grupo $pgid)"
        ps_groups "$pgid"
        kill -- "-$pgid"
        sleep 1
        if [ "$(ps_groups "$pgid" | wc -l)" -gt 1 ]; then ps_groups "$pgid"
        else note "ningún proceso del grupo $pgid vivo"; fi
    else
        note "En B: la orden de la tarea $task ya no está viva"
    fi
    path "caso 5" "$before"
    result "caso 5" "$ok" "pals ask: $(sed -n 's/^código //p' "$LOGS/caso5-ask.out"); resume: código $code; $(echo "$ev" | sed -n 's/^  eventos task.accepted de esas tareas: /task.accepted: /p'); ask nuevo: código $hcode; $(elapsed "$t0")"
}

caso6() {
    say "Caso 6. Revocación"
    pb grant ls --issued
    local gid
    gid=$(pb grant ls --issued | awk '/project demo .* on skill:ask .* to a$/ {print $1}' | tail -n 1)
    note "grant demo/skill:ask de a: $gid"
    pb grant revoke "$gid"
    local out code=0 curl_out before
    before=$(counters)
    out=$(en_a <<< 'pals ask b "hola tras la revocación"' 2>&1) || code=$?
    note "En A: pals ask -> $out (código $code)"
    curl_out=$(en_a <<'EOS'
curl -sS -i -X POST http://127.0.0.1:7420/peers/b/rest/message:send \
    -H 'A2A-Version: 1.0' -H 'Content-Type: application/json' \
    -d '{"message":{"messageId":"caso6-curl","role":"ROLE_USER","parts":[{"text":"hola"}]}}'
EOS
)
    echo "$curl_out"
    local ok=fail
    if echo "$out" | grep -qx 'pals: refused: HTTP 401: revoked' && [ "$code" = 2 ] \
        && echo "$curl_out" | grep -q '^HTTP/1.1 401 Unauthorized' \
        && echo "$curl_out" | tr -d '\r' | grep -qix 'www-authenticate: Bearer realm="agentnet", error="invalid_token", error_description="revoked"' \
        && echo "$curl_out" | tail -n 1 | grep -qx '{"error":"revoked"}'; then ok=ok; fi
    path "caso 6" "$before"
    result "caso 6" "$ok" "pals ask: código $code"

    note "Grant nuevo de B para A, para los casos 7 y 8"
    local token
    token=$(pb grant issue --to a --project demo --ttl 24h) || die "B cannot issue a grant"
    en_a <<< "pals grant import '$token'" || die "A cannot import the grant"
}

caso7() {
    say "Caso 7. Blob sin grant: una tercera identidad, C, en A"
    local out before
    before=$(counters)
    out=$(en_a <<EOS 2>&1
TICKET_B='$(pb id --ticket)'
mkdir -p ~/.pals-c
cat > ~/.pals-c/config.toml <<'EOF'
[agent]
name = "pals-c"
description = "Tercera identidad de la prueba de salida del hito 1"

[gateway]
listen = "127.0.0.1:7421"

[net]
relay = true
EOF
PALS_HOME=~/.pals-c pals init
PALS_HOME=~/.pals-c nohup pals daemon >> ~/pals-salida/daemon-c.log 2>&1 & echo \$! > ~/pals-salida/daemon-c.pid
sleep 5
PALS_HOME=~/.pals-c pals contact add "\$TICKET_B" --name b
PALS_HOME=~/.pals-c pals contact add "\$(pals id --ticket)" --name a
A=\$(pals id); B=\$(pals contact ls | awk '\$1 == "b" {print \$2}')
HASH=\$(sed -n 's/^file grande.bin: //p' ~/pals-salida/caso2.out); HASH=\${HASH##*/}
HASH2=\$(sed -n 's/^file generado.bin: //p' ~/pals-salida/caso2b.out); HASH2=\${HASH2##*/}
echo "== B, fichero del caso 2"; curl -sS -i "http://127.0.0.1:7421/blobs/\$B/\$HASH"; echo
echo "== A, fichero del caso 2"; curl -sS -i "http://127.0.0.1:7421/blobs/\$A/\$HASH"; echo
echo "== B, fichero que B generó en el caso 2"; curl -sS -i "http://127.0.0.1:7421/blobs/\$B/\$HASH2"; echo
PALS_HOME=~/.pals-c pals grant ls
PALS_HOME=~/.pals-c pals ask b hola; echo "código \$?"
ls ~/.pals-c/blobs/data 2>/dev/null | wc -l | sed 's/^/blobs en el almacén de C: /'
kill "\$(cat ~/pals-salida/daemon-c.pid)"
EOS
)
    echo "$out"
    local forbidden ok=fail b_id
    forbidden=$(echo "$out" | grep -c '^HTTP/1.1 403 Forbidden')
    b_id=$(pb id)
    if [ "$forbidden" = 3 ] && [ "$(echo "$out" | grep -c '^{"error":"the holder denied the blob"}')" = 3 ] \
        && echo "$out" | grep -qx "pals: refused: HTTP 401: no grant held for $b_id" \
        && echo "$out" | grep -qx 'código 2'; then ok=ok; fi
    path "caso 7 (C y B; lo que C habla con A no sale por eth0)" "$before"
    result "caso 7" "$ok" "403 en $forbidden de 3 peticiones de blobs; pals ask desde C: $(echo "$out" | sed -n 's/^código //p')"
}

caso8() {
    say "Caso 8. Cliente estándar: el SDK oficial de Python en A, contra el gateway de A, con B como par"
    local out code=0 before
    en_a <<'EOS' 2>&1
cd "$REPO"
python3 -m venv ~/pals-salida/venv
~/pals-salida/venv/bin/pip install -q -r tests/interop/requirements.txt
~/pals-salida/venv/bin/pip list 2>/dev/null | grep '^a2a-sdk '
EOS
    before=$(counters)
    out=$(en_a <<'EOS' 2>&1
~/pals-salida/venv/bin/python ~/pals-salida/caso8.py http://127.0.0.1:7420 "$(pals contact ls | awk '$1 == "b" {print $2}')"
EOS
)  || code=$?
    echo "$out"
    note "código $code"
    path "caso 8" "$before"
    local alive=1 _
    for _ in $(seq 1 20); do
        alive=$(b_workers | wc -l)
        [ "$alive" = 0 ] && break
        sleep 0.5
    done
    note "En B: órdenes de pals-prueba.sh vivas: $alive"
    local ok=fail
    if [ "$code" = 0 ] && echo "$out" | grep -qx 'caso 8 superado' && [ "$alive" = 0 ]; then ok=ok; fi
    result "caso 8" "$ok" "código $code; órdenes vivas en B: $alive"
}

limpieza() {
    say "Limpieza (sección 6 del runbook)"
    local rules=0 alive ok=fail
    en_a <<'EOS' || rules=$?
para
r=0
if [ -f ~/pals-salida/iptables.antes ]; then reglas_como_antes || r=$?
else echo "sin copia de las reglas de antes: el caso 3 no se ejecutó"; fi
rm -rf ~/.pals-c
exit "$r"
EOS
    para_b
    alive=$(b_workers)
    if [ -z "$alive" ]; then
        note "En B: ninguna orden viva"
    else
        local p gs=()
        for p in $alive; do gs+=("$(group_of "$p")"); done
        ps_groups "${gs[@]}"
    fi
    if [ "$rules" = 0 ] && [ -z "$alive" ]; then ok=ok; fi
    result "limpieza" "$ok"
}

DESMONTADO=0
desmontar() {
    DESMONTADO=1
    say "Desmontaje"
    local owner p
    # Only what this run made: the label of the container and the network
    # must be this PALS_EXIT_DIR.
    if container_exists; then
        owner=$(container_dir)
        if [ "$owner" = "$WORK" ]; then
            docker exec "$A" sh -c 'cat ~/pals-salida/daemon.log' > "$LOGS/a-daemon.log" 2> /dev/null
            docker exec "$A" sh -c 'cat ~/pals-salida/daemon-c.log' > "$LOGS/c-daemon.log" 2> /dev/null
            docker rm -f "$A" > /dev/null && note "contenedor $A eliminado"
        else
            note "AVISO: el contenedor $A es de otra ejecución (${owner:-sin etiqueta pals-exit.dir}): no se toca"
        fi
    fi
    if network_exists; then
        owner=$(network_dir)
        if [ "$owner" = "$WORK" ]; then
            docker network rm "$NET" > /dev/null && note "red $NET eliminada"
        else
            note "AVISO: la red $NET es de otra ejecución (${owner:-sin etiqueta pals-exit.dir}): no se toca"
        fi
    fi
    if [ "$KEEP_IMAGE" = 0 ] \
        && [ -n "$(docker image inspect -f '{{ index .Config.Labels "pals-exit.image" }}' "$IMAGE" 2> /dev/null)" ]; then
        docker image rm "$IMAGE" > /dev/null && note "imagen $IMAGE eliminada"
    else
        note "la imagen $IMAGE se conserva (PALS_EXIT_KEEP_IMAGE=0 la elimina)"
    fi
    para_b
    # Any test-worker command still alive, from B's directory only.
    for p in $(b_workers); do
        kill -- "-$(group_of "$p")" && note "grupo de procesos de la orden $p terminado"
    done
    [ ! -f "$B_SAL/daemon.log" ] || cp "$B_SAL/daemon.log" "$LOGS/b-daemon.log"
    # Inside the marked PALS_EXIT_DIR (checked at start); the project copy only
    # if preparar cloned it there.
    rm -rf -- "$B_HOME" "$B_SAL"
    local gone="el PALS_HOME de B y su directorio de la prueba"
    if grep -qx proyecto "$MARKER" && [ "$PROYECTO" = "$WORK/proyecto" ] && [ -e "$PROYECTO" ]; then
        rm -rf -- "$PROYECTO"
        gone="$gone, y la copia del proyecto"
    fi
    note "borrados $gone; quedan los registros en $LOGS"
}

# --- main ------------------------------------------------------------------

# Ctrl-C or SIGTERM tears the run down. `todo` also tears down when it stops
# early; single phases leave the setup for the next phase.
on_interrupt() {
    trap - INT TERM
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
        cambio-worker) cambio_worker ;;
        caso2) caso2 ;;
        caso3) caso3 ;;
        caso4) caso4 ;;
        caso5) caso5 ;;
        caso6) caso6 ;;
        caso7) caso7 ;;
        caso8) caso8 ;;
        limpieza) limpieza ;;
        desmontar) desmontar ;;
        *) die "unknown phase $phase" ;;
    esac
done
if [ -f "$LOGS/resultados.txt" ]; then
    say "Resultados"
    cat "$LOGS/resultados.txt"
    ! grep -q FALLIDO "$LOGS/resultados.txt"
fi
