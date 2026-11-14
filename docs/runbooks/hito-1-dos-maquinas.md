# Runbook: prueba de salida del hito 1 en dos máquinas

Instrucciones para ejecutar la prueba que cierra el hito 1: dos daemons `pals` en dos máquinas Linux reales, A (pregunta) y B (responde), y los ocho casos del criterio de salida. Cada caso trae los comandos exactos y el resultado esperado. Los resultados se anotan en [`docs/conformance/hito-1/exit-test.md`](../conformance/hito-1/exit-test.md), que hoy recoge solo un ensayo en una máquina.

| Caso | Qué se comprueba |
|-|-|
| 1. Pregunta útil | B responde con `claude -p` sobre un proyecto real; la respuesta usa contenido de ese proyecto |
| 2. Transferencia autorizada | Un fichero de 256 MiB va de A a B y vuelve como artefacto con el mismo hash |
| 3. Interrupción de transferencia | Un corte de red de 10 s en A a mitad de una transferencia: termina y verifica. Por camino directo y por relay, anotando cuál se usó |
| 4. Reintento sin duplicar | Un corte de red en A durante una tarea larga: `pals ask` termina y B tiene un solo `task.accepted` para ese `messageId` |
| 5. Caída del proveedor | `kill -9` del daemon de B a mitad de tarea: A recibe `TASK_STATE_FAILED` con `interrupted`, y un `ask` nuevo funciona al rearrancar B |
| 6. Revocación | `pals grant revoke` en B: el siguiente `ask` de A recibe 401 |
| 7. Blob sin grant | Una tercera identidad que conoce el hash del caso 2 no puede descargarlo |
| 8. Cliente estándar | El SDK oficial de Python, en A, contra el gateway de A, con B como par |

Tiempo estimado: entre una y dos horas, más el cambio de red para la repetición por relay del caso 3.

## 0. Requisitos

En las dos máquinas:

- Linux con `bash`, `curl`, `sha256sum`, `stat` de GNU coreutils, `ip` y `ss` (iproute2), `python3` con `venv`, y `sudo` (para cortar la red en los casos 3 y 4).
- Rust 1.91 o posterior (`rustup`) y `git`. La compilación del hito 1 se comprobó con Rust 1.99.1.
- Los relojes sincronizados: los grants toleran 60 s de diferencia. `timedatectl` debe decir `System clock synchronized: yes`.
- Salida a internet hacia los relays públicos de Iroh (TCP 443) y UDP saliente permitido, para el camino directo.
- El mismo commit de `rosepetal-pals`.

En B, además:

- Claude Code (`claude`) instalado y con sesión iniciada por el usuario que ejecuta el daemon. El worker arranca cada orden con un entorno limpio: solo hereda `PATH`, `HOME` y `LANG` (más las variables `PALS_*` de la tarea), así que `ANTHROPIC_API_KEY` no llega. `claude` debe encontrar sus credenciales bajo `HOME` (las que guarda el inicio de sesión de Claude Code) o arrancarse desde un guion envoltorio que exporte la clave (paso 2).
- Un proyecto real, un repositorio en disco, sobre el que A preguntará. A podrá leer lo que el `claude -p` de B lea de él.

Para la repetición por relay del caso 3: las dos máquinas en redes distintas (por ejemplo, una de ellas en la red móvil de un teléfono).

La tercera identidad del caso 7 es un tercer daemon en A, con otro directorio; no hace falta una tercera máquina.

## 1. Instalación (A y B)

```bash
git clone https://github.com/rosepetal-ai/rosepetal-pals.git
cd rosepetal-pals
git checkout feat/hito-1        # la rama del hito 1, o `dev` cuando esté fusionada
cargo install --locked --path crates/pals
pals --version
git rev-parse --short HEAD      # anótalo: debe ser el mismo en A y en B
mkdir -p ~/pals-salida
```

Si la rama aún no está publicada, copia el repositorio a cada máquina por otro medio (`git bundle`, `rsync`) y comprueba el commit igual.

Dos funciones para arrancar y parar el daemon dejan su PID y su registro en `~/pals-salida`. Defínelas en cada terminal donde las uses:

```bash
arranca() {
    nohup pals daemon >> ~/pals-salida/daemon.log 2>&1 &
    echo $! > ~/pals-salida/daemon.pid
    sleep 5
    grep 'pals daemon:' ~/pals-salida/daemon.log | tail -n 1
}
para() {
    local pid; pid=$(cat ~/pals-salida/daemon.pid)
    kill "$pid"
    while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
}
```

`arranca` debe imprimir `pals daemon: agent <id> serving, gateway at http://127.0.0.1:7420`. `para` envía `SIGTERM` y espera a que el daemon termine, lo que puede llevar unos segundos: hasta entonces conserva el bloqueo de su directorio de datos, y un `arranca` inmediato fallaría con `a pals daemon is already running on this data directory`. El daemon usa `~/.pals` (o `$PALS_HOME`) para su configuración, su clave y su almacén.

El registro, `~/pals-salida/daemon.log`, recoge además los diagnósticos del daemon desde el nivel `INFO`, como la descarga fallida de un fichero de entrada o un error al comprobar el grant de una petición de blobs. Si un caso falla sin explicación, para el daemon, ejecuta `export RUST_LOG=pals=debug` en esa terminal y vuelve a `arranca`: el registro detallará cada intento (`RUST_LOG` sigue la sintaxis de `tracing-subscriber`, p. ej. `RUST_LOG=pals=debug,iroh=info`).

## 2. Configuración

**`net.relay = true` en las dos máquinas.** Con `relay = false` el daemon se enlaza solo a `127.0.0.1` y no es alcanzable desde otra máquina; además su puerto cambia en cada arranque y los tickets guardados dejan de valer. Con `relay = true` usa los relays públicos de Iroh y su búsqueda de direcciones, de modo que un par se encuentra por su id aunque cambie de puerto.

`limits.blob_fetch_timeout_s` se sube de 120 s (valor por defecto) a 1800 s: es el tiempo máximo de una descarga, y 256 MiB por un enlace de subida lento o por un relay pueden superar los 120 s.

**En A:**

```bash
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
```

A usa el gateway por defecto, `127.0.0.1:7420`, y el worker `echo`.

**En B**, con `claude -p` para el caso 1:

```bash
PROYECTO=/ruta/absoluta/al/proyecto      # el repositorio real del caso 1
mkdir -p ~/.pals
cat > ~/.pals/config.toml <<EOF
[agent]
name = "pals-b"
description = "Máquina B de la prueba de salida del hito 1"

[worker]
adapter = "exec"
command = "claude"
args = ["-p", "--output-format", "text"]

[net]
relay = true

[limits]
blob_fetch_timeout_s = 1800

[projects.demo]
path = "$PROYECTO"
EOF
pals init
```

Antes de arrancar, comprueba que `claude` funciona en el entorno que le dará el worker, en el directorio del proyecto:

```bash
cd "$PROYECTO"
echo '¿Qué hace este proyecto? Responde en una frase.' \
    | env -i PATH="$PATH" HOME="$HOME" LANG="${LANG:-C.UTF-8}" claude -p --output-format text
```

Si pide credenciales, inicia sesión en Claude Code con este usuario, o guarda la clave en un fichero que solo él pueda leer (`~/.config/pals-salida/anthropic-key`, permisos `600`), crea un envoltorio y pon su ruta en `command`:

```bash
cat > ~/pals-salida/claude-con-clave.sh <<'EOF'
#!/bin/sh
ANTHROPIC_API_KEY=$(cat "$HOME/.config/pals-salida/anthropic-key")
export ANTHROPIC_API_KEY
exec claude "$@"
EOF
chmod +x ~/pals-salida/claude-con-clave.sh
# y en ~/.pals/config.toml: command = "/home/<usuario>/pals-salida/claude-con-clave.sh"
```

El daemon pasa a la orden su propio `PATH`: arráncalo desde una terminal donde `claude` esté en el `PATH`.

```bash
arranca
```

## 3. Contactos y grants, en los dos sentidos

Un *ticket* lleva el id del agente y sus direcciones; solo lo imprime un daemon en marcha. Espera unos segundos tras `arranca` para que incluya el relay. Los tickets y los tokens son una sola línea: cópialos entre máquinas tal cual (por SSH, `scp` o portapapeles).

```bash
# En B
pals id --ticket                 # copia la salida: TICKET_B
# En A
pals id --ticket                 # copia la salida: TICKET_A
pals contact add '<TICKET_B>' --name b
# En B
pals contact add '<TICKET_A>' --name a
```

Grant de B a A (A podrá preguntar a B en el proyecto `demo`), y de A a B (B podrá preguntar a A):

```bash
# En B
pals grant issue --to a --project demo --ttl 24h     # stdout: el token; stderr: el resumen
# En A
pals grant import '<token de B>'
pals grant issue --to b --project demo --ttl 24h
# En B
pals grant import '<token de A>'
```

Comprobación de los dos sentidos:

```bash
# En B: A responde con su worker echo
pals ask a hola                  # esperado: "echo: hola", código de salida 0
# En A
pals grant ls                    # held: el grant de B, active, task.send,task.read,task.cancel on skill:ask
pals contact ls                  # b <id de B>
```

## 4. Casos

Anota en cada caso la hora, la salida y el código de salida (`echo $?`). Códigos de `pals ask` y `pals task resume`: `0` tarea completada; `2` tarea fallida, rechazada o cancelada, o petición rechazada; `3` sin resultado (par inalcanzable o espera agotada; la pregunta se guarda y `pals task resume <messageId>` la retoma sin ejecutarla dos veces).

### Caso 1. Pregunta útil

En A, una pregunta cuya respuesta solo esté en el proyecto de B (un fichero, una orden, una dependencia concreta):

```bash
time pals ask b "Lee el README y los ficheros de configuración de este proyecto. ¿Cómo se arranca y qué dependencias principales tiene? Cita los ficheros."
echo $?
```

**Esperado:** código 0 y una respuesta que cita ficheros y datos de ese proyecto. Si `claude` dice que no pudo leer ficheros, añade `"--allowedTools", "Read,Grep,Glob"` a `args` en B, `para`, `arranca` y repite.

### Entre el caso 1 y el 2: B cambia al worker de pruebas

Los casos 2 a 8 usan un worker de pruebas en B, un guion `sh`, en lugar de `claude`. En B:

```bash
cat > ~/pals-salida/pals-prueba.sh <<'EOF'
#!/bin/sh
# Worker de la prueba de salida del hito 1, en la máquina B (adaptador exec).
# La pregunta llega por stdin y la respuesta sale por stdout.
#   devuelve     devuelve, como ficheros de la respuesta, los ficheros recibidos
#   genera <n>   responde con un fichero nuevo de <n> MiB aleatorios
#   espera <n>   tarda <n> segundos en responder (tarea larga)
#   otra cosa    responde «eco: <pregunta>»
set -eu
pregunta=$(cat)
case "$pregunta" in
    devuelve*)
        for f in "$PALS_IN_DIR"/*; do
            if [ -f "$f" ]; then cp "$f" "$PALS_OUT_DIR"/; fi
        done
        cd "$PALS_IN_DIR" && sha256sum -- * ;;
    "genera "*)
        n=${pregunta#genera }
        dd if=/dev/urandom of="$PALS_OUT_DIR/generado.bin" bs=1M count="$n" status=none
        cd "$PALS_OUT_DIR" && sha256sum generado.bin ;;
    "espera "*)
        n=${pregunta#espera }
        sleep "$n"
        echo "he esperado $n s" ;;
    *)
        echo "eco: $pregunta" ;;
esac
EOF
para
cat > ~/.pals/config.toml <<EOF
[agent]
name = "pals-b"
description = "Máquina B de la prueba de salida del hito 1"

[worker]
adapter = "exec"
command = "sh"
args = ["$HOME/pals-salida/pals-prueba.sh"]
timeout_s = 900

[net]
relay = true

[limits]
blob_fetch_timeout_s = 1800

[projects.demo]
path = "$PROYECTO"
EOF
arranca
```

En A: `pals ask b hola` debe responder `eco: hola` con código 0. B ha cambiado de puerto al rearrancar; si A no lo alcanza (código 3), anótalo (con `relay = true` no debería pasar), vuelve a guardar el ticket nuevo de B con `pals contact add '<TICKET_B>' --name b` y retoma la pregunta con `pals task resume <messageId>`.

### Caso 2. Transferencia autorizada

En A:

```bash
cd ~/pals-salida
head -c 256M /dev/urandom > grande.bin
sha256sum grande.bin
time pals ask b devuelve --attach grande.bin | tee caso2.out; echo "código ${PIPESTATUS[0]}"
URL=$(sed -n 's/^file grande.bin: //p' caso2.out); echo "$URL"
curl -sS -o devuelto.bin -w '%{http_code} %{size_download}\n' "$URL"
sha256sum grande.bin devuelto.bin
```

**Esperado:** código 0; B responde con el `sha256` del fichero que recibió, igual al de `grande.bin`, y una línea `file grande.bin: http://127.0.0.1:7420/blobs/<id de B>/<hash>`; `curl` imprime `200 268435456`; los dos `sha256sum` coinciden.

El fichero devuelto es idéntico al enviado, y el gateway de A sirve un blob que ya tiene completo desde su propio almacén: la vuelta no cruza la red. El sentido de B a A se comprueba con un fichero que crea B:

```bash
pals ask b "genera 256" | tee caso2b.out; echo "código ${PIPESTATUS[0]}"
URL=$(sed -n 's/^file generado.bin: //p' caso2b.out)
time curl -sS -o generado.bin -w '%{http_code} %{size_download}\n' "$URL"
sha256sum generado.bin          # igual al que B escribió en su respuesta
```

### Caso 3. Interrupción de transferencia

Dos guiones en A. `corte-red.sh` corta la red de A durante unos segundos, salvo loopback y SSH (puerto 22), y la restablece aunque se cierre la sesión SSH, porque se lanza con `setsid`. Si entras en A por Tailscale, la sesión se congela durante el corte y se recupera después. Este guion no se ha podido ejecutar en el ensayo: guarda antes las reglas del cortafuegos para comprobar después que quedan como estaban.

```bash
sudo iptables-save | grep -v '^[#:]' > ~/pals-salida/iptables.antes
sudo ip6tables-save | grep -v '^[#:]' > ~/pals-salida/ip6tables.antes
reglas_como_antes() {
    diff <(sudo iptables-save | grep -v '^[#:]') ~/pals-salida/iptables.antes \
        && diff <(sudo ip6tables-save | grep -v '^[#:]') ~/pals-salida/ip6tables.antes \
        && echo "reglas del cortafuegos como antes"
}
cat > ~/pals-salida/corte-red.sh <<'EOF'
#!/bin/sh
# Corta la red de esta máquina durante $1 segundos (10 por defecto), salvo
# loopback y SSH (puerto 22), y después la restablece.
# Uso: sudo setsid -f sh corte-red.sh 10
s=${1:-10}
for t in iptables ip6tables; do
    $t -I INPUT 1 -j DROP;  $t -I INPUT 1 -p tcp --dport 22 -j ACCEPT;  $t -I INPUT 1 -i lo -j ACCEPT
    $t -I OUTPUT 1 -j DROP; $t -I OUTPUT 1 -p tcp --sport 22 -j ACCEPT; $t -I OUTPUT 1 -o lo -j ACCEPT
done
logger -t corte-red "red cortada $s s"
sleep "$s"
for t in iptables ip6tables; do
    $t -D INPUT -i lo -j ACCEPT;  $t -D INPUT -p tcp --dport 22 -j ACCEPT;  $t -D INPUT -j DROP
    $t -D OUTPUT -o lo -j ACCEPT; $t -D OUTPUT -p tcp --sport 22 -j ACCEPT; $t -D OUTPUT -j DROP
done
logger -t corte-red "red restablecida"
EOF
```

`caso3.sh` lanza una transferencia de 256 MiB, espera a que lleve 32 MiB, corta la red de A 10 s y espera a que termine. Informa además de los bytes que han pasado por las conexiones del daemon a los relays (TCP 443): si al terminar han crecido en unos 268 MB, el camino fue por relay; si apenas cambian, fue directo (UDP). PALS no informa por sí mismo del camino de Iroh.

```bash
cat > ~/pals-salida/caso3.sh <<'EOF'
#!/bin/bash
# Caso 3 en la máquina A: corta la red de A 10 s a mitad de una transferencia
# de 256 MiB y comprueba que termina y verifica.
#   bash caso3.sh bajada   B genera un fichero nuevo y A lo descarga de B
#   bash caso3.sh subida   A adjunta un fichero nuevo y B lo descarga de A
set -eu
cd ~/pals-salida
sudo -v                       # la contraseña de sudo ahora, no a mitad de la transferencia
PID=$(cat daemon.pid)
IFACE=$(ip route get 1.1.1.1 | awk '{for (i = 1; i < NF; i++) if ($i == "dev") print $(i + 1)}')
relay() {   # bytes de las conexiones TCP del daemon al puerto 443 (los relays)
    ss -tinpH '( dport = :443 )' | grep -A1 "pid=$PID," | grep -o 'bytes_\(received\|acked\):[0-9]*' \
        | awk -F: '{s[$1] += $2} END {printf "relay: recibidos %d, enviados %d\n", s["bytes_received"], s["bytes_acked"]}'
}
espera_y_corta() {   # $1: función que imprime los bytes transferidos; $2: PID de la transferencia
    until [ "$($1)" -gt 33554432 ]; do
        kill -0 "$2" 2>/dev/null || { echo "la transferencia terminó antes del corte: $($1) bytes"; return; }
        sleep 0.01
    done
    echo "$(date -u +%T) transferidos $($1) bytes: corte de 10 s"
    sudo setsid -f sh ~/pals-salida/corte-red.sh 10
    sleep 1; echo "$(date -u +%T) durante el corte: $($1) bytes"
    sleep 5; echo "$(date -u +%T) durante el corte: $($1) bytes"
    sleep 6; echo "$(date -u +%T) tras el corte: $($1) bytes"
}
relay
case "${1:-bajada}" in
bajada)
    pals ask b "genera 256" | tee caso3.out
    URL=$(sed -n 's/^file generado.bin: //p' caso3.out)
    [ -n "$URL" ] || { echo "la respuesta no trae el fichero"; exit 1; }
    PARTE=~/.pals/blobs/data/${URL##*/}.data
    descargado() { stat -c %s "$PARTE" 2>/dev/null || echo 0; }
    curl -sS -o caso3.bin -w '%{http_code} %{size_download}\n' "$URL" > caso3.curl &
    CURL=$!
    espera_y_corta descargado "$CURL"
    code=0; wait "$CURL" || code=$?
    echo "curl: código $code, $(cat caso3.curl)"
    sha256sum caso3.bin ;;
subida)
    head -c 256M /dev/urandom > caso3.bin
    sha256sum caso3.bin
    TX0=$(cat "/sys/class/net/$IFACE/statistics/tx_bytes")
    enviado() { echo $(( $(cat "/sys/class/net/$IFACE/statistics/tx_bytes") - TX0 )); }
    pals ask b devuelve --attach caso3.bin > caso3.out &
    ASK=$!
    espera_y_corta enviado "$ASK"
    code=0; wait "$ASK" || code=$?
    echo "pals ask: código $code"; cat caso3.out ;;
esac
relay
EOF
```

**3a. Camino directo** (las dos máquinas en la misma red, o en redes distintas si Iroh consigue camino directo). En A:

```bash
bash ~/pals-salida/caso3.sh bajada
bash ~/pals-salida/caso3.sh subida
reglas_como_antes
```

**Esperado en `bajada`:** `curl: código 0, 200 268435456` y un `sha256sum` de `caso3.bin` igual al que B escribió en su respuesta. **En `subida`:** `pals ask: código 0` y, en `caso3.out`, el `sha256` que B recibió, igual al que se imprimió al principio. En los dos: las dos líneas `durante el corte` muestran la transferencia parada por debajo de 268435456 bytes, la línea `tras el corte` muestra que siguió, y la diferencia entre las dos líneas `relay:` dice el camino. Si la transferencia acaba antes del corte (`durante el corte` ya da el tamaño completo, o el guion dice que terminó antes), limita la subida de la máquina que envía durante la prueba (`sudo tc qdisc add dev <iface> root tbf rate 50mbit burst 64kb latency 400ms`, y después `sudo tc qdisc del dev <iface> root`) y repite.

**3b. Por relay.** Pon las máquinas en redes distintas y repite `bajada` y `subida`. Si Iroh encuentra igualmente camino directo (las líneas `relay:` apenas cambian), fuerza el relay bloqueando el UDP saliente de A salvo DNS, y rearranca su daemon:

```bash
for t in iptables ip6tables; do
    sudo $t -I OUTPUT 1 -p udp -j DROP; sudo $t -I OUTPUT 1 -p udp --dport 53 -j ACCEPT
done
para; arranca
bash ~/pals-salida/caso3.sh bajada
bash ~/pals-salida/caso3.sh subida
# deshacer:
for t in iptables ip6tables; do
    sudo $t -D OUTPUT -p udp --dport 53 -j ACCEPT; sudo $t -D OUTPUT -p udp -j DROP
done
para; arranca
```

**Esperado:** lo mismo que en 3a, con las líneas `relay:` creciendo en unos 268 MB. Anota en cada repetición el camino observado y cómo se dedujo.

### Caso 4. Reintento sin duplicar

En B, el guion que cuenta las tareas y los eventos `task.accepted` de un `messageId` en el almacén de B:

```bash
cat > ~/pals-salida/caso4-eventos.py <<'EOF'
"""Cuenta, en el almacén de B, las tareas y los eventos task.accepted de un messageId.

Uso: python3 caso4-eventos.py <pals.db de B> <messageId>
"""
import json
import sqlite3
import sys

db = sqlite3.connect(f'file:{sys.argv[1]}?mode=ro', uri=True)
message_id = sys.argv[2]
dedup = [t for (t,) in db.execute('SELECT task_id FROM dedup WHERE message_id = ?', (message_id,))]
tasks = [t for (t,) in db.execute('SELECT task_id FROM tasks WHERE instr(task_json, ?) > 0', (message_id,))]
accepted = [json.loads(p)['task_id'] for (p,) in db.execute("SELECT payload FROM events WHERE kind = 'task.accepted'")]
print(f'messageId {message_id}')
print(f'  filas de deduplicación: {len(dedup)} -> tarea {dedup}')
print(f'  tareas que lo contienen: {len(tasks)} -> {tasks}')
print(f'  eventos task.accepted de esas tareas: {sum(accepted.count(t) for t in set(tasks) | set(dedup))}')
EOF
```

En A, una tarea de 120 s y, a los 15 s, un corte de 45 s, más largo que los 30 s tras los que Iroh da una conexión por perdida:

```bash
sudo -v
pals ask b "espera 120" & ASK=$!
sleep 15; sudo setsid -f sh ~/pals-salida/corte-red.sh 45
wait $ASK; echo "código $?"
pals task ls | tail -n 1        # messageId, estado, par, tarea, intentos
```

Si `pals ask` terminó con código 3, retómala: `pals task resume <messageId>`. En B, con el `messageId` de A:

```bash
python3 ~/pals-salida/caso4-eventos.py ~/.pals/pals.db <messageId>
```

**Esperado:** en A, `he esperado 120 s` (código 0, directamente o tras `pals task resume`) y, en `pals task ls`, `TASK_STATE_COMPLETED` con dos o más intentos. En B: una fila de deduplicación, una tarea y **un** evento `task.accepted`.

### Caso 5. Caída del proveedor

En A:

```bash
pals ask b "espera 300" & ASK=$!
sleep 15; pals task ls | tail -n 1     # anota el messageId
```

En B, durante la tarea:

```bash
kill -9 "$(cat ~/pals-salida/daemon.pid)"
ps -eo pid,pgid,etime,args | grep '[p]als-prueba'     # limitación conocida: la orden sigue viva; anota su PGID
```

En A, `pals ask` reintenta y termina con código 3 (`b could not be reached; the question is kept`) si B no vuelve antes; en el ensayo tardó algo más de un minuto. Rearranca B y retoma la pregunta en A:

```bash
# En A
wait $ASK; echo "código $?"
# En B
arranca
# En A
pals task resume <messageId>; echo $?
pals ask b "hola tras el rearranque"; echo $?
# En B: termina la orden que sobrevivió al kill -9
kill -- -<PGID>
```

**Esperado:** `pals task resume` imprime `task <id> ended TASK_STATE_FAILED: interrupted: daemon restarted` con código 2 (o el `pals ask` original termina así, si B volvió dentro de sus reintentos); la tarea no se ejecuta otra vez (`caso4-eventos.py` en B da un `task.accepted`); el `ask` nuevo responde `eco: hola tras el rearranque` con código 0. Si A no alcanza a B tras el rearranque, anótalo, vuelve a guardar su ticket y repite `pals task resume`.

### Caso 6. Revocación

En B, el grant que A usa para preguntar es el de `project demo … on skill:ask … to a`. `pals grant ls --issued` lista también los grants `project blobs … blob.fetch` que PALS emite solo para compartir los ficheros de cada respuesta: no son ese.

```bash
pals grant ls --issued
pals grant revoke <grant_id del grant demo/skill:ask de a>
```

En A:

```bash
pals ask b "hola tras la revocación"; echo $?
curl -sS -i -X POST http://127.0.0.1:7420/peers/b/rest/message:send \
    -H 'A2A-Version: 1.0' -H 'Content-Type: application/json' \
    -d '{"message":{"messageId":"caso6-curl","role":"ROLE_USER","parts":[{"text":"hola"}]}}'
```

**Esperado:** `pals: refused: HTTP 401: revoked`, código 2; `curl` recibe `401 Unauthorized` con `www-authenticate: Bearer realm="agentnet", error="invalid_token", error_description="revoked"` y el cuerpo `{"error":"revoked"}`.

Los casos 7 y 8 necesitan un grant en vigor: en B, `pals grant issue --to a --project demo --ttl 24h`, y en A, `pals grant import '<token>'`. El gateway usa el grant más reciente que tenga para B.

### Caso 7. Blob sin grant

Una tercera identidad, C, en A, con su propio directorio y su gateway en el puerto 7421. Conoce a A y a B como contactos, pero nadie le ha dado un grant. En A:

```bash
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
PALS_HOME=~/.pals-c nohup pals daemon >> ~/pals-salida/daemon-c.log 2>&1 & echo $! > ~/pals-salida/daemon-c.pid
sleep 5
PALS_HOME=~/.pals-c pals contact add '<TICKET_B>' --name b
PALS_HOME=~/.pals-c pals contact add "$(pals id --ticket)" --name a
A=$(pals id); B=$(pals contact ls | awk '$1 == "b" {print $2}')
HASH=$(sed -n 's/^file grande.bin: //p' ~/pals-salida/caso2.out); HASH=${HASH##*/}
curl -sS -i "http://127.0.0.1:7421/blobs/$B/$HASH"     # el fichero del caso 2, que tiene B
curl -sS -i "http://127.0.0.1:7421/blobs/$A/$HASH"     # el mismo, que tiene A
PALS_HOME=~/.pals-c pals ask b hola; echo $?
kill "$(cat ~/pals-salida/daemon-c.pid)"
```

**Esperado:** los dos `curl` reciben `403 Forbidden` con `{"error":"the holder denied the blob"}`; `pals ask` desde C, `pals: refused: HTTP 401: no grant held for <id de B>` con código 2. Un `502` en lugar de `403` significa que C no alcanzó al poseedor: no prueba nada; revisa los contactos de C y repite.

### Caso 8. Cliente estándar

En A, el SDK oficial de Python con las versiones fijadas en el repositorio, contra el gateway de A, con B como par. `tests/interop/python_client.py` hace estas mismas comprobaciones, pero arranca sus propios daemons en la máquina y no puede apuntar a un gateway que ya existe; `caso8.py` las hace contra A y B:

```bash
cd ~/rosepetal-pals             # el clon del paso 1
python3 -m venv ~/pals-salida/venv
~/pals-salida/venv/bin/pip install -r tests/interop/requirements.txt
cat > ~/pals-salida/caso8.py <<'EOF'
"""Caso 8 de la prueba de salida del hito 1: el SDK oficial de Python (a2a-sdk,
fijado en tests/interop/requirements.txt) contra el gateway local de A, con B
como par. B usa el worker pals-prueba.sh.

Uso: python caso8.py <URL del gateway de A> <agent id de B>
"""

import asyncio
import sys

import httpx
from a2a.client import A2ACardResolver, ClientConfig, ClientFactory
from a2a.helpers.proto_helpers import get_artifact_text, new_text_message
from a2a.types import (
    CancelTaskRequest,
    GetTaskRequest,
    Role,
    SendMessageConfiguration,
    SendMessageRequest,
    TaskState,
)

BINDINGS = ['JSONRPC', 'HTTP+JSON']
DONE = TaskState.TASK_STATE_COMPLETED


def expect(condition, what):
    if not condition:
        raise SystemExit(f'FALLO: {what}')


def answer(task):
    return '\n'.join(get_artifact_text(a) for a in task.artifacts).strip()


def message(text, *, now=False):
    msg = new_text_message(text, role=Role.ROLE_USER)
    if now:
        config = SendMessageConfiguration(return_immediately=True)
        return SendMessageRequest(message=msg, configuration=config)
    return SendMessageRequest(message=msg)


async def main(gateway, peer):
    base = f'{gateway}/peers/{peer}'
    async with httpx.AsyncClient(timeout=120.0) as http:
        card = await A2ACardResolver(http, base).get_agent_card()
        found = {i.protocol_binding: i.url for i in card.supported_interfaces}
        expect(found == {'JSONRPC': f'{base}/jsonrpc', 'HTTP+JSON': f'{base}/rest'}, f'interfaces {found}')
        print(f'card de B desde el gateway de A: {card.name!r}, interfaces {", ".join(found)}')
        for binding in BINDINGS:
            def client(streaming):
                config = ClientConfig(
                    streaming=streaming, httpx_client=http, supported_protocol_bindings=[binding]
                )
                return ClientFactory(config).create(card)

            replies = [r async for r in client(False).send_message(message('hola'))]
            expect(len(replies) == 1 and replies[0].HasField('task'), f'{binding}: SendMessage {replies}')
            task = replies[0].task
            expect(task.status.state == DONE, f'{binding}: SendMessage {TaskState.Name(task.status.state)}')
            expect(answer(task) == 'eco: hola', f'{binding}: respuesta {answer(task)!r}')
            print(f'{binding}: SendMessage -> {TaskState.Name(task.status.state)}, {answer(task)!r}')

            events = [e async for e in client(True).send_message(message('hola en streaming'))]
            last = events[-1]
            state = last.status_update.status.state if last.HasField('status_update') else last.task.status.state
            expect(state == DONE, f'{binding}: el stream termina {TaskState.Name(state)}')
            print(f'{binding}: SendStreamingMessage -> {len(events)} eventos, el último {TaskState.Name(state)}')

            got = await client(False).get_task(GetTaskRequest(id=task.id))
            expect(got.id == task.id and got.status.state == DONE, f'{binding}: GetTask {got.status}')
            print(f'{binding}: GetTask -> {TaskState.Name(got.status.state)}')

            replies = [r async for r in client(False).send_message(message('espera 60', now=True))]
            expect(len(replies) == 1 and replies[0].HasField('task'), f'{binding}: tarea larga {replies}')
            long_task = replies[0].task
            canceled = await client(False).cancel_task(CancelTaskRequest(id=long_task.id))
            expect(
                canceled.status.state == TaskState.TASK_STATE_CANCELED,
                f'{binding}: CancelTask {TaskState.Name(canceled.status.state)}',
            )
            print(f'{binding}: CancelTask -> {TaskState.Name(canceled.status.state)}')
    print('caso 8 superado')


if __name__ == '__main__':
    asyncio.run(main(sys.argv[1].rstrip('/'), sys.argv[2]))
EOF
~/pals-salida/venv/bin/python ~/pals-salida/caso8.py http://127.0.0.1:7420 "$(pals contact ls | awk '$1 == "b" {print $2}')"
```

**Esperado:** código 0 y estas líneas, con cada operación por los dos bindings:

```
card de B desde el gateway de A: 'pals-b', interfaces JSONRPC, HTTP+JSON
JSONRPC: SendMessage -> TASK_STATE_COMPLETED, 'eco: hola'
JSONRPC: SendStreamingMessage -> 3 eventos, el último TASK_STATE_COMPLETED
JSONRPC: GetTask -> TASK_STATE_COMPLETED
JSONRPC: CancelTask -> TASK_STATE_CANCELED
HTTP+JSON: SendMessage -> TASK_STATE_COMPLETED, 'eco: hola'
HTTP+JSON: SendStreamingMessage -> 3 eventos, el último TASK_STATE_COMPLETED
HTTP+JSON: GetTask -> TASK_STATE_COMPLETED
HTTP+JSON: CancelTask -> TASK_STATE_CANCELED
caso 8 superado
```

En B, después, `ps -eo args | grep -c '[p]als-prueba'` debe dar `0`: cancelar la tarea larga terminó su orden.

## 5. Registro

En [`docs/conformance/hito-1/exit-test.md`](../conformance/hito-1/exit-test.md), en una sección nueva para la prueba en dos máquinas:

- Fecha y hora (UTC) de inicio y fin.
- Las dos máquinas: sistema, CPU, cómo se conectan (misma red, redes distintas, qué redes) y las versiones: commit de `rosepetal-pals`, `pals --version`, `rustc --version`, `claude --version` en B y `a2a-sdk` en A.
- Por caso: comandos, salida relevante, código de salida y resultado (superado o fallido). En el caso 3, el camino observado en cada repetición y cómo se dedujo.
- Cualquier desviación de este runbook y cualquier anomalía, aunque el caso pase.

Un caso que falla es un defecto: se corrige, se repite el caso y se anota todo. El hito no se cierra con casos abiertos.

## 6. Limpieza

```bash
# En A y en B
para
# En B: ninguna orden viva
ps -eo pid,pgid,args | grep '[p]als-prueba'
# En A: ninguna regla de los cortes (reglas_como_antes, del caso 3)
reglas_como_antes
rm -rf ~/.pals-c
```

`~/.pals` conserva la identidad, los contactos, los grants y el almacén de blobs de cada máquina; bórralo solo si no vas a repetir la prueba. Los ficheros de prueba ocupan unos 2 GiB en `~/pals-salida` y en los almacenes de blobs: los blobs subidos o compartidos se conservan sin límite de tiempo en el hito 1.
