# Conformidad A2A del hito 1

Evidencia de la compatibilidad con A2A v1.0 del intercambio final de PALS: el TCK oficial ejecutado a través del gateway local, y un cliente independiente hecho con el SDK oficial de Python. Este documento dice qué queda respaldado y qué no.

**Resultado en una línea:** el TCK no deja **ningún requisito MUST automatizado fallido** en `jsonrpc` ni en `http_json` (72 superados; los saltados lo son por capacidades que PALS no declara o por ser de gRPC), y el SDK oficial de Python completa sus 4 comprobaciones por los dos bindings. Queda pendiente un requisito `MANUAL` de nivel MUST, `AUTH-TLS-001`, que necesita la aceptación expresa del responsable.

## Ejecución

| Dato | Valor |
|-|-|
| Fecha | 2026-11-14 |
| Commit de `pals` | `27ef71f` (el código, tras la última ronda de correcciones; este informe y los del TCK llegan en el commit siguiente, sin tocar código). Los resultados coinciden requisito a requisito con los de la ejecución anterior, en `2f4d1cc` |
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

Cada petición del TCK sale del gateway con el grant, viaja por Iroh con los bindings propios [`http-over-iroh/v1`](../../bindings/http-over-iroh-v1.md) (`…/v1/jsonrpc` o `…/v1/http-json`, según el binding estándar que lleve dentro) y llega al servidor A2A del proveedor. El TCK no activa la extensión de idempotencia de PALS, así que el proveedor se comporta como los SDK de referencia ante un `messageId` repetido (ver [`idempotency-v1`](../../extensions/idempotency-v1.md)).

**Reproducir** (desde la raíz del repo, con Rust, Python 3 y acceso a GitHub y PyPI):

```bash
tests/interop/run_tck.sh
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/python_client.py
```

`run_tck.sh` clona el TCK en `target/tck` en el commit fijado, lo instala con `uv`, monta los dos daemons, ejecuta `--level must` y `--level should` y copia aquí los informes: [`compatibility.json`](compatibility.json) y [`compatibility.html`](compatibility.html) (MUST), [`compatibility-should.json`](compatibility-should.json) y [`compatibility-should.html`](compatibility-should.html) (SHOULD). Sale con el código de la ejecución MUST, `0` en esta ejecución.

## Resultado

Requisitos MUST por transporte, según `per_requirement` de [`compatibility.json`](compatibility.json) (los informes cuentan también pruebas sueltas, por eso sus totales son otros):

| Estado | `jsonrpc` | `http_json` |
|-|-|-|
| Superado | 58 | 56 |
| **Fallido** | **0** | **0** |
| Saltado por una capacidad que PALS no declara | 14 | 13 |

Además, la card (`CARD-DISC-001`, `CARD-STRUCT-001`, `CARD-PROTO-001`, `CARD-PROTO-002`, `BIND-FIELD-001`) pasa entera. En total, de los 115 requisitos MUST: 72 superados, 0 fallidos, 20 saltados, 7 `MANUAL` y 16 sin prueba automática en el TCK.

**Saltados, todos por el propio TCK al leer la card:** notificaciones *push* (`PUSH-CREATE-001`, `PUSH-CREATE-002`, `PUSH-GET-001`, `PUSH-GET-002`, `PUSH-LIST-001`, `PUSH-DEL-001`, `PUSH-DEL-002`, `PUSH-DELIVER-001` a `003`), card extendida (`CARD-EXT-001`, `CARD-EXT-002`), extensión obligatoria (`CORE-CAP-004`: la card no declara ninguna como obligatoria), streaming no soportado (`CORE-CAP-002`: PALS sí lo soporta, la prueba no aplica) y gRPC (`GRPC-SVC-001`, `GRPC-SVC-002`, `GRPC-META-001`, `GRPC-ERR-001` a `003`).

**Sin prueba automática en el TCK** (`NOT TESTED`): `AUTH-SERVER-002`, `AUTH-INTASK-001` a `004`, `AUTH-SCOPE-001` a `003`, `VER-CLIENT-001`, `VER-CLIENT-002`, `VER-SERVER-001`, `BIND-EQUIV-001` a `004`, `GRPC-SVC-003`. No cuentan como superados.

**Nivel SHOULD** ([`compatibility-should.json`](compatibility-should.json)): 8 superados, 1 fallido, 1 saltado, 2 `MANUAL` y 2 sin prueba automática. El fallido es `HTTP_JSON-SVC-001`: pide que los errores de HTTP+JSON lleven `Content-Type: application/a2a+json`, mientras que el requisito MUST `HTTP_JSON-ERR-001` exige un tipo que contenga `application/json`. Las dos comprobaciones del TCK no se pueden cumplir a la vez; PALS cumple la MUST y sirve los errores como `application/json`. El saltado es `BIND-CUSTOM-001`: la card que ve el TCK, la del gateway, no declara bindings propios.

### Requisitos `MANUAL` del TCK

El TCK no los puede comprobar desde fuera; no cuentan como superados.

| Requisito | Nivel | Estado en PALS |
|-|-|-|
| `CARD-CLIENT-001` (el cliente usa el *tenant* de la interfaz elegida) | MUST | Ninguna interfaz declara *tenant* y el cliente de PALS no lo envía. Sin verificación independiente |
| `CARD-SIGN-001` a `CARD-SIGN-004` (firma JWS de la card) | MUST | No aplica en el hito 1: la card no está firmada (sin `signatures`). La firma llega en el hito 2 |
| `AUTH-TLS-001` (tráfico cifrado en producción) | MUST | **Pendiente de aceptación del responsable.** Entre pares, QUIC de Iroh (TLS 1.3). El gateway local es HTTP sin TLS y solo escucha en loopback: el tráfico no sale de la máquina |
| `AUTH-TLS-002` (TLS moderno) | SHOULD | Entre pares, TLS 1.3 de QUIC. No aplica al gateway de loopback |
| `AUTH-SERVER-001` (el cliente valida el certificado contra CA de confianza) | SHOULD | Desviación deliberada: la identidad del par es su clave Ed25519, comprobada en el handshake de QUIC, no un certificado de una CA |
| `BIND-CUSTOM-002` (un cambio incompatible del binding usa URI nueva) | MUST | Comprometido en la [especificación de los bindings](../../bindings/http-over-iroh-v1.md#1-identificación): las URI de `v1` no cambian de significado; un cambio incompatible será `v2` |

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

Del propio PALS:

- El gateway acepta `.../peers/<id>/jsonrpc/` (con barra final) como el endpoint JSON-RPC: el cliente JSON-RPC del TCK, como cualquier cliente `httpx` con `base_url`, envía `POST /` relativo a la URL de la interfaz.
- La card declara `defaultInputModes: ["text/plain", "*/*"]`: el agente acepta partes de cualquier tipo (los ficheros van como blobs) y antes declaraba solo `text/plain`.
- La idempotencia por `messageId` pasa a ser la extensión opcional [`idempotency/v1`](../../extensions/idempotency-v1.md), que el cliente activa con `A2A-Extensions`. Sin ella, un `messageId` repetido es un mensaje nuevo, como en los SDK de referencia; el TCK reutiliza el mismo `messageId` con contenidos distintos entre requisitos, y antes recibía `-32602` en cinco de ellos. El cliente de PALS la activa siempre.
- La respuesta a una petición que activa la extensión la nombra en su cabecera `A2A-Extensions`, como A2A recomienda para las extensiones activadas; el gateway la devuelve sin cambios.
- La card canónica que el agente sirve a sus pares declara sus dos interfaces entre pares (A2A §5.2): `iroh://<id>/jsonrpc` con `…/bindings/http-over-iroh/v1/jsonrpc` y `iroh://<id>/rest` con `…/bindings/http-over-iroh/v1/http-json`. Antes solo declaraba la primera. La vista del gateway no cambia.
- La vista de la card en el gateway lleva `Cache-Control: max-age=300` y un `ETag` fuerte, y responde `304` a un `If-None-Match` que lo nombra (A2A §8.6; `CARD-CACHE-001`, `CARD-CACHE-002`).
- El gateway comprueba cada respuesta del par antes de dársela al cliente: como mucho 16 MiB por cuerpo o por evento SSE, JSON en las respuestas correctas y en los datos de cada evento, sin `Content-Encoding`, y ningún `taskId` ni `id` de tarea inseguro (un cliente que normaliza `../` en la URL de su siguiente petición llegaría a otro par con el grant del gateway). El TCK y el SDK de Python pasan por esas comprobaciones en cada petición; un par PALS no envía nada que las active.
- `ListTasks` ordena de la tarea actualizada más recientemente a la más antigua, pagina con un cursor opaco y omite los artefactos salvo con `includeArtifacts: true` (A2A §3.1.4; ver la tabla siguiente).

Rodeos de defectos del SDK oficial de Rust fijado, cada uno en una capa propia de PALS y con su prueba en `crates/pals/tests/conformance.rs`; los textos para comunicarlos aguas arriba están en [`sdk-issues.md`](sdk-issues.md):

| Requisito | Defecto del SDK | Rodeo en PALS |
|-|-|-|
| `CORE-ERR-002` | `a2a-pb 0.3.2`, `from_proto_send_message_request`: un `SendMessage` sin `message` se convierte en un mensaje vacío con `messageId` nuevo, y nada valida los campos obligatorios | El guardián rechaza con `InvalidParamsError` un mensaje sin `messageId`, sin `role` o sin partes |
| `VER-SERVER-002`, `HTTP_JSON-STATUS-001` | `a2a-server-lf 0.5.3`: el router HTTP+JSON no comprueba `A2A-Version`, y el de JSON-RPC solo la lee de la cabecera | Una capa delante del router HTTP+JSON la comprueba como el router JSON-RPC del SDK y responde `VersionNotSupportedError` con `400` y `google.rpc.Status`. En los dos bindings, si falta la cabecera, la versión se toma del parámetro de la petición `A2A-Version` (A2A §3.6.1); sin ninguno de los dos cuenta como `0.3` |
| `HTTP_JSON-ERR-001` | `a2a-server-lf 0.5.3`, `rest_error_response`: errores con `Content-Type: application/problem+json` | La misma capa los sirve como `application/json`; el cuerpo ya era el `google.rpc.Status` de §11.6 |
| `STREAM-SUB-001`, `STREAM-SUB-002`, `STREAM-ORDER-002` a `004` | `a2a-server-lf 0.5.3`, `subscribe_to_task`: `TaskNotFoundError` para una tarea no terminal sin ejecución viva (p. ej. esperando entrada) | El guardián emite la tarea como está y la sigue, con las ejecuciones que reciba, hasta un estado terminal |
| `CORE-LIST-002`, `CORE-LIST-003`, `CORE-LIST-005` | `a2a-server-lf 0.5.3`, `InMemoryTaskStore::list`: ordena por id, pagina con desplazamientos y devuelve siempre los artefactos. El TCK lista un contexto vacío y solo comprueba la forma de la respuesta, así que no lo detecta | El almacén de PALS lista de la tarea actualizada más recientemente a la más antigua (marca de tiempo del estado; a igualdad, por id), pagina con un cursor opaco y omite `artifacts` salvo con `includeArtifacts: true` (A2A §3.1.4). Estos MUST se cumplen ahora por comportamiento, no solo por la forma; prueba también en `crates/pals/tests/task_store.rs` |

## Lo que queda respaldado

> **A2A v1.0 con los bindings JSON-RPC y HTTP+JSON a través del gateway local; entre pares, el binding propio `http-over-iroh/v1` (`…/v1/jsonrpc` y `…/v1/http-json`), que transporta esos mismos bindings.**

En concreto: todos los requisitos MUST que el TCK en `104a7b4ae0d8` comprueba de forma automática para esos dos bindings se superan, salvo los de capacidades que la card no declara (notificaciones *push*, card extendida) y los de gRPC, que PALS no ofrece; y el SDK oficial de Python opera con PALS por los dos bindings. No queda respaldado: los 16 requisitos MUST sin prueba automática en el TCK, los `MANUAL` (entre ellos `AUTH-TLS-001`, pendiente de aceptación) y el SHOULD `HTTP_JSON-SVC-001`.
