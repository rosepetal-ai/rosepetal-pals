# Prueba de salida del hito 2

Este documento recoge la ejecución de los casos del criterio de salida del hito 2 (spec §14), siguiendo el runbook [`docs/runbooks/hito-2-consulta-de-servicios.md`](../../runbooks/hito-2-consulta-de-servicios.md), con un contenedor Docker por agente. La ejecuta el arnés [`tests/exit/hito-2.sh`](../../../tests/exit/README.md#prueba-de-salida-del-hito-2-con-docker).

**No es una prueba en varias máquinas físicas.** Los agentes comparten kernel, reloj y máquina, y entre ellos solo hay el bridge de Docker (ver [«Lo que no cubre»](#lo-que-no-cubre)).

## Resultado (2026-11-15)

Casos 1 a 8 superados en la ejecución registrada, a la primera, y superada también la comprobación final sobre toda la prueba: ningún servidor MCP recibió un `tools/call`. Es la ejecución sobre el código final del hito, después de la tanda de correcciones de su revisión final; repite la anterior, registrada en `818e68c`, que también superó los ocho casos. El caso 9 se repitió también sobre ese código, el 2026-11-15, sin cambios (ver [`README.md`](README.md) de este directorio). Los ensayos sacaron a la luz un fallo de producto, corregido en `eea2281`, y varias correcciones del arnés y del runbook; la revisión de la tarea y la revisión final, otras, que esta ejecución ya incluye (ver [«Correcciones»](#correcciones-antes-de-esta-ejecución)).

| Caso | Resultado | Evidencia | Tiempo |
|-|-|-|-|
| 1. Elegir entre al menos tres candidatos | Superado | `pals discover … --project demo --ask`: B `available` y C `busy` en el ranking; D, E y G descartados con su motivo; responde B (`b: Revisa src/lib.rs`), código 0; una ejecución en B y ninguna en C, D, E ni G | 2 s |
| 2. Requisitos sobre integraciones y recursos | Superado | E descartado `mcp_integration:example:everything not operational`; G `min_offered_ram_mib not met`; B cumple el MCP con base `observed`. En la vista de B, su MCP `ok` por `mcp tools/list` con `echo` y `get-sum`; en la de E, `failed`, `missing tools: read_file` | < 1 s |
| 3. Validar cards | Superado | La card firmada de B, por el gateway de A: `3 checks passed` (a2a-sdk), `verified` (a2a-go), `verified` (@a2a-js/sdk); solo la skill `ask`, nada de lo que es de proyecto. Los pares de prueba con la card alterada: los dos tests en verde. F alterado, con su JWK Set accesible: `card signature unverifiable` y descartado `card not verified` | 9 s |
| 4. No ejecutar trabajo al consultar | Superado | Ráfaga de 50: 4 `200` y 46 `429` (`too many searches`). 50 seguidas, de 4 en 4: 50 `200`, las 50 con el ranking completo (se exigen 45). En B, C, D, E y G: `task.accepted` +0, ejecuciones +0, `tools/list` +0 (el temporizador de B podía vencer una vez en el caso, y no venció), el mismo PID; ningún `tools/call` | 16 s |
| 5. Oferta vencida y proveedor ocupado | Superado | 5a: oferta de B vencida por el reloj de A, `offer expired: run pals discover again`, código 2, nada enviado. 5b: C `busy`, H `timeout`, la cobertura `partial` (`timeout`); la oferta de B renovada (`offer-8754ffe1…` → `offer-53983388…`) y la pregunta, una ejecución en B y ninguna en C | 8 s |
| 6. Vistas y caché por permisos | Superado | A (`task:demo`): 124 entradas en dos páginas, con las de proyecto. G (`public`): las 120 notas públicas, cada una idéntica a la de la vista pública de B, y nada más; ninguna clave fuera del esquema. Los cursores cruzados, `400` con el mismo error en los dos sentidos | 2 s |
| 7. Contacto A2A corriente | Superado | F por `https`, con la CA de prueba en A: `card signature verified f-1`; `pals ask f`: `F: hola, F`; JSON-RPC y HTTP+JSON por el gateway de A, `TASK_STATE_COMPLETED`; `http://` a otra máquina, rechazado | < 1 s |
| 8. Migración | Superado | M1 y M2 del hito 1: esquema 1 → 2, cada fila de las nueve tablas del hito 1 conservada; `contact ls`, `grant ls` y `task ls` iguales con las dos versiones; una pregunta nueva con el hito 2; `events_verify: 7 events … verified` (M1) y `1 events … verified` (M2) | 6 s |
| 9. Sin regresión A2A | Superado | TCK con código 0 y ningún MUST fallido: 58 superados en `jsonrpc` y 57 en `http_json`, `CARD-EXT-001` superado y `CORE-CAP-003` saltado, porque la card declara ahora card extendida; el SDK de Python, `4 checks passed`; `check_schemas.py`, `7 documents valid`. Repetido sobre el código final: requisito a requisito, sin cambios. Ver [`README.md`](README.md) | 4 min |
| Sin `tools/call`, toda la prueba | Superado | Al desmontar, en lo que recibió el servidor MCP de cada proveedor y de H: `b 0 c 0 d 0 e 0 g 0 h 0` | |

Las líneas del arnés:

```text
RESULTADO caso 1: superado (intentos: 1; ejecuciones nuevas: b 1 c 0 d 0 e 0 g 0; metrics: contacted 5, messages 15, bytes sent 5565, bytes received 30813, first valid 261 ms, matching 30 us, elapsed 665 ms)
RESULTADO caso 2: superado (mcp b: ok, mcp tools/list, -, herramientas [example:everything/echo example:everything/get-sum];mcp e: failed, mcp tools/list, missing tools: read_file, herramientas [];)
RESULTADO caso 3: superado (Python, Go y JS verifican la card de B; las alteradas, rechazadas)
RESULTADO caso 4: superado (rafaga:  4 200, 46 429, seguidas:  50 200, ; seguidas con el ranking completo: 50 de 50 (se exigen 45); b: task.accepted +0, ejecuciones +0, tools/list +0 (temporizador 1); c: task.accepted +0, ejecuciones +0, tools/list +0 (temporizador 0); d: task.accepted +0, ejecuciones +0, tools/list +0 (temporizador 0); e: task.accepted +0, ejecuciones +0, tools/list +0 (temporizador 0); g: task.accepted +0, ejecuciones +0, tools/list +0 (temporizador 0); tools/call: b 0 c 0 d 0 e 0 g 0 h 0)
RESULTADO caso 5: superado (intentos: 1; oferta del ranking offer-8754ffe1… 2026-11-15T10:05:16Z;oferta usada offer-53983388… 2026-11-15T10:05:19Z;renovada;)
RESULTADO caso 6: superado (cursores cruzados {'error': 'peer refused: invalid cursor'} {'error': 'peer refused: invalid cursor'} True)
RESULTADO caso 7: superado (F por https con la CA de prueba; jsonrpc TASK_STATE_COMPLETED F: por JSON-RPC;rest TASK_STATE_COMPLETED F: por HTTP+JSON;)
RESULTADO caso 8: superado (m1: esquema 1 -> 2;m2: esquema 1 -> 2;events_verify: 7 events of <M1> verified;events_verify: 1 events of <M2> verified;)
RESULTADO sin tools/call en toda la prueba: superado (b 0 c 0 d 0 e 0 g 0 h 0)
```

### Entorno

| Dato | Valor |
|-|-|
| Fecha | 2026-11-15, de 09:59:25 a 10:05:36 UTC, la construcción de la imagen incluida (hasta las 10:03:49) |
| Commit | `1df72ec`: el código final del hito, con la tanda de correcciones de la revisión final (ver «Correcciones»), que `preparar` encontró sin cambios sin commit. Después de esta ejecución solo cambian este registro y una frase del README del arnés |
| Binario | `cargo build --release --locked -p pals` en el anfitrión: `pals 0.1.0`, el mismo fichero en todos los contenedores (sha256 `2914f58e…`) |
| Caso 8 | El `pals` del hito 1, de `71092a0` (`git archive 71092a0`, `cargo build --release --locked -p pals` con su propio `CARGO_TARGET_DIR`; sha256 `42ed9f66…`), y el ejemplo `events_verify` de `pals-core` |
| Rust | 1.99.1 |
| Versiones fijadas | `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`, `a2a-pb =0.3.2`, `rmcp =3.10.0` |
| Verificadores de cards | `a2a-sdk==1.3.0` (Python 3.13.5, en A); `github.com/a2aproject/a2a-go/v2 v2.7.0` (Go 1.27.3, en el anfitrión); `@a2a-js/sdk` 1.3.0 (Node 26.13.0, en el anfitrión) |
| F | `a2a-sdk==1.3.0` con las dependencias de [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt), y `starlette==1.10.0`, `sse-starlette==3.6.0`, `uvicorn==0.54.0` y `click==8.5.0` ([`requirements-f.txt`](../../../tests/exit/hito-2/requirements-f.txt)) |
| Servidor MCP | `@modelcontextprotocol/server-everything` 2026.8.31 (se anuncia como `mcp-servers/everything` 2.0.0) por stdio, con `@modelcontextprotocol/sdk` 1.32.2 y el resto de sus dependencias fijadas en [`package-lock.json`](../../../tests/exit/hito-2/mcp/package-lock.json); Node 20.19.2 de Debian |
| Imagen | `pals-exit-h2:latest` (id `af2e77bd…`), construida por el arnés desde [`tests/exit/hito-2/Dockerfile`](../../../tests/exit/hito-2/Dockerfile) sobre `debian:trixie-slim@sha256:a72b4433…` (Debian 13) |
| Anfitrión | Linux x86_64, kernel 6.12 de Debian 13, 32 CPU, compartido con otros proyectos. Carga media al empezar: 85,9 / 83,8 / 66,0; durante la ejecución osciló entre 60 y 90. El arnés corre en un entorno Ubuntu 24.04.4 con Docker 29.4.1 |
| Contenedores | Ocho, en la red bridge `pals-exit-h2-net` (172.28.0.0/16): `a` .2, `b` .3, `c` .4, `d` .5, `e` .6, `g` .7 (G y H), `f` .8, `m` .9. Solo A tiene `NET_ADMIN`, para contar bytes. En cada uno, los ficheros que git sigue, tal como estaban en el árbol de trabajo, sin ninguno sin seguimiento; ese árbol no tenía cambios sin commit respecto a `1df72ec` |
| Relays | `net.relay = true` en los siete daemons de PALS con relay, A, B, C, D, E, G y H (M1 y M2, en loopback, van sin él): los relays públicos de n0. Los siete eligieron `euc1-1.relay.n0.iroh.link`, y B otra vez al rearrancar en el caso 5; A lo alcanzaba por HTTPS antes de empezar (`200` en 0,21 s) |

El registro completo de la ejecución quedó en la máquina de quien la hizo, sin versionar. Aquí se copia lo que respalda cada resultado. Los ids de agente se escriben `<A>`, `<B>`…, y los hashes y los ids de oferta se acortan. No se copian tickets ni tokens.

### Montaje y camino de red

El arnés construye la imagen, crea la red y los ocho contenedores, copia el binario y los ficheros que git sigue a cada uno y extrae del runbook sus ocho guiones de `~/pals-h2`. Cada orden en un contenedor es una terminal nueva que carga `funciones.sh` y elige su agente, como manda el runbook. Los tickets y los tokens pasan de un contenedor a otro sin imprimirse.

- **F (2.1).** Una CA de prueba (P-256, 7 días) y el certificado de F para `pals-exit-h2-f`, firmado por ella; F en el 8443 con su card firmada y en el 8444 con la card alterada. Los dos procesos comparten la clave de firma, que el primero crea de forma atómica. El certificado de la CA pasa a A por el anfitrión; su clave no sale de F.
- **A (2.2).** La CA en su almacén del sistema (`update-ca-certificates`) antes de arrancar. A alcanza a F por `https` en los dos puertos, su card y su JWK Set.
- **Proveedores (2.3).** `pals inventory refresh` en cada uno: `observed 1, ok 1, failed 0` en B, C, D, G y H; en E, `observed 1, ok 0, failed 1` y `failed: example:everything missing tools: read_file`. B recibió de su servidor `"serverInfo":{"name":"mcp-servers/everything","title":"Everything Reference Server","version":"2.0.0"}`.
- **Contactos y grants (2.4).** Cada proveedor de B a G guarda a A y le da un grant de `demo` para `task.send,task.read,task.cancel` en `skill:ask`; G guarda a B, sin grant. A ve la card firmada de cada uno: cinco `verified: <x> blake3:…`.
- **C, lleno (2.5).** `pals ask c "espera 3600" --wait 10s`: código 3, y la tarea sigue en C, su única plaza.

**Cómo se determinó el camino.** PALS no informa del camino de Iroh. Como en el hito 1, el arnés añade al final de las cadenas de A cuatro reglas de `iptables`, dentro del contenedor de A, que solo cuentan bytes por su `eth0`: UDP salvo DNS (el camino directo entre contenedores) y TCP 443 (los relays). En el caso 1, **camino mixto**: 81,1 KB enviados y 89,3 KB recibidos por UDP, y 39,3 KB y 28,3 KB por TCP 443. Es más que el tráfico de fondo con el relay de la ejecución anterior (8,1 KB y 5,7 KB): con la carga del anfitrión cerca de 85, parte de las primeras conexiones fue por el relay antes de pasar a UDP directo (observación 5).

### Caso 1. Elegir entre al menos tres candidatos: superado

```text
$ pals discover --capability code.rust.review --requirements-file req.json --project demo --ask "Revisa src/lib.rs"
ranked:
  1. b (agent <B>): available, skill repo-review, time 1100 ms, preferred met 0, benchmark none, completed 0 failed 0, grant held, card verified <B>, offer offer-a066796f… until 2026-11-15T10:04:58Z
  2. c (agent <C>): busy, skill repo-review, time 110 ms, preferred met 0, benchmark none, completed 0 failed 0, grant held, card verified <C>, offer offer-4dfd5682… until 2026-11-15T10:04:58Z
discarded:
  d (offer): external_cloud_allowed not met
  e (offer): mcp_integration:example:everything not operational
  g (offer): min_offered_ram_mib not met
coverage: complete
metrics: contacted 5, messages 15, bytes sent 5565, bytes received 30813, first valid 261 ms, matching 30 us, elapsed 665 ms
asked: b
b: Revisa src/lib.rs
código 0
```

Cinco candidatos consultados; C, más rápido que B (110 ms frente a 1100 ms), va detrás porque está lleno. Ejecuciones nuevas, contadas en el registro del worker de cada proveedor: B 1, C 0, D 0, E 0, G 0. La búsqueda nombra el proyecto (`--project demo`): desde la revisión final, los ids de inventario de los requisitos solo van a los candidatos con un grant de ese proyecto, y una búsqueda sin proyecto no se los envía a ninguno. Cada consulta lleva por eso `"project":"demo"`, y la búsqueda envía 5565 bytes, 17 más por candidato que en la ejecución anterior.

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

E declara la conexión y ofrece `read_file`, que server-everything no sirve: su adaptador la observa `failed`, y la oferta de E no cumple el requisito. G ofrece 1024 MiB de RAM a una tarea, menos de los 4096 pedidos, aunque tiene 65536 instalados: el requisito se compara con lo ofrecido. B cumple el MCP por la observación de su adaptador, `initialize` y `tools/list` contra el servidor real, y nunca un `tools/call` (caso 4 y la comprobación final).

### Caso 3. Validar cards: superado

La card firmada de B, `GET /peers/<B>/signed-agent-card.json` del gateway de A (1849 bytes, sha256 `1b0a4e8d…`). El arnés comprueba que es la pública: solo la skill `ask`, y ninguna mención de lo que es de proyecto (`repo-review`, `example:everything`, `example:coder`, `example:host`, `demo`):

```text
skills ask
menciones de proyecto ninguna
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

Y en vivo, F alterado, un agente corriente cuya firma ya no cubre su card. Su JWK Set responde (lo comprobó `preparar`), así que `unverifiable` solo puede ser la firma:

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
50 búsquedas, de 50 en 50, en 1356 ms; códigos HTTP:
      4 200
     46 429
50 búsquedas, de 4 en 4, en 10715 ms; códigos HTTP:
     50 200
rafaga: 50 búsquedas, códigos {'200': 4, '429': 46}, completas 4, descartes por cuotas o plazos ninguno, anómalas ninguna
seguidas: 50 búsquedas, códigos {'200': 50}, completas 50, descartes por cuotas o plazos ninguno, anómalas ninguna
healthz: a 200 b 200 c 200 d 200 e 200 g 200
búsqueda de después -> HTTP 200
tools/call recibidos hasta ahora por cada servidor MCP: b 0 c 0 d 0 e 0 g 0 h 0
```

| Proveedor | `task.accepted` | Ejecuciones | `tools/list` | PID |
|-|-|-|-|-|
| B | 1 → 1 | 1 → 1 | 2 → 2 | 99 → 99 |
| C | 1 → 1 | 1 → 1 | 2 → 2 | 99 → 99 |
| D | 0 → 0 | 0 → 0 | 2 → 2 | 99 → 99 |
| E | 0 → 0 | 0 → 0 | 2 → 2 | 101 → 101 |
| G | 0 → 0 | 0 → 0 | 2 → 2 | 99 → 99 |

Lo que el arnés exige, y se cumplió:

- **La cuota del daemon recorta la ráfaga:** al menos un `200` y un `429`. A hace a lo sumo 4 búsquedas a la vez, y responde a las demás, en el acto, `429` con `{"error":"too many searches"}`.
- **Las búsquedas llegan a los proveedores:** de las 50 seguidas, al menos 45 con el ranking completo, B y C en el ranking y D, E y G descartados por sus propias ofertas, sin descartes por cuotas o plazos (`timeout`, `time budget`, `refused: RateLimited`). Fueron 50 de 50. Entre los casos 1 y 4, en unos 27 s, cada proveedor recibió de A 57 consultas (58 peticiones B y E, con la lectura de su inventario del caso 2), por debajo de su cuota de 60 por minuto, y ninguno rechazó ninguna.
- **Nada se ejecuta:** los mismos `task.accepted` y ejecuciones (los que ya había son el caso 1 en B y la tarea larga en C), y los mismos PID.
- **`tools/list`, solo por el temporizador:** cada proveedor puede recibir como mucho uno por cada vencimiento de su temporizador de 60 s durante el caso, contado desde su primera comprobación, la primera línea de su `*-mcp-arranques.log`, con 3 s de margen. Los proveedores arrancaron de 10:04:12 a 10:04:21 y el caso fue de 10:04:55 a 10:05:09: solo el de B podía vencer en él, y no venció dentro; ningún `tools/list` nuevo.
- **Ningún `tools/call`** en lo que recibió el servidor MCP de cada proveedor y de H; y otra vez, sobre toda la prueba, al desmontar. La comprobación falla cerrada: una copia que falta o no se puede leer, o una cuenta que no es un número, la hace fallar.

Los `tools/list` de toda la prueba, por la hora de cada arranque del servidor MCP: en cada proveedor y en H, uno al arrancar su daemon y otro por `pals inventory refresh` (de 10:04:12 a 10:04:26), todos antes del caso 4; después del caso 4, el temporizador de 60 s en C, D, E y G (de 10:05:14 a 10:05:22), y en B, uno al rearrancar en el caso 5 (10:05:12). H, parado en el caso 5, se quedó en dos (observación 3).

### Caso 5. Oferta vencida y proveedor ocupado: superado

H guarda a A, A guarda a H con su grant, y H se para. B vuelve a arrancar con `offer_valid_for_ms = 2000`; A lo vuelve a encontrar por su id, sin guardar otra vez su ticket. C sigue lleno.

**5a.** La oferta que da `pals discover`, usada cuando vence por el reloj de A:

```text
$ pals discover --capability code.rust.review --requirements-file req.json --project demo --peer b
  1. b (agent <B>): available, …, card verified <B>, offer offer-dd3775a5… until 2026-11-15T10:05:14Z
la oferta offer-dd3775a5… vence 2026-11-15T10:05:14Z; ahora 2026-11-15T10:05:14Z
$ pals ask b "pregunta con la oferta vencida" --offer offer-dd3775a5…
pals: offer expired: run pals discover again
código 2
```

B no ejecutó nada: A no envió la pregunta.

**5b.** La búsqueda del caso 1 entre B, C y H, con pregunta, por el gateway de A. H, caído, agota los 3 s de su candidato, y la oferta de B, de 2 s, vence entre la búsqueda y la pregunta:

```text
ranking b:available c:busy
descartes h: timeout
busqueda 3001 ms
elegido b TASK_STATE_COMPLETED b: pregunta del caso 5, intento 1
probados []
oferta del ranking offer-8754ffe1… 2026-11-15T10:05:16Z
oferta usada       offer-53983388… 2026-11-15T10:05:19Z
renovada
```

La cobertura de esa búsqueda es `partial`, con el motivo `timeout`: desde la revisión final, un candidato que agota su propio plazo hace parcial la cobertura, y la búsqueda ya no dice `complete` como si todos hubieran respondido. A vio vencida la oferta del ranking, la renovó, una vez, y preguntó con la nueva, que B aceptó. La pregunta se ejecutó **una** vez en B (`grep -c` en su registro de ejecuciones: 1), ninguna en C (lleno, nunca preguntado) y ninguna en H. La renovación la prueba el id de la oferta usada, distinto del de la del ranking; el arnés no acepta otra señal.

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

- G, sin grant, ve las 120 notas públicas y nada más. El arnés compara cada entrada entera, campos y dimensiones incluidos, con la vista pública que B renderiza para sí (`pals inventory show --scope public`): iguales. En las dos páginas no aparece ni un id de lo que es de proyecto ni las palabras `demo` o `project`, y no hay ninguna clave fuera del esquema: ni totales, ni cuentas, ni hashes de lo omitido. Su `seq` es 1, y el de A, 2: cada ámbito tiene su propia revisión, y la de lo público no se movió con la observación del MCP, que solo ven los de `demo`.
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

La card de F la firma el propio SDK de Python (`create_agent_card_signer`, `EdDSA`, `kid` `f-1`, `jku` en el origen de F), y A la verifica con el JWK Set de ese origen. El certificado de la CA de prueba sirvió: no hizo falta el camino de reserva por `http://` a loopback. Desde la revisión final, `pals ask f` lee antes la card de F por el gateway de A: declara `streaming` y las dos interfaces, así que el outbox pregunta por HTTP+JSON; como no declara la extensión de idempotencia, solo reenviaría la pregunta si es seguro que no le llegó. Llegó y la contestó a la primera, y el gateway le pidió respuestas sin comprimir (`Accept-Encoding: identity`).

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

### Caso 9. Sin regresión A2A: superado

En el anfitrión, sobre el código final del hito (`7a60c7c`; sus informes los guarda `1df72ec`). Repite la ejecución anterior, en `812f885` con el código de `eea2281`. Son las órdenes del runbook:

```text
$ PALS_CONFORMANCE_DIR=docs/conformance/hito-2 tests/interop/run_tck.sh
│ MUST   │ 72     │ 0      │ 20      │ 7      │ 16         │ 115   │
  jsonrpc:       75/90 (15 skipped) ✓
  http_json:     71/85 (14 skipped) ✓
[run_tck] TCK --level must exited 0
$ target/interop-venv/bin/python tests/interop/python_client.py
4 checks passed
$ target/interop-venv/bin/python tests/interop/check_schemas.py
7 documents valid
```

Requisito a requisito, los mismos estados y transportes que en la ejecución anterior, en los dos niveles. Por requisito, en `jsonrpc` hay 58 MUST superados (58 en el hito 1) y en `http_json`, 57 (56). Solo dos requisitos cambian de estado respecto al hito 1, los dos por la card extendida que la card declara ahora. `CARD-EXT-001` pasa a superado. `CORE-CAP-003`, que el TCK solo prueba sin card extendida, pasa a saltado. Los informes, los requisitos uno a uno y lo que queda respaldado están en [`README.md`](README.md). Los informes del hito 1 no cambian.

## Métricas del §14

Salvo que se diga otra cosa, las da `SearchMetrics` de cada búsqueda, medida por el buscador de A. Son las 57 búsquedas que A respondió por su gateway, sobre las 103 que se le lanzaron: el caso 2, las 54 respondidas del caso 4, la de después y la del caso 5b, con `metricas.py` del runbook sobre sus respuestas. Se añade la del caso 1, con la línea `metrics:` de `pals discover`.

| Métrica | Valor | Cómo se midió |
|-|-|-|
| Tiempo hasta el primer candidato válido | 261 ms en el caso 1. En las 57: mediana 489 ms, mínimo 96 ms, máximo 2795 ms. En las 50 seguidas: mediana 498,5 ms, máximo 2795 ms, con la carga del anfitrión entre 60 y 90 (en la ejecución anterior, con 25 a 31: mediana 94 ms) | `time_to_first_valid_ms`: desde que empieza la búsqueda hasta que termina la evaluación del primer candidato que queda en el ranking, en el reloj monótono de A |
| Porcentaje de búsquedas con candidato | 57 de 57 respondidas (100 %); 57 de 103 lanzadas (55 %), porque la cuota de A rechazó 46 de la ráfaga | Respuestas con `ranked` no vacío, sobre las respondidas y sobre las lanzadas. Además, por la CLI: caso 1, 5a y 7, con candidato; caso 3, sin él, como se buscaba (solo F alterado) |
| Mensajes por búsqueda | 15 con cinco candidatos (3 por candidato: card pública, card extendida y consulta); 6 con B, C y H | `messages` (`search.rs`): cada consulta y cada petición de informe de benchmark (ninguno pedido aquí) cuentan al enviarse, hayan tenido respuesta o no; cada card, pública o extendida, cuenta solo cuando se leyó bien. H, caído, no suma: su card no llegó, así que no se le envió consulta. Cuenta en `contacted`, que es a quién intentó la búsqueda llegar |
| Bytes por búsqueda | Con cinco candidatos: 5565 B enviados y 30813 B recibidos, en todas. Con B, C y H: 2226 B y 12318 B | `bytes_sent`: las tramas de las consultas, que ahora nombran el proyecto (17 B más cada una); `bytes_received`: los cuerpos de las cards y las tramas de las respuestas. No incluye la sobrecarga de QUIC y TLS. Por la red, en el caso 1 con su pregunta y la respuesta, los contadores de `iptables` de A dan 81,1 KB enviados y 89,3 KB recibidos por UDP, y 39,3 KB y 28,3 KB por TCP 443 |
| Candidatos contactados | 5 en los casos 1, 2 y 4; 3 en el 5b; 1 en el 5a | `contacted`: candidatos a los que la búsqueda envió algo, respondieran o no |
| Coste del matching | Caso 1: 30 µs. En las 57: mediana 25 µs, mínimo 17 µs, máximo 76 µs | `matching_us`: lo que tardan en A las dos etapas, la de la card y el ranking. **No medido:** el coste de evaluar la consulta en cada proveedor, que ninguno informa |
| Filtraciones de metadatos observadas | Ninguna | El arnés comprueba, en el caso 6, las páginas públicas de B que recibió G: ni los cuatro ids de proyecto, ni `demo` ni `project`, ninguna clave fuera del esquema, y cada entrada igual a la vista pública de B; `not_offered` igual para el servicio de proyecto y para uno inexistente; el mismo error para los dos cursores ajenos. En el caso 3, que la card pública de B, la que lee cualquiera, solo tiene la skill `ask` y no nombra nada de proyecto. **No observado:** el contenido de las consultas por la red, cifrado por QUIC; que las consultas a quien no tiene grant del proyecto van sin ids ni proyecto, y sin ids las de una búsqueda que no nombra proyecto, lo cubren `selection::inventory_requirement_ids_go_only_to_authorized_candidates`, `selection::a_search_that_names_no_project_sends_inventory_ids_to_no_one` y `selection::a_grant_of_another_project_never_carries_this_ones_name_or_ids` |

## Correcciones antes de esta ejecución

Seis ensayos y una primera ejecución completa, en `0e1e4d0`, que superó los casos 1 a 8, llevaron a la ejecución registrada en `818e68c`, que también los superó; la revisión final del hito llevó a esta. Un caso fallido se corrigió y se repitió:

1. **Producto: `pals discover` no nombraba las ofertas (`eea2281`).** `pals ask --offer` pide la oferta «por su id, como la imprimió `pals discover`», y una vencida dice «run pals discover again», pero el ranking no imprimía ningún id de oferta: el operador no podía buscar y preguntar después con la oferta, como hace el caso 5a. Ahora cada candidato con oferta termina su línea con `, offer <id> until <expires_at>`. Es salida de la CLI; el cable no cambia. Test de regresión `selection::ask_names_the_offer_discover_printed`, que falló antes de la corrección (`no offer named`) y pasa después; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked` y `cargo +1.91 check --workspace --all-targets --locked`, en verde.
2. **Runbook y arnés: la primera búsqueda con las conexiones sin abrir.** En un ensayo, el caso 1 empezó sin que A se hubiera conectado aún a ningún proveedor. Cada candidato agotó sus 3 s (`b (card): timeout` … `g (offer): timeout`, 6 s en total) y no se ejecutó nada: `no candidate took the task`, código 2. El runbook comprueba ahora, al guardar los contactos, que A ve la card firmada de cada proveedor, que es también su primera conexión con cada uno. Explica además qué significa una búsqueda cortada solo por plazos, y el arnés la repite hasta tres veces. No se reprodujo: cinco búsquedas justo después de rearrancar A, con todas las conexiones por abrir, tardaron entre 375 y 478 ms (observación 1).
3. **Arnés: los plazos y las cuotas no son anomalías.** En el caso 4, un candidato descartado por la cuota de un proveedor (`refused: RateLimited`) o por sus plazos (`timeout`, `time budget`) se cuenta aparte.
4. **Arnés: un caso 5 que sí pruebe la renovación.** Si la oferta de B sigue en vigor al preguntar, el intento no prueba la renovación: el arnés repite 5b con otra pregunta, hasta tres veces, y exige que alguna renueve.
5. **Tras la revisión de la tarea (`818e68c`).**
   - **Caso 4, con evidencia.** Exige un `429` en la ráfaga y al menos 45 de las 50 seguidas con el ranking completo, de modo que un 200 con todos los candidatos descartados por plazos ya no basta. Deriva el margen de `tools/list` de los vencimientos posibles del temporizador de cada proveedor, y no de un uno fijo. Comprueba que ningún servidor MCP recibe un `tools/call`, en el caso 4 y, al desmontar, sobre toda la prueba.
   - **Caso 3 y preparación.** El caso 3 comprueba lo que muestra la card pública de B. `preparar` espera además al JWK Set de F en los dos puertos.
   - **Caso 6.** Compara cada entrada entera, y no solo los ids.
   - **Caso 5.** Solo cuenta como renovación una oferta usada distinta de la del ranking.
   - **Copia y escritura.** A los contenedores van solo los ficheros que git sigue. `put` ya no mete su destino en el guion del shell remoto.
   - **F.** Crea su clave de firma de forma atómica, para que sus dos procesos no lean nunca una a medio escribir.

   Con todo ello, la ejecución en `818e68c`: los casos 1 a 8 a la primera, y el 5b sin necesitar su reintento.
6. **Tras la revisión final del hito (de `39c4e7f` a `1df72ec`).** Corrige el producto en lo que la revisión encontró, con un test de regresión para cada cambio de comportamiento; los que esta ejecución ejercita:
   - **Las búsquedas nombran el proyecto.** Una búsqueda que no nombra proyecto ya no envía a nadie los ids de inventario de sus requisitos: el caso 1, el 2, el 4 y el 5 buscan ahora con `--project demo` (y `"project": "demo"` en el cuerpo de `POST /discovery/search`), en el runbook y en el arnés.
   - **La cobertura dice cuándo un candidato agotó su plazo.** En 5b, con H caído, la cobertura es `partial` con `timeout`, y no `complete` (observación 1).
   - **El outbox y los contactos A2A corrientes.** `pals ask f` lee antes la card de F por el gateway, y solo reenviaría la pregunta a un agente sin la extensión de idempotencia si es seguro que no le llegó; el gateway pide a F respuestas sin comprimir.
   - **Solo se guardan las ofertas que una tarea puede nombrar**, las de un ámbito de tarea: las de 5a y 5b, de `task:demo`, sí; las de las consultas sin grant, como las de G en el caso 6, ya no.
   - **El arnés: la comprobación de `tools/call` falla cerrada** (caso 4 y desmontaje).

   Con todo ello, esta ejecución: los casos 1 a 8 a la primera, y el 5b sin necesitar su reintento, con la carga del anfitrión entre 60 y 90.

## Observaciones

1. **El plazo de 3 s por candidato incluye abrir la conexión.** Con las conexiones por abrir, la búsqueda tiene que hacerlo dentro de esos 3 s: el dial de Iroh, una conexión QUIC para el binding A2A (las cards) y otra para `agentnet-discovery/0` (la consulta). En el ensayo del punto 2 de «Correcciones», con la carga del anfitrión subiendo hasta 32, no le bastaron. Es el valor por defecto del §7.3, configurable (`per_candidate_timeout_ms`, hasta 10000), y la búsqueda informa bien lo que pasó. En redes reales, con relay o NAT, convendría medir cuánto cuesta esa primera conexión antes de decidir si se cuenta dentro del plazo del candidato. La cobertura de esa búsqueda decía entonces `complete` aunque todos los candidatos agotaran su plazo, lo que podía leerse como «todos respondieron». Desde la revisión final, un candidato que agota su propio plazo hace parcial la cobertura, con el motivo `timeout`, como se ve en 5b.
2. **5b puede necesitar su reintento.** En la ejecución en `0e1e4d0`, B acababa de rearrancar, y A tuvo que volver a conectarse a él dentro de la búsqueda. La oferta de B llegó a los 2873 ms, y su caducidad, por segundos enteros, aún no había pasado cuando A preguntó. Se preguntó con ella, B la aceptó y la ejecutó una vez, y el arnés repitió con otra pregunta, que sí renovó. En esta ejecución, la oferta de B llegó a los 278 ms y se renovó en el primer intento.
3. **El temporizador del adaptador venció después del caso 4.** Los proveedores lo tienen a 60 s. En esta ejecución vivieron algo más de un minuto (de 10:04:12 a 10:05:26), y su temporizador comprobó otra vez su servidor MCP de 10:05:14 a 10:05:22, después del caso 4 (de 10:04:55 a 10:05:09), en el que solo el de B podía vencer, y no venció dentro. El contador detecta cada llamada: tres `tools/list` en B (arranque, `pals inventory refresh` y el rearranque del caso 5), tres en C, D, E y G (arranque, `refresh` y el temporizador) y dos en H, parado en el caso 5. Que el temporizador los repite cada `refresh_interval_s` lo prueban también los tests del adaptador.
4. La carga del anfitrión fue de 60 a 90, de 25 a 31 en la ejecución anterior. Los tiempos de las búsquedas crecieron (mediana del primer candidato válido, 489 ms frente a 94 ms), pero ninguna quedó cortada por cuotas o plazos, y el caso 1 no necesitó repetirse (punto 2 de «Correcciones»).
5. **El camino del caso 1 fue mixto.** Por TCP 443 pasaron 39,3 KB enviados y 28,3 KB recibidos, frente a 8,1 KB y 5,7 KB, el tráfico de fondo con el relay, en la ejecución anterior; por UDP, 81,1 KB y 89,3 KB, como entonces. Con el anfitrión así de cargado, parte de las primeras conexiones de A fue por el relay antes de pasar a UDP directo. PALS no informa del camino de Iroh; el arnés solo lo deduce de esos contadores.

## Lo que no cubre

- Todos los agentes comparten el kernel, el reloj y la máquina, y entre ellos solo hay el bridge de Docker. No hay varias máquinas, redes ni NAT, y los relays solo se usaron para registrarse: el camino fue UDP directo. Los relojes nunca difieren, así que el desfase de 60 s de ofertas, vistas y registros no se ejercita aquí; lo hacen los tests (`expired_record_is_refused_beyond_skew`, `expired_page_is_refused_beyond_skew`).
- F es un agente corriente sobre el SDK oficial de Python, en un contenedor, con un certificado de una CA de prueba: no un agente de terceros en internet.
- La card alterada de un par PALS solo la sirven los pares de prueba de los tests de integración, en el anfitrión; en vivo, la card alterada es la de F.
- H comparte el contenedor de G, y M1 y M2 el suyo: H solo tiene que estar caído, y el caso 8 es una migración en loopback.
- El caso 9, el TCK y el cliente de Python, se registra aparte ([`README.md`](README.md)).
