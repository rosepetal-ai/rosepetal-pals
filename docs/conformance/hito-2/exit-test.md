# Prueba de salida del hito 2

Este documento recoge la ejecución de los casos del criterio de salida del hito 2 (spec §14), siguiendo el runbook [`docs/runbooks/hito-2-consulta-de-servicios.md`](../../runbooks/hito-2-consulta-de-servicios.md), con un contenedor Docker por agente. La ejecuta el arnés [`tests/exit/hito-2.sh`](../../../tests/exit/README.md#prueba-de-salida-del-hito-2-con-docker).

**No es una prueba en varias máquinas físicas.** Los agentes comparten kernel, reloj y máquina, y entre ellos solo hay el bridge de Docker (ver [«Lo que no cubre»](#lo-que-no-cubre)).

## Resultado (2026-11-15)

Casos 1 a 8 superados en la ejecución registrada, sin repetir ninguno salvo el reintento previsto del caso 5 (observación 2). El caso 9 está **pendiente: ver [`README.md`](README.md) de este directorio**. Los ensayos anteriores sacaron a la luz un fallo de producto, corregido en `eea2281` antes de esta ejecución, y tres correcciones del arnés y del runbook (ver [«Correcciones»](#correcciones-antes-de-esta-ejecución)).

| Caso | Resultado | Evidencia | Tiempo |
|-|-|-|-|
| 1. Elegir entre al menos tres candidatos | Superado | `pals discover … --ask`: B `available` y C `busy` en el ranking; D, E y G descartados con su motivo; responde B (`b: Revisa src/lib.rs`), código 0; una ejecución en B y ninguna en C, D, E ni G | 1 s |
| 2. Requisitos sobre integraciones y recursos | Superado | E descartado `mcp_integration:example:everything not operational`; G `min_offered_ram_mib not met`; B cumple el MCP con base `observed`. En la vista de B, su MCP `ok` por `mcp tools/list` con `echo` y `get-sum`; en la de E, `failed`, `missing tools: read_file` | 1 s |
| 3. Validar cards | Superado | La card firmada de B, por el gateway de A: `3 checks passed` (a2a-sdk), `verified` (a2a-go), `verified` (@a2a-js/sdk). Los pares de prueba con la card alterada: los dos tests en verde. F alterado: `card signature unverifiable` y descartado `card not verified` | 2 s |
| 4. No ejecutar trabajo al consultar | Superado | Ráfaga de 50: 4 `200` y 46 `429` (`too many searches`). 50 seguidas, de 4 en 4: 50 `200`. En B, C, D, E y G: `task.accepted` +0, ejecuciones +0, `tools/list` +0, el mismo PID; ningún `tools/call` en toda la prueba | 5 s |
| 5. Oferta vencida y proveedor ocupado | Superado | 5a: oferta de B vencida por el reloj de A, `offer expired: run pals discover again`, código 2, nada enviado. 5b: C `busy`, H `timeout`; la oferta de B renovada (`offer-2dd97c2d…` → `offer-3a4951d4…`) y la pregunta, una ejecución en B y ninguna en C | 13 s |
| 6. Vistas y caché por permisos | Superado | A (`task:demo`): 124 entradas en dos páginas, con las de proyecto. G (`public`): las 120 notas públicas y nada más, ninguna clave fuera del esquema. Los cursores cruzados, `400` con el mismo error en los dos sentidos | 2 s |
| 7. Contacto A2A corriente | Superado | F por `https`, con la CA de prueba en A: `card signature verified f-1`; `pals ask f`: `F: hola, F`; JSON-RPC y HTTP+JSON por el gateway de A, `TASK_STATE_COMPLETED`; `http://` a otra máquina, rechazado | 2 s |
| 8. Migración | Superado | M1 y M2 del hito 1: esquema 1 → 2, cada fila de las nueve tablas del hito 1 conservada; `contact ls`, `grant ls` y `task ls` iguales con las dos versiones; una pregunta nueva con el hito 2; `events_verify: 7 events … verified` (M1) y `1 events … verified` (M2) | 3 s |
| 9. Sin regresión A2A | Pendiente | Ver [`README.md`](README.md) de este directorio | |

### Entorno

| Dato | Valor |
|-|-|
| Fecha | 2026-11-15, de 06:21:10 a 06:22:26 UTC |
| Commit | `0e1e4d0`: el arnés y el runbook; el código es el de `eea2281`, que incluye la corrección de este hito (ver «Correcciones») |
| Binario | `cargo build --release --locked -p pals` en el anfitrión: `pals 0.1.0`, el mismo fichero en todos los contenedores (sha256 `ecdaa98a…`) |
| Caso 8 | El `pals` del hito 1, de `71092a0` (`git archive 71092a0`, `cargo build --release --locked -p pals` con su propio `CARGO_TARGET_DIR`; sha256 `42ed9f66…`), y el ejemplo `events_verify` de `pals-core` |
| Rust | 1.99.1 |
| Versiones fijadas | `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`, `a2a-pb =0.3.2`, `rmcp =3.10.0` |
| Verificadores de cards | `a2a-sdk==1.3.0` (Python 3.13.5, en A); `github.com/a2aproject/a2a-go/v2 v2.7.0` (Go 1.27.3, en el anfitrión); `@a2a-js/sdk` 1.3.0 (Node 26.13.0, en el anfitrión) |
| F | `a2a-sdk==1.3.0` con las dependencias de [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt), y `starlette==1.10.0`, `sse-starlette==3.6.0`, `uvicorn==0.54.0` y `click==8.5.0` ([`requirements-f.txt`](../../../tests/exit/hito-2/requirements-f.txt)) |
| Servidor MCP | `@modelcontextprotocol/server-everything` 2026.8.31 (se anuncia como `mcp-servers/everything` 2.0.0) por stdio, con `@modelcontextprotocol/sdk` 1.32.2 y el resto de sus dependencias fijadas en [`package-lock.json`](../../../tests/exit/hito-2/mcp/package-lock.json); Node 20.19.2 de Debian |
| Imagen | `pals-exit-h2:latest` (id `2ed813b1…`), construida por el arnés desde [`tests/exit/hito-2/Dockerfile`](../../../tests/exit/hito-2/Dockerfile) sobre `debian:trixie-slim@sha256:a72b4433…` (Debian 13) |
| Anfitrión | Linux x86_64, kernel 6.12 de Debian 13, 32 CPU, compartido con otros proyectos. Carga media al empezar: 25,5 / 21,3 / 20,3; al terminar: 30,8 / 24,2 / 21,4. El arnés corre en un entorno Ubuntu 24.04.4 con Docker 29.4.1 |
| Contenedores | Ocho, en la red bridge `pals-exit-h2-net` (172.27.0.0/16): `a` .2, `b` .3, `c` .4, `d` .5, `e` .6, `g` .7 (G y H), `f` .8, `m` .9. Solo A tiene `NET_ADMIN`, para contar bytes |
| Relays | `net.relay = true` en todos: los relays públicos de n0. Todos eligieron `euc1-1.relay.n0.iroh.link`, que A alcanzaba por HTTPS antes de empezar (`200` en 0,21 s) |

El registro completo de la ejecución quedó en la máquina de quien la hizo, sin versionar. Aquí se copia lo que respalda cada resultado. Los ids de agente se escriben `<A>`, `<B>`…, y los hashes y los ids de oferta se acortan. No se copian tickets ni tokens.

### Montaje y camino de red

El arnés construye la imagen, crea la red y los ocho contenedores, copia el binario y el árbol del repositorio a cada uno y extrae del runbook sus ocho guiones de `~/pals-h2`. Cada orden en un contenedor es una terminal nueva que carga `funciones.sh` y elige su agente, como manda el runbook. Los tickets y los tokens pasan de un contenedor a otro sin imprimirse.

- **F (2.1).** Una CA de prueba (P-256, 7 días) y el certificado de F para `pals-exit-h2-f`, firmado por ella; F en el 8443 con su card firmada y en el 8444 con la card alterada. El certificado de la CA pasa a A por el anfitrión; su clave no sale de F.
- **A (2.2).** La CA en su almacén del sistema (`update-ca-certificates`) antes de arrancar. A alcanza a F por `https` en los dos puertos.
- **Proveedores (2.3).** `pals inventory refresh` en cada uno: `observed 1, ok 1, failed 0` en B, C, D, G y H; en E, `observed 1, ok 0, failed 1` y `failed: example:everything missing tools: read_file`. B recibió de su servidor `"serverInfo":{"name":"mcp-servers/everything","title":"Everything Reference Server","version":"2.0.0"}`.
- **Contactos y grants (2.4).** Cada proveedor de B a G guarda a A y le da un grant de `demo` para `task.send,task.read,task.cancel` en `skill:ask`; G guarda a B, sin grant. A ve la card firmada de cada uno: cinco `verified: <x> blake3:…`.
- **C, lleno (2.5).** `pals ask c "espera 3600" --wait 10s`: código 3, y la tarea sigue en C, su única plaza.

**Cómo se determinó el camino.** PALS no informa del camino de Iroh. Como en el hito 1, el arnés añade al final de las cadenas de A cuatro reglas de `iptables`, dentro del contenedor de A, que solo cuentan bytes por su `eth0`: UDP salvo DNS (el camino directo entre contenedores) y TCP 443 (los relays). En el caso 1, **camino directo**: 77,6 KB enviados y 88,2 KB recibidos por UDP, frente a 6,5 KB y 2,4 KB por TCP 443, el tráfico de fondo con el relay.

### Caso 1. Elegir entre al menos tres candidatos: superado

```text
$ pals discover --capability code.rust.review --requirements-file req.json --ask "Revisa src/lib.rs"
ranked:
  1. b (agent <B>): available, skill repo-review, time 1100 ms, preferred met 0, benchmark none, completed 0 failed 0, grant held, card verified <B>, offer offer-9467df7b… until 2026-11-15T06:22:05Z
  2. c (agent <C>): busy, skill repo-review, time 110 ms, preferred met 0, benchmark none, completed 0 failed 0, grant held, card verified <C>, offer offer-dc8cb233… until 2026-11-15T06:22:05Z
discarded:
  d (offer): external_cloud_allowed not met
  e (offer): mcp_integration:example:everything not operational
  g (offer): min_offered_ram_mib not met
coverage: complete
metrics: contacted 5, messages 15, bytes sent 5480, bytes received 30813, first valid 63 ms, matching 16 us, elapsed 112 ms
asked: b
b: Revisa src/lib.rs
código 0
```

Cinco candidatos consultados; C, más rápido que B (110 ms frente a 1100 ms), va detrás porque está lleno. Ejecuciones nuevas, contadas en el registro del worker de cada proveedor: B 1, C 0, D 0, E 0, G 0.

### Caso 2. Requisitos sobre integraciones y recursos: superado

La misma búsqueda, sin pregunta, por `POST /discovery/search` del gateway de A (`200`), y la conexión MCP de B y la de E como las publican a A (`pals inventory get <x> --sections mcp`, `verified: <x> task:demo seq 1`):

```text
ranking b c
b external_cloud_allowed met declared
b mcp_integration:example:everything met observed
b min_offered_ram_mib met declared
descarte d offer external_cloud_allowed not met
descarte e offer mcp_integration:example:everything not operational
descarte g offer min_offered_ram_mib not met
mcp b: ok, mcp tools/list, -, herramientas [example:everything/echo example:everything/get-sum]
mcp e: failed, mcp tools/list, missing tools: read_file, herramientas []
```

E declara la conexión y ofrece `read_file`, que server-everything no sirve: su adaptador la observa `failed`, y la oferta de E no cumple el requisito. G ofrece 1024 MiB de RAM a una tarea, menos de los 4096 pedidos, aunque tiene 65536 instalados: el requisito se compara con lo ofrecido. B cumple el MCP por la observación de su adaptador, `initialize` y `tools/list` contra el servidor real, y nunca un `tools/call` (sección del caso 4).

### Caso 3. Validar cards: superado

La card firmada de B, `GET /peers/<B>/signed-agent-card.json` del gateway de A (1849 bytes, sha256 `057797f2…`; solo la skill `ask`, y ninguna mención de lo que es de proyecto):

```text
$ card_signatures.py --gateway http://127.0.0.1:7420 --peer <B>          (a2a-sdk, en A)
check 1 passed: the card verifies with kid <B>
check 2 passed: a changed skills[0].description is InvalidSignaturesError
check 3 passed: with 8139770e… expected instead, InvalidSignaturesError
3 checks passed
$ go -C tests/interop/card_signatures/go run . <B> <tarjeta-b.json>       (a2a-go, en el anfitrión)
verified
$ node tests/interop/card_signatures/js/verify.mjs <B> <tarjeta-b.json>   (@a2a-js/sdk, en el anfitrión)
verified
```

Los verificadores de Go y JS aceptan, desde esta tarea, un id de agente y el fichero de la card además del vector congelado ([`card-signatures.md`](card-signatures.md)); los dos comprueban también que una copia con `skills[0].description` cambiado no verifica, y que la card no verifica si se espera otro agente.

**Una card alterada queda descartada.** Ningún daemon PALS sirve una card alterada de sí mismo: la firma al arrancar. Lo hacen los pares de prueba de los tests de integración, que sirven una card firmada y cambiada después; el arnés los ejecuta en el anfitrión:

```text
test gateway_refuses_a_tampered_peer_card ... ok      (cards.rs: el gateway responde bad card signature y no guarda nada)
test cards_are_validated ... ok                       (selection.rs: la búsqueda la descarta en la etapa de card, sin preguntarle)
```

Y en vivo, F alterado, un agente corriente cuya firma ya no cubre su card:

```text
$ pals contact add-a2a https://pals-exit-h2-f:8444 --name f-alterada
saved contact f-alterada: https://pals-exit-h2-f:8444 (Agente F), card signature unverifiable
$ pals discover --capability example:eco --peer f-alterada
ranked:
discarded:
  f-alterada (card): card not verified
```

### Caso 4. No ejecutar trabajo al consultar: superado

En B a G, antes y después: los `task.accepted` del registro de eventos, las ejecuciones del worker, los `tools/list` que recibió su servidor MCP (la copia de lo que lee por stdin) y el PID del daemon. En A, la búsqueda del caso 1, sin pregunta, entre B, C, D, E y G (`caso4.sh`):

```text
50 búsquedas, de 50 en 50, en 264 ms; códigos HTTP:
      4 200
     46 429
50 búsquedas, de 4 en 4, en 2473 ms; códigos HTTP:
     50 200
rafaga: 50 búsquedas, códigos {'200': 4, '429': 46}, descartes por cuotas o plazos ninguno, anómalas ninguna
seguidas: 50 búsquedas, códigos {'200': 50}, descartes por cuotas o plazos ninguno, anómalas ninguna
healthz: a 200 b 200 c 200 d 200 e 200 g 200
búsqueda de después -> HTTP 200
```

| Proveedor | `task.accepted` | Ejecuciones | `tools/list` | PID |
|-|-|-|-|-|
| B | 1 → 1 | 1 → 1 | 2 → 2 | 102 → 102 |
| C | 1 → 1 | 1 → 1 | 2 → 2 | 101 → 101 |
| D | 0 → 0 | 0 → 0 | 2 → 2 | 101 → 101 |
| E | 0 → 0 | 0 → 0 | 2 → 2 | 101 → 101 |
| G | 0 → 0 | 0 → 0 | 2 → 2 | 100 → 100 |

La cuota del daemon recorta la ráfaga: a lo sumo 4 búsquedas a la vez, y las demás, `429` con `{"error":"too many searches"}`, en el acto. Las 50 seguidas llegan todas a los proveedores. Entre los casos 1 y 4, en 8 s, cada proveedor recibió de A 57 consultas (58 peticiones B y E, con la lectura de su inventario del caso 2), por debajo de su cuota de 60 por minuto, y ninguno rechazó ninguna. Las 54 búsquedas respondidas del caso 4, y la de después, dan todas B y C en el ranking y D, E y G descartados por sus motivos. Los `task.accepted` y las ejecuciones que ya había son el caso 1 en B y la tarea larga en C.

Los `tools/list` de toda la prueba, por la hora de cada arranque del servidor MCP: en cada proveedor, uno al arrancar su daemon y otro por `pals inventory refresh` (06:21:30 a 06:21:35), todos antes del caso 4, que fue de 06:21:54 a 06:21:57; en B, uno más al rearrancar en el caso 5 (06:22:02). El temporizador de 60 s no llegó a vencer: los daemons vivieron menos de un minuto (observación 3). Ningún `tools/call` en lo que recibió ningún servidor.

### Caso 5. Oferta vencida y proveedor ocupado: superado

H guarda a A, A guarda a H con su grant, y H se para. B vuelve a arrancar con `offer_valid_for_ms = 2000`; A lo vuelve a encontrar por su id, sin guardar otra vez su ticket. C sigue lleno.

**5a.** La oferta que da `pals discover`, usada cuando vence por el reloj de A:

```text
$ pals discover --capability code.rust.review --requirements-file req.json --peer b
  1. b (agent <B>): available, …, card verified <B>, offer offer-9eb11098… until 2026-11-15T06:22:04Z
la oferta offer-9eb11098… vence 2026-11-15T06:22:04Z; ahora 2026-11-15T06:22:04Z
$ pals ask b "pregunta con la oferta vencida" --offer offer-9eb11098…
pals: offer expired: run pals discover again
código 2
```

B no ejecutó nada: A no envió la pregunta.

**5b.** La búsqueda del caso 1 entre B, C y H, con pregunta, por el gateway de A. H, caído, agota los 3 s de su candidato, y la oferta de B, de 2 s, vence entre la búsqueda y la pregunta:

```text
intento 1                                             intento 2
ranking b:available c:busy                            ranking b:available c:busy
descartes h: timeout                                  descartes h: timeout
busqueda 3002 ms                                      busqueda 3003 ms
elegido b TASK_STATE_COMPLETED                        elegido b TASK_STATE_COMPLETED
  b: pregunta del caso 5, intento 1                     b: pregunta del caso 5, intento 2
oferta del ranking offer-aedfab6e… 06:22:09Z          oferta del ranking offer-2dd97c2d… 06:22:10Z
oferta usada       offer-aedfab6e… 06:22:09Z          oferta usada       offer-3a4951d4… 06:22:13Z
la misma                                              renovada
```

En el intento 1 la oferta de B seguía en vigor al preguntar, y el arnés repitió con otra pregunta (observación 2). En el intento 2, A renovó la oferta vencida, una vez, y preguntó con la nueva: B la aceptó. Cada pregunta se ejecutó **una** vez en B (`grep -c` en su registro de ejecuciones: 1 y 1), ninguna en C (lleno, nunca preguntado) y ninguna en H.

### Caso 6. Vistas y caché por permisos: superado

`GET /discovery/inventory/<B>` por el gateway de cada solicitante (`vista.py`), y después el cursor de cada uno en el gateway del otro:

```text
a-1: código 200   scope task:demo, audience <A>, seq 2   entradas 100: services 1, mcp 1, models 1, information 97
                  de proyecto: example:coder, example:everything, repo-review   cursor: sí, truncated_by page_entries
a-2: código 200   scope task:demo, audience <A>, seq 2   entradas 24: information 23, hardware 1
                  de proyecto: example:host   cursor: no
g-1: código 200   scope public, audience -, seq 1        entradas 100: information 100
                  de proyecto: ninguna   claves fuera del esquema: ninguna   cursor: sí, truncated_by page_entries
g-2: código 200   scope public, audience -, seq 1        entradas 20: information 20
                  de proyecto: ninguna   claves fuera del esquema: ninguna   cursor: no
g-con-cursor-de-a: código 400   {'error': 'peer refused: invalid cursor'}
a-con-cursor-de-g: código 400   {'error': 'peer refused: invalid cursor'}
```

- G, sin grant, ve las 120 notas públicas, exactamente las de la vista pública de B (`pals inventory show --scope public` en B, entrada por entrada), y nada más. En las dos páginas no aparece ni un id de lo que es de proyecto ni las palabras `demo` o `project`, y no hay ninguna clave fuera del esquema: ni totales, ni cuentas, ni hashes de lo omitido. Su `seq` es 1, y el de A, 2: cada ámbito tiene su propia revisión, y la de lo público no se movió con la observación del MCP, que solo ven los de `demo`.
- `pals query b` desde G: `not_offered` para `code.rust.review`, el servicio de B solo para `demo`, igual que para `example:no-existe`, que B no tiene.
- El cursor de A no sirve a G, ni el de G a A, con el mismo error en los dos sentidos.

### Caso 7. Contacto A2A corriente: superado

F por `https://pals-exit-h2-f:8443`, con el certificado de la CA de prueba instalada en el almacén del sistema de A:

```text
$ pals contact add-a2a https://pals-exit-h2-f:8443 --name f
saved contact f: https://pals-exit-h2-f:8443 (Agente F), card signature verified f-1
$ pals ask f "hola, F"
F: hola, F
código 0
$ curl … $(gw)/peers/f/jsonrpc        (SendMessage)    -> task TASK_STATE_COMPLETED, artefacto "F: por JSON-RPC"
$ curl … $(gw)/peers/f/rest/message:send               -> task TASK_STATE_COMPLETED, artefacto "F: por HTTP+JSON"
$ curl $(gw)/peers/f/.well-known/agent-card.json       -> interfaces http://127.0.0.1:7420/peers/f/jsonrpc y …/peers/f/rest, sin securitySchemes
$ pals discover --capability example:eco --peer f --peer f-alterada
ranked:
  1. f (plain A2A agent): card only, skill eco, time unknown, …, no grant needed, card verified f-1
discarded:
  f-alterada (card): card not verified
$ pals contact add-a2a http://pals-exit-h2-f:8443 --name f-http
Error: a plain A2A agent's URL is https://<host>[/<path>]: http:// is only for 127.0.0.1, [::1] and localhost
código 1
```

La card de F la firma el propio SDK de Python (`create_agent_card_signer`, `EdDSA`, `kid` `f-1`, `jku` en el origen de F), y A la verifica con el JWK Set de ese origen. El certificado de la CA de prueba sirvió: no hizo falta el camino de reserva por `http://` a loopback.

### Caso 8. Migración: superado

En `pals-exit-h2-m`, `caso8.sh` del runbook: M1 y M2 con el `pals` del hito 1, en loopback y sin relay. M2 guarda a M1, recibe de él dos grants (uno de `demo`, y otro, solo para blobs, que M1 revoca) y le pregunta con un fichero adjunto (`echo: hola desde el hito 1`). Con los dos daemons parados, el `pals` del hito 2 arranca sobre los mismos directorios:

```text
m1: esquema 1, contacts 1, grants_issued 2, grants_held 0, revocations 1, tasks 1, task_owners 1, dedup 1, events 5, outbox 0
m2: esquema 1, contacts 1, grants_issued 1, grants_held 2, revocations 0, tasks 0, task_owners 0, dedup 0, events 1, outbox 1
== hito 2
m1: esquema 1 -> 2
  contacts, grants_issued, grants_held, revocations, tasks, task_owners, dedup, events, outbox: conservadas
m2: esquema 1 -> 2
  contacts, grants_issued, grants_held, revocations, tasks, task_owners, dedup, events, outbox: conservadas
m1: contact ls, grant ls y task ls, iguales con las dos versiones
m2: contact ls, grant ls y task ls, iguales con las dos versiones
M2 pregunta a M1 con el hito 2: echo: hola desde el hito 2
events_verify: 7 events of <M1> verified
events_verify: 1 events of <M2> verified
m1 eventos: grant.issued, grant.issued, grant.revoked, task.accepted, task.terminal, task.accepted, task.terminal
m2 eventos: blob.shared
caso 8: superado
```

«Conservadas» compara, tabla por tabla, cada fila de las nueve tablas del hito 1 antes y después, columna por columna de las del hito 1. Los listados siguen siendo los mismos: el grant revocado sigue `revoked`, y la pregunta del hito 1, `TASK_STATE_COMPLETED`. `events_verify` ([`crates/pals-core/examples/events_verify.rs`](../../../crates/pals-core/examples/events_verify.rs)) es `Store::events_verify` sobre el directorio migrado. La cadena de M1 son los cinco eventos firmados por el hito 1 y, a continuación, los dos de la pregunta del hito 2, y verifica entera: secuencia sin huecos, hash del anterior, hash del contenido y firma de cada uno. Con `relay = false` el puerto de Iroh cambia en cada arranque, así que M2 guardó otra vez el ticket de M1 antes de la segunda pregunta.

### Caso 9. Sin regresión A2A: pendiente

Pendiente: ver [`README.md`](README.md) de este directorio.

## Métricas del §14

Salvo que se diga otra cosa, las da `SearchMetrics` de cada búsqueda, medida por el buscador de A, en las 58 búsquedas que A respondió por su gateway (`metricas.py` del runbook sobre sus respuestas: el caso 2, las 54 del caso 4 y las dos del caso 5b), más la del caso 1 (línea `metrics:` de `pals discover`).

| Métrica | Valor | Cómo se midió |
|-|-|-|
| Tiempo hasta el primer candidato válido | 63 ms en el caso 1. En las 58: mediana 131 ms, mínimo 20 ms, máximo 2873 ms (5b, intento 1, con B recién rearrancado). En las 50 seguidas: mediana 132 ms, máximo 281 ms | `time_to_first_valid_ms`: desde que empieza la búsqueda hasta que termina la evaluación del primer candidato que queda en el ranking, en el reloj monótono de A |
| Porcentaje de búsquedas con candidato | 58 de 58 respondidas (100 %); 58 de 104 lanzadas (56 %), porque la cuota de A rechazó 46 de la ráfaga | Respuestas con `ranked` no vacío, sobre las respondidas y sobre las lanzadas. Además, por la CLI: caso 1, 5a y 7, con candidato; caso 3, sin él, como se buscaba (solo F alterado) |
| Mensajes por búsqueda | 15 con cinco candidatos (3 por candidato: card pública, card extendida y consulta); 6 con B, C y H (H no llega a responder) | `messages`: peticiones que envió la búsqueda: cards, consultas e informes de benchmark (ninguno pedido aquí) |
| Bytes por búsqueda | Con cinco candidatos: 5480 B enviados y 30813 B recibidos, en todas. Con B, C y H: unos 2190 B y 12318 B | `bytes_sent`: las tramas de las consultas; `bytes_received`: los cuerpos de las cards y las tramas de las respuestas. No incluye la sobrecarga de QUIC y TLS. Por la red, en el caso 1 con su pregunta y la respuesta, los contadores de `iptables` de A dan 77,6 KB enviados y 88,2 KB recibidos por UDP |
| Candidatos contactados | 5 en los casos 1, 2 y 4; 3 en el 5b; 1 en el 5a | `contacted`: candidatos a los que la búsqueda envió algo |
| Coste del matching | Caso 1: 16 µs. En las 58: mediana 20 µs, mínimo 12 µs, máximo 121 µs | `matching_us`: lo que tardan en A las dos etapas, la de la card y el ranking. **No medido:** el coste de evaluar la consulta en cada proveedor, que ninguno informa |
| Filtraciones de metadatos observadas | Ninguna | Caso 6: en las páginas públicas de B que recibió G, búsqueda de los cuatro ids de proyecto y de `demo` y `project`, y de claves fuera del esquema (ninguna), comparación con la vista pública de B (igual), `not_offered` igual para el servicio de proyecto y para uno inexistente, el mismo error para los dos cursores ajenos. Caso 3: la card pública de B, la que lee cualquiera, solo tiene la skill `ask`. **No observado:** el contenido de las consultas por la red, cifrado por QUIC; que las consultas a quien no tiene grant van sin ids ni proyecto lo cubre `selection::inventory_requirement_ids_go_only_to_authorized_candidates` |

## Correcciones antes de esta ejecución

Seis ensayos completos o parciales del arnés llevaron a esta ejecución. Un caso fallido se corrigió y se repitió:

1. **Producto: `pals discover` no nombraba las ofertas (`eea2281`).** `pals ask --offer` pide la oferta «por su id, como la imprimió `pals discover`», y una vencida dice «run pals discover again», pero el ranking no imprimía ningún id de oferta: el operador no podía buscar y preguntar después con la oferta, como hace el caso 5a. Ahora cada candidato con oferta termina su línea con `, offer <id> until <expires_at>`. Es salida de la CLI; el cable no cambia. Test de regresión `selection::ask_names_the_offer_discover_printed`, que falló antes de la corrección (`no offer named`) y pasa después; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked` y `cargo +1.91 check --workspace --all-targets --locked`, en verde.
2. **Runbook y arnés: la primera búsqueda con las conexiones sin abrir.** En un ensayo, el caso 1 empezó sin que A se hubiera conectado aún a ningún proveedor. Cada candidato agotó sus 3 s (`b (card): timeout` … `g (offer): timeout`, 6 s en total) y no se ejecutó nada: `no candidate took the task`, código 2. El runbook comprueba ahora, al guardar los contactos, que A ve la card firmada de cada proveedor, que es también su primera conexión con cada uno. Explica además qué significa una búsqueda cortada solo por plazos, y el arnés la repite hasta tres veces. No se reprodujo: cinco búsquedas justo después de rearrancar A, con todas las conexiones por abrir, tardaron entre 375 y 478 ms (observación 1).
3. **Arnés: los plazos y las cuotas no son anomalías.** En el caso 4, un candidato descartado por la cuota de un proveedor (`refused: RateLimited`) o por sus plazos (`timeout`, `time budget`) se cuenta aparte. Lo comprobado es que nada se ejecuta y nada se cae.
4. **Arnés: un caso 5 que sí pruebe la renovación.** Si la oferta de B sigue en vigor al preguntar, el intento no prueba la renovación: el arnés repite 5b con otra pregunta, hasta tres veces, y exige que alguna renueve.

## Observaciones

1. **El plazo de 3 s por candidato incluye abrir la conexión.** Con las conexiones por abrir, la búsqueda tiene que hacerlo dentro de esos 3 s: el dial de Iroh, una conexión QUIC para el binding A2A (las cards) y otra para `agentnet-discovery/0` (la consulta). En el ensayo del punto 2 de «Correcciones», con la carga del anfitrión subiendo hasta 32, no le bastaron. Es el valor por defecto del §7.3, configurable (`per_candidate_timeout_ms`, hasta 10000), y la búsqueda informa bien lo que pasó. En redes reales, con relay o NAT, convendría medir cuánto cuesta esa primera conexión antes de decidir si se cuenta dentro del plazo del candidato. La cobertura de esa búsqueda decía `complete` aunque todos los candidatos agotaran su plazo: es lo especificado (solo el presupuesto total y el límite de candidatos la hacen parcial), pero ese «complete» puede leerse como «todos respondieron».
2. **5b, intento 1.** B acababa de rearrancar, y A tuvo que volver a conectarse a él dentro de la búsqueda. La oferta de B llegó a los 2873 ms, y su caducidad, por segundos enteros, aún no había pasado cuando A preguntó: se preguntó con ella, B la aceptó y la ejecutó una vez. En el intento 2, con la conexión ya abierta, la oferta llegó pronto, venció y se renovó.
3. **El temporizador del adaptador no venció durante la prueba.** Los proveedores lo tienen a 60 s y vivieron menos de un minuto (de 06:21:30 a 06:22:20). Los `tools/list` contados son el arranque y `pals inventory refresh`; que el temporizador los repite cada `refresh_interval_s` lo prueban los tests del adaptador. El contador sí detecta cada llamada: las dos de cada proveedor y la tercera de B, al rearrancar.
4. La carga del anfitrión fue de 20 a 31, no de 60 a 100 como otras veces. Con más carga, los tiempos de las búsquedas crecen y el caso 1 puede necesitar repetirse (punto 2 de «Correcciones»).

## Lo que no cubre

- Todos los agentes comparten el kernel, el reloj y la máquina, y entre ellos solo hay el bridge de Docker. No hay varias máquinas, redes ni NAT, y los relays solo se usaron para registrarse: el camino fue UDP directo. Los relojes nunca difieren, así que el desfase de 60 s de ofertas, vistas y registros no se ejercita aquí; lo hacen los tests (`expired_record_is_refused_beyond_skew`, `expired_page_is_refused_beyond_skew`).
- F es un agente corriente sobre el SDK oficial de Python, en un contenedor, con un certificado de una CA de prueba: no un agente de terceros en internet.
- La card alterada de un par PALS solo la sirven los pares de prueba de los tests de integración, en el anfitrión; en vivo, la card alterada es la de F.
- El caso 9, el TCK y el cliente de Python, se registra aparte ([`README.md`](README.md)).
