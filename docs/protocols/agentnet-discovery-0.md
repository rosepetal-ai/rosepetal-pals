# Protocolo `agentnet-discovery/0`

Especificación del protocolo con el que un agente PALS pregunta a otro qué ofrece antes de encargarle ninguna tarea: su inventario, filtrado según el permiso de quien pregunta, y, en tareas posteriores del hito 2, sus informes de benchmark y si puede atender un caso concreto (diseño §7, §9.8 y §10). Es un protocolo propio de PALS, no una parte de A2A: la tarea elegida se envía después, como en el hito 1, con los bindings estándar de A2A v1.0.

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

- `sections`: los nombres de las secciones (`services`, `skills`, `plugins`, `mcp`, `runtime`, `models`, `information`, `hardware`, `network`), en cualquier orden y con repeticiones, como mucho 32 nombres; la página las lleva ordenadas y sin repetir. Vacío: todas. Un nombre que no es una sección del inventario es `BAD_REQUEST` («unknown section»), sin repetirlo.
- `cursor`: el `next_cursor` de la página anterior; vacío para la primera. Uno que no sirve para este solicitante, ámbito, secciones y revisión es `INVALID_CURSOR`, sin decir por qué.
- `page_entries`: cuántas entradas como mucho; `0`, las que quepan hasta 100.

Una página privada lleva en `audience` al solicitante; una pública no lleva `audience`. `seq` es la revisión del ámbito sobre todas sus secciones, y `agent_card_hash` el hash de la card de ese ámbito (la pública si el ámbito no tiene card propia). El agente guarda las páginas que firma por todo lo que las distingue, solicitante incluido, durante la mitad de su vida (150 s): una página servida desde la caché siempre tiene al menos la mitad de su vida por delante, y nunca se sirve a otro solicitante. Una entrada que no cabe sola en una página es `TOO_LARGE`.

### `GetBenchmarkReport` y `ServiceQuery`

`UNSUPPORTED_OPERATION` en esta versión del daemon; las tareas 7 y 8 del hito 2 las implementan. Una petición sin operación es `BAD_REQUEST`.

## 6. Respuestas y errores

Toda respuesta, también las de error, lleva `version = 0`, `limits` y los registros firmados de contacto y de anuncio del agente que estén en vigor en ese momento (`agentnet.contact/1` y `agentnet.announce/1`, en JSON): los renovados después de arrancar, no los del arranque.

| `ErrorCode` | Cuándo |
|-|-|
| `UNSUPPORTED_VERSION` | `version` distinta de `0`; con `supported_versions` |
| `BAD_REQUEST` | frame que no es una petición, cortado, seguido de más bytes, que no llega entero a tiempo («request timed out»), sin operación, o con secciones desconocidas o demasiadas |
| `TOO_LARGE` | frame de petición de más de 20 KiB, respuesta mayor que `max_response_bytes`, o entrada que no cabe en una página |
| `RATE_LIMITED` | cuota superada; con `retry_after_ms` |
| `NOT_FOUND` | reservado (informes de benchmark, tarea 8) |
| `INVALID_CURSOR` | cursor que no sirve, sea cual sea el motivo |
| `UNAUTHENTICATED` | grant no aceptado; el mensaje es el motivo |
| `UNSUPPORTED_OPERATION` | `GetBenchmarkReport` y `ServiceQuery` en esta versión |
| `INTERNAL` | el almacén no se puede leer, o no se puede firmar |

Los mensajes de error son textos fijos de 256 bytes como mucho. Nunca repiten lo que envió el solicitante ni dicen si existe algo que el solicitante no puede ver: un error no distingue «no existe» de «no lo puedes ver».

**Lo que comprueba el cliente.** Antes de usar una respuesta, el cliente de PALS comprueba los dos registros: que verifican (`Signed::from_json`), que los firma el par de la conexión y que están en vigor, con 60 s de desfase. Luego los compara con los más recientes que conoce de ese par: un registro con `seq` menor que el conocido, o el mismo `seq` con otros bytes, es una regresión, y la respuesta entera se descarta. Los registros aceptados se guardan; el conocido se olvida solo un `RECORD_TTL_S` entero después de caducar. La página la verifica quien la usa, con su propia identidad como `audience`.

## 7. Cuotas y plazos

Por stream, antes de leer nada de la petición:

| Cuota | Valor | Al superarla |
|-|-|-|
| Streams de un par a la vez | 4 | `RATE_LIMITED`, `retry_after_ms = 1000` |
| Peticiones de un par por minuto | 60, en una ventana de 60 s del reloj del nodo | `RATE_LIMITED`, `retry_after_ms` hasta que la más antigua sale de la ventana |
| Streams de todos los pares a la vez | 64 | `RATE_LIMITED`, `retry_after_ms = 1000` |
| Petición entera (prefijo, cuerpo y FIN) | 5 s | `BAD_REQUEST` «request timed out» |

Lo que pasa de una cuota se rechaza, no se encola. «Un par» es un `AgentId`, con todas sus conexiones. La conexión concede crédito para cuatro streams más que la cuota del par, para que el que la supera reciba su `RATE_LIMITED` en vez de quedarse esperando crédito.

## 8. Lo que el protocolo nunca hace

- **Ejecutar trabajo.** La respuesta la construye `pals_core::discovery::answer`, en un crate que no abre sockets ni tiene endpoint, worker, ejecutor ni adaptador: lee el inventario, el almacén y los registros del agente, y firma con su clave. Ninguna petición crea una tarea, llama al worker, invoca una herramienta MCP ni ejecuta nada que venga en ella (diseño §7.3 y §13).
- **Reenviar.** El servidor (`DiscoveryServer`) solo tiene el contexto de descubrimiento: ni el cliente HTTP de pares, ni el de descubrimiento, ni los blobs, ni el worker. No pregunta a otros pares, y no hay ninguna opción que lo haga.
- **Autorizar.** Una página no es un grant: la ejecución vuelve a comprobar el grant del hito 1 al aceptar la tarea.

## 9. Interfaz local

- **Gateway** (solo loopback, detrás del guardia local): `GET /discovery/inventory/{peer}?sections=a,b&cursor=…` pide una página al par con el grant más reciente que este agente tiene para él y que sirve tareas (la misma regla con la que el gateway presenta grants a la interfaz A2A del par), o sin grant si no tiene ninguno. Devuelve la página tal como la firmó el par, recodificada en ASCII, una vez verificada con este agente como `audience`. La guarda por par, por contexto de autorización (`public` o `grant:<emisor>/<grant_id>`) y por petición mientras está en vigor, y solo la vuelve a servir a una petición del mismo contexto.
- **CLI:** `pals inventory get <par> [--sections a,b] [--all]` imprime cada página tal como la firmó el par, seguida de `verified: <agent id> <ámbito> seq <revisión>`; con `--all` sigue los cursores hasta el final de la cadena.
