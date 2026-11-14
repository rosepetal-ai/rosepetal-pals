# Conformidad A2A del hito 1

Evidencia de la compatibilidad con A2A v1.0 del intercambio final de PALS: el TCK oficial ejecutado a través del gateway local, y un cliente independiente hecho con el SDK oficial de Python. Este documento dice qué queda respaldado y qué no, con el motivo de cada fallo.

**Resultado en una línea:** el SDK oficial de Python completa sus 4 comprobaciones por los dos bindings; el TCK deja **9 requisitos MUST automatizados en estado fallido** (6 en `jsonrpc`, 9 en `http_json`). Cuatro de ellos tienen su causa en el SDK oficial de Rust fijado y cinco en que el TCK reutiliza un mismo `messageId` con contenido distinto, que PALS rechaza. **Por tanto, la conformidad plena con el TCK no está demostrada todavía.**

## Ejecución

| Dato | Valor |
|-|-|
| Fecha | 2026-11-14 |
| Commit de `pals` | `d142355` (el código; los scripts y este informe llegan en el commit siguiente, sin tocar código) |
| TCK | `a2aproject/a2a-tck` en `104a7b4ae0d83f65c075e3793e322953a830eaba`, Python 3.15.2, dependencias de su `uv.lock` (`uv sync --frozen`, uv 0.13.8) |
| Transportes | `jsonrpc`, `http_json` (el TCK no prueba gRPC: PALS no lo ofrece) |
| `TCK_STREAMING_TIMEOUT` | `2.0` (el valor por defecto) |
| SDK de Rust | `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`, y `a2a-pb 0.3.2` como dependencia de los anteriores |
| Resto | `iroh =1.4.1`, `iroh-blobs =0.105.0`, Rust 1.99.1 |
| Cliente independiente | `a2a-sdk==1.3.0` (SDK oficial de Python, protocolo 1.0), Python 3.12.3, dependencias fijadas en [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt) |

**Montaje.** Dos daemons en la misma máquina, en directorios temporales, con Iroh solo en 127.0.0.1 y sin relay: un proveedor con el adaptador `tck` (que reproduce los escenarios de `scenarios/*.feature` del TCK por prefijo de `messageId`) y un solicitante con su gateway. Intercambian contactos y un grant (`task.send,task.read,task.cancel` sobre `skill:ask`) con la CLI `pals`. El TCK habla con el gateway del solicitante como lo haría cualquier cliente estándar:

```
--sut-host http://127.0.0.1:<puerto del gateway>/peers/<id del proveedor>
```

Cada petición del TCK sale del gateway con el grant, viaja por Iroh con el binding [`http-over-iroh/v1`](../../bindings/http-over-iroh-v1.md) y llega al servidor A2A del proveedor.

**Reproducir** (desde la raíz del repo, con Rust, Python 3 y acceso a GitHub y PyPI):

```bash
tests/interop/run_tck.sh
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/python_client.py
```

`run_tck.sh` clona el TCK en `target/tck` en el commit fijado, lo instala con `uv`, monta los dos daemons, ejecuta `--level must` y `--level should` y copia aquí los informes: [`compatibility.json`](compatibility.json) y [`compatibility.html`](compatibility.html) (MUST), [`compatibility-should.json`](compatibility-should.json) y [`compatibility-should.html`](compatibility-should.html) (SHOULD). Sale con el código de la ejecución MUST.

## Resultado por transporte

Requisitos MUST por transporte, según `per_requirement` de [`compatibility.json`](compatibility.json) (los informes cuentan también pruebas sueltas, por eso sus totales son otros):

| Estado | `jsonrpc` | `http_json` |
|-|-|-|
| Superado | 40 | 35 |
| **Fallido** | **6** | **9** |
| Saltado por una capacidad que PALS no declara (*push*, card extendida, extensión obligatoria) | 14 | 13 |

Además: la card (`CARD-DISC-001`, `CARD-STRUCT-001`, `CARD-PROTO-001`, `CARD-PROTO-002`, `BIND-FIELD-001`) pasa entera; 12 requisitos MUST no se llegaron a ejercitar en ningún transporte y 6 son solo de gRPC (abajo); 16 no tienen prueba automática en el TCK (`NOT TESTED`) y 7 son `MANUAL`. En total, 51 de los 115 requisitos MUST superados y 9 fallidos.

Nivel SHOULD ([`compatibility-should.json`](compatibility-should.json)): 3 superados, 3 fallidos, 4 saltados, 2 `MANUAL` y 2 sin prueba automática.

### Requisitos MUST fallidos

**Causa en el SDK oficial de Rust fijado** (no en PALS; no se aceptan ni se dan por buenos):

| Requisito | Transporte | Qué pasa | Origen |
|-|-|-|-|
| `CORE-ERR-002` | `jsonrpc`, `http_json` | Un `SendMessage` sin `message` se acepta y crea una tarea | `a2a-pb 0.3.2`, `from_proto_send_message_request`: si falta `message`, pone `Message::new(Role::User, vec![])`, con un `messageId` nuevo, y el servidor no valida los campos obligatorios |
| `VER-SERVER-002` | `http_json` | `A2A-Version: 99.0` se atiende con normalidad | `a2a-server-lf 0.5.3`: el router HTTP+JSON no comprueba `A2A-Version`; el de JSON-RPC sí (`check_a2a_version`), y en `jsonrpc` el requisito pasa |
| `HTTP_JSON-STATUS-001` | `http_json` | Solo falla la parte de la versión, por la misma causa que `VER-SERVER-002` | `a2a-server-lf 0.5.3` |
| `HTTP_JSON-ERR-001` | `http_json` | Los errores llevan `Content-Type: application/problem+json`; el TCK exige que contenga `application/json` | `a2a-server-lf 0.5.3` (`rest.rs`). El ejemplo de A2A §11.6 usa `application/a2a+json`, que tampoco pasaría esta comprobación del TCK |

**Causa en la interacción entre el TCK y la idempotencia de PALS:**

| Requisito | Transporte | Qué pasa |
|-|-|-|
| `CORE-EXECUTION-MODE-001`, `CORE-EXECUTION-MODE-002`, `CORE-MULTI-001a`, `CORE-MULTI-002a`, `CORE-MULTI-003` | `jsonrpc`, `http_json` | PALS responde `-32602` (`400` en HTTP+JSON) «messageId reused with different content» |

El TCK genera el `messageId` de estos requisitos con `tck_id("complete-task")` más el transporte, el mismo valor durante toda la sesión, y lo envía con contenido distinto en cada requisito (`CORE-SEND-001`, `CORE-EXECUTION-MODE-*`, `CORE-MULTI-*`). A2A define `messageId` como identificador único creado por quien envía el mensaje (`a2a.proto`) y permite que el agente lo use para detectar duplicados (§3.3.1, «MAY»). PALS lo hace: repetir un `messageId` con el mismo contenido devuelve la tarea del primer envío sin ejecutar otra vez; repetirlo con otro contenido se rechaza. Los propios mantenedores del TCK han corregido reutilizaciones parecidas (#164, #250). Una ejecución de diagnóstico con un binario en el que el guardián no deduplicaba (no versionado) dejó solo los cuatro fallos del SDK: 63 MUST superados y 4 fallidos.

### Requisitos MUST no ejercitados

- **Por capacidades no declaradas** (correcto): notificaciones *push* (`PUSH-*`, `CORE-CAP-002`), card extendida (`CARD-EXT-001`, `CARD-EXT-002`), extensión obligatoria (`CORE-CAP-004`), gRPC (`GRPC-*`).
- **Porque la tarea compartida del TCK no estaba en el estado esperado**: `CORE-GET-001`, `CORE-CANCEL-001`, `CORE-CANCEL-002`, `CORE-SEND-002`, `CORE-MULTI-005`, `CORE-MULTI-006`, `STREAM-SUB-003`. Las funciones auxiliares del TCK (`create_completed_task`, `create_working_task`, `create_multiturn_task`) reutilizan su `messageId` entre pruebas y entre transportes; con la idempotencia de PALS reciben la tarea de un envío anterior (ya cancelada o completada) o el rechazo de contenido distinto, y el TCK salta la prueba. Sin deduplicación, en la ejecución de diagnóstico, estos siete pasan.
- **Por el SDK de Rust**: `STREAM-SUB-001`, `STREAM-SUB-002`, `STREAM-ORDER-002`, `STREAM-ORDER-003`, `STREAM-ORDER-004`. Se suscriben con `SubscribeToTask` a una tarea en `TASK_STATE_INPUT_REQUIRED`; `a2a-server-lf 0.5.3` responde `TaskNotFoundError` a una tarea no terminal que no tiene una ejecución activa (`handler.rs`, `subscribe_to_task`), y el TCK salta la prueba.
- **Sin prueba automática en el TCK** (`NOT TESTED`): `AUTH-SERVER-002`, `AUTH-INTASK-001` a `004`, `AUTH-SCOPE-001` a `003`, `VER-CLIENT-001`, `VER-CLIENT-002`, `VER-SERVER-001`, `BIND-EQUIV-001` a `004`, `GRPC-SVC-003`.

### Requisitos SHOULD fallidos

| Requisito | Causa |
|-|-|
| `CARD-CACHE-001`, `CARD-CACHE-002` | La card que sirve el gateway (`/peers/<id>/.well-known/agent-card.json`) no lleva `Cache-Control` ni `ETag`. Es del gateway de PALS, que compone la vista de la card; queda pendiente |
| `HTTP_JSON-SVC-001` | Los errores HTTP+JSON llevan `application/problem+json` en lugar de `application/a2a+json`: SDK de Rust, como `HTTP_JSON-ERR-001` |

### Requisitos `MANUAL` del TCK

El TCK no los puede comprobar desde fuera; no cuentan como superados.

| Requisito | Nivel | Estado en PALS |
|-|-|-|
| `CARD-CLIENT-001` (el cliente usa el *tenant* de la interfaz elegida) | MUST | Ninguna interfaz declara *tenant* y el cliente de PALS no lo envía. Sin verificación independiente |
| `CARD-SIGN-001` a `CARD-SIGN-004` (firma JWS de la card) | MUST | No aplica en el hito 1: la card no está firmada (sin `signatures`). La firma llega en el hito 2 |
| `AUTH-TLS-001` (tráfico cifrado en producción) | MUST | Entre pares, QUIC de Iroh (TLS 1.3). El gateway local es HTTP sin TLS y solo escucha en loopback: el tráfico no sale de la máquina. Pendiente de aceptación explícita |
| `AUTH-TLS-002` (TLS moderno) | SHOULD | Entre pares, TLS 1.3 de QUIC. No aplica al gateway de loopback |
| `AUTH-SERVER-001` (el cliente valida el certificado contra CA de confianza) | SHOULD | Desviación deliberada: la identidad del par es su clave Ed25519, comprobada en el handshake de QUIC, no un certificado de una CA |
| `BIND-CUSTOM-002` (un cambio incompatible del binding usa URI nueva) | MUST | Comprometido en la [especificación del binding](../../bindings/http-over-iroh-v1.md#1-identificación): `v1` no cambia de significado; un cambio incompatible será `v2` |

## Cliente independiente con el SDK oficial de Python

`tests/interop/python_client.py` monta un solicitante y dos proveedores (uno con el adaptador `echo` y otro con el adaptador `tck`, cuya tarea `test-resubscribe-message-id` sigue en curso el tiempo suficiente para cancelarla), resuelve las cards desde el gateway con `A2ACardResolver` y comprueba cada punto con los dos bindings que declara la card:

```
card resolved from the gateway: 'pals-interop-echo', interfaces JSONRPC, HTTP+JSON
check 1 passed: SendMessage answers "echo: hola" (JSONRPC, HTTP+JSON)
check 2 passed: SendStreamingMessage ends TASK_STATE_COMPLETED (JSONRPC, HTTP+JSON)
check 3 passed: GetTask returns the completed task (JSONRPC, HTTP+JSON)
check 4 passed: CancelTask leaves a long task TASK_STATE_CANCELED (JSONRPC, HTTP+JSON)
4 checks passed
```

## Cambios en PALS hechos para la conformidad

- El gateway acepta `.../peers/<id>/jsonrpc/` (con barra final) como el endpoint JSON-RPC: el cliente JSON-RPC del TCK, como cualquier cliente `httpx` con `base_url`, envía `POST /` relativo a la URL de la interfaz. Sin esto fallaban todas las pruebas JSON-RPC.
- La card declara `defaultInputModes: ["text/plain", "*/*"]`: el agente acepta partes de cualquier tipo (los ficheros van como blobs) y antes declaraba solo `text/plain`.

## Lo que queda respaldado

Con las excepciones de los apartados anteriores, que siguen abiertas:

> **A2A v1.0 con los bindings JSON-RPC y HTTP+JSON a través del gateway local; entre pares, el binding propio `http-over-iroh/v1`, que transporta esos mismos bindings.**

Respaldado: las operaciones de tarea (`SendMessage`, `SendStreamingMessage`, `GetTask`, `ListTasks`, `CancelTask`, `SubscribeToTask`), el modelo de datos y su serialización, la card y su descubrimiento, los errores de A2A y su forma JSON-RPC, y el streaming SSE, según los 51 requisitos MUST que el TCK da por superados y el cliente oficial de Python. No respaldado aún: los 9 requisitos MUST fallidos, los 12 que no se pudieron ejercitar por el TCK o el SDK, y los `MANUAL` pendientes.
