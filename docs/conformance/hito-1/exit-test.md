# Prueba de salida del hito 1

Este documento recoge dos ejecuciones de los casos del criterio de salida del hito 1, siguiendo el runbook [`docs/runbooks/hito-1-dos-maquinas.md`](../../runbooks/hito-1-dos-maquinas.md):

1. **[Ejecución con Docker en el papel de la segunda máquina](#ejecución-con-docker-en-el-papel-de-la-segunda-máquina-2026-11-14)** (2026-11-14, por la tarde). Los ocho casos, con red real entre dos espacios de red, cortes de red reales con `iptables`, los relays públicos de Iroh y Claude Code en el caso 1. La ejecuta el arnés [`tests/exit/`](../../../tests/exit/README.md).
2. **[El ensayo anterior en una sola máquina](#ensayo-anterior-en-una-sola-máquina-2026-11-14-1038-a-1057-utc)** (2026-11-14, por la mañana): loopback, sin relay y con los cortes simulados congelando un daemon.

**Ninguna de las dos es la prueba en dos máquinas físicas.** En la ejecución con Docker, A y B comparten kernel, reloj e IP pública, y el camino por relay se fuerza descartando UDP (ver [«Lo que no cubre»](#lo-que-no-cubre)). La ejecución con Docker cierra el hito 1, y repetirla en dos máquinas, en redes distintas, es un extra opcional ([registro de decisión](../../decisions/2026-11-16-cierre-hitos-1-y-2.md#d2-las-pruebas-de-salida-con-docker-cierran-los-hitos-1-y-2), 2026-11-16).

## Ejecución con Docker en el papel de la segunda máquina (2026-11-14)

Los ocho casos, el paso intermedio y la limpieza del runbook, superados. El caso 3, por camino directo y por relay, en los dos sentidos. Ningún caso falló; quedan dos tiempos sin explicar (observaciones 3 y 4). Aparecieron seis fallos del runbook, corregidos antes de esta ejecución (ver [«Correcciones al runbook»](#correcciones-al-runbook)).

| Caso | Resultado | Camino | Evidencia | Tiempo |
|-|-|-|-|-|
| 1. Pregunta útil | Superado | Directo | `claude -p` en B responde citando `README.md`, `Cargo.toml`, `crates/pals/src/config.rs`, la CI y `tests/interop/requirements.txt`, con líneas exactas; código 0 | 40 s |
| Paso intermedio | Superado | Sin medir | `eco: hola`, código 0, sin volver a guardar el ticket de B | < 1 s |
| 2. Transferencia autorizada | Superado | Directo | De A a B y vuelta: el mismo sha256 (`3b212fc4…`) enviado, recibido por B y devuelto; `200 268435456`. De B a A: `906aff5c…` en B y en A | 72 s y 2 s |
| 3a. Corte de 10 s, camino directo | Superado, `bajada` y `subida` | Directo | Parada durante el corte (36306944 y 34372051 bytes), sigue después; hash correcto | 20 s y 32 s |
| 3b. Corte de 10 s, por relay | Superado, `bajada` y `subida` | Relay (UDP saliente de A descartado) | Parada durante el corte (33652736 y 33572829 bytes), sigue después; hash correcto; los 256 MiB pasan por TCP 443 (282,6 MB recibidos en `bajada`, 282,1 MB enviados en `subida`) | 1073 s y 1069 s |
| 4. Reintento sin duplicar | Superado | Directo | Corte de 45 s; `he esperado 120 s`, código 0, 3 intentos; **un** `task.accepted` | 120 s |
| 5. Caída del proveedor | Superado | Directo¹ | `pals ask` termina con código 3; `resume`: `TASK_STATE_FAILED: interrupted: daemon restarted`, código 2; un `task.accepted`; pregunta nueva, código 0 | 95 s |
| 6. Revocación | Superado | Directo¹ | `pals: refused: HTTP 401: revoked`, código 2; `curl`: `401` con `error_description="revoked"` | < 1 s |
| 7. Blob sin grant | Superado | Directo¹ | Tres `403 {"error":"the holder denied the blob"}`; `no grant held for <B>`, código 2 | 5 s |
| 8. Cliente estándar | Superado | Directo¹ | Las cuatro operaciones por los dos bindings con el SDK oficial de Python; ninguna orden viva en B | 11 s |
| Limpieza | Superada | | `reglas del cortafuegos como antes`; ninguna orden viva en B | |

¹ Medido en una ejecución parcial posterior (ver [«Camino de los casos 5 a 8»](#camino-de-los-casos-5-a-8)).

### Entorno

| Dato | Valor |
|-|-|
| Fecha | 2026-11-14, de 16:11:52 a 16:54:55 UTC |
| Commit | `040bc01` (el arnés y el runbook corregido; el código es el de `5d4db29`, sin cambios) |
| Binario | `cargo build --release --locked -p pals` en el anfitrión: `pals 0.1.0`, el mismo fichero en A y en B (sha256 `7728b570…`) |
| Rust | 1.99.1 |
| Versiones fijadas | `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`, `rmcp =3.10.0` |
| Anfitrión | Linux x86_64, kernel 6.12 de Debian 13, 32 CPU, compartido con otros proyectos. Carga media al empezar: 19,3 / 19,0 / 18,5; al terminar: 12,2 / 17,2 / 16,9 |
| Docker | Motor 29.4.1 |
| A | Contenedor `debian:trixie-slim` ([`tests/exit/Dockerfile`](../../../tests/exit/Dockerfile)) en su propia red bridge de Docker, con NAT hacia fuera; `--cap-add NET_ADMIN` e `--init`. Python 3.13.5 y `a2a-sdk==1.3.0` con las dependencias de [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt). Trabaja como root, sin `sudo` |
| B | El entorno de desarrollo: Ubuntu 24.04.4 en un contenedor que usa la red del anfitrión (sin espacio de red propio), fuera de la red de A. `PALS_HOME` propio en un directorio temporal, gateway en `127.0.0.1:17420`. Claude Code 2.1.325, con la sesión del usuario que ejecuta el daemon |
| C (caso 7) | Un tercer daemon dentro de A, gateway en `127.0.0.1:7421` |
| Relays | `net.relay = true` en los tres daemons: los relays públicos de n0. A y B eligieron como relay propio `euc1-1.relay.n0.iroh.link`, y los dos lo alcanzaban por HTTPS antes de empezar (`200` en 0,27 s desde A y 0,24 s desde B) |

El registro completo de la ejecución quedó en la máquina de quien la hizo, sin versionar. Aquí se copia lo que respalda cada resultado. Los ids de agente se escriben `<A>`, `<B>` y `<C>`. Los hashes y los `messageId` se acortan a sus 8 primeros caracteres. No se copian tickets ni tokens.

### Montaje y camino de red

El arnés construye la imagen, crea la red y el contenedor, copia el binario y el árbol del repositorio en A y extrae del runbook sus guiones (`funciones.sh`, `pals-prueba.sh`, `corte-red.sh`, `caso3.sh`, `caso4-eventos.py` y `caso8.py`). A sigue el runbook al pie de la letra (`~/.pals`, `~/pals-salida`, `arranca`, `para`); cada orden en A es una terminal nueva que carga `funciones.sh`. B ejecuta las mismas órdenes con su `PALS_HOME`. Los tickets y los tokens de los grants pasan de una máquina a otra sin imprimirse. Contactos y grants en los dos sentidos, como en el paso 3:

```text
[16:12:14] En B: pals ask a hola -> echo: hola (código 0)
issued:
  g-…  active  project demo  task.send,task.read,task.cancel on skill:ask  until 2026-11-15T16:12:14Z  to b
held:
  g-…  active  project demo  task.send,task.read,task.cancel on skill:ask  until 2026-11-15T16:12:14Z  from b
```

**Cómo se determinó el camino.** PALS no informa del camino de Iroh. El arnés añade al final de las cadenas de A cuatro reglas de `iptables` que solo cuentan bytes por `eth0`: UDP salvo DNS (el camino directo, más el descubrimiento de direcciones de Iroh) y TCP 443 (los relays), en cada sentido. Antes y después de cada caso compara esos contadores, y llama «directo» al caso en que el UDP supera cuatro veces al TCP 443, y «relay» al contrario. En el caso 3 se añade la medida del runbook: las líneas `relay:` de `caso3.sh`, con los bytes de las conexiones TCP 443 del propio daemon de A. En los casos con mensajes cortos, los dos contadores llevan además el ruido de fondo de Iroh, del orden de decenas de KB.

### Caso 1. Pregunta útil: superado

B responde con `claude -p --output-format text` (la configuración del paso 2, sin `--allowedTools`). Su proyecto `demo` es una copia del propio repositorio de PALS en `040bc01`. La comprobación del paso 2 (`claude -p` con el entorno del worker, en el proyecto) respondió en una frase. La pregunta del runbook, desde A:

```text
$ time pals ask b "Lee el README y los ficheros de configuración de este proyecto. ¿Cómo se arranca y qué dependencias principales tiene? Cita los ficheros."
PALS es un único binario Rust, `pals`, que hace a la vez de daemon y de CLI. Se compila con Cargo, se inicializa con `pals init` y se arranca con `pals daemon`.
## Cómo se arranca
**1. Requisitos e instalación.** Hace falta Rust 1.91 o posterior, edición 2024 (`README.md:32`; `Cargo.toml:7-8`):
…
**4. Configuración.** `pals init` escribe la plantilla `INIT_TEMPLATE` (`crates/pals/src/config.rs:29-85`), con todas las opciones en su valor por defecto:
…
## Dependencias principales
Es un workspace con dos crates: `crates/pals-core`, sin red, y `crates/pals`, con el daemon y la CLI (`Cargo.toml:3`). Las versiones críticas están fijadas con `=`, y subir cualquiera obliga a repetir el TCK (`Cargo.toml:13-23`; `README.md:187`).
| Área | Crate y versión | Dónde |
|-|-|-|
| Red P2P (QUIC) | `iroh =1.4.1`, `iroh-tickets =1.0.0` | `Cargo.toml:15,19` |
| Ficheros | `iroh-blobs =0.105.0` (solo `fs-store`) | `Cargo.toml:18` |
| Protocolo A2A (SDK oficial) | `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9` | `Cargo.toml:20-22` |
…
- **CI** (`.github/workflows/ci.yml`): compila con el toolchain fijado 1.99.1 (fmt, clippy, tests) y además hace `cargo check` con la MSRV 1.91 (líneas 22-44). El repo no tiene `rust-toolchain.toml`.
- **Conformidad:** usa el SDK oficial de Python `a2a-sdk==1.3.0` con todas sus dependencias fijadas (`tests/interop/requirements.txt:4`). Solo hace falta para `tests/interop/python_client.py`, no para ejecutar `pals`.
Para responder solo he leído los ficheros; no he compilado ni arrancado el daemon.
real	0m40.000s
código 0
```

La respuesta, de 56 líneas (41 con texto), cita ficheros y líneas del proyecto. Se comprobaron a mano algunas citas, y son exactas: las versiones fijadas en `Cargo.toml:15-23`, el arranque en `README.md:43-47`, `INIT_TEMPLATE` en `crates/pals/src/config.rs:29`, `a2a-sdk==1.3.0` en `tests/interop/requirements.txt:4` y la ausencia de `rust-toolchain.toml`. `claude` pudo leer ficheros sin `--allowedTools`. Fueron dos llamadas a `claude -p` en total: la comprobación y la pregunta. Camino: directo (UDP: 309 KB enviados y 345 KB recibidos por A; TCP 443: 18 KB y 20 KB).

### Paso intermedio: B cambia al worker de pruebas: superado

B se paró, cambió su `[worker]` al guion `pals-prueba.sh` del runbook y arrancó otra vez, con un puerto de Iroh nuevo. A lo alcanzó sin volver a guardar su ticket: con `relay = true`, A busca a B por su id. Es la observación 1 del ensayo anterior, ya resuelta.

```text
[16:13:01] En A: pals ask b hola -> eco: hola (código 0)
```

### Caso 2. Transferencia autorizada: superado

De A a B y vuelta, y de B a A, por camino directo:

```text
$ head -c 256M /dev/urandom > grande.bin; sha256sum grande.bin
3b212fc4…  grande.bin
$ pals ask b devuelve --attach grande.bin | tee caso2.out
3b212fc4…  grande.bin                                  (el sha256 que calculó B)
file grande.bin: http://127.0.0.1:7420/blobs/<B>/eda00e39…
código 0                                               (70236 ms)
$ curl -sS -o devuelto.bin -w '%{http_code} %{size_download}\n' "$URL"
200 268435456
$ sha256sum grande.bin devuelto.bin
3b212fc4…  grande.bin
3b212fc4…  devuelto.bin
camino: directo; por eth0 de A, UDP 277,4 MB enviados, 1,0 MB recibidos; TCP 443 10 KB y 12 KB

$ pals ask b "genera 256" | tee caso2b.out
906aff5c…  generado.bin
file generado.bin: http://127.0.0.1:7420/blobs/<B>/6c8f84a6…
código 0
$ curl -sS -o generado.bin -w '%{http_code} %{size_download}\n' "$URL"
200 268435456                                          (675 ms)
$ sha256sum generado.bin
906aff5c…  generado.bin
camino: directo; por eth0 de A, UDP 0,7 MB enviados, 276,2 MB recibidos; TCP 443 4 KB y 1 KB
```

La ida de A a B tardó 70 s, frente a 14 s en la primera ejecución con la misma carga de la máquina; la vuelta de B a A, menos de 1 s (observación 3).

### Caso 3. Interrupción de transferencia: superado por camino directo y por relay

`caso3.sh` del runbook, extraído tal cual, con el corte real de `corte-red.sh`: `iptables` descarta en A todo salvo loopback y SSH durante 10 s. Ninguna transferencia acabó antes del corte, así que no hizo falta limitar el emisor.

**3a, camino directo, `bajada`** (A descarga de B un fichero que B acaba de generar):

```text
16:14:16 transferidos 34865152 bytes: corte de 10 s
16:14:16 corte-red: red cortada 10 s
16:14:17 durante el corte: 36306944 bytes
16:14:22 durante el corte: 36306944 bytes
16:14:26 corte-red: red restablecida
16:14:32 tras el corte: 40894464 bytes
curl: código 0, 200 268435456
c2983216…  caso3.bin                                   (B escribió c2983216… en su respuesta)
relay: recibidos 45798, enviados 43830  →  relay: recibidos 56873, enviados 60828
camino: directo; por eth0 de A, UDP 0,7 MB enviados, 289,0 MB recibidos; TCP 443 18 KB y 13 KB
```

**3a, camino directo, `subida`** (B descarga de A el fichero que A adjunta):

```text
ded69f01…  caso3.bin
16:14:46 transferidos 34183034 bytes: corte de 10 s
16:14:46 corte-red: red cortada 10 s
16:14:47 durante el corte: 34372051 bytes
16:14:52 durante el corte: 34372051 bytes
16:14:56 corte-red: red restablecida
16:14:57 tras el corte: 44238182 bytes
pals ask: código 0
ded69f01…  caso3.bin                                   (el sha256 que calculó B)
camino: directo; por eth0 de A, UDP 294,7 MB enviados, 0,4 MB recibidos; TCP 443 6 KB y 21 KB
```

Después, `reglas_como_antes` imprimió `reglas del cortafuegos como antes`.

**3b, por relay.** A añade las reglas del runbook (descartar el UDP saliente salvo DNS y loopback) y rearranca su daemon (`para` tardó menos de 1 s). La prueba de que el camino fue el relay es el TCP 443: por él pasaron los 256 MiB, 282,6 MB en `bajada` y 282,1 MB en `subida`. Que A no enviara UDP es consecuencia de la regla `DROP`: los contadores están detrás de ella y no ven lo que descarta.

`bajada`:

```text
16:17:24 transferidos 33652736 bytes: corte de 10 s
16:17:24 corte-red: red cortada 10 s
16:17:25 durante el corte: 33652736 bytes
16:17:30 durante el corte: 33652736 bytes
16:17:34 corte-red: red restablecida
16:17:46 tras el corte: 34865152 bytes
curl: código 0, 200 268435456
0c39aa4a…  caso3.bin                                   (B escribió 0c39aa4a… en su respuesta)
relay: recibidos 8591, enviados 1704  →  relay: recibidos 242894893, enviados 1413119
camino: relay; por eth0 de A, UDP 0 B enviados, 11 KB recibidos; TCP 443 4,0 MB enviados, 282,6 MB recibidos
```

La descarga tardó 1073 s desde la pregunta, unos 250 KB/s, y en el registro de B aparece cuatro veces `The relay is rate-limiting this endpoint; outbound relay traffic is being throttled`. La línea `relay:` del runbook creció en 243 MB, menos que el fichero: solo cuenta las conexiones del daemon a los relays que siguen abiertas al medir. Los contadores de `iptables`, que lo cuentan todo, dan 282,6 MB.

`subida`:

```text
633d1208…  caso3.bin
16:35:06 transferidos 33563553 bytes: corte de 10 s
16:35:06 corte-red: red cortada 10 s
16:35:07 durante el corte: 33572829 bytes
16:35:12 durante el corte: 33572829 bytes
16:35:16 corte-red: red restablecida
16:35:26 tras el corte: 35555816 bytes
pals ask: código 0                                     (con --wait 30m)
633d1208…  caso3.bin                                   (el sha256 que calculó B)
relay: recibidos 242894893, enviados 1413119  →  relay: recibidos 1911478, enviados 244572523
camino: relay; por eth0 de A, UDP 0 B enviados, 22 KB recibidos; TCP 443 282,1 MB enviados, 6,2 MB recibidos
```

La subida tardó 1069 s, más que los 600 s que `pals ask` espera por defecto: sin la corrección 4, `pals ask` habría terminado con código 3. El registro de A avisa también de que el relay lo limita. Al deshacer las reglas, A rearrancó otra vez y `reglas_como_antes` imprimió `reglas del cortafuegos como antes`.

Las cuatro transferencias se pararon durante el corte (las dos líneas `durante el corte` dan el mismo número, por debajo de 268435456), siguieron después en la misma petición y terminaron con el hash correcto. Por relay, `curl` y `pals ask` no notaron el corte más que en el tiempo.

### Caso 4. Reintento sin duplicar: superado

Una tarea de 120 s y, a los 15 s, un corte real de 45 s en A, más largo que los 30 s tras los que Iroh da una conexión por perdida:

```text
16:51:00 pals ask b 'espera 120'
16:51:15 corte-red: red cortada 45 s
16:52:01 corte-red: red restablecida
he esperado 120 s
código 0
16:53:00 fin
$ pals task ls | tail -n 1
819f184a…	TASK_STATE_COMPLETED	b	01a1daf8…	3
camino: directo; por eth0 de A, UDP 529 KB enviados, 428 KB recibidos; TCP 443 52 KB y 46 KB
$ python3 caso4-eventos.py <pals.db de B> 819f184a…     (en B)
  filas de deduplicación: 1 -> tarea ['01a1daf8…']
  tareas que lo contienen: 1 -> ['01a1daf8…']
  eventos task.accepted de esas tareas: 1
```

`pals ask` terminó por sí solo, sin `pals task resume`, con la respuesta a los 120 s de la tarea y tres intentos. B ejecutó la tarea una sola vez.

### Caso 5. Caída del proveedor: superado

```text
16:53:00 pals ask b 'espera 300'                       (en A, en segundo plano)
59379b56…	pending	b	01a1dafa…	1
[16:53:15] En B: kill -9 del daemon
[16:53:17] grupo de procesos de la orden, tras el kill -9:
    PID    PGID     ELAPSED COMMAND
 351633  351633       00:16 sh <b-salida>/pals-prueba.sh
 351635  351633       00:16 sleep 300
pals: b could not be reached; the question is kept: `pals task resume 59379b56…` tries again
código 3
16:54:28 fin                                           (73 s después del kill -9)
[16:54:28] En B: arranca
$ pals task resume 59379b56…                           (en A)
pals: task 01a1dafa… ended TASK_STATE_FAILED: interrupted: daemon restarted
código 2
$ pals ask b 'hola tras el rearranque'
eco: hola tras el rearranque
código 0
$ python3 caso4-eventos.py <pals.db de B> 59379b56…     (en B)
  eventos task.accepted de esas tareas: 1
$ kill -- -351633                                      (en B: la orden que sobrevivió)
```

A recibe `TASK_STATE_FAILED` con `interrupted: daemon restarted`, la tarea no se ejecuta otra vez y una pregunta nueva funciona tras el rearranque, sin volver a guardar el ticket de B. Se confirma la limitación conocida: la orden (`sh` y su `sleep 300`) sobrevive al `kill -9` del daemon y hay que terminarla a mano, como dice el runbook. Camino: directo (medido en la ejecución parcial, ver [«Camino de los casos 5 a 8»](#camino-de-los-casos-5-a-8)).

### Caso 6. Revocación: superado

```text
$ pals grant ls --issued                               (en B: seis grants «project blobs … blob.fetch», uno por fichero compartido, y el de tareas)
  g-…  active  project demo  task.send,task.read,task.cancel on skill:ask  until 2026-11-15T16:12:14Z  to a
$ pals grant revoke g-…
revoked grant g-…
$ pals ask b "hola tras la revocación"                 (en A)
pals: refused: HTTP 401: revoked
código 2
$ curl -sS -i -X POST http://127.0.0.1:7420/peers/b/rest/message:send -H 'A2A-Version: 1.0' -H 'Content-Type: application/json' -d '{"message":{"messageId":"caso6-curl","role":"ROLE_USER","parts":[{"text":"hola"}]}}'
HTTP/1.1 401 Unauthorized
content-type: application/json
www-authenticate: Bearer realm="agentnet", error="invalid_token", error_description="revoked"
{"error":"revoked"}
```

Después B emitió un grant nuevo para A, y A lo importó para los casos 7 y 8. Camino: directo (medido en la ejecución parcial, ver [«Camino de los casos 5 a 8»](#camino-de-los-casos-5-a-8)).

### Caso 7. Blob sin grant: superado

C, un tercer daemon en A con su propio directorio y su gateway en `127.0.0.1:7421`, guarda los tickets de A y de B y no tiene ningún grant:

```text
$ PALS_HOME=~/.pals-c pals grant ls
issued:
held:
$ curl -sS -i http://127.0.0.1:7421/blobs/<B>/eda00e39…   # fichero del caso 2 (lo tiene B)
HTTP/1.1 403 Forbidden
{"error":"the holder denied the blob"}
$ curl -sS -i http://127.0.0.1:7421/blobs/<A>/eda00e39…   # el mismo (lo tiene A, que lo subió)
HTTP/1.1 403 Forbidden
{"error":"the holder denied the blob"}
$ curl -sS -i http://127.0.0.1:7421/blobs/<B>/6c8f84a6…   # el que B generó en el caso 2
HTTP/1.1 403 Forbidden
{"error":"the holder denied the blob"}
$ PALS_HOME=~/.pals-c pals ask b hola
pals: refused: HTTP 401: no grant held for <B>
código 2
blobs en el almacén de C: 0
```

Ningún `502`: C alcanzó a los dos poseedores, y los dos le negaron el blob. Camino: directo (medido en la ejecución parcial, ver [«Camino de los casos 5 a 8»](#camino-de-los-casos-5-a-8)).

### Caso 8. Cliente estándar: superado

`caso8.py` del runbook, con `a2a-sdk==1.3.0` instalado en A desde PyPI con las versiones de `tests/interop/requirements.txt`, contra el gateway de A, con B como par:

```text
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
código 0
En B: órdenes de pals-prueba.sh vivas: 0
```

Cancelar las dos tareas `espera 60` terminó sus órdenes en B. Camino: directo (medido en la ejecución parcial, ver [«Camino de los casos 5 a 8»](#camino-de-los-casos-5-a-8)).

### Limpieza: superada

La sección 6 del runbook: `para` en A y en B, `reglas_como_antes` en A (`reglas del cortafuegos como antes`), ninguna orden de `pals-prueba.sh` viva en B y `~/.pals-c` borrado. Después el arnés borró el contenedor, la red, el `PALS_HOME` de B, su directorio de la prueba y la copia del proyecto; la imagen `pals-exit-a:latest` se conservó. Tras el desmontaje no quedaba ningún contenedor ni red `pals-exit-*`, ningún proceso `pals` ni de `pals-prueba.sh`, y nada escuchando en el puerto 17420.

### Camino de los casos 5 a 8

**Qué arnés dio qué resultado.** La ejecución completa (todos los resultados de arriba salvo el camino de los casos 5 a 8) se hizo con el arnés de `040bc01`. El camino de los casos 5 a 8 viene de una ejecución parcial con el arnés del commit siguiente, `5a1cb38`. Las correcciones posteriores del arnés, tras dos revisiones (ficheros de PID comprobados, directorio marcado, etiquetas de Docker, desmontaje al interrumpir con Ctrl-C o `SIGTERM` también a mitad de un paso largo, camino del caso 3 comprobado), solo se han probado por partes, no de principio a fin; el `run.sh` de `5a1cb38` tampoco se ejecutó entero, ni su limpieza ni su desmontaje nuevos.

El arnés de `040bc01` solo medía los contadores en los casos 1 a 4. Para los demás se añadió la medida, y una ejecución parcial (de 16:59 a 17:03 UTC, sin `claude`: preparación y casos 2 y 5 a 8) repitió los casos 5 a 8. Los cuatro se superaron otra vez, todos por camino directo:

| Caso | UDP sin DNS, enviado / recibido por A | TCP 443, enviado / recibido |
|-|-|-|
| 5 | 706 KB / 208 KB | 54 KB / 25 KB |
| 6 | 2,7 KB / 0,8 KB | 0 / 0 |
| 7 (lo que C habla con B; lo que habla con A no sale por `eth0`) | 156 KB / 121 KB | 12 KB / 25 KB |
| 8 (solo `caso8.py`, sin la instalación desde PyPI) | 15 KB / 10 KB | 0,2 KB / 0,2 KB |

Esa ejecución parcial encontró además dos fallos del arnés, corregidos en `5a1cb38`. Ninguno es de PALS ni del runbook, y ninguno cambia un resultado de la ejecución completa:

- **El arnés buscaba la orden que sobrevive al `kill -9` del caso 5** con el `grep '[p]als-prueba'` del runbook. En un primer intento, ese `grep` encontró antes otro proceso ajeno a la prueba: la orden que lanzó el arnés, cuya línea contenía ese texto. Así terminó un grupo equivocado, y la orden real siguió viva hasta el caso 8. Ahora el arnés busca exactamente `sh <directorio de B>/pals-prueba.sh`. En la ejecución completa no pasó: listó el grupo correcto (`sh` y `sleep 300`), lo terminó, y el caso 8 contó 0 órdenes vivas.
- **La limpieza tomaba el código de salida de `rm -rf ~/.pals-c`**, no el de `reglas_como_antes`. En la ejecución completa, `reglas_como_antes` imprimió `reglas del cortafuegos como antes`.

### Correcciones al runbook

Una primera ejecución del arnés, de 15:43 a 16:11 UTC y sin el caso 1, encontró fallos del runbook, no de PALS. Se corrigieron en `c0b7d14`, y la ejecución de arriba es la segunda, completa, con el runbook corregido. En aquella primera ejecución los casos 2 y 4 a 8 pasaron igual que en esta.

1. **`reglas_como_antes` fallaba sin quedar ninguna regla.** En A no existía la tabla `filter` de `ip6tables` antes del primer corte. `corte-red.sh` la crea, y al quitar sus reglas queda vacía: `ip6tables-save` imprime entonces `*filter` y `COMMIT`, que la copia de antes no tenía (`1,2d0 < *filter < COMMIT`). Ahora se comparan solo las líneas de reglas (`grep -- '^-'`).
2. **La línea `tras el corte` llegaba antes de que la transferencia siguiera.** `caso3.sh` la tomaba 2 s después de que volviera la red, y QUIC puede tardar más en volver a probar el camino. En la primera ejecución, la descarga por camino directo seguía en 38141952 bytes a las 15:44:42 (la red volvió a las 15:44:40), pero terminó con el hash correcto a las 15:44:45. Por relay seguía en 33587200 bytes 2 s después, y también terminó bien. Ahora `caso3.sh` espera, hasta 60 s, a que la transferencia avance 1 MiB, y la línea dice cuándo. En esta ejecución, la línea `tras el corte` llegó entre 1 y 12 s después de volver la red (observación 1).
3. **Sin DNS al forzar el relay en un contenedor.** La regla del runbook que deja pasar el UDP al puerto 53 no basta con el resolvedor de Docker, `127.0.0.11`: el propio contenedor redirige sus consultas a otro puerto antes del filtro, y la regla siguiente las descarta. Sin DNS, A no encuentra el relay. Comprobado en A: con las reglas del runbook, `getent hosts euc1-1.relay.n0.iroh.link` falla y la regla `DROP` cuenta 8 paquetes; con una excepción para loopback, resuelve. El runbook añade `-o lo -j ACCEPT`.
4. **`pals ask` dejaba de esperar antes de que acabara la subida por relay.** Los relays públicos de n0 limitan el tráfico de cada endpoint, y el daemon que envía lo avisa (`The relay is rate-limiting this endpoint; outbound relay traffic is being throttled`). En la primera ejecución, la descarga de 256 MiB por relay tardó 1156 s, unos 230 KB/s, más que los 600 s que espera `pals ask` por defecto. `caso3.sh subida` usa ahora `--wait 30m`, como el `blob_fetch_timeout_s = 1800` del paso 2.
5. **Variables y funciones perdidas en una terminal nueva** (pendientes de una revisión anterior). `arranca`, `para`, `reglas_como_antes`, `$PROYECTO` y la ruta del clon van ahora en `~/pals-salida/funciones.sh`, que carga cada bloque que los usa. El caso 8 ya no supone que el clon está en `~/rosepetal-pals`.
6. **Como root, sin `sudo`.** Las órdenes y los guiones usan `$SUDO`, vacío para root. `corte-red.sh` imprime además la hora del corte y la de la vuelta, y no falla si no hay `syslog`.

Después de esta ejecución, una revisión corrigió algo más del runbook, sin volver a ejecutar la prueba completa: `para` comprueba que el PID guardado sigue siendo un `pals daemon` antes de enviarle la señal, espera como mucho 60 s y borra el fichero del PID; el caso 5, el caso 8 y la limpieza buscan la orden del worker de pruebas con `grupos_prueba` (exactamente `sh ~/pals-salida/pals-prueba.sh`, no un `grep` sobre `ps`), y el caso 5 pide comprobar su grupo antes de matarlo; el cambio de worker no escribe nada si falta `$PROYECTO`; y la vuelta atrás de 3b va en un bloque propio. `para` y `grupos_prueba` se probaron por separado con procesos señuelo.

### Desviaciones del runbook

- B no usa `~/.pals` ni `~/pals-salida`, sino un `PALS_HOME` y un directorio propios bajo `PALS_EXIT_DIR`, con el gateway en el puerto 17420.
- El binario se compiló una vez en el anfitrión y se copió a A, en lugar de usar `cargo install` en cada máquina. A recibió una copia del árbol del repositorio.
- Las cuatro reglas que solo cuentan bytes en A, para informar del camino.
- En el caso 7, C pide también el fichero que B generó en el caso 2.
- El arnés limita el emisor del caso 3 si la transferencia acaba antes del corte (en `bajada`, con un policía de entrada de `tc` en A, porque la red del anfitrión no se toca). En esta ejecución no hizo falta: todos los cortes cayeron a mitad de transferencia.

### Observaciones

Ninguna es un fallo de un caso, pero conviene tenerlas en cuenta en la prueba en dos máquinas.

1. **Tras un corte, la transferencia tarda unos segundos en seguir.** La línea `tras el corte` llega cuando la transferencia ha avanzado 1 MiB. Llegó 6 s y 1 s después de volver la red por camino directo, y 12 s y 10 s por relay, donde avanzar 1 MiB ya lleva unos 4 s. Ninguna transferencia empezó de cero ni falló.
2. **Los relays públicos de n0 limitan el tráfico de cada endpoint:** unos 250 KB/s. El daemon que envía lo avisa en su registro (`The relay is rate-limiting this endpoint; outbound relay traffic is being throttled`). Por relay, los 256 MiB tardaron unos 18 min en cada sentido.
3. **La ida del caso 2 tardó 70 s** en la ejecución completa, frente a 14 s y 15 s en las otras dos ejecuciones, con una carga parecida de la máquina. La vuelta tardó siempre menos de 1 s. No se investigó la causa: la máquina era compartida, y la ida incluye la subida del fichero al gateway de A, su descarga por B y el trabajo del worker.
4. **`para` en A tardó unos 30 s** en la primera ejecución, al rearrancar con el UDP ya descartado. En la ejecución completa tardó menos de 1 s las dos veces. No se investigó; el runbook ya avisa de que `para` puede tardar unos segundos.
5. **Avisos de Iroh:** en el registro de A, `failed closing path err=LastOpenPath` (de `noq_proto`), una vez durante el corte del caso 4 y otra tras el `kill -9` del caso 5. En el de B (línea 11), `Ignoring connection: invalid handshake: aborted by peer` (de `iroh::protocol`) a las 16:52:00, al final del corte del caso 4 (la red de A volvió a las 16:52:01). Ninguno afectó a un resultado.
6. **Con `relay = true`, un rearranque no invalida los tickets.** A alcanzó a B tras el paso intermedio y tras el caso 5, y B alcanzó a A tras los dos rearranques de A en el caso 3b, sin volver a guardar ningún ticket. Queda resuelta la observación 1 del ensayo anterior.
7. **`pals ask` imprime en stderr su `messageId`** (`pals: message … kept; resume with: pals task resume …`) antes de esperar la respuesta, también cuando la petición se rechaza. Los resultados esperados del runbook solo citan la otra línea.
8. **El daemon sigue sin informar del camino de red.** El camino se dedujo de los contadores de A, como se explica arriba.

### Lo que no cubre

- **Dos máquinas físicas en redes distintas, tras NAT distintos.** A y B están en la misma máquina: comparten kernel, reloj, interfaces de red e IP pública, y entre ellos solo está el NAT de Docker. Nunca se atravesó un NAT de operador ni se probó la perforación de NAT (*hole punching*) entre dos redes reales.
- **Wi-Fi y redes móviles reales**, con su latencia, sus pérdidas y sus cambios de dirección.
- **El camino por relay se forzó** descartando el UDP saliente de A, no poniendo las máquinas en redes distintas. Los dos daemons usaron el mismo relay, `euc1-1`.
- **Solo se cortó la red de A.** B nunca perdió la red; en el caso 5, B cayó por `kill -9`.
- **Relojes**: A y B comparten reloj, así que no se ejercitó la tolerancia de 60 s de los grants.
- **IPv6**: la red de Docker de A no tenía IPv6; todo fue por IPv4.
- **Claude Code** respondió en el mismo anfitrión, con la sesión de quien ejecutó la prueba; no se probó con una clave de API ni con el guion envoltorio del paso 2.
- **El camino con `sudo`.** A trabajó como root, así que `$SUDO` iba vacío: no se ejecutó ninguna orden ni ningún guion con `sudo` (`sudo -v`, `sudo setsid -f sh corte-red.sh`, `sudo iptables-save`).
- **Las órdenes del runbook en B.** El arnés no ejecuta en B los bloques del runbook: los reimplementa con las rutas de B (`pb`, `arranca_b`, `para_b`, `write_b_config`) y nunca carga `funciones.sh` en B. Quedan sin probar tal como están escritos: el paso 2 en B, el bloque del cambio de worker (con su comprobación de `$PROYECTO`), la parte del caso 5 en B (`kill -9`, `grupos_prueba`, `kill -- -<PGID>`) y la limpieza en B.
- **Las órdenes del runbook en A, fuera de los guiones.** Del runbook solo se extraen los seis guiones de `~/pals-salida` (`funciones.sh`, `pals-prueba.sh`, `corte-red.sh`, `caso3.sh`, `caso4-eventos.py` y `caso8.py`). Los bloques de órdenes de A (la configuración del paso 2, el reglaje del relay en 3b, los casos 2, 4, 6, 7 y 8) son copias dentro de `run.sh`: un cambio en esos bloques del runbook no lo prueba el arnés hasta que se copie a mano.

## Ensayo anterior en una sola máquina (2026-11-14, 10:38 a 10:57 UTC)

Ensayo hecho antes de la ejecución con Docker, y conservado tal cual: dos daemons `pals` (y un tercero para el caso 7) en directorios distintos que se hablan por loopback, sin relay, con los casos del criterio de salida que se pueden simular así. Sirvió para comprobar, antes de la prueba real, que el binario se comporta como espera cada caso y que el runbook funciona; no sustituye a una red real, a un relay, a NAT ni a Claude Code. Lo que no cubría lo cubre en parte la ejecución con Docker de arriba.

### Resumen

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

### Ejecución

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

### Montaje

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

### Caso 1. Pregunta útil: no ensayado con `claude`

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

### Paso intermedio: B cambia al worker de pruebas

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

### Caso 2. Transferencia autorizada: superado

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

### Caso 3. Interrupción de transferencia: superado en su forma de una máquina

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

### Caso 4. Reintento sin duplicar: superado

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

### Caso 5. Caída del proveedor: superado

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

### Caso 6. Revocación: superado

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

### Caso 7. Blob sin grant: superado

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

### Caso 8. Cliente estándar: superado

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

### Observaciones del ensayo

1. **Con `net.relay = false`, el puerto de Iroh cambia en cada arranque** y los tickets que guardaron los pares dejan de valer: tras cada rearranque de B (el cambio de worker y el caso 5), A no lo alcanzó hasta volver a guardar su ticket. Con `relay = true`, el ticket incluye el relay y el par se busca por su id; la prueba en dos máquinas debe confirmarlo en el paso intermedio y en el caso 5.
2. **El gateway sirve un blob que ya tiene completo desde su propio almacén**, sin contactar con el poseedor que nombra la URL. Por eso, en el caso 2, la vuelta del fichero idéntico no cruza la red, y el runbook comprueba el sentido de B a A con un fichero nuevo. Por lo mismo, tras revocar por error el grant de blob de un fichero que A había subido, A siguió descargándolo (`200`) de su propia copia.
3. **`pals grant ls --issued` mezcla los grants de tareas con los de blobs** (`project blobs … blob.fetch`, uno por fichero compartido). Para revocar el acceso de un par a las tareas hay que elegir el de `skill:ask`.
4. **`kill -9` del daemon deja viva la orden en curso** (limitación conocida, caso 5).
5. **Con el par caído, `pals ask` tarda algo más de un minuto en rendirse** (77 s en las dos ejecuciones del caso 5): la conexión abierta tarda hasta 30 s en darse por perdida, y después vienen tres reintentos con esperas de 1, 2 y 4 s, cada uno con un intento de conexión de hasta 10 s.
6. **El registro del daemon solo tiene las líneas de arranque y parada.** El daemon no instala un suscriptor de `tracing`, así que no informa del camino de Iroh (directo o relay) ni de los reintentos. El runbook deduce el camino de las conexiones del daemon a los relays.
7. **`tests/interop/python_client.py` no sirve tal cual para el caso 8 en dos máquinas**: siempre arranca sus propios daemons locales. El runbook usa `caso8.py`.
