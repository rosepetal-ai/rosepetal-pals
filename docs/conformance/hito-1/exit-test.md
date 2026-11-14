# Prueba de salida del hito 1: ensayo en una sola máquina

**Esto no es la prueba de salida del hito 1.** Es un ensayo en una sola máquina: dos daemons `pals` (y un tercero para el caso 7) en directorios distintos que se hablan por loopback, sin relay, con los casos del criterio de salida que se pueden simular así. La prueba real, en dos máquinas, está **pendiente**: la hará el responsable del proyecto siguiendo el runbook [`docs/runbooks/hito-1-dos-maquinas.md`](../../runbooks/hito-1-dos-maquinas.md), y sus resultados se añadirán a este documento. Hasta entonces, el criterio de salida del hito 1 no está cumplido.

El ensayo sirve para comprobar, antes de la prueba real, que el binario se comporta como espera cada caso y que el runbook funciona; no sustituye a una red real, a un relay, a NAT ni a Claude Code.

## Resumen

| Caso | Ensayo | Resultado | Qué no cubre el ensayo |
|-|-|-|-|
| 1. Pregunta útil | No ensayado con `claude`: necesitaría las credenciales del responsable. Ensayado el mismo camino con un sustituto de `claude` | Sustituto: superado | Claude Code y su respuesta sobre un proyecto real |
| 2. Transferencia autorizada | Ensayado: 256 MiB de A a B y vuelta, y 256 MiB nuevos de B a A | Superado | Red real; ancho de banda |
| 3. Interrupción de transferencia | Ensayado por camino directo de loopback, con el corte de red simulado congelando 10 s el daemon del otro extremo, en los dos sentidos | Superado | Corte real de red; camino por relay; qué camino informa Iroh |
| 4. Reintento sin duplicar | Ensayado, con el corte simulado congelando 45 s el daemon de B y, en otra ejecución, el de A | Superado: un `task.accepted` en cada ejecución | Corte real de red |
| 5. Caída del proveedor | Ensayado (dos veces) | Superado | — |
| 6. Revocación | Ensayado | Superado | — |
| 7. Blob sin grant | Ensayado con una tercera identidad | Superado | — |
| 8. Cliente estándar | Ensayado con el SDK oficial de Python contra el gateway de A, con B como par | Superado | — |

Ningún caso ensayado falló. Las observaciones del final no son fallos de los casos, pero conviene tenerlas en cuenta en la prueba real.

## Ejecución

| Dato | Valor |
|-|-|
| Fecha | 2026-11-14, de 10:38 a 10:57 UTC |
| Máquina | Una sola máquina de desarrollo: Linux x86_64, Ubuntu 24.04 en un contenedor Docker con la red del anfitrión, kernel 6.12, 32 CPU |
| Commit de `pals` | `d80afac` (el código; este documento llega en el commit siguiente, sin tocar código) |
| Binario | `cargo build --release --locked -p pals --bin pals`: `pals 0.1.0` |
| Rust | 1.99.1 |
| Versiones fijadas | `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`, `rmcp =3.10.0` |
| Cliente del caso 8 | `a2a-sdk==1.3.0` con las dependencias de [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt), Python 3.12.3 |
| Red | Iroh solo en 127.0.0.1, sin relay (`net.relay = false`) |

Los registros completos (cada orden con su salida y los registros de los daemons) quedaron en la copia de trabajo de quien ensayó, sin versionar; aquí se copia lo que respalda cada resultado. `$W` es el directorio del ensayo, `target/exit-rehearsal/`.

## Montaje

Tres directorios de PALS, cada uno con su `config.toml`, su clave y su almacén: `$W/a` (A, quien pregunta, worker `echo`, gateway en `127.0.0.1:17420`), `$W/b` (B, quien responde, gateway en `17421`) y `$W/c` (C, la tercera identidad del caso 7, gateway en `17422`). Todos con:

```toml
[net]
relay = false
```

Las órdenes se escriben con estas funciones de ayuda; `start` arranca un daemon en segundo plano, guarda su PID y espera a que su gateway dé el ticket:

```bash
pa() { PALS_HOME=$W/a pals "$@"; }     # y pb, pc para B y C
start() {   # start NOMBRE [VARIABLE=valor…]
    local name=$1; shift
    PALS_HOME=$W/$name setsid env "$@" pals daemon >>daemon-$name.log 2>&1 </dev/null &
    echo $! > $W/$name.pid
    until PALS_HOME=$W/$name pals id --ticket > $W/$name.ticket 2>/dev/null; do sleep 0.5; done
}
stop() {   # SIGTERM, y espera hasta 15 s a que el daemon termine
    local pid; pid=$(cat $W/$1.pid)
    kill "$pid"
    for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.25; done
    echo "el daemon $1 sigue vivo"; return 1
}
```

Contactos en los dos sentidos y un grant de B para A, como en el runbook:

```text
$ pa contact add "$(cat $W/b.ticket)" --name b
saved contact b: 4bcef594ede3eacca3606059099ffafd9b71d6f609ff8264d0a8b70196d23f44
$ pb contact add "$(cat $W/a.ticket)" --name a
saved contact a: b552fcba6402681c6cb8504cbfa17d22af808806618c4bfc5c368624fb4ef404
$ pb grant issue --to a --project demo --ttl 24h
issued grant g-305ad7a6bc549161e22a2f603ce485ee to a: project demo, task.send,task.read,task.cancel on skill:ask, until 2026-11-15T10:38:37Z
$ pa grant import "$TOKEN"
imported grant g-305ad7a6bc549161e22a2f603ce485ee from 4bcef594…: project demo, until 2026-11-15T10:38:37Z
```

**Los cortes de red se simulan.** El contenedor comparte la red del anfitrión y no tiene `CAP_NET_ADMIN`: no se puede usar `iptables`, `tc` ni un espacio de red propio sin tocar la red de la máquina. En su lugar se congela un daemon con `kill -STOP` y se descongela con `kill -CONT`. Para el otro extremo es lo mismo que un corte: el congelado ni envía ni contesta. No es igual en el extremo congelado: sus temporizadores no corren durante la congelación, mientras que en un corte real sí corren. Por eso los casos 3 y 4 se ensayan congelando unas veces un extremo y otras el otro.

## Caso 1. Pregunta útil: no ensayado con `claude`

La configuración de B es la del runbook para el caso 1 (`adapter = "exec"`, `command = "claude"`, `args = ["-p", "--output-format", "text"]`, proyecto `demo` en el propio repositorio), pero con `relay = false` y con un **sustituto** de `claude` en el `PATH` del daemon: un guion que no llama a ningún modelo y responde con datos del proyecto, que es su directorio de trabajo. El daemon de B se arrancó con `ANTHROPIC_API_KEY=placeholder-not-a-key` en su entorno para comprobar que no llega a la orden. En la salida, la ruta de la máquina se abrevia con `…` y el valor de `HOME` se reduce a si está o no.

```text
$ pa ask b "¿De qué trata este proyecto?"
sustituto de claude, argumentos: -p --output-format text
pregunta: ¿De qué trata este proyecto?
directorio de trabajo: …/rosepetal-pals
ANTHROPIC_API_KEY en el entorno: no
HOME en el entorno: sí
primera línea de README.md: # PALS — Peer Agent Linking System
Cargo.toml declara: members = ["crates/pals-core", "crates/pals"]
exit=0
```

Queda comprobado el camino: la pregunta llega por stdin, la orden corre en el directorio del proyecto del grant con los argumentos configurados, el entorno pierde `ANTHROPIC_API_KEY` y conserva `HOME`, y la respuesta vuelve a A. **No queda comprobado** que Claude Code responda bien sobre un proyecto real: es el caso 1 de la prueba en dos máquinas.

## Paso intermedio: B cambia al worker de pruebas

Como en el runbook, B se para, cambia su `[worker]` a `command = "sh"`, `args = ["$W/pals-prueba.sh"]`, `timeout_s = 900` y arranca otra vez. El guion `pals-prueba.sh` es el del runbook, idéntico byte a byte; su rama `genera` se añadió durante el ensayo, tras el caso 6, al ver que la vuelta del caso 2 no cruzaba la red (observación 2); las partes de B a A de los casos 2 y 3 se ensayaron después.

```text
$ pa ask b hola
pals: b could not be reached; the question is kept: `pals task resume 9a072660-0fa5-42b0-8d59-c7bcfd117d03` tries again
exit=3
$ pa contact add "$(cat $W/b.ticket)" --name b
$ pa task resume 9a072660-0fa5-42b0-8d59-c7bcfd117d03
eco: hola
exit=0
```

Con `relay = false`, B se enlaza a un puerto nuevo en cada arranque y el ticket que A guardó deja de valer (observación 1). Con el ticket nuevo, `pals task resume` envía la pregunta guardada y termina; `pals task ls` muestra esa pregunta `TASK_STATE_COMPLETED` con 5 intentos.

## Caso 2. Transferencia autorizada: superado

```text
$ head -c 256M /dev/urandom > $W/files/grande.bin
$ pa ask b devuelve --attach $W/files/grande.bin | tee $W/caso2.out
eff17a369f4aa8616f5137fd86b7a0550f9febe812a8d253920c2208532cbf33  grande.bin
file grande.bin: http://127.0.0.1:17420/blobs/4bcef594ede3eacca3606059099ffafd9b71d6f609ff8264d0a8b70196d23f44/bd7b8d4e0a7a4f0c44d96d513f0047720c34aa6ffe75c3cb1362c9fd2ea1d5f6
exit=0 (11085 ms)
$ curl -sS -o $W/files/devuelto.bin -w "%{http_code} %{size_download}\n" "$URL"
200 268435456
$ sha256sum $W/files/grande.bin $W/files/devuelto.bin
eff17a369f4aa8616f5137fd86b7a0550f9febe812a8d253920c2208532cbf33  $W/files/grande.bin
eff17a369f4aa8616f5137fd86b7a0550f9febe812a8d253920c2208532cbf33  $W/files/devuelto.bin
```

B recibió el fichero con el mismo `sha256` y lo devolvió como artefacto, y el fichero descargado es idéntico. La vuelta no cruzó la red: el gateway de A ya tenía ese blob completo (lo subió él) y lo sirvió de su almacén (observación 2). El sentido de B a A, con un fichero nuevo que crea B:

```text
$ pa ask b "genera 256" | tee $W/caso2b.out
bbb979375e6337149034fbc7228ee02ed513e702300b4f3d7e6a1f8ede7349fc  generado.bin
file generado.bin: http://127.0.0.1:17420/blobs/4bcef594…/d7219386270da185da78a3abc04dfe7799aaa2b10d9c2a190368938b17755004
exit=0 (2888 ms)
$ curl -sS -o $W/files/generado.bin -w "%{http_code} %{size_download}\n" "$URL"
200 268435456
(2787 ms)
$ sha256sum $W/files/generado.bin
bbb979375e6337149034fbc7228ee02ed513e702300b4f3d7e6a1f8ede7349fc  $W/files/generado.bin
```

## Caso 3. Interrupción de transferencia: superado en su forma de una máquina

**De A a B** (B descarga de A el fichero adjunto, un fichero aleatorio nuevo para que B no lo tenga ya). Cuando la copia parcial de B pasa de 32 MiB, se congela el daemon de A 10 s:

```text
[10:40:41.539] $ pa ask b devuelve --attach $W/files/grande2.bin  (en segundo plano)
[10:40:48.900] B descarga d3f602c7….data: 34799616 bytes (7363 ms desde el ask)
[10:40:48.905] kill -STOP <daemon A>  (A congelado)
[10:40:50.911] durante el corte: 35291136 bytes
[10:40:58.924] durante el corte: 35291136 bytes
[10:40:58.931] kill -CONT <daemon A>  (A vuelve)
[10:41:05.843] pals ask terminó: exit=0 (24315 ms desde el ask)
0a3499c3bf875b781430ab7b506e072eca89f8016cff02f61e7471b086d7916b  grande2.bin
$ sha256sum $W/files/grande2.bin $W/files/devuelto2.bin
0a3499c3bf875b781430ab7b506e072eca89f8016cff02f61e7471b086d7916b  $W/files/grande2.bin
0a3499c3bf875b781430ab7b506e072eca89f8016cff02f61e7471b086d7916b  $W/files/devuelto2.bin
```

**De B a A** (el gateway de A descarga de B un fichero que B acaba de generar). Cuando la copia parcial de A pasa de 32 MiB, se congela el daemon de B 10 s:

```text
[10:49:15.307] $ curl -sS -o $W/files/generado2.bin "$URL"  (en segundo plano)
[10:49:15.722] A descarga 33fee303….data: 38633472 bytes (417 ms)
[10:49:15.729] kill -STOP <daemon B>  (B congelado)
[10:49:17.735] durante el corte: 39419904 bytes
[10:49:25.772] durante el corte: 39419904 bytes
[10:49:25.777] kill -CONT <daemon B>  (B vuelve)
[10:49:29.461] curl terminó: exit=0, 200 268435456 (14153 ms)
$ sha256sum $W/files/generado2.bin
79cb2b011b0f7daf1cd1b0f3f00d5e9113efd63a0ce2c4f261599797c7c74021   (B escribió 79cb2b01… en su respuesta)
```

En los dos sentidos la descarga se para durante el corte, sigue en la misma conexión (10 s quedan por debajo de los 15 s de inactividad de un camino de Iroh y de los 30 s de la conexión) y termina con el hash correcto. Además, el guion `caso3.sh` del runbook (modo `bajada`), extraído tal cual del documento, se ejecutó contra los daemons del ensayo con dos sustitutos: `sudo` sin efecto y un `corte-red.sh` que congela el daemon de A en lugar de usar `iptables`. Superado: `durante el corte: 39354368 bytes` dos veces, `curl: código 0, 200 268435456` y el `sha256` que B escribió.

**No ensayado:** un corte real de red (el `corte-red.sh` del runbook, con `iptables`, no se ha podido ejecutar aquí), el modo `subida` de `caso3.sh` (en loopback cuenta también la subida local al gateway) y el camino por relay. Tampoco se puede anotar qué camino informa Iroh: PALS no lo muestra (observación 6); el runbook lo deduce de los bytes de las conexiones del daemon a los relays.

## Caso 4. Reintento sin duplicar: superado

Una tarea de 90 s; a los 6 s se congela 45 s un daemon, más que los 30 s tras los que Iroh da la conexión por perdida. El guion `caso4-eventos.py` del runbook cuenta en el almacén de B las tareas y los eventos `task.accepted` del `messageId`.

**4a, congelando B** (A sigue vivo y reintenta, como en un corte real visto desde A):

```text
[10:41:27.282] $ pa ask b 'espera 90'  (en segundo plano)
[10:41:33.304] $ pa task ls | tail -1
b1dc886a-d23b-4b5e-b9f4-3ae7c6d8f345	pending	b	01a1d9a5-d92d-747d-aaf9-1f3af33da91c	1
[10:41:33.311] kill -STOP <daemon B>  (B congelado; su 'sleep 90' sigue)
[10:42:18.319] kill -CONT <daemon B>  (B vuelve)
[10:42:57.611] pals ask terminó: exit=0 (90334 ms)
he esperado 90 s
$ pa task ls | tail -1
b1dc886a-d23b-4b5e-b9f4-3ae7c6d8f345	TASK_STATE_COMPLETED	b	01a1d9a5-d92d-747d-aaf9-1f3af33da91c	2
$ python3 caso4-eventos.py $W/b/pals.db $MSG
messageId b1dc886a-d23b-4b5e-b9f4-3ae7c6d8f345
  filas de deduplicación: 1 -> tarea ['01a1d9a5-d92d-747d-aaf9-1f3af33da91c']
  tareas que lo contienen: 1 -> ['01a1d9a5-d92d-747d-aaf9-1f3af33da91c']
  eventos task.accepted de esas tareas: 1
```

**4b, congelando A** (B sigue vivo, como en un corte real visto desde B):

```text
[10:43:11.207] 51141cc9-761b-4823-b486-f516f6bf53ac	pending	b	01a1d9a7-57d1-77c1-8357-c0354d206341	1
[10:43:11.227] kill -STOP <daemon A>  (A congelado)
[10:43:56.232] kill -CONT <daemon A>  (A vuelve)
[10:44:35.918] pals ask terminó: exit=0 (90719 ms)
he esperado 90 s
51141cc9-761b-4823-b486-f516f6bf53ac	TASK_STATE_COMPLETED	b	01a1d9a7-57d1-77c1-8357-c0354d206341	2
  filas de deduplicación: 1 -> tarea ['01a1d9a7-57d1-77c1-8357-c0354d206341']
  tareas que lo contienen: 1 -> ['01a1d9a7-57d1-77c1-8357-c0354d206341']
  eventos task.accepted de esas tareas: 1
```

En las dos, `pals ask` reconecta (segundo intento), termina con la respuesta a los 90 s de la tarea y B ejecutó la tarea una sola vez.

## Caso 5. Caída del proveedor: superado

Se ejecutó dos veces; la segunda, con el listado del grupo de procesos corregido (en la primera, `ps -g` filtraba por sesión y no listó nada; las dos dieron el mismo resultado). La segunda:

```text
[10:46:26.859] $ pa ask b 'espera 120'  (en segundo plano)
[10:46:32.869] 82a071e1-26e8-4c6c-9d8a-341fd7ce23a1	pending	b	01a1d9aa-6b8c-7176-bfbe-f581b7b8e663	1
[10:46:32.903] grupo del comando antes del kill:
2267099 2267099       00:05 sh $W/pals-prueba.sh
2267101 2267099       00:05 sleep 120
[10:46:32.920] kill -9 <daemon B>
[10:46:33.924] grupo del comando después del kill -9:
2267099 2267099       00:06 sh $W/pals-prueba.sh
2267101 2267099       00:06 sleep 120
[10:47:44.409] pals ask terminó: exit=3 (77551 ms)
pals: b could not be reached; the question is kept: `pals task resume 82a071e1-26e8-4c6c-9d8a-341fd7ce23a1` tries again
[10:47:44.422] $ start b
$ pa contact add "$(cat $W/b.ticket)" --name b
[10:47:44.964] $ pa task resume 82a071e1-26e8-4c6c-9d8a-341fd7ce23a1
pals: task 01a1d9aa-6b8c-7176-bfbe-f581b7b8e663 ended TASK_STATE_FAILED: interrupted: daemon restarted
exit=2
[10:47:45.015] $ pa ask b 'hola tras el rearranque'
eco: hola tras el rearranque
exit=0
$ python3 caso4-eventos.py $W/b/pals.db $MSG
  eventos task.accepted de esas tareas: 1
[10:47:45.159] grupo del comando con B ya rearrancado:
2267099 2267099       01:18 sh $W/pals-prueba.sh
2267101 2267099       01:18 sleep 120
[10:47:45.194] $ kill -- -2267099
```

A recibe `TASK_STATE_FAILED` con `interrupted: daemon restarted`, la tarea no se ejecuta de nuevo y una pregunta nueva funciona tras el rearranque. Se confirma la limitación conocida: la orden que corría (`sh` y su `sleep 120`) sobrevive al `kill -9` del daemon y a su rearranque, hasta que se mata a mano.

## Caso 6. Revocación: superado

```text
$ pb grant ls --issued | awk '/project demo .* on skill:ask .* to a$/ {print $1}'
g-305ad7a6bc549161e22a2f603ce485ee
$ pb grant revoke g-305ad7a6bc549161e22a2f603ce485ee
revoked grant g-305ad7a6bc549161e22a2f603ce485ee
$ pa ask b "hola tras la revocación"
pals: refused: HTTP 401: revoked
exit=2
$ curl -sS -i -X POST http://127.0.0.1:$PA/peers/b/rest/message:send -H "A2A-Version: 1.0" -H "Content-Type: application/json" -d '{"message":{"messageId":"caso6-curl-2","role":"ROLE_USER","parts":[{"text":"hola"}]}}'
HTTP/1.1 401 Unauthorized
content-type: application/json
www-authenticate: Bearer realm="agentnet", error="invalid_token", error_description="revoked"
{"error":"revoked"}
```

Un primer intento revocó por error del guion de ensayo otro grant: `pals grant ls --issued` lista también los grants `project blobs … blob.fetch` que PALS emite para compartir los ficheros de cada respuesta, y el filtro tomó el primero «to a» (observación 3). Ese `ask` respondió, como debía, porque el grant de tareas seguía en vigor. Tras el caso, B emitió un grant nuevo y A lo importó para seguir; el gateway usó el más reciente.

## Caso 7. Blob sin grant: superado

C se inicializa con su propio directorio, guarda los tickets de A y de B y no tiene ningún grant. Pide el fichero del caso 2 a sus dos poseedores, y el que B generó en el caso 2:

```text
$ pc grant ls
issued:
held:
$ curl -sS -i http://127.0.0.1:$PC/blobs/<B>/bd7b8d4e…   # fichero del caso 2 (lo tiene B)
HTTP/1.1 403 Forbidden
{"error":"the holder denied the blob"}
$ curl -sS -i http://127.0.0.1:$PC/blobs/<A>/bd7b8d4e…   # fichero del caso 2 (lo tiene A, que lo subió)
HTTP/1.1 403 Forbidden
{"error":"the holder denied the blob"}
$ curl -sS -i http://127.0.0.1:$PC/blobs/<B>/d7219386…   # fichero del caso 2, de B a A
HTTP/1.1 403 Forbidden
{"error":"the holder denied the blob"}
$ pc ask b "hola desde C"
pals: refused: HTTP 401: no grant held for 4bcef594ede3eacca3606059099ffafd9b71d6f609ff8264d0a8b70196d23f44
exit=2
$ ls $W/c/blobs/data
(vacío)
```

## Caso 8. Cliente estándar: superado

`tests/interop/python_client.py` arranca sus propios daemons y no puede apuntar a un gateway que ya existe (observación 7). El caso se ensayó con `caso8.py` del runbook, que hace las mismas comprobaciones con el mismo SDK contra el gateway de A, con B como par:

```text
$ target/interop-venv/bin/python caso8.py http://127.0.0.1:$PA $(pb id)
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
exit=0
```

Después no quedaba viva ninguna orden de `pals-prueba.sh`: cancelar las dos tareas `espera 60` terminó sus órdenes.

Comprobación adicional para el `README.md`: el endpoint MCP del gateway de A (`/mcp`) responde a `initialize` con `serverInfo` `pals 0.1.0` y la capacidad `tools`.

## Observaciones del ensayo

1. **Con `net.relay = false`, el puerto de Iroh cambia en cada arranque** y los tickets que guardaron los pares dejan de valer: tras cada rearranque de B (el cambio de worker y el caso 5), A no lo alcanzó hasta volver a guardar su ticket. Con `relay = true`, el ticket incluye el relay y el par se busca por su id; la prueba en dos máquinas debe confirmarlo en el paso intermedio y en el caso 5.
2. **El gateway sirve un blob que ya tiene completo desde su propio almacén**, sin contactar con el poseedor que nombra la URL. Por eso, en el caso 2, la vuelta del fichero idéntico no cruza la red, y el runbook comprueba el sentido de B a A con un fichero nuevo. Por lo mismo, tras revocar por error el grant de blob de un fichero que A había subido, A siguió descargándolo (`200`) de su propia copia.
3. **`pals grant ls --issued` mezcla los grants de tareas con los de blobs** (`project blobs … blob.fetch`, uno por fichero compartido). Para revocar el acceso de un par a las tareas hay que elegir el de `skill:ask`.
4. **`kill -9` del daemon deja viva la orden en curso** (limitación conocida, caso 5).
5. **Con el par caído, `pals ask` tarda algo más de un minuto en rendirse** (77 s en las dos ejecuciones del caso 5): la conexión abierta tarda hasta 30 s en darse por perdida, y después vienen tres reintentos con esperas de 1, 2 y 4 s, cada uno con un intento de conexión de hasta 10 s.
6. **El registro del daemon solo tiene las líneas de arranque y parada.** El daemon no instala un suscriptor de `tracing`, así que no informa del camino de Iroh (directo o relay) ni de los reintentos. El runbook deduce el camino de las conexiones del daemon a los relays.
7. **`tests/interop/python_client.py` no sirve tal cual para el caso 8 en dos máquinas**: siempre arranca sus propios daemons locales. El runbook usa `caso8.py`.

## Prueba en dos máquinas

**Pendiente del responsable del proyecto.** Se ejecuta con el runbook [`docs/runbooks/hito-1-dos-maquinas.md`](../../runbooks/hito-1-dos-maquinas.md) y se registra aquí con el formato de su sección 5: fecha, máquinas, versiones, camino de red observado en cada repetición del caso 3 y resultado de cada caso.
