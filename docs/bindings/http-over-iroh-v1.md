# Binding `http-over-iroh/v1`

Especificación del *custom protocol binding* de A2A con el que dos agentes PALS se hablan entre pares. Cubre los puntos que exige la especificación A2A v1.0.1 en su §12 (*Custom Binding Guidelines*) y la identificación de §5.8.

En una frase: **los bindings estándar JSON-RPC (§9) y HTTP+JSON (§11) de A2A, sin cambios, con cada intercambio HTTP/1.1 transportado por un stream bidireccional de una conexión QUIC de [Iroh](https://www.iroh.computer/)**. Todo lo que no dice este documento se hereda tal cual de esos dos bindings.

Los valores que aparecen aquí son los del código: `crates/pals/src/net/` (transporte), `crates/pals/src/a2a/card.rs` (identificación) y `crates/pals/src/a2a/auth.rs` (autenticación).

## 1. Identificación

| Elemento | Valor |
|-|-|
| URI del binding (`AgentInterface.protocolBinding`) | `https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1` (constante `BINDING_URI`) |
| URL de la interfaz (`AgentInterface.url`) | `iroh://<agent_id>/jsonrpc` |
| Versión del protocolo (`AgentInterface.protocolVersion`) | `1.0` |
| ALPN de Iroh | `agentnet-a2a-http/1` (constante `ALPN_A2A_HTTP`) |

`<agent_id>` es la clave pública Ed25519 del agente en 64 caracteres hexadecimales en minúscula. Es a la vez el `EndpointId` de Iroh del agente: la URL nombra el endpoint al que hay que conectarse, no una dirección de red. Las direcciones (IP directas o relay) se obtienen aparte, del *ticket* del agente (`pals id --ticket`) o del descubrimiento de Iroh.

La card canónica que el agente sirve a sus pares declara una sola interfaz: la de JSON-RPC, con este binding. El mismo servidor atiende también el binding HTTP+JSON bajo el prefijo `/rest` del mismo origen (`iroh://<agent_id>/rest/...`, con las rutas de §11: `/rest/message:send`, `/rest/tasks/{id}`, …); el cliente de PALS lo usa entre pares. El gateway local, que es donde llegan los clientes estándar, declara las dos interfaces con sus URL `http://` y los nombres estándar `JSONRPC` y `HTTP+JSON`.

**Versionado.** Un cambio incompatible de este binding usará una URI nueva (`.../http-over-iroh/v2`) y un ALPN nuevo; `v1` no cambia de significado (A2A §5.8, requisito BIND-CUSTOM-002 del TCK).

## 2. Transporte

- **Conexión.** El cliente abre una conexión QUIC de Iroh al endpoint cuyo id es `<agent_id>`, con el ALPN `agentnet-a2a-http/1`. El handshake (TLS 1.3 de QUIC con las claves Ed25519 de ambos endpoints) prueba que al otro lado está ese agente. El cliente reutiliza una conexión por par; marcarla tarda como mucho 10 s.
- **Un intercambio por stream.** Cada petición abre un stream bidireccional nuevo que lleva exactamente un intercambio HTTP/1.1: una petición y su respuesta, sin *keep-alive* ni *pipelining*. No se usa HTTP/2.
- **Fin de la respuesta.** La respuesta termina con el FIN del stream de envío del servidor, además de lo que indique su propio enmarcado HTTP (`Content-Length` o *chunked*).
- **`Host`.** HTTP/1.1 lo exige; el cliente envía el id del agente en hexadecimal cuando la petición no trae otro. El servidor no lo usa para enrutar.
- **Rutas.** Las del servidor A2A del agente, relativas al origen `iroh://<agent_id>`: `/.well-known/agent-card.json` (card), `/jsonrpc` (JSON-RPC) y `/rest/...` (HTTP+JSON).

**Límites** (los de la tarea 1, `TunnelLimits`):

| Límite | Valor | Al superarlo |
|-|-|-|
| Cuerpo de una petición | 1 MiB por defecto (`limits.max_body_bytes`) | `413 Payload Too Large`, también si lo anuncia `Content-Length` antes de leerlo |
| Streams atendidos a la vez por conexión | 16 | el servidor no concede crédito para más streams; los demás esperan |
| Tiempo para recibir la cabecera completa de la petición | 10 s | el servidor cierra el stream |
| Marcado de una conexión (cliente) | 10 s | el cliente informa de que el par no responde |

Los ficheros no viajan por este canal: van como *blobs* por el protocolo de iroh-blobs (ALPN propio) y la petición A2A solo los referencia (§3).

## 3. Mapeo de tipos

Sin cambios respecto a A2A §9 y §11: los cuerpos son el JSON de ProtoJSON de `a2a.proto` (campos en *camelCase*), y por tanto:

- **Mensajes protobuf:** cada mensaje de `a2a.proto` como objeto JSON, igual que en los bindings estándar.
- **Marcas de tiempo:** RFC 3339 en UTC (A2A §5.6.1), p. ej. `2026-11-14T09:24:38.765Z`.
- **Binarios:** base64 (p. ej. `Part.raw`).
- **Enumeraciones:** cadenas con su nombre protobuf (`TASK_STATE_COMPLETED`, `ROLE_AGENT`).
- **`Struct`/`Value`** (p. ej. `Part.data`): sus números son `double`; `42` puede viajar como `42.0`, que es el mismo número JSON.

Convención propia dentro de los campos estándar: un fichero que el agente `<holder>` tiene en su almacén se referencia con un `Part` cuyo `url` es `iroh-blob://<holder>/<hash BLAKE3 en hex>` y cuyo `metadata` lleva `"agentnet.blob/1": {"hash": "<hex>", "size": <bytes>}`. Una referencia no concede permiso para descargar el fichero. El gateway local presenta esas URL a los clientes estándar como `http://127.0.0.1:<puerto>/blobs/<holder>/<hash>`, y las devuelve a su forma `iroh-blob://` en lo que envía a un par.

## 4. Parámetros de servicio

- **Mecanismo:** cabeceras HTTP de la petición, como en los bindings estándar basados en HTTP (A2A §3.2.6): `A2A-Version` (siempre `1.0`; sin ella cuenta como `0.3` y se rechaza en los dos bindings), `A2A-Extensions` y `Authorization`, entre otras. Nombres insensibles a mayúsculas.
- **Extensiones:** `A2A-Extensions` activa las extensiones que la card declara. PALS declara una, opcional: [`idempotency/v1`](../extensions/idempotency-v1.md), que hace idempotentes `SendMessage` y `SendStreamingMessage` por `messageId`. El cliente de PALS la activa siempre.
- **Valores:** los de una cabecera HTTP/1.1 (RFC 9110): texto visible ASCII. El servidor rechaza un cuerpo mayor de 1 MiB, no un tamaño de cabeceras concreto distinto del de su biblioteca HTTP (hyper).
- **Nombres reservados por el binding:** todo `x-agentnet-*`. El cliente no debe enviarlos; el servidor los descarta y fija:
  - `x-agentnet-peer`: el id del agente remoto que autenticó el handshake de QUIC, en hexadecimal. Lo pone el túnel del servidor y sustituye cualquier valor recibido.
  - `x-agentnet-grant-id` y `x-agentnet-project`: el id y el proyecto del grant aceptado. Los pone la capa de autenticación.
- **Alternativa sin cabeceras:** no aplica; HTTP/1.1 siempre las tiene.

## 5. Mapeo de errores

Los errores de A2A se representan exactamente como en los bindings estándar: en JSON-RPC, un objeto `error` con los códigos de A2A §9 (`-32001` `TaskNotFoundError`, `-32002` `TaskNotCancelableError`, …) y `google.rpc.ErrorInfo` en `data`; en HTTP+JSON, el código de estado de A2A §5.4 con un `google.rpc.Status` en el cuerpo (A2A §11.6), servido como `application/json`.

Antes de llegar al servidor A2A, el binding puede responder:

| Estado | Cuándo | Cuerpo |
|-|-|-|
| `401 Unauthorized` | Falta el grant, o no es usable: mal formado, demasiado grande, esquema desconocido, firma inválida, emisor no aceptado, otro sujeto u otra audiencia, aún no válido, caducado o revocado | `{"error": "<motivo>"}`, con `WWW-Authenticate: Bearer realm="agentnet", error="invalid_token", error_description="<motivo>"` |
| `403 Forbidden` | Grant válido que no cubre la operación o el recurso | `{"error": "operation_not_granted"}` o `{"error": "resource_not_granted"}` |
| `413 Payload Too Large` | Cuerpo mayor que el límite | texto |
| `400 Bad Request` | Cuerpo ilegible | texto |
| `500 Internal Server Error` | No se pudo leer el almacén de revocaciones | `{"error": "internal_error"}` |

Los motivos de `401` son `missing_token`, `unknown_peer`, `malformed`, `too_large`, `unknown_schema`, `bad_signature`, `issuer_not_accepted`, `wrong_subject`, `wrong_audience`, `not_yet_valid`, `expired` y `revoked`.

Si el intercambio no llega a producir una respuesta (el par no responde, la conexión se pierde, el stream se corta), no hay error HTTP: el cliente lo ve como un fallo de transporte. El gateway local lo traduce a `502 {"error": "peer unreachable"}` o `502 {"error": "peer protocol error"}` para sus clientes.

## 6. Streaming

- **Mecanismo:** Server-Sent Events, como en los bindings estándar: `SendStreamingMessage` y `SubscribeToTask` responden `200` con `Content-Type: text/event-stream`, y cada evento es una línea `data:` con la respuesta JSON-RPC (JSON-RPC) o el `StreamResponse` (HTTP+JSON). El cuerpo viaja según se produce, sin acumularse.
- **Orden:** el de generación. El flujo empieza con el `Task` (o con el único `Message`, si el agente responde con un mensaje) y sigue con las actualizaciones de estado y de artefactos en el orden en que el agente las produce; todas viajan por el mismo stream, que QUIC entrega en orden.
- **Fin:** el último evento es el de un estado terminal o interrumpido de la tarea; después el servidor termina la respuesta y cierra su stream (FIN). Un stream que termina sin ese evento se ha cortado.
- **Reconexión:** no se reanuda un stream cortado. Cortar el stream no cancela la tarea. El cliente se vuelve a suscribir con `SubscribeToTask` (tarea no terminada, también si espera entrada) o consulta `GetTask` (tarea ya terminada), en un stream nuevo y por una conexión nueva si hace falta. Con la extensión [`idempotency/v1`](../extensions/idempotency-v1.md) activada, repetir `SendMessage`/`SendStreamingMessage` con el mismo `messageId` y el mismo contenido no ejecuta el trabajo otra vez: el agente lo une a la tarea del primer intento (A2A §3.3.1).

## 7. Autenticación y autorización

Dos capas, ambas obligatorias:

1. **Identidad del endpoint (transporte).** El handshake de Iroh autentica las dos claves Ed25519. El cliente sabe que habla con `<agent_id>` porque es el endpoint al que se conectó; el servidor conoce el id del cliente y lo pasa como `x-agentnet-peer`.
2. **Grant como token `Bearer` (aplicación).** Toda petición, salvo `GET /.well-known/agent-card.json`, lleva `Authorization: Bearer <token>`, el esquema que la card declara en `securitySchemes.agentnetGrant` (`httpAuthSecurityScheme`, `scheme: Bearer`, `bearerFormat: agentnet.grant/1`) y exige en `securityRequirements` (A2A §7.3). El token es el grant firmado: base64url sin relleno del JCS de `{"grant": …, "signature": …}`, de 8 KiB como mucho. Se acepta si su audiencia es este agente, su sujeto es `x-agentnet-peer`, lo emitió este agente o uno de sus emisores de confianza, no está revocado, está en vigor (con 60 s de tolerancia de reloj) y permite la operación de la petición sobre el recurso `skill:ask`:

| Operación A2A | Operación del grant |
|-|-|
| `SendMessage`, `SendStreamingMessage` | `task.send` |
| `GetTask`, `ListTasks`, `SubscribeToTask` | `task.read` |
| `CancelTask` | `task.cancel` |
| configuración de *push*, card extendida | cualquier grant válido (el agente responde después que no las ofrece) |

El reto de autenticación es el `401` con `WWW-Authenticate` de §5. Además, cada tarea pertenece al par que la creó: otro par recibe `TaskNotFoundError`, igual que si no existiera.

## 8. Desviaciones respecto a los bindings estándar

**Ninguna en comportamiento.** Las mismas peticiones (método, ruta, consulta, cabeceras y cuerpo), las mismas respuestas (código, cabeceras, cuerpo y eventos SSE) y los mismos errores que JSON-RPC y HTTP+JSON sobre HTTP. Lo que cambia es solo el transporte y lo que se añade:

- URL `iroh://` en lugar de `https://`, y QUIC de Iroh (TLS 1.3 con claves Ed25519, sin certificados de una CA) en lugar de TCP con TLS.
- Un intercambio por stream; sin *keep-alive*, *pipelining* ni HTTP/2.
- Las cabeceras reservadas `x-agentnet-*` y la autenticación obligatoria con grant.
- Los límites de §2.

Lo que el servidor A2A hace para cumplir la especificación donde el SDK oficial de Rust fijado no la cumple, y lo que mide el TCK, está en [`docs/conformance/hito-1/README.md`](../conformance/hito-1/README.md).
