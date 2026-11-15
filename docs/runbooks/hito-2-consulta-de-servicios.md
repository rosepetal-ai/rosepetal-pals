# Runbook: prueba de salida del hito 2, consulta de servicios

Instrucciones para ejecutar la prueba que cierra el hito 2: siete agentes, cada uno con su `PALS_HOME`, en una o varias máquinas Linux (y un octavo, H, que se para en el caso 5), y los nueve casos del criterio de salida (spec §14). A pregunta; B, C, D, E y G proveen, con los inventarios de la tarea 10 (`crates/pals/tests/selection.rs`); F es un agente A2A de serie, sin PALS. Cada caso trae las órdenes exactas y el resultado esperado. Los resultados se anotan en [`docs/conformance/hito-2/exit-test.md`](../conformance/hito-2/exit-test.md).

| Caso | Qué se comprueba |
|-|-|
| 1. Elegir entre al menos tres candidatos | `pals discover … --ask` desde A consulta a B, C, D, E y G, elige a B y dice por qué descarta a los demás; se anotan las métricas |
| 2. Requisitos sobre integraciones y recursos | E queda descartado porque a su conexión MCP le falta la herramienta que ofrece; G, por la RAM ofrecida; B cumple con la observación `ok` de su servidor MCP real |
| 3. Validar cards | La card firmada de B, leída por el gateway de A, verifica con los verificadores de Python, Go y JS; una card alterada queda descartada |
| 4. No ejecutar trabajo al consultar | 50 búsquedas a la vez desde A, y otras 50 seguidas: ningún `task.accepted` nuevo en B a G, `tools/list` solo por el temporizador, las cuotas recortan la ráfaga y nada se cae |
| 5. Oferta vencida y proveedor ocupado | Con ofertas de 2 s en B y C lleno: la oferta vencida no se usa, se renueva o se elige a otro, y nada se ejecuta dos veces |
| 6. Vistas y caché por permisos | Un par sin grant no ve lo que es de proyecto ni su existencia; el cursor de otro solicitante se rechaza |
| 7. Contacto A2A corriente | A pregunta a F por su gateway, por `https` con un certificado de una CA de prueba |
| 8. Migración | Un directorio de datos del hito 1 arranca con el hito 2 y conserva contactos, grants, tareas y eventos, con la cadena de eventos verificada |
| 9. Sin regresión A2A | El TCK oficial y el cliente de Python, como en el hito 1 |

Tiempo estimado: una hora la primera vez, sin contar la compilación.

**Con Docker en una sola máquina.** [`tests/exit/hito-2.sh`](../../tests/exit/README.md#prueba-de-salida-del-hito-2-con-docker) ejecuta los casos 1 a 8 con un contenedor por agente en una red bridge de Docker. Extrae de este runbook sus guiones de `~/pals-h2`, así que sirve también para probar un cambio en ellos. El caso 9 se ejecuta aparte, en la máquina donde se compiló.

## Los agentes

| Agente | Papel | Gateway | Inventario (tarea 10) |
|-|-|-|-|
| A | Pide: busca proveedor y delega | `127.0.0.1:7420` | Ninguno; worker `echo` |
| B | Proveedor compatible | `127.0.0.1:7421` | Sin nube externa, 8192 MiB ofrecidos, arranque 100 ms y duración 1000 ms; MCP `example:everything` que ofrece `echo` y `get-sum`, que su servidor sirve. Además, 120 entradas públicas de información (caso 6) |
| C | Proveedor compatible y lleno | `127.0.0.1:7422` | Como B, más rápido (10 ms y 100 ms) y con MCP que ofrece `echo`; su worker admite una tarea y ninguna en cola, y tiene una tarea larga en marcha |
| D | Usa nube externa | `127.0.0.1:7423` | Como B, con `external_cloud_used = true` |
| E | Le falta la herramienta MCP | `127.0.0.1:7424` | Como B, pero ofrece `read_file`, que el servidor no sirve |
| G | Poca RAM ofrecida | `127.0.0.1:7426` | Como B, con 1024 MiB ofrecidos. También pide, sin grant de B (caso 6) |
| H | Contacto caído (caso 5) | `127.0.0.1:7428` | Como B; se para después de intercambiar contactos |
| F | Agente A2A de serie | `https://<máquina de F>:8443` | [`tests/exit/hito-2/agente_f.py`](../../tests/exit/hito-2/agente_f.py), sobre el SDK oficial de Python; su copia con la card alterada, en el puerto 8444 |

Cada agente PALS tiene su `PALS_HOME` en `~/pals-h2/<agente>` de la máquina donde corre, y los gateways tienen puertos distintos: pueden ir todos en una máquina, cada uno en la suya o repartidos. H corre en cualquier máquina de un proveedor. Todos usan `net.relay = true`, los relays públicos de n0, como en el hito 1: con `relay = false` un daemon solo escucha en `127.0.0.1` y no lo alcanza ninguna otra máquina.

Los proveedores responden con un worker de pruebas, `proveedor.sh`, que anota cada ejecución: no se arranca ningún LLM. Sus conexiones MCP las comprueba el adaptador de inventario con [`@modelcontextprotocol/server-everything`](../../tests/exit/hito-2/mcp/package.json) por stdio, la versión `2026.8.31` fijada con sus dependencias en `package-lock.json`.

## 0. Requisitos

En cada máquina:

- Linux con `bash`, `curl`, `python3` con `venv`, y GNU coreutils. Los guiones leen `/proc` para reconocer los daemons antes de enviarles una señal.
- Rust 1.91 o posterior, y `git`.
- Los relojes sincronizados (`timedatectl`: `System clock synchronized: yes`): ofertas, vistas y registros toleran 60 s de diferencia.
- Salida a internet hacia los relays de n0 (TCP 443) y UDP entre las máquinas, para el camino directo.
- El mismo commit de `rosepetal-pals`.

Además:

- En las máquinas de B a H: Node 20 o posterior con `npm`, para el servidor MCP.
- En la de F: `openssl`, para la CA de prueba. En la de A: permiso para instalar esa CA en el almacén del sistema (`sudo` o root).
- Para el caso 3, en la máquina de A: Go 1.26 o posterior y Node 20 o posterior, para los verificadores independientes.
- Para el caso 8: el `pals` del hito 1, del commit `71092a0` (sección 1), y el programa `events_verify` de este commit.

## 1. Instalación (cada máquina)

```bash
git clone https://github.com/rosepetal-ai/rosepetal-pals.git
cd rosepetal-pals
git checkout feat/hito-2        # la rama del hito 2, o `dev` cuando esté fusionada
cargo install --locked --path crates/pals
pals --version
git rev-parse --short HEAD      # anótalo: debe ser el mismo en todas
mkdir -p ~/pals-h2/proyecto && pwd > ~/pals-h2/repo
# El servidor MCP de los proveedores (B a H) y el venv del SDK oficial de Python (A y F):
mkdir -p ~/pals-h2/mcp && cp tests/exit/hito-2/mcp/package*.json ~/pals-h2/mcp/
npm ci --prefix ~/pals-h2/mcp --ignore-scripts --no-audit --no-fund
python3 -m venv ~/pals-h2/venv
~/pals-h2/venv/bin/pip install --no-deps -r tests/interop/requirements.txt -r tests/exit/hito-2/requirements-f.txt
```

`~/pals-h2/proyecto` es el directorio del proyecto `demo`, donde corre el worker de los proveedores; puede estar vacío.

Para el caso 8, en la máquina donde se haga, el `pals` del hito 1 y `events_verify`:

```bash
mkdir -p ~/pals-h2/hito1-src
git archive 71092a0 | tar -x -C ~/pals-h2/hito1-src
(cd ~/pals-h2/hito1-src && CARGO_TARGET_DIR=~/pals-h2/hito1-target cargo build --release --locked -p pals)
cp ~/pals-h2/hito1-target/release/pals ~/pals-h2/pals-hito1 && rm -rf ~/pals-h2/hito1-target
cargo build --release --locked -p pals-core --example events_verify
cp target/release/examples/events_verify ~/pals-h2/
```

### Funciones y guiones

Las funciones de la prueba van en `~/pals-h2/funciones.sh`, que se carga en cada terminal nueva. Después, `agente <x>` elige el agente: las órdenes que siguen son de x (`PALS_HOME=~/pals-h2/<x>`). Los bloques de abajo empiezan por esas dos líneas.

```bash
cat > ~/pals-h2/funciones.sh <<'EOF'
# Funciones y variables de la prueba de salida del hito 2. Cárgalas en cada
# terminal nueva con  . ~/pals-h2/funciones.sh  y elige el agente con
# agente <x>.
H2=~/pals-h2
SUDO=sudo; [ "$(id -u)" -ne 0 ] || SUDO=     # como root, sin sudo
REPO=$(cat "$H2/repo" 2>/dev/null)
MCP_EVERYTHING=$H2/mcp/node_modules/@modelcontextprotocol/server-everything/dist/index.js
# Lo que solo ven los grants del proyecto demo, en los inventarios de B a H.
PRIVADAS="repo-review example:everything example:coder example:host"

# agente <x>: las órdenes que siguen son del agente x (a, b, c, d, e, g o h).
agente() {
    case $1 in a|b|c|d|e|g|h) ;; *) echo "agente desconocido: $1"; return 1 ;; esac
    AGENTE=$1
    export PALS_HOME=$H2/$1
    mkdir -p "$PALS_HOME"
}
# puerto [x]: el puerto del gateway del agente x, o del actual.
puerto() {
    case ${1:-$AGENTE} in
        a) echo 7420 ;; b) echo 7421 ;; c) echo 7422 ;; d) echo 7423 ;;
        e) echo 7424 ;; g) echo 7426 ;; h) echo 7428 ;;
        *) return 1 ;;
    esac
}
# gw [x]: la URL del gateway del agente x, o del actual.
gw() { echo "http://127.0.0.1:$(puerto "$@")"; }
# espera <segundos> <qué> <orden...>: repite la orden cada medio segundo
# hasta que termina bien; falla, diciendo qué esperaba, al acabar el plazo.
espera() {
    local hasta=$(( $(date +%s) + $1 )) que=$2
    shift 2
    until "$@" > /dev/null 2>&1; do
        if [ "$(date +%s)" -ge "$hasta" ]; then echo "AGOTADO: $que"; return 1; fi
        sleep 0.5
    done
}
# arranca: el daemon del agente actual, en segundo plano; espera a que su
# gateway dé el ticket. PID y registro en su PALS_HOME.
arranca() {
    nohup pals daemon >> "$PALS_HOME/daemon.log" 2>&1 < /dev/null &
    echo $! > "$PALS_HOME/daemon.pid"
    espera 120 "el daemon de $AGENTE" pals id --ticket || return 1
    grep 'pals daemon:' "$PALS_HOME/daemon.log" | tail -n 1
}
# es_daemon <pid>: si ese PID es `pals daemon` con el PALS_HOME actual.
es_daemon() {
    case "$(tr '\0' ' ' < "/proc/$1/cmdline" 2> /dev/null)" in
        "pals daemon "|*/pals\ daemon\ ) ;;
        *) return 1 ;;
    esac
    tr '\0' '\n' < "/proc/$1/environ" 2> /dev/null | grep -qxF "PALS_HOME=$PALS_HOME"
}
# para: SIGTERM al daemon del agente actual, si su PID sigue siendo ese
# daemon, y espera hasta 60 s a que termine.
para() {
    local pid i=0
    pid=$(cat "$PALS_HOME/daemon.pid" 2> /dev/null) || { echo "$AGENTE: no hay daemon.pid"; return 0; }
    if ! es_daemon "$pid"; then
        echo "$AGENTE: el PID $pid ya no es su pals daemon: no se toca"
        rm -f "$PALS_HOME/daemon.pid"; return 0
    fi
    kill "$pid"
    while kill -0 "$pid" 2> /dev/null && [ "$i" -lt 300 ]; do sleep 0.2; i=$((i + 1)); done
    if kill -0 "$pid" 2> /dev/null; then echo "$AGENTE: el daemon $pid sigue vivo 60 s después"; return 1; fi
    rm -f "$PALS_HOME/daemon.pid"
}
# config_a: el config.toml de A, el que pide.
config_a() {
    cat > "$PALS_HOME/config.toml" <<CFG
[agent]
name = "pals-a"
description = "A, quien pide en la prueba de salida del hito 2"

[gateway]
listen = "127.0.0.1:$(puerto a)"

[net]
relay = true
CFG
}
# config_proveedor [--lleno] [--oferta-ms <ms>]: el config.toml del
# proveedor actual: el worker de pruebas en el proyecto demo, hasta 2
# tareas a la vez y 8 en cola (--lleno: 1 y ninguna), ofertas de 15 s
# (--oferta-ms: otro plazo) y el adaptador de inventario cada 60 s.
config_proveedor() {
    local concurrentes=2 cola=8 oferta=15000
    while [ $# -gt 0 ]; do
        case $1 in
            --lleno) concurrentes=1 cola=0 ;;
            --oferta-ms) oferta=$2; shift ;;
            *) echo "config_proveedor: no entiendo $1"; return 1 ;;
        esac
        shift
    done
    cat > "$PALS_HOME/config.toml" <<CFG
[agent]
name = "pals-$AGENTE"
description = "Proveedor $AGENTE de la prueba de salida del hito 2"

[gateway]
listen = "127.0.0.1:$(puerto)"

[worker]
adapter = "exec"
command = "sh"
args = ["$H2/proveedor.sh", "$AGENTE"]
timeout_s = 7200
max_concurrent = $concurrentes
max_queue = $cola

[net]
relay = true

[inventory]
file = "inventory.toml"
refresh_interval_s = 60

[discovery]
offer_valid_for_ms = $oferta

[projects.demo]
path = "$H2/proyecto"
CFG
}
# inventario <nube> <ram_ofrecida_mib> <arranque_ms> <duración_ms> "<herramientas>":
# el inventory.toml del proveedor actual, el de la tarea 10: el servicio de
# revisión del proyecto demo, su conexión MCP, que el adaptador comprueba con
# server-everything y que ofrece esas herramientas, un modelo y el equipo,
# que ofrece esa RAM a una tarea. Todo visible solo en el proyecto demo.
inventario() {
    local t declaradas= ofrecidas=
    for t in $5; do
        declaradas="$declaradas${declaradas:+, }{ name = \"$t\" }"
        ofrecidas="$ofrecidas${ofrecidas:+, }\"$t\""
    done
    cat > "$PALS_HOME/inventory.toml" <<INV
[[services]]
id = "repo-review"
name = "Repository review"
description = "Reviews the demo repository"
capabilities = ["code.rust.review"]
input_modes = ["text/plain"]
output_modes = ["text/plain"]
external_cloud_used = $1
estimated_start_delay_ms = $3
estimated_duration_ms = $4
visibility = { project = ["demo"] }

[[mcp]]
id = "example:everything"
description = "The MCP reference server"
provenance = "operator"
visibility = { project = ["demo"] }
server = "@modelcontextprotocol/server-everything"
tools = [$declaradas]
offered_tools = [$ofrecidas]
[mcp.adapter]
transport = "stdio"
command = "sh"
args = ["$H2/mcp-everything.sh", "$AGENTE", "$MCP_EVERYTHING"]

[[models]]
id = "example:coder"
description = "The coding model"
provenance = "operator"
visibility = { project = ["demo"] }
provider = "Example"
model_id = "example-coder"
revision = "2026-10-01"
role = "primary"
execution = { kind = "local" }

[hardware]
id = "example:host"
description = "Build host"
provenance = "operator"
visibility = { project = ["demo"] }
ram_mib = 65536
offered = { ram_mib = $2 }
INV
}
# publicas <n>: añade al inventario del agente actual n fuentes de
# información públicas, example:nota-001 y siguientes.
publicas() {
    local i
    for i in $(seq 1 "$1"); do
        printf '\n[[information]]\nid = "example:nota-%03d"\ndescription = "Nota pública %d"\nprovenance = "operator"\nvisibility = "public"\n' "$i" "$i"
    done >> "$PALS_HOME/inventory.toml"
}
# eventos <tipo>: cuántos eventos de ese tipo tiene el registro del agente actual.
eventos() {
    python3 - "$PALS_HOME/pals.db" "$1" <<'PY'
import sqlite3, sys
db = sqlite3.connect(f'file:{sys.argv[1]}?mode=ro', uri=True)
print(db.execute('SELECT COUNT(*) FROM events WHERE kind = ?', (sys.argv[2],)).fetchone()[0])
PY
}
# ejecuciones: cuántas veces ha ejecutado algo el worker del agente actual.
ejecuciones() { cat "$H2/$AGENTE-ejecuciones.log" 2> /dev/null | wc -l; }
# tools_list: cuántos tools/list ha recibido el servidor MCP del agente actual.
tools_list() { grep -cE '"method": ?"tools/list"' "$H2/$AGENTE-mcp-entrada.log" 2> /dev/null || true; }
# cuerpo [--ask <pregunta>] [<par>...]: el cuerpo de POST /discovery/search
# para la búsqueda del caso 1, code.rust.review con req.json, entre esos
# pares (entre todos los contactos si no se nombra ninguno).
cuerpo() {
    python3 - "$H2/req.json" "$@" <<'PY'
import json, sys
search = {'capability': 'code.rust.review', 'requirements': json.load(open(sys.argv[1]))}
args, body = sys.argv[2:], {}
while args:
    arg = args.pop(0)
    if arg == '--ask':
        body.update(ask=args.pop(0), wait_s=600)
    else:
        search.setdefault('peers', []).append(arg)
print(json.dumps({'search': search, **body}))
PY
}
# busca <nombre> [--ask <pregunta>] [<par>...]: esa búsqueda por el gateway de
# A; guarda su respuesta en ~/pals-h2/busquedas/<nombre>.json e imprime el
# código HTTP.
busca() {
    local nombre=$1
    shift
    mkdir -p "$H2/busquedas"
    cuerpo "$@" | curl -sS --max-time 700 -o "$H2/busquedas/$nombre.json" -w '%{http_code}\n' \
        -X POST -H 'content-type: application/json' --data-binary @- "$(gw a)/discovery/search"
}
EOF
```

El worker de los proveedores, el servidor MCP que observa su adaptador y los requisitos de la búsqueda:

```bash
cat > ~/pals-h2/proveedor.sh <<'EOF'
#!/bin/sh
# Worker de los proveedores de la prueba de salida del hito 2 (adaptador
# exec): la pregunta llega por stdin y la respuesta sale por stdout. Anota
# cada ejecución, con su tarea y su pregunta, en
# ~/pals-h2/<agente>-ejecuciones.log.
#   espera <n>   tarda n segundos en responder (deja el worker ocupado)
#   otra cosa    responde «<agente>: <pregunta>»
#   sh proveedor.sh <agente>
q=$(cat)
printf '%s %s %s\n' "$(date -u +%FT%TZ)" "$PALS_TASK_ID" "$(printf '%s' "$q" | tr '\n' ' ' | cut -c1-120)" \
    >> "$HOME/pals-h2/$1-ejecuciones.log"
case $q in
    "espera "*) sleep "${q#espera }" ;;
esac
printf '%s: %s\n' "$1" "$q"
EOF
cat > ~/pals-h2/mcp-everything.sh <<'EOF'
#!/bin/sh
# El servidor MCP que comprueba el adaptador de inventario de un proveedor:
# @modelcontextprotocol/server-everything por stdio. Anota la hora de cada
# arranque en ~/pals-h2/<agente>-mcp-arranques.log, y copia lo que recibe,
# un mensaje JSON-RPC por línea, en ~/pals-h2/<agente>-mcp-entrada.log, y lo
# que responde en ~/pals-h2/<agente>-mcp-salida.log, para contar sus
# tools/list.
#   sh mcp-everything.sh <agente> <dist/index.js de server-everything>
date -u +%FT%TZ >> "$HOME/pals-h2/$1-mcp-arranques.log"
tee -a "$HOME/pals-h2/$1-mcp-entrada.log" | node "$2" stdio | tee -a "$HOME/pals-h2/$1-mcp-salida.log"
EOF
cat > ~/pals-h2/req.json <<'EOF'
{
  "external_cloud_allowed": false,
  "inventory": {
    "required": {
      "mcp_integration_ids": ["example:everything"],
      "min_offered_ram_mib": 4096
    }
  }
}
EOF
```

Las métricas del §14 de las búsquedas que se guardan con `busca` y en el caso 4:

```bash
cat > ~/pals-h2/metricas.py <<'EOF'
"""Las métricas del §14 de las búsquedas guardadas en un directorio
(~/pals-h2/busquedas por defecto): las respuestas de POST /discovery/search,
una por fichero .json, con su SearchOutcome (o {"search": ..., "chosen": ...}
cuando la búsqueda preguntó). Las que el daemon rechazó ({"error": ...}) se
cuentan aparte.

Uso: python3 metricas.py [<directorio>] [<prefijo>]
"""
import json
import statistics
import sys
from pathlib import Path

carpeta = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.home() / 'pals-h2' / 'busquedas'
prefijo = sys.argv[2] if len(sys.argv) > 2 else ''
filas, rechazos = [], {}
for f in sorted(carpeta.glob(f'{prefijo}*.json')):
    if f.name.endswith('-cuerpo.json'):
        continue
    try:
        doc = json.loads(f.read_text())
    except ValueError:
        rechazos['no es JSON'] = rechazos.get('no es JSON', 0) + 1
        continue
    outcome = doc.get('search', doc) if isinstance(doc, dict) else {}
    if not isinstance(outcome, dict) or 'metrics' not in outcome:
        error = str(doc.get('error', 'sin métricas')) if isinstance(doc, dict) else 'sin métricas'
        rechazos[error] = rechazos.get(error, 0) + 1
        continue
    filas.append((f.stem, outcome))

cols = ['time_to_first_valid_ms', 'contacted', 'messages', 'bytes_sent', 'bytes_received', 'matching_us', 'elapsed_ms']
print('búsqueda\tcandidatos\t' + '\t'.join(cols))
for nombre, o in filas:
    m = o['metrics']
    print(f"{nombre}\t{len(o['ranked'])}\t" + '\t'.join(str(m.get(c)) for c in cols))
total = len(filas) + sum(rechazos.values())
con = sum(1 for _, o in filas if o['ranked'])
print(f'búsquedas: {total}; con respuesta: {len(filas)}; rechazadas: {sum(rechazos.values())} {rechazos}')
if filas:
    print(f'con al menos un candidato: {con} de {len(filas)} respondidas ({100 * con / len(filas):.0f} %), {con} de {total} lanzadas ({100 * con / total:.0f} %)')
    for c in cols:
        vals = [o['metrics'][c] for _, o in filas if o['metrics'].get(c) is not None]
        if vals:
            print(f'{c}: mediana {statistics.median(vals):g}, mínimo {min(vals)}, máximo {max(vals)}')
EOF
```

## 2. Configuración

### 2.1. F y su CA de prueba (máquina de F)

Una CA de prueba, válida 7 días, y el certificado de F, firmado por ella para el nombre con que A alcanza la máquina de F (aquí `$F_HOST`). F se sirve dos veces: con su card firmada, en el 8443, y con la card alterada después de firmarla, en el 8444.

```bash
. ~/pals-h2/funciones.sh
F_HOST=<nombre de la máquina de F>      # el que A resuelve: DNS, /etc/hosts
cd ~/pals-h2
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 7 \
    -subj "/CN=CA de prueba del hito 2 de PALS" -keyout ca.key -out ca.crt \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$F_HOST" -keyout f.key -out f.csr
printf 'subjectAltName=DNS:%s\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' "$F_HOST" > f.ext
openssl x509 -req -in f.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 7 -extfile f.ext -out f.crt
for p in 8443 8444; do
    extra=; [ "$p" = 8444 ] && extra=--tamper
    nohup ~/pals-h2/venv/bin/python "$REPO/tests/exit/hito-2/agente_f.py" --listen "0.0.0.0:$p" \
        --url "https://$F_HOST:$p" --tls-cert f.crt --tls-key f.key --sign-key f-firma.key $extra \
        >> "f-$p.log" 2>&1 < /dev/null &
    echo $! > "f-$p.pid"
done
```

Copia `ca.crt` a `~/pals-h2/ca.crt` de la máquina de A. `ca.key` no sale de la de F. En A, `F_HOST` es el mismo nombre: los bloques de A que lo usan (casos 3 y 7) lo necesitan definido.

### 2.2. A (máquina de A)

La CA de prueba de F va al almacén del sistema **antes** de arrancar el daemon: el gateway verifica los certificados de los agentes corrientes con él (en Debian y Ubuntu; en otras distribuciones, su equivalente de `update-ca-certificates`).

```bash
. ~/pals-h2/funciones.sh; agente a
cd ~/pals-h2
$SUDO cp ca.crt /usr/local/share/ca-certificates/pals-h2-prueba.crt && $SUDO update-ca-certificates
config_a
pals init
arranca
```

### 2.3. Los proveedores (máquinas de B a H)

Uno por terminal, o seguidos en la misma máquina. B añade 120 entradas públicas, para que su vista pública tenga dos páginas (caso 6).

```bash
. ~/pals-h2/funciones.sh
agente b; config_proveedor;         inventario false 8192 100 1000 "echo get-sum"; publicas 120; pals init; arranca
agente c; config_proveedor --lleno; inventario false 8192 10 100 "echo";                       pals init; arranca
agente d; config_proveedor;         inventario true 8192 1 1 "echo";                           pals init; arranca
agente e; config_proveedor;         inventario false 8192 1 1 "read_file";                     pals init; arranca
agente g; config_proveedor;         inventario false 1024 1 1 "echo";                          pals init; arranca
agente h; config_proveedor;         inventario false 8192 1 1 "echo";                          pals init; arranca
```

`pals init` no toca el `config.toml` ni el `inventory.toml` que ya existen. Después, en cada uno, el adaptador comprueba ya su conexión MCP (también lo hizo al arrancar):

```bash
. ~/pals-h2/funciones.sh
for x in b c d e g h; do agente $x; echo "== $x"; pals inventory refresh; done
```

**Esperado:** `observed 1, ok 1, failed 0` en todos menos en E, que dice `observed 1, ok 0, failed 1` y `failed: example:everything missing tools`. En `~/pals-h2/b-mcp-salida.log`, la respuesta de server-everything a `initialize` (`"serverInfo":{"name":"mcp-servers/everything"…`) y a `tools/list`.

### 2.4. Contactos y grants

Cada proveedor de B a G guarda a A, A guarda a cada uno, y cada uno da a A un grant del proyecto `demo` para las tres operaciones de tarea. G guarda además a B, sin grant (caso 6). H no entra todavía: A lo guarda en el caso 5, justo antes de que se pare, para que las búsquedas de los casos 1 a 4 no lo cuenten.

```bash
# En A
. ~/pals-h2/funciones.sh; agente a; pals id --ticket       # -> <ticket de A>
# En cada proveedor x (b, c, d, e, g)
. ~/pals-h2/funciones.sh; agente x
pals contact add <ticket de A> --name a
pals id --ticket                                            # -> <ticket de x>
pals grant issue --to a --project demo --ttl 24h            # -> <token de x>
# En A, para cada x
pals contact add <ticket de x> --name x
pals grant import <token de x>
# En G, con el ticket de B
. ~/pals-h2/funciones.sh; agente g; pals contact add <ticket de B> --name b
```

### 2.5. C, lleno

C admite una tarea a la vez y ninguna en cola. A le pide una tarea de una hora y deja de esperarla a los 10 s; la tarea sigue en C:

```bash
# En A
. ~/pals-h2/funciones.sh; agente a
pals ask c "espera 3600" --wait 10s; echo "código $?"
# En C: la tarea está en marcha
. ~/pals-h2/funciones.sh; agente c; espera 60 "la tarea larga de C" grep -q 'espera 3600' "$H2/c-ejecuciones.log" && ejecuciones
```

**Esperado:** en A, código 3 (`pals task resume` la retomaría); en C, `1`.

## 3. Casos

Antes de cada caso, en las máquinas que lo necesiten: `. ~/pals-h2/funciones.sh` y `agente <x>`.

### Caso 1. Elegir entre al menos tres candidatos

En A:

```bash
. ~/pals-h2/funciones.sh; agente a
cd ~/pals-h2
pals discover --capability code.rust.review --requirements-file req.json --ask "Revisa src/lib.rs"; echo "código $?"
```

**Esperado:** código 0 y, en este orden:

```text
ranked:
  1. b (agent <B>): available, skill repo-review, time 1100 ms, …, grant held, card verified <B>, offer <oferta de B> until <hora>
  2. c (agent <C>): busy, skill repo-review, time 110 ms, …, grant held, card verified <C>, offer <oferta de C> until <hora>
discarded:
  d (offer): external_cloud_allowed not met
  e (offer): mcp_integration:example:everything not operational
  g (offer): min_offered_ram_mib not met
coverage: complete
metrics: contacted 5, messages …, bytes sent …, bytes received …, first valid … ms, matching … us, elapsed … ms
asked: b
b: Revisa src/lib.rs
```

C es más rápido que B, pero está lleno. En cada proveedor, `ejecuciones`: B 1, C 1 (la tarea larga), D, E y G 0. Anota la línea `metrics:`.

### Caso 2. Requisitos sobre integraciones y recursos

La misma búsqueda, sin pregunta, por el gateway de A, para leer las ofertas enteras; y la conexión MCP de B y la de E, como las publican a A:

```bash
. ~/pals-h2/funciones.sh; agente a
busca caso2
python3 - ~/pals-h2/busquedas/caso2.json <<'PY'
import json, sys
o = json.load(open(sys.argv[1]))
for c in o['ranked']:
    print(c['peer'], c['offer']['status'], [(r['requirement'], r['result'], r.get('basis')) for r in c['offer']['requirement_results']])
for d in o['discarded']:
    print(d['peer'], d['stage'], d['reason'])
PY
pals inventory get b --sections mcp
pals inventory get e --sections mcp
```

**Esperado:** `200`; B `available`, con `external_cloud_allowed`, `mcp_integration:example:everything` y `min_offered_ram_mib` `met`, el MCP con base `observed`; E descartado `mcp_integration:example:everything not operational`, G `min_offered_ram_mib not met`. En la vista de B, la entrada `example:everything` con `operational` `ok`, observada por el adaptador (`"source": {"kind": "adapter", "method": "mcp tools/list"}`), y las herramientas `example:everything/echo` y `example:everything/get-sum`; en la de E, `failed` con `missing tools: read_file`.

### Caso 3. Validar cards

La card firmada de B, como la sirve el gateway de A, con el verificador del SDK de Python, y guardada en un fichero para los de Go y JS. Cada verificador comprueba además que una copia alterada (`skills[0].description`) no verifica y que la card no verifica si se espera otro agente.

```bash
. ~/pals-h2/funciones.sh; agente a
B=$(pals contact ls | awk '$1 == "b" {print $2}')
~/pals-h2/venv/bin/python "$REPO/tests/interop/card_signatures.py" --gateway "$(gw)" --peer "$B"
curl -sS -o ~/pals-h2/tarjeta-b.json "$(gw)/peers/$B/signed-agent-card.json"
go -C "$REPO/tests/interop/card_signatures/go" run . "$B" ~/pals-h2/tarjeta-b.json
(cd "$REPO/tests/interop/card_signatures/js" && npm ci --no-audit --no-fund) > /dev/null
node "$REPO/tests/interop/card_signatures/js/verify.mjs" "$B" ~/pals-h2/tarjeta-b.json
```

**Esperado:** `3 checks passed`, `verified` y `verified`.

Una card alterada queda descartada. Ningún daemon PALS sirve una card alterada de sí mismo; lo hacen los pares de prueba de los tests de integración, que sirven una card firmada y cambiada después: el gateway la rechaza (`bad card signature`) y la búsqueda la descarta en la etapa de card sin preguntarle nada. En la máquina donde se compiló:

```bash
cargo test --locked -p pals --test cards gateway_refuses_a_tampered_peer_card
cargo test --locked -p pals --test selection cards_are_validated
```

Y en vivo, F alterado (sección 2.1), un agente corriente cuya firma ya no cubre su card:

```bash
. ~/pals-h2/funciones.sh; agente a
pals contact add-a2a "https://$F_HOST:8444" --name f-alterada
pals discover --capability example:eco --peer f-alterada
```

**Esperado:** los dos tests en verde (`1 passed`); `card signature unverifiable` al guardarlo, y `f-alterada (card): card not verified` en la búsqueda.

### Caso 4. No ejecutar trabajo al consultar

En B a G, antes y después, los `task.accepted` del registro, las ejecuciones del worker y los `tools/list` que recibió su servidor MCP:

```bash
. ~/pals-h2/funciones.sh
for x in b c d e g; do agente $x; echo "$x task.accepted $(eventos task.accepted) ejecuciones $(ejecuciones) tools/list $(tools_list) pid $(cat $PALS_HOME/daemon.pid)"; done
```

En A, la búsqueda del caso 1 sin pregunta, entre B, C, D, E y G: primero 50 a la vez, la ráfaga; después otras 50 de 4 en 4, el máximo de búsquedas a la vez del daemon, para que lleguen todas a los proveedores:

```bash
cat > ~/pals-h2/caso4.sh <<'EOF'
#!/bin/bash
# Caso 4, en A: n búsquedas (50 por defecto) por el gateway de A, la del
# caso 1 sin pregunta, entre b, c, d, e y g, de p en p (todas a la vez por
# defecto). Guarda cada respuesta en ~/pals-h2/busquedas/caso4-<nombre>-<i>.json
# y su código HTTP en el .codigo, y cuenta los códigos.
#   bash caso4.sh <nombre> [<n> [<p>]]
. ~/pals-h2/funciones.sh
agente a
nombre=$1 n=${2:-50}
p=${3:-$n}
dir=$H2/busquedas
mkdir -p "$dir"
rm -f "$dir/caso4-$nombre-"*
cuerpo b c d e g > "$dir/caso4-$nombre-cuerpo.json"
export dir nombre url="$(gw)/discovery/search"
inicio=$(date +%s%N)
seq 1 "$n" | xargs -P "$p" -I '{}' sh -c 'curl -sS --max-time 120 -o "$dir/caso4-$nombre-$1.json" \
    -w "%{http_code}\n" -X POST -H "content-type: application/json" \
    --data-binary @"$dir/caso4-$nombre-cuerpo.json" "$url" > "$dir/caso4-$nombre-$1.codigo" 2>&1' sh '{}'
echo "$n búsquedas, de $p en $p, en $(( ($(date +%s%N) - inicio) / 1000000 )) ms; códigos HTTP:"
cat "$dir/caso4-$nombre-"*.codigo | sort | uniq -c
EOF
bash ~/pals-h2/caso4.sh rafaga 50
bash ~/pals-h2/caso4.sh seguidas 50 4
python3 ~/pals-h2/metricas.py ~/pals-h2/busquedas caso4-rafaga-
python3 ~/pals-h2/metricas.py ~/pals-h2/busquedas caso4-seguidas-
curl -sS -o /dev/null -w '%{http_code}\n' "$(gw a)/healthz"; busca caso4-despues b c d e g
```

**Esperado:**

- La ráfaga: solo códigos `200` y `429` (`too many searches`: el daemon hace a lo sumo 4 búsquedas a la vez y rechaza en el acto las demás), al menos un `200` y ningún `5xx`.
- Las seguidas: 50 `200`.
- Cada búsqueda respondida, con B y C en su ranking, por ese orden. Un proveedor que ya recibió 60 consultas de A en el último minuto rechaza las siguientes (`refused: RateLimited`), y A lo descarta en esa búsqueda: es su cuota, no un fallo.
- Después, en B a G, los mismos `task.accepted` y ejecuciones que antes, los mismos PID y `tools/list` sin cambios, o uno más por cada vez que venció el temporizador de 60 s mientras tanto: `~/pals-h2/<x>-mcp-arranques.log` tiene la hora de cada comprobación; en A, `200`, y la búsqueda de después, `200`.

### Caso 5. Oferta vencida y proveedor ocupado

H pasa a ser un contacto de A, con su grant, y se para: desde aquí es un contacto caído. B vuelve a arrancar con ofertas de 2 s. C sigue lleno con la tarea larga.

```bash
# En H, con el ticket de A
. ~/pals-h2/funciones.sh; agente h
pals contact add <ticket de A> --name a
pals id --ticket                                            # -> <ticket de H>
pals grant issue --to a --project demo --ttl 24h            # -> <token de H>
# En A
. ~/pals-h2/funciones.sh; agente a
pals contact add <ticket de H> --name h
pals grant import <token de H>
# En H
. ~/pals-h2/funciones.sh; agente h; para
# En B
. ~/pals-h2/funciones.sh; agente b
para && config_proveedor --oferta-ms 2000 && arranca
```

**5a. Una oferta vencida no se usa.** En A, la oferta de B que da `pals discover`, y, cuando vence por el reloj de A, una pregunta que la nombra:

```bash
. ~/pals-h2/funciones.sh; agente a
pals discover --capability code.rust.review --requirements-file ~/pals-h2/req.json --peer b | tee ~/pals-h2/oferta-b.txt
read -r OFERTA HASTA < <(sed -nE 's/^  1\. b .*, offer ([^ ]+) until ([^ ]+)$/\1 \2/p' ~/pals-h2/oferta-b.txt)
espera 30 "que venza la oferta" sh -c "[ \$(date +%s) -ge $(date -d "$HASTA" +%s) ]"
pals ask b "pregunta con la oferta vencida" --offer "$OFERTA"; echo "código $?"
```

**Esperado:** B `available`, con su oferta y una caducidad a 2 s; después, `pals: offer expired: run pals discover again` y código 2. En B, `ejecuciones` igual que antes: no se envió nada.

**5b. La oferta vence entre la búsqueda y la pregunta.** En A, la búsqueda del caso 1 entre B, C y H, con pregunta. H, caído, agota los 3 s que tiene cada candidato, así que la oferta de B, de 2 s, ha vencido cuando la búsqueda termina:

```bash
. ~/pals-h2/funciones.sh; agente a
busca caso5 --ask "pregunta del caso 5" b c h
python3 - ~/pals-h2/busquedas/caso5.json <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
s, c = r['search'], r.get('chosen')
print('ranking:', [(x['peer'], x['offer']['status'], x['offer']['offer_id'], x['offer']['expires_at']) for x in s['ranked']])
print('descartes:', [(d['peer'], d['reason']) for d in s['discarded']])
if c:
    print('elegido:', c['candidate']['peer'], c['candidate']['offer']['offer_id'], c['outcome']['state'], c['outcome'].get('answer'))
    print('probados antes:', c['tried'])
else:
    print('sin elegido:', r.get('error'), r.get('tried'))
PY
pals task ls | tail -n 3
# En B y en C
for x in b c; do agente $x; echo "$x: $(grep -c 'pregunta del caso 5' "$H2/$x-ejecuciones.log" 2> /dev/null) ejecuciones de la pregunta, $(ejecuciones) en total"; done
```

**Esperado:** en el ranking, B `available` y C `busy`; H descartado (`timeout` o `card: peer unreachable`). La pregunta la hace B, `TASK_STATE_COMPLETED`, `b: pregunta del caso 5`, con **otra** oferta que la del ranking: la renovada, de un `offer_id` distinto y que vence después. Si B rechaza la oferta por vencida al recibir la tarea, `pals task ls` muestra esa pregunta `refused` y otra completada, y la renovación es la de esa negativa. Si no hubiera podido renovarla, A habría pasado a C, que la rechaza por lleno (`tried: c: busy`). En cualquier caso, la pregunta se ejecuta **una** vez en B y ninguna en C.

### Caso 6. Vistas y caché por permisos

A tiene un grant de B para el proyecto `demo`; G no tiene ninguno. Cada uno lee la vista del inventario de B por su propio gateway (`GET /discovery/inventory/<B>`), y prueba el cursor del otro. El guion imprime, de cada página, el código HTTP, su ámbito, su destinatario, sus entradas por sección, las entradas de proyecto que contiene y si trae cursor (que guarda en un fichero):

```bash
cat > ~/pals-h2/vista.py <<'EOF'
"""Caso 6: una página de la vista del inventario de un par, por el gateway
de este agente: GET <gateway>/discovery/inventory/<par>[?cursor=...].

Uso: python3 vista.py <gateway> <par> <nombre> [<fichero con un cursor>]

Guarda la respuesta en ~/pals-h2/vistas/<nombre>.json y el cursor de la
página siguiente, si lo hay, en ~/pals-h2/vistas/<nombre>.cursor. Imprime el
código HTTP y, de una página: su ámbito, su destinatario, su seq, cuántas
entradas trae de cada sección, cuáles de PRIVADAS (las entradas del proyecto
demo) contiene, y las claves que no son del esquema de una página.
"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

gateway, par, nombre = sys.argv[1:4]
carpeta = Path.home() / 'pals-h2' / 'vistas'
carpeta.mkdir(parents=True, exist_ok=True)
url = f'{gateway.rstrip("/")}/discovery/inventory/{par}'
if len(sys.argv) > 4:
    url += '?cursor=' + urllib.parse.quote(Path(sys.argv[4]).read_text().strip(), safe='')
try:
    with urllib.request.urlopen(urllib.request.Request(url), timeout=120) as r:
        codigo, cuerpo = r.status, r.read()
except urllib.error.HTTPError as e:
    codigo, cuerpo = e.code, e.read()
(carpeta / f'{nombre}.json').write_bytes(cuerpo)
print(f'{nombre}: código {codigo}')
doc = json.loads(cuerpo)
if codigo != 200:
    print(f'  {doc}')
    sys.exit(0)
vista = doc['view']
privadas = set(os.environ.get('PRIVADAS', '').split())
por_seccion = {}
for e in vista['entries']:
    por_seccion[e['section']] = por_seccion.get(e['section'], 0) + 1
esquema = {'schema', 'agent_id', 'scope', 'audience', 'seq', 'issued_at', 'expires_at',
           'agent_card_hash', 'sections', 'entries', 'next_cursor', 'truncated_by'}
print(f"  scope {vista['scope']}, audience {vista.get('audience', '-')}, seq {vista['seq']}")
print(f"  entradas {len(vista['entries'])}: {por_seccion}")
print(f"  de proyecto: {sorted(e['id'] for e in vista['entries'] if e['id'] in privadas) or 'ninguna'}")
print(f"  claves fuera del esquema: {sorted(set(vista) - esquema) or 'ninguna'}")
cursor = vista.get('next_cursor')
if cursor:
    (carpeta / f'{nombre}.cursor').write_text(cursor)
print(f"  cursor: {'sí' if cursor else 'no'}, truncated_by {vista.get('truncated_by', '-')}")
EOF
# En A
. ~/pals-h2/funciones.sh; agente a; export PRIVADAS
python3 ~/pals-h2/vista.py "$(gw)" b a-1
python3 ~/pals-h2/vista.py "$(gw)" b a-2 ~/pals-h2/vistas/a-1.cursor
# En G
. ~/pals-h2/funciones.sh; agente g; export PRIVADAS
python3 ~/pals-h2/vista.py "$(gw)" b g-1
python3 ~/pals-h2/vista.py "$(gw)" b g-2 ~/pals-h2/vistas/g-1.cursor
pals query b --capability code.rust.review | tail -n 1
pals query b --capability example:no-existe | tail -n 1
```

Después, cada cursor en el gateway del otro: copia `a-1.cursor` de A a la máquina de G y `g-1.cursor` de G a la de A (si están en la misma máquina, ya están), y:

```bash
# En G, con el cursor de A
python3 ~/pals-h2/vista.py "$(gw)" b g-con-cursor-de-a ~/pals-h2/vistas/a-1.cursor
# En A, con el cursor de G
python3 ~/pals-h2/vista.py "$(gw)" b a-con-cursor-de-g ~/pals-h2/vistas/g-1.cursor
```

**Esperado:**

- A: `scope task:demo`, `audience <A>`; entre las dos páginas, 124 entradas: `repo-review` (servicio), `example:everything`, `example:coder`, `example:host` y las 120 notas. Página 1 con cursor (`truncated_by page_entries`), página 2 sin él.
- G: `scope public`, sin `audience`; entre las dos páginas, las 120 notas y nada más: `de proyecto: ninguna`, `claves fuera del esquema: ninguna`. Nada en la página dice cuántas entradas quedan fuera ni deja calcularlo.
- `pals query` desde G: `not_offered` para `code.rust.review`, igual que para una capacidad que B no tiene.
- Los cursores cruzados: `código 400` y `{'error': 'peer refused: invalid cursor'}`, el mismo en los dos sentidos: el error no dice de quién era el cursor ni qué ámbito tenía.

### Caso 7. Contacto A2A corriente

En A, F por `https` con el certificado de la CA de prueba (sección 2.2), por el gateway de A: con `pals ask`, con un cliente corriente por los dos bindings y en una búsqueda junto a F alterado.

```bash
. ~/pals-h2/funciones.sh; agente a
pals contact add-a2a "https://$F_HOST:8443" --name f
pals ask f "hola, F"; echo "código $?"
curl -sS -X POST -H 'content-type: application/json' -H 'a2a-version: 1.0' "$(gw)/peers/f/jsonrpc" \
    -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"caso7-jsonrpc","role":"ROLE_USER","parts":[{"text":"por JSON-RPC"}]}}}'; echo
curl -sS -X POST -H 'content-type: application/json' -H 'a2a-version: 1.0' "$(gw)/peers/f/rest/message:send" \
    -d '{"message":{"messageId":"caso7-rest","role":"ROLE_USER","parts":[{"text":"por HTTP+JSON"}]}}'; echo
curl -sS "$(gw)/peers/f/.well-known/agent-card.json"; echo
pals discover --capability example:eco --peer f --peer f-alterada
pals contact add-a2a "http://$F_HOST:8443" --name f-http; echo "código $?"
```

**Esperado:** `card signature verified f-1` al guardarlo; `F: hola, F` y código 0; las dos llamadas, una tarea `TASK_STATE_COMPLETED` con el artefacto `F: por JSON-RPC` y `F: por HTTP+JSON`; la card de F con sus interfaces en el gateway de A (`…/peers/f/jsonrpc`, `…/peers/f/rest`) y sin `securitySchemes`; en la búsqueda, `f (plain A2A agent): card only, skill eco, …, card verified f-1` y `f-alterada (card): card not verified`; y `http://` a otra máquina rechazado al guardarlo (`http:// is only for 127.0.0.1, [::1] and localhost`).

Si el entorno no permite instalar la CA, F se ejecuta en la máquina de A, por `http://127.0.0.1:8443` (`--url http://127.0.0.1:8443`, sin `--tls-*`): las reglas de los contactos corrientes solo permiten `http://` a loopback.

### Caso 8. Migración

En cualquier máquina, con el `pals` del hito 1 y `events_verify` de la sección 1. El guion crea dos agentes con el hito 1, M1 y M2, en loopback y sin relay: M2 guarda a M1, recibe de él dos grants (y M1 revoca uno), y le pregunta una vez con un fichero adjunto. Para los dos daemons, copia el estado de los dos almacenes y arranca el `pals` del hito 2 sobre los mismos directorios. Comprueba que el esquema pasa a la versión 2 y que cada fila de las tablas del hito 1 sigue igual, y compara lo que listan las dos versiones de `pals`. Por último, M2 pregunta otra vez a M1, ya con el hito 2, y `events_verify` verifica la cadena de eventos de cada uno: la de M1 sigue la del hito 1 con los eventos de esa pregunta.

```bash
cat > ~/pals-h2/caso8.sh <<'EOF'
#!/bin/bash
# Caso 8: un directorio de datos del hito 1 arranca con el hito 2.
#   bash caso8.sh <pals del hito 1> <events_verify>
# El pals del hito 2 es el del PATH. Trabaja en ~/pals-h2/migracion, que
# borra antes; los gateways, en 127.0.0.1:7431 (M1) y 7432 (M2).
set -uo pipefail
H1=$(realpath "$1"); EV=$(realpath "$2")
M=~/pals-h2/migracion
rm -rf "$M"; mkdir -p "$M"
fallos=0
falla() { echo "FALLO: $*"; fallos=$((fallos + 1)); }
espera() {
    local hasta=$(( $(date +%s) + $1 )); shift
    until "$@" > /dev/null 2>&1; do [ "$(date +%s)" -lt "$hasta" ] || return 1; sleep 0.5; done
}
arranca() {   # arranca <m1|m2> <binario>
    PALS_HOME=$M/$1 nohup "$2" daemon >> "$M/$1/daemon.log" 2>&1 < /dev/null &
    echo $! > "$M/$1/daemon.pid"
    espera 120 env PALS_HOME="$M/$1" "$2" id --ticket || { falla "$1 no arranca con $2"; return 1; }
}
vivo() {      # vivo <pid>: si el proceso existe y no es un zombi
    [ -e "/proc/$1" ] && ! grep -q '^State:.*Z' "/proc/$1/status" 2> /dev/null
}
para() {      # para <m1|m2>: SIGTERM al PID si es un pals daemon con ese PALS_HOME
    local pid; pid=$(cat "$M/$1/daemon.pid")
    case "$(tr '\0' ' ' < "/proc/$pid/cmdline" 2> /dev/null)" in *" daemon ") ;; *) return 0 ;; esac
    tr '\0' '\n' < "/proc/$pid/environ" | grep -qxF "PALS_HOME=$M/$1" || return 0
    kill "$pid"
    local hasta=$(( $(date +%s) + 60 ))
    while vivo "$pid"; do [ "$(date +%s)" -lt "$hasta" ] || { falla "$1 no termina"; return 1; }; sleep 0.2; done
}
estado() {    # estado <m1|m2> <fichero>: versión del esquema y filas de las tablas del hito 1
    python3 - "$M/$1/pals.db" "$2" <<'PY'
import json, sqlite3, sys
db = sqlite3.connect(f'file:{sys.argv[1]}?mode=ro', uri=True)
tablas = ['contacts', 'grants_issued', 'grants_held', 'revocations', 'tasks', 'task_owners', 'dedup', 'events', 'outbox']
estado = {'user_version': db.execute('PRAGMA user_version').fetchone()[0]}
for t in tablas:
    cols = [c[1] for c in db.execute(f'PRAGMA table_info({t})')]
    estado[t] = {'columnas': cols, 'filas': [list(map(str, r)) for r in db.execute(f'SELECT * FROM {t} ORDER BY 1, 2')]}
json.dump(estado, open(sys.argv[2], 'w'), indent=1)
print(f"{sys.argv[1]}: esquema {estado['user_version']}, " + ', '.join(f"{t} {len(estado[t]['filas'])}" for t in tablas))
PY
}
listados() {  # listados <m1|m2> <binario>: lo que lista pals de ese agente
    local b=$2
    export PALS_HOME=$M/$1
    { "$b" contact ls; "$b" grant ls; "$b" task ls | cut -f1-5; } 2>&1 | sed -E 's/until [0-9T:Z-]+/until …/'
    unset PALS_HOME
}

echo "== hito 1: $("$H1" --version)"
for x in m1 m2; do
    mkdir -p "$M/$x"
    port=7431; [ "$x" = m2 ] && port=7432
    printf '[agent]\nname = "%s"\n\n[gateway]\nlisten = "127.0.0.1:%s"\n\n[net]\nrelay = false\n' "$x" "$port" > "$M/$x/config.toml"
    PALS_HOME=$M/$x "$H1" init > /dev/null
    arranca "$x" "$H1" || exit 1
done
t1=$(PALS_HOME=$M/m1 "$H1" id --ticket); t2=$(PALS_HOME=$M/m2 "$H1" id --ticket)
PALS_HOME=$M/m2 "$H1" contact add "$t1" --name m1
PALS_HOME=$M/m1 "$H1" contact add "$t2" --name m2
PALS_HOME=$M/m2 "$H1" grant import "$(PALS_HOME=$M/m1 "$H1" grant issue --to m2 --project demo --ttl 24h)"
# Un segundo grant, solo para blobs (el gateway no lo presenta para tareas), revocado.
PALS_HOME=$M/m2 "$H1" grant import "$(PALS_HOME=$M/m1 "$H1" grant issue --to m2 --project otro --ops blob.fetch --ttl 1h)"
PALS_HOME=$M/m1 "$H1" grant revoke "$(PALS_HOME=$M/m1 "$H1" grant ls --issued | awk '/project otro/ {print $1}')"
echo "adjunto del caso 8" > "$M/adjunto.txt"
PALS_HOME=$M/m2 "$H1" ask m1 "hola desde el hito 1" --attach "$M/adjunto.txt" || falla "la pregunta del hito 1"
for x in m1 m2; do para "$x"; done
for x in m1 m2; do estado "$x" "$M/$x-antes.json"; listados "$x" "$H1" > "$M/$x-listado-antes.txt"; done

echo "== hito 2: $(pals --version)"
for x in m1 m2; do arranca "$x" pals || exit 1; done
for x in m1 m2; do estado "$x" "$M/$x-despues.json"; listados "$x" pals > "$M/$x-listado-despues.txt"; done
python3 - "$M" <<'PY' || fallos=$((fallos + 1))
import json, sys
m, ok = sys.argv[1], True
for x in ('m1', 'm2'):
    antes, despues = (json.load(open(f'{m}/{x}-{c}.json')) for c in ('antes', 'despues'))
    print(f"{x}: esquema {antes['user_version']} -> {despues['user_version']}")
    ok &= antes['user_version'] == 1 and despues['user_version'] == 2
    for t, a in antes.items():
        if t == 'user_version':
            continue
        d = despues[t]
        # Las columnas del hito 1, en las filas de después.
        idx = [d['columnas'].index(c) for c in a['columnas']]
        filas = sorted([[r[i] for i in idx] for r in d['filas']])
        igual = sorted(a['filas']) == filas
        ok &= igual
        print(f"  {t}: {len(a['filas'])} filas antes, {len(d['filas'])} después: {'conservadas' if igual else 'DISTINTAS'}")
sys.exit(0 if ok else 1)
PY
for x in m1 m2; do
    if diff "$M/$x-listado-antes.txt" "$M/$x-listado-despues.txt" > /dev/null; then
        echo "$x: contact ls, grant ls y task ls, iguales con las dos versiones"
    else
        falla "$x: los listados cambian"; diff "$M/$x-listado-antes.txt" "$M/$x-listado-despues.txt"
    fi
    sed 's/^/    /' "$M/$x-listado-despues.txt"
done
# Ya con el hito 2: sin relay, los puertos de Iroh cambiaron; M2 guarda el ticket nuevo de M1.
PALS_HOME=$M/m2 pals contact add "$(PALS_HOME=$M/m1 pals id --ticket)" --name m1 > /dev/null
r=$(PALS_HOME=$M/m2 pals ask m1 "hola desde el hito 2") || falla "la pregunta del hito 2"
echo "M2 pregunta a M1 con el hito 2: $r"
for x in m1 m2; do para "$x"; done
for x in m1 m2; do
    id=$(PALS_HOME=$M/$x pals id)
    "$EV" "$M/$x/pals.db" "$id" || falla "events_verify de $x"
done
python3 - "$M" <<'PY'
import sqlite3, sys
for x in ('m1', 'm2'):
    db = sqlite3.connect(f'file:{sys.argv[1]}/{x}/pals.db?mode=ro', uri=True)
    print(x, 'eventos:', db.execute("SELECT group_concat(kind, ', ') FROM (SELECT kind FROM events ORDER BY seq)").fetchone()[0])
PY
[ "$fallos" = 0 ] && echo "caso 8: superado" || echo "caso 8: FALLIDO ($fallos)"
[ "$fallos" = 0 ]
EOF
bash ~/pals-h2/caso8.sh ~/pals-h2/pals-hito1 ~/pals-h2/events_verify
```

**Esperado:** `esquema 1 -> 2` en M1 y M2 y, para cada tabla del hito 1, `conservadas`: en M1, dos grants emitidos, una revocación, una tarea, su fila de deduplicación y sus eventos (`grant.issued` dos veces, `grant.revoked`, `task.accepted`, `task.terminal`); en M2, dos grants recibidos, el grant del fichero adjunto que dio a M1, la pregunta en el outbox y su evento `blob.shared`. `contact ls`, `grant ls` y `task ls`, iguales con las dos versiones. `M2 pregunta a M1 con el hito 2: echo: hola desde el hito 2`. `events_verify: <n> events of <id> verified` de los dos, con los eventos de la pregunta del hito 2 al final de la cadena de M1. Y `caso 8: superado`.

### Caso 9. Sin regresión A2A

En la máquina donde se compiló, como en el hito 1, con los informes en `docs/conformance/hito-2`:

```bash
PALS_CONFORMANCE_DIR=docs/conformance/hito-2 tests/interop/run_tck.sh
python3 -m venv target/interop-venv && target/interop-venv/bin/pip install -r tests/interop/requirements.txt
target/interop-venv/bin/python tests/interop/python_client.py
target/interop-venv/bin/python tests/interop/check_schemas.py
```

**Esperado:** el TCK con código 0 y ningún MUST fallido en `jsonrpc` ni en `http_json`, con al menos 58 y 56 superados, y `CARD-EXT-001` superado; `4 checks passed`; todos los documentos válidos. Las diferencias con el hito 1 se explican en `docs/conformance/hito-2/README.md`.

## 4. Registro

Anota en [`docs/conformance/hito-2/exit-test.md`](../conformance/hito-2/exit-test.md): la fecha; las máquinas o contenedores, el commit, `pals --version`, `rustc --version`, las versiones fijadas (iroh, SDK de A2A, server-everything, a2a-sdk de Python) y el camino de red; el resultado de cada caso con su evidencia; y las métricas del §14:

- **Tiempo hasta el primer candidato válido:** `first valid` de la línea `metrics:` (`time_to_first_valid_ms`), en el caso 1 y en las búsquedas respondidas del caso 4.
- **Porcentaje de búsquedas con candidato:** el que da `metricas.py`, sobre las respondidas y sobre las lanzadas.
- **Mensajes y bytes por búsqueda:** `messages`, `bytes sent` y `bytes received`.
- **Candidatos contactados:** `contacted`.
- **Coste del matching:** `matching` (`matching_us`), el tiempo de las dos etapas en A.
- **Filtraciones de metadatos observadas:** lo que el caso 6 vio en las páginas públicas y en las respuestas a G.

No copies tickets ni tokens de grants: los ids de agente y los hashes sí pueden ir.

## 5. Limpieza

```bash
# En cada máquina, para cada agente que corra en ella
. ~/pals-h2/funciones.sh
for x in a b c d e g h; do [ -d ~/pals-h2/$x ] && { agente $x; para; }; done
# En la de F: sus dos servidores, si sus PID siguen siendo agente_f.py
for p in 8443 8444; do pid=$(cat ~/pals-h2/f-$p.pid 2> /dev/null) && tr '\0' ' ' < /proc/$pid/cmdline | grep -q agente_f.py && kill "$pid"; done
# En la de A: la CA de prueba fuera del almacén del sistema
$SUDO rm -f /usr/local/share/ca-certificates/pals-h2-prueba.crt && $SUDO update-ca-certificates --fresh
```

La tarea larga de C termina con su daemon (el worker mata su grupo de procesos). Después, `rm -rf ~/pals-h2` si ya no hace falta nada.
