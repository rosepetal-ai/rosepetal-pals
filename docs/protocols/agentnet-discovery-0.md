# Protocolo `agentnet-discovery/0`

Especificación del protocolo con el que un agente PALS pregunta a otro qué ofrece antes de encargarle ninguna tarea: su inventario, filtrado según el permiso de quien pregunta, si puede atender un caso concreto (una consulta de servicio, que se contesta con una oferta firmada) y sus informes de benchmark, los que ya tiene (diseño §7, §9.7, §9.8 y §10). Es un protocolo propio de PALS, no una parte de A2A: la tarea elegida se envía después, como en el hito 1, con los bindings estándar de A2A v1.0.

Los valores que aparecen aquí son los del código: `crates/pals-core/src/discovery/` (mensajes, frames y respuesta) y `crates/pals/src/discovery/{server,client}.rs` (transporte, cuotas y cliente). Los mensajes están en [`agentnet-discovery-0.proto`](agentnet-discovery-0.proto), y el vector `crates/pals-core/tests/vectors/discovery-v0.json` congela su codificación.

## 1. Identificación

| Elemento | Valor |
|-|-|
| ALPN de Iroh | `agentnet-discovery/0` (constante `ALPN_DISCOVERY`) |
| Versión | `0` (`DISCOVERY_VERSION`) |
| Mensajes | paquete protobuf `agentnet.discovery.v0` |
| Declaración | la card pública declara la extensión [`inventory/v1`](../extensions/inventory-v1.md) con `params.discoveryAlpn = "agentnet-discovery/0"`, porque el daemon sirve este ALPN |

El ALPN, los números de campo y los valores de `ErrorCode` no cambian de significado: un cambio incompatible será `agentnet-discovery/1`, con ALPN propio.

El spec (§10) preveía una operación `GetAgentCard`; no existe. La card pública llega por `/.well-known/agent-card.json` y la extendida por `GetExtendedAgentCard`, por el túnel A2A del hito 1 (decisión H2-D1).

## 2. Transporte y frames

- **Conexión.** El solicitante abre una conexión QUIC de Iroh al endpoint del agente, con el ALPN `agentnet-discovery/0`. El handshake prueba quién está a cada lado: el servidor conoce el `AgentId` del par por la conexión, y nada de lo que el par envía lo cambia. El cliente de PALS reutiliza una conexión por par.
- **Un intercambio por stream.** Cada petición abre un stream bidireccional nuevo con exactamente un frame de petición, que el solicitante cierra con FIN, y un frame de respuesta, que el servidor cierra con FIN. Lo que llegue tras el frame de petición la invalida (`BAD_REQUEST`).
- **Frame.** La longitud del mensaje protobuf codificado, como `u32` big-endian, y el mensaje. Un mensaje de longitud 0 es válido: el mensaje vacío.
- **Límites de frame.** Petición de 20 KiB como mucho (un grant de 8 KiB y una consulta de 8 KiB, con margen); respuesta de 80 KiB como mucho (una página de 64 KiB, con los registros y el resto). Quien lee un frame lee primero los cuatro bytes de su longitud y, si pasa del límite, lo rechaza sin leer el cuerpo: el servidor contesta `TOO_LARGE` y deja de leer el stream; el cliente deja de leer y no usa la respuesta.

## 3. Negociación

`Request.version` es la versión que habla el solicitante. Con otra que no sea `0`, la respuesta es `UNSUPPORTED_VERSION` con `supported_versions = [0]`, y viene en versión `0`.

Cada respuesta lleva en `limits` los límites del servidor:

| Campo | Valor |
|-|-|
| `max_request_bytes` | 20480 |
| `max_response_bytes` | 81920 |
| `max_query_bytes` | 8192 |
| `max_offer_bytes` | 16384 |
| `max_page_bytes` | 65536 |
| `max_page_entries` | 100 |
| `max_query_total_bytes` | 1048576 |
| `max_card_bytes` | 32768 |

`Request.max_response_bytes` es lo más que el solicitante lee de cuerpo de respuesta; `0` es el límite del servidor. La respuesta respeta el menor de los dos: si no cabe, se sustituye por un `TOO_LARGE` («response larger than max_response_bytes»), que se envía aunque tampoco quepa. Para recibir páginas pequeñas, `GetInventory.page_entries` pide menos entradas por página.

## 4. Autorización y ámbito

Lo que ve quien pregunta sale del grant que presenta y de nada más que envíe.

1. **Grant.** Si `grant_token` no está vacío, se acepta con la misma regla que aplica la capa HTTP del hito 1 a las peticiones A2A (`pals_core::grant::accept_grant`): firma válida, emisor este agente o uno de sus emisores de confianza, audiencia este agente, sujeto el par de la conexión, no revocado y en plazo, con 60 s de desfase. Una operación no concedida no es un error de autenticación: solo deja el ámbito en público. Si el grant no se acepta, la respuesta es `UNAUTHENTICATED` con el motivo en minúsculas, el mismo que la capa HTTP pone en `WWW-Authenticate`: `expired`, `not_yet_valid`, `revoked`, `wrong_subject`, `wrong_audience`, `issuer_not_accepted`, `bad_signature`, `malformed`, `too_large` o `unknown_schema`. Un grant cuyo proyecto no es un nombre de proyecto (ASCII imprimible sin espacios) es `malformed`: ninguna página firmada podría nombrar su ámbito.
2. **Ámbito.** El que da el grant (`Scope::for_grant`, [inventario](../extensions/inventory-v1.md#visibilidad-y-ámbitos)): el de tarea del proyecto con `task.send` sobre `skill:ask`, el del proyecto con otra operación de tarea, y el público en otro caso. Sin grant, el público; si el agente tiene `[discovery] public = false`, una vista pública vacía, de revisión 1, cuyas páginas nunca se guardan en la caché: así nunca ocupa el lugar de la vista pública que da un grant sin operaciones de tarea, ni al revés.
3. **Operación.**

## 5. Operaciones

### `GetInventory`

Una página firmada de la vista del ámbito (`agentnet.inventory-view/1`, ver [Vistas](../extensions/inventory-v1.md#vistas)), en `inventory_page`.

- `sections`: los nombres de las secciones (`services`, `skills`, `plugins`, `mcp`, `runtime`, `models`, `information`, `hardware`, `network` y `benchmarks`), en cualquier orden y con repeticiones, como mucho 32 nombres; la página las lleva ordenadas y sin repetir. Vacío: las nueve del inventario; `benchmarks` solo sale si se nombra. Un nombre que no es una sección es `BAD_REQUEST` («unknown section»), sin repetirlo.
- `cursor`: el `next_cursor` de la página anterior; vacío para la primera. Uno que no sirve para este solicitante, ámbito, secciones y revisión es `INVALID_CURSOR`, sin decir por qué.
- `page_entries`: cuántas entradas como mucho; `0`, las que quepan hasta 100.

Una página privada lleva en `audience` al solicitante; una pública no lleva `audience`. `seq` es la revisión del ámbito sobre todas sus secciones, `benchmarks` incluida, y `agent_card_hash` el hash de la card de ese ámbito (la pública si el ámbito no tiene card propia). El agente guarda las páginas que firma por todo lo que las distingue, solicitante incluido, durante la mitad de su vida (150 s): una página servida desde la caché siempre tiene al menos la mitad de su vida por delante, y nunca se sirve a otro solicitante. Una entrada que no cabe sola en una página es `TOO_LARGE`.

### `ServiceQuery`

Una consulta de servicio (`agentnet.service-query/1`, esquema [`service-query.schema.json`](../extensions/inventory-v1/service-query.schema.json)) en `query`, como su JCS, y una oferta firmada (`agentnet.service-offer/1`, esquema [`service-offer.schema.json`](../extensions/inventory-v1/service-offer.schema.json)) en `service_offer`. Las reglas de la consulta, del matching y de la oferta están en [Consultas de servicio y ofertas](../extensions/inventory-v1.md#consultas-de-servicio-y-ofertas); aquí, lo que toca al protocolo:

- **Consulta.** De 8 KiB como mucho (`TOO_LARGE` si no, sin leerla), en su JCS y con una sola grafía: lo ausente se omite, nunca es `null` ni `[]`. Una que no lo es, o que incumple sus reglas, es `BAD_REQUEST` con un motivo fijo que nunca la repite; entre ellas, `execution_authorized` tiene que ser `false` («a query never authorizes execution»).
- **Ámbito.** El del grant presentado (§4). `project` solo elige entre los proyectos que ese grant ya cubre: como un grant v1 cubre uno, si coincide con el del grant el ámbito es el del grant, y si no, o sin grant, la consulta se contesta como pública. Nunca amplía lo que se ve.
- **Contestar no ejecuta nada.** El matching es una función pura sobre el inventario tal como lo muestra el ámbito, sus observaciones y los contadores de carga del worker: ningún LLM, herramienta, adaptador ni par.
- **Oferta.** Firmada con el dominio `agentnet.offer/1\n`, de 16 KiB como mucho (si no cabe, se recortan por el final las limitaciones, luego los resultados de requisitos y luego los requisitos desconocidos, y termina en la limitación `truncated`), para el par de la conexión (`requester`) y vinculada a la consulta (`query_hash`, BLAKE3 de los bytes recibidos). Lleva el ámbito, el hash de la card de ese ámbito (la pública si no tiene card propia) y la revisión de la vista del ámbito, que cambia con todo cambio de lo que muestra, también con las fechas de una observación. Dura `[discovery] offer_valid_for_ms` (15000 por defecto, como mucho 300000), redondeado al segundo por arriba: `expires_at = issued_at + ⌈valid_for_ms / 1000⌉` s, con el reloj del agente. El agente guarda cada oferta que emite (`offers_issued`) y purga cada hora las que caducaron hace más de un día.
- **Misma forma.** Si el ámbito no muestra ningún servicio de la capacidad, la oferta es `not_offered`, sin servicio, sin resultados de requisitos, sin tiempos ni limitaciones: la misma exista o no un servicio que el solicitante no puede ver.

### `GetBenchmarkReport`

Un informe de benchmark (`agentnet.benchmark-report/1`, esquema [`benchmark-report.schema.json`](../extensions/inventory-v1/benchmark-report.schema.json)) que el agente ya tiene, en `benchmark_report`, tal como lo firmó quien midió: los bytes que guardó al importarlo, que verifican con la clave de `provenance.measured_by` (ver [Informes de benchmark](../extensions/inventory-v1.md#informes-de-benchmark)).

- **Visible en el ámbito.** El del grant presentado (§4). El ámbito ve un informe si la visibilidad que el agente le dio lo muestra en él y si el ámbito muestra lo que mide (el servicio, el modelo o el hardware), o si mide una pareja de nodos.
- **Igual que si no existiera.** Un informe que el ámbito no ve, uno que no existe y un `benchmark_id` que no es un id de informe tienen la misma respuesta: `NOT_FOUND` con el mensaje «not found», que nunca repite el id.
- **Nunca mide.** El agente solo lee informes que ya existen: no arranca un LLM, una descarga ni un test de red, y nunca descarga la `evidence` de un informe (§9.7). Si no hay medidas, la vista dice `no_measurement`; nunca cero.

Una petición sin operación es `BAD_REQUEST`.

## 6. Respuestas y errores

Toda respuesta, también las de error, lleva `version = 0`, `limits` y los registros firmados de contacto y de anuncio del agente que estén en vigor en ese momento (`agentnet.contact/1` y `agentnet.announce/1`, en JSON): los renovados después de arrancar, no los del arranque.

| `ErrorCode` | Cuándo |
|-|-|
| `UNSUPPORTED_VERSION` | `version` distinta de `0`; con `supported_versions` |
| `BAD_REQUEST` | frame que no es una petición, cortado, seguido de más bytes, que no llega entero a tiempo («request timed out»), sin operación, con secciones desconocidas o demasiadas, o con una consulta de servicio inválida |
| `TOO_LARGE` | frame de petición de más de 20 KiB, consulta de servicio de más de 8 KiB, respuesta mayor que `max_response_bytes`, o entrada que no cabe en una página |
| `RATE_LIMITED` | cuota superada; con `retry_after_ms` |
| `NOT_FOUND` | `GetBenchmarkReport` de un informe que no existe o que el ámbito no ve, sin distinguirlos |
| `INVALID_CURSOR` | cursor que no sirve, sea cual sea el motivo |
| `UNAUTHENTICATED` | grant no aceptado; el mensaje es el motivo |
| `UNSUPPORTED_OPERATION` | reservado: toda operación de esta versión se atiende |
| `INTERNAL` | el almacén no se puede leer o escribir, o no se puede firmar |

Los mensajes de error son textos fijos de 256 bytes como mucho. Nunca repiten lo que envió el solicitante ni dicen si existe algo que el solicitante no puede ver: un error no distingue «no existe» de «no lo puedes ver».

**Lo que comprueba el cliente.** Lee un solo frame de respuesta y después exige el final del stream: cualquier byte más invalida la respuesta. Antes de usarla, el cliente de PALS comprueba los dos registros: que verifican (`Signed::from_json`), que los firma el par de la conexión y que están en vigor, con 60 s de desfase. Luego los compara con los más recientes que conoce de ese par: un registro con `seq` menor que el conocido, o el mismo `seq` con otros bytes, es una regresión, y la respuesta entera se descarta. Los registros aceptados se guardan; el conocido se olvida solo un `RECORD_TTL_S` entero después de caducar. La página la verifica quien la usa, con su propia identidad como `audience`. Del mensaje de un error del par usa como mucho 256 bytes, cortados al final de un carácter, como los envía un servidor de este protocolo.

Una oferta la acepta el cliente con una sola función, `pals_core::discovery::accept_offer`, solo si, en este orden: cabe en 16 KiB (sin leerla si no), es una oferta firmada válida, la firma el par al que preguntó, es para él mismo (`requester`), es de su consulta (`query_id` y `query_hash`) y no ha caducado, con 60 s de desfase solo para rechazar. Entonces la guarda (`offers_received`) con la clave (proveedor, `offer_id`): dos proveedores pueden usar el mismo `offer_id`. Las caducadas se purgan cada hora. Si una oferta guardada sigue sirviendo para preguntar lo decide quien pregunta con su propio reloj, sin desfase.

## 7. Cuotas y plazos

Por stream, antes de leer nada de la petición:

| Cuota | Valor | Al superarla |
|-|-|-|
| Streams de un par a la vez | 4 | `RATE_LIMITED`, `retry_after_ms = 1000` |
| Peticiones de un par por minuto | 60, en una ventana de 60 s del reloj del nodo | `RATE_LIMITED`, `retry_after_ms` hasta que la más antigua sale de la ventana |
| Streams de todos los pares a la vez | 64 | `RATE_LIMITED`, `retry_after_ms = 1000` |
| Petición entera (prefijo, cuerpo y FIN) | 5 s | `BAD_REQUEST` «request timed out» |
| Respuesta entera (frame y FIN) | 5 s | el stream se reinicia (`RESET_STREAM`) |

Lo que pasa de una cuota se rechaza, no se encola. El plazo de la respuesta cubre también las de rechazo: un par que no concede crédito de control de flujo para leerla, o que no la lee, no retiene más de ese plazo ni un stream de su cuota ni uno del total. «Un par» es un `AgentId`, con todas sus conexiones. La conexión concede crédito para cuatro streams más que la cuota del par, para que el que la supera reciba su `RATE_LIMITED` en vez de quedarse esperando crédito.

## 8. Lo que el protocolo nunca hace

- **Ejecutar trabajo.** La respuesta la construye `pals_core::discovery::answer`, en un crate que no abre sockets ni tiene endpoint, worker, ejecutor ni adaptador: lee el inventario, el almacén y los registros del agente, y firma con su clave. Ninguna petición crea una tarea, llama al worker, invoca una herramienta MCP ni ejecuta nada que venga en ella (diseño §7.3 y §13).
- **Reenviar.** El servidor (`DiscoveryServer`) solo tiene el contexto de descubrimiento: ni el cliente HTTP de pares, ni el de descubrimiento, ni los blobs, ni el worker. No pregunta a otros pares, y no hay ninguna opción que lo haga.
- **Autorizar.** Ni una página ni una oferta son un grant: una oferta no reserva capacidad, y la ejecución vuelve a comprobar el grant del hito 1 al aceptar la tarea.

## 9. Interfaz local

- **Gateway** (solo loopback, detrás del guardia local): `GET /discovery/inventory/{peer}?sections=a,b&cursor=…` pide una página al par con el grant más reciente que este agente tiene para él y que sirve tareas (la misma regla con la que el gateway presenta grants a la interfaz A2A del par), o sin grant si no tiene ninguno. Devuelve la página tal como la firmó el par, recodificada en ASCII, una vez verificada con este agente como `audience`. La guarda por par, por contexto de autorización (`public` o `grant:<emisor>/<grant_id>`) y por petición mientras está en vigor, y solo la vuelve a servir a una petición del mismo contexto.
- **Gateway:** `POST /discovery/query/{peer}` con una consulta de servicio en JSON (64 KiB de cuerpo como mucho; su JCS, 8 KiB, si no `413`; una consulta inválida es `400` con el motivo, sin preguntar al par) se la pregunta al par con el grant que este agente tiene para el `project` de la consulta si lo tiene, si no con el que presentaría a la interfaz A2A del par, o sin grant. Devuelve la oferta tal como la firmó el par, recodificada en ASCII, una vez aceptada (y guardada) por el cliente; una oferta rechazada es `502` con el motivo.
- **CLI:** `pals inventory get <par> [--sections a,b] [--all]` imprime cada página tal como la firmó el par, seguida de `verified: <agent id> <ámbito> seq <revisión>`; con `--all` sigue los cursores hasta el final de la cadena. `pals query <par> --capability <id> [--input <mime>]… [--output <mime>]… [--requirements-file <json>] [--project <p>]` comprueba la consulta antes de preguntar, imprime la oferta tal como la firmó el par y después `verified: <agent id> <offer_id> <estado>`.
