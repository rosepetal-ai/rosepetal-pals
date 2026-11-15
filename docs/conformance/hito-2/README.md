# Conformidad A2A del hito 2

Evidencia de que el hito 2 no cambia la compatibilidad con A2A v1.0 del intercambio final de PALS. Es el caso 9 de la [prueba de salida](exit-test.md). Como en el [hito 1](../hito-1/README.md), se repiten el TCK oficial, ejecutado a través del gateway local, y el cliente independiente hecho con el SDK oficial de Python. Además, los vectores congelados del hito 2 se validan contra los esquemas JSON de la extensión de inventario. Este documento dice qué queda respaldado, qué no y en qué difiere del hito 1, requisito a requisito.

**Resultado en una línea:** el TCK sigue sin dejar **ningún requisito MUST automatizado fallido** en `jsonrpc` ni en `http_json`. Hay 72 requisitos MUST superados, como en el hito 1; por transporte, 58 en `jsonrpc` (58 en el hito 1) y 57 en `http_json` (56). `CARD-EXT-001` pasa a superado, porque la card declara ahora una card extendida. `CORE-CAP-003`, que el TCK solo prueba cuando no la hay, pasa a saltado. El SDK oficial de Python completa sus 4 comprobaciones por los dos bindings, y los 7 documentos de los vectores del hito 2 son válidos contra sus esquemas. Sigue pendiente `AUTH-TLS-001`, que necesita la aceptación expresa del responsable.

## Ejecución

| Dato | Valor |
|-|-|
| Fecha | 2026-11-15, de 09:52 a 09:56 UTC |
| Commit de `pals` | `7a60c7c`, el de la tanda de correcciones de la revisión final del hito (outbox y gateway de los contactos A2A corrientes, consultas y búsquedas, ajuste de las vistas). Repite la ejecución anterior, en `812f885` con el código de `eea2281`: requisito a requisito, los mismos estados y transportes en los dos niveles; solo cambian la URL del gateway, el id del proveedor temporal y las horas |
| TCK | `a2aproject/a2a-tck` en `104a7b4ae0d83f65c075e3793e322953a830eaba`, el mismo commit que en el hito 1. Python 3.15.2 y las dependencias de su `uv.lock` (`uv sync --frozen`, uv 0.13.8) |
| Transportes | `jsonrpc` y `http_json`. El TCK no prueba gRPC, que PALS no ofrece |
| `TCK_STREAMING_TIMEOUT` | `2.0` (el valor por defecto). Una sola ejecución de cada nivel, sin repetir ninguna prueba |
| SDK de Rust | `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3` y `a2a-client-lf =0.2.9`, los mismos que en el hito 1, y `a2a-pb =0.3.2`, fijado también directamente porque `pals-core` lo usa (formas protobuf de las cards), con `prost =0.14.6` para los mensajes de `agentnet-discovery/0` |
| Resto | `iroh =1.4.1`, `iroh-blobs =0.105.0`, `rmcp =3.10.0`, Rust 1.99.1 |
| Cliente independiente | `a2a-sdk==1.3.0` (SDK oficial de Python, protocolo 1.0), Python 3.12.3, dependencias fijadas en [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt) |
| Validador de esquemas | `jsonschema==4.26.0` (Draft 2020-12), fijado en el mismo fichero |
| Anfitrión | Linux x86_64, 32 CPU, compartido con otros proyectos. Carga media al empezar: 65,4 / 64,1 / 51,5; al terminar: 85,9 / 78,5 / 59,9 |

**Montaje.** Es el del hito 1: dos daemons en la misma máquina, en directorios temporales, con Iroh solo en 127.0.0.1 y sin relay. Uno es el proveedor, con el adaptador `tck`; el otro, el solicitante, con su gateway. Intercambian contactos y un grant (`task.send,task.read,task.cancel` sobre `skill:ask`) con la CLI `pals`. El TCK habla con el gateway del solicitante, en `http://127.0.0.1:<puerto del gateway>/peers/<id del proveedor>`, y cada petición viaja por Iroh con los bindings propios [`http-over-iroh/v1`](../../bindings/http-over-iroh-v1.md).

El inventario del proveedor está vacío: `pals init` escribe `inventory.toml` solo con ejemplos comentados. Con el hito 2, la card que ve el TCK (la vista del gateway, en `compatibility.json`, clave `agent_card`) cambia en tres puntos:

- declara `capabilities.extendedAgentCard: true`;
- declara también la extensión [`inventory/v1`](../../extensions/inventory-v1.md), con `params` `discoveryAlpn: agentnet-discovery/0` y `profile: agentnet.inventory/1`;
- la extensión `idempotency/v1` ya no lleva `"required": false`. Las cards que emite PALS están normalizadas, sin valores por defecto, para que las tres formas canónicas en uso sean los mismos bytes ([firmas de la card](card-signatures.md)).

La vista no lleva `signatures`: el gateway reescribe las interfaces, que la firma cubría. La card firmada, tal cual, se sirve en `GET /peers/<id>/signed-agent-card.json`. Con el inventario vacío, la card extendida tiene la misma skill `ask` que la pública. El TCK no activa la extensión de inventario ni la de idempotencia, así que el proveedor atiende sus peticiones como en el hito 1.

**Reproducir** (desde la raíz del repo, con Rust, Python 3 y acceso a GitHub y PyPI):

```bash
PALS_CONFORMANCE_DIR=docs/conformance/hito-2 tests/interop/run_tck.sh
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/python_client.py
target/interop-venv/bin/python tests/interop/check_schemas.py
```

Con `PALS_CONFORMANCE_DIR`, `run_tck.sh` copia los informes a este directorio: [`compatibility.json`](compatibility.json) y [`compatibility.html`](compatibility.html) (MUST), y [`compatibility-should.json`](compatibility-should.json) y [`compatibility-should.html`](compatibility-should.html) (SHOULD). Sin la variable los copia, como antes, a `docs/conformance/hito-1/`. Los informes del hito 1 no se tocaron en esta ejecución: `git diff --stat docs/conformance/hito-1` sale vacío. La ejecución salió con el código `0` de la ejecución MUST.

Los cuatro informes copiados no contienen rutas del anfitrión, comprobado con `grep`: solo la URL del gateway, en loopback, y el id del proveedor temporal. El informe `tck_report.html` de pytest-html sí incluye rutas absolutas del clon del TCK; `run_tck.sh` no lo copia, y queda en `target/tck-run/<hora UTC>/reports-<nivel>/`.

Salida del TCK, nivel MUST:

```text
│ MUST   │ 72     │ 0      │ 20      │ 7      │ 16         │ 115   │
  agent_card:    6/6 ✓
  grpc:          0/65 (65 skipped) ✓
  jsonrpc:       75/90 (15 skipped) ✓
  http_json:     71/85 (14 skipped) ✓
134 passed, 95 skipped, 34 deselected in 97.98s (0:01:37)
[run_tck] TCK --level must exited 0
```

Esas columnas son superados, fallidos, saltados, `MANUAL` y sin prueba. Las líneas por transporte cuentan pruebas, no requisitos: el hito 1 tenía 75/90 en `jsonrpc` y 70/85 en `http_json`.

## Resultado

Requisitos MUST por transporte, según `per_requirement` de [`compatibility.json`](compatibility.json), con el hito 1 entre paréntesis:

| Estado | `jsonrpc` | `http_json` |
|-|-|-|
| Superado | 58 (58) | 57 (56) |
| **Fallido** | **0** (0) | **0** (0) |
| Saltado por una capacidad que PALS no declara, o cuya precondición no se da | 14 (14) | 12 (13) |

Además, la card (`CARD-DISC-001`, `CARD-STRUCT-001`, `CARD-PROTO-001`, `CARD-PROTO-002` y `BIND-FIELD-001`) pasa entera. En total, de los 115 requisitos MUST hay 72 superados, 0 fallidos, 20 saltados, 7 `MANUAL` y 16 sin prueba automática en el TCK. Son las mismas cifras que en el hito 1, porque los dos cambios de estado se compensan.

**Saltados, todos por el propio TCK:**

- **Notificaciones *push*:** `PUSH-CREATE-001`, `PUSH-CREATE-002`, `PUSH-GET-001`, `PUSH-GET-002`, `PUSH-LIST-001`, `PUSH-DEL-001`, `PUSH-DEL-002` y `PUSH-DELIVER-001` a `003`.
- **Card extendida:** `CARD-EXT-002`, porque la card extendida está configurada, y `CORE-CAP-003`, porque la card la declara. Ver la tabla siguiente.
- **Extensión obligatoria:** `CORE-CAP-004`. La card no declara ninguna como obligatoria.
- **Streaming no soportado:** `CORE-CAP-002`. PALS sí lo soporta, así que la prueba no aplica.
- **gRPC:** `GRPC-SVC-001`, `GRPC-SVC-002`, `GRPC-META-001` y `GRPC-ERR-001` a `003`.

**Sin prueba automática en el TCK** (`NOT TESTED`), los mismos 16 que en el hito 1: `AUTH-SERVER-002`, `AUTH-INTASK-001` a `004`, `AUTH-SCOPE-001` a `003`, `VER-CLIENT-001`, `VER-CLIENT-002`, `VER-SERVER-001`, `BIND-EQUIV-001` a `004` y `GRPC-SVC-003`. No cuentan como superados.

**Nivel SHOULD** ([`compatibility-should.json`](compatibility-should.json)), igual que en el hito 1: 8 superados, 1 fallido, 1 saltado, 2 `MANUAL` y 2 sin prueba automática.

- El fallido sigue siendo `HTTP_JSON-SVC-001`, con el mismo motivo: sus dos comprobaciones no se pueden cumplir a la vez con la MUST `HTTP_JSON-ERR-001` (ver el [hito 1](../hito-1/README.md#resultado)).
- El saltado, `BIND-CUSTOM-001`: la card que ve el TCK, la del gateway, no declara bindings propios.

## Diferencias con el hito 1, requisito a requisito

Comparados los dos `compatibility.json` y los dos `compatibility-should.json` requisito a requisito, estado y transportes, solo cambian estos dos:

| Requisito | Nivel | Hito 1 | Hito 2 | Motivo |
|-|-|-|-|-|
| `CARD-EXT-001` (una petición autenticada de `GetExtendedAgentCard` recibe la card extendida) | MUST | Saltado en `jsonrpc`, `http_json` y `grpc` | **Superado** en `jsonrpc` y `http_json`; `grpc`, saltado | La card declara ahora `capabilities.extendedAgentCard: true` (tarea 4 del plan). El TCK llama sin credenciales propias. El gateway añade el grant que tiene del proveedor, y el proveedor responde con la card extendida del ámbito de ese grant. El gateway la verifica, firmada por el par, y devuelve su vista. Sin un grant aceptado, el proveedor responde `401` (`cards::extended_card_without_grant_is_401`) |
| `CORE-CAP-003` (sin card extendida declarada, `GetExtendedAgentCard` devuelve `UnsupportedOperationError`) | MUST, condicional | Superado en `jsonrpc` | Saltado en `jsonrpc` | El TCK solo lo prueba cuando la card no declara `extendedAgentCard` (`tests/compatibility/core_operations/test_error_handling.py:384-386` en `104a7b4`), y PALS la declara: la condición del requisito ya no se da. La comprobación sigue siendo la del SDK, sin cambios: `a2a-server-lf 0.5.3`, `handler.rs:1144-1148`, responde `UnsupportedOperationError` cuando la card no declara la capacidad. PALS no la rodea ni la sustituye |

En los dos niveles, ningún otro requisito cambia de estado ni de transportes. Por eso las cifras por requisito quedan así: en `jsonrpc`, 58 superados como en el hito 1 (uno más y uno menos); en `http_json`, 57 en lugar de 56, y 12 saltados en lugar de 13.

Cambian sin cambiar de estado:

- **`CARD-EXT-002`** (card extendida declarada pero no configurada) sigue saltado, por otro motivo. En el hito 1, la card no declaraba la capacidad. Ahora sí la declara, y el TCK, que sondea el endpoint, recibe la card: «Extended card is configured (test precondition does not hold)».
- **`CARD-SIGN-001` a `CARD-SIGN-004`** siguen `MANUAL`, pero ya aplican: la card va firmada (ver la tabla siguiente).

Las cards ganan la firma y la card extendida. La tarea se sigue enviando con los bindings del hito 1, sin cambios: una petición que no activa la extensión de inventario se atiende como en el hito 1. Los rodeos de defectos del SDK fijado son los mismos que en el hito 1, con sus pruebas en `crates/pals/tests/conformance.rs` (ver la [tabla del hito 1](../hito-1/README.md#cambios-en-pals-hechos-para-la-conformidad) y [`sdk-issues.md`](../hito-1/sdk-issues.md)).

### Requisitos `MANUAL` del TCK

El TCK no los puede comprobar desde fuera, y no cuentan como superados. Respecto al hito 1, cambia lo que se dice de la firma de la card, de `AUTH-TLS-001` y de `AUTH-SERVER-001`.

| Requisito | Nivel | Estado en PALS |
|-|-|-|
| `CARD-CLIENT-001` (el cliente usa el *tenant* de la interfaz elegida) | MUST | Como en el hito 1: ninguna interfaz declara *tenant* y el cliente de PALS no lo envía. Sin verificación independiente |
| `CARD-SIGN-001` a `CARD-SIGN-004` (firma JWS de la card) | MUST | **Aplican desde el hito 2.** El agente firma la card que sirve a sus pares (`pals_core::card_jws`) con un JWS `EdDSA` separado. Lo firmado es la JCS (RFC 8785) de la card sin `signatures` (001, 002). La cabecera protegida es `{"alg":"EdDSA","kid":"<id del agente>","typ":"JOSE"}` (003). La clave es la identidad Ed25519 del agente, que en este hito no caduca ni se revoca. Al verificar, solo se acepta como `kid` el id del par, el que probó el handshake de Iroh (004). La verifican, de forma independiente, los SDK oficiales de Python, Go y JS: el vector congelado en [`card-signatures.md`](card-signatures.md), y la card en vivo de un par en el [caso 3 de la prueba de salida](exit-test.md#caso-3-validar-cards-superado). La card que ve el TCK, la del gateway, no lleva firmas |
| `AUTH-TLS-001` (tráfico cifrado en producción) | MUST | **Pendiente de aceptación del responsable**, como en el hito 1. Entre pares, el tráfico va por QUIC de Iroh (TLS 1.3), y `agentnet-discovery/0` va por el mismo endpoint. El gateway local es HTTP sin TLS y solo escucha en loopback, así que el tráfico no sale de la máquina. Los contactos A2A corrientes del hito 2 se alcanzan por `https://`, o por `http://` solo a `127.0.0.1`, `[::1]` y `localhost` |
| `AUTH-TLS-002` (TLS moderno) | SHOULD | Entre pares, TLS 1.3 de QUIC. No aplica al gateway de loopback |
| `AUTH-SERVER-001` (el cliente valida el certificado contra CA de confianza) | SHOULD | Entre pares, sigue la desviación deliberada del hito 1: la identidad del par es su clave Ed25519, comprobada en el handshake de QUIC, no un certificado de una CA. Hacia los contactos A2A corrientes, el cliente de PALS valida el certificado con el verificador de la plataforma, contra las CA de confianza del sistema. Lo ejercita el caso 7 de la prueba de salida, con una CA de prueba instalada |
| `BIND-CUSTOM-002` (un cambio incompatible del binding usa URI nueva) | MUST | Como en el hito 1: comprometido en la [especificación de los bindings](../../bindings/http-over-iroh-v1.md#1-identificación). Las URI de `v1` no cambian de significado; un cambio incompatible será `v2` |

## Cliente independiente con el SDK oficial de Python

`tests/interop/python_client.py` no ha cambiado desde el hito 1; `requirements.txt` solo gana las dependencias de `card_signatures.py` y `check_schemas.py` (`PyJWT`, `jsonschema` y las suyas), con `a2a-sdk==1.3.0` igual. Su salida:

```text
card resolved from the gateway: 'pals-interop-echo', interfaces JSONRPC, HTTP+JSON
check 1 passed: SendMessage answers "echo: hola" (JSONRPC, HTTP+JSON)
check 2 passed: SendStreamingMessage ends TASK_STATE_COMPLETED (JSONRPC, HTTP+JSON)
check 3 passed: GetTask returns the completed task (JSONRPC, HTTP+JSON)
check 4 passed: CancelTask leaves a long task TASK_STATE_CANCELED (JSONRPC, HTTP+JSON)
4 checks passed
```

`A2ACardResolver` resuelve la card del gateway con la extensión nueva y `extendedAgentCard: true`, y el SDK opera igual que en el hito 1.

## Esquemas de la extensión de inventario

`tests/interop/check_schemas.py` comprueba dos cosas, con un validador independiente (`jsonschema`, Draft 2020-12). Primero, que cada esquema de [`docs/extensions/inventory-v1/`](../../extensions/inventory-v1/) es un esquema Draft 2020-12 válido. Después, que el documento de cada vector congelado del hito 2 es válido contra el suyo. Los vectores son de `crates/pals-core/tests/vectors/`: registro de contacto, de anuncio, inventario, vista, consulta de servicio, oferta e informe de benchmark. Su salida:

```text
7 documents valid
```

## Lo que queda respaldado

> **A2A v1.0 con los bindings JSON-RPC y HTTP+JSON a través del gateway local; entre pares, el binding propio `http-over-iroh/v1` (`…/v1/jsonrpc` y `…/v1/http-json`), que transporta esos mismos bindings. Sin regresión respecto al hito 1. La card que el agente sirve a sus pares va firmada con JWS (A2A §8.4), y la verifican los SDK oficiales de Python, Go y JS.**

En concreto: todos los requisitos MUST que el TCK en `104a7b4ae0d8` comprueba de forma automática para esos dos bindings se superan, ahora también `CARD-EXT-001`, la card extendida. Quedan fuera los de capacidades que la card no declara (notificaciones *push*), los dos cuya precondición no se da ahora que hay card extendida, declarada y configurada (`CORE-CAP-003` y `CARD-EXT-002`) y los de gRPC, que PALS no ofrece. El SDK oficial de Python opera con PALS por los dos bindings. La firma de la card queda respaldada por [`card-signatures.md`](card-signatures.md) y por el caso 3 de la [prueba de salida](exit-test.md).

No queda respaldado:

- los 16 requisitos MUST sin prueba automática en el TCK;
- los `MANUAL`, entre ellos `AUTH-TLS-001`, pendiente de aceptación;
- el SHOULD `HTTP_JSON-SVC-001`;
- lo que el hito 2 añade antes del intercambio A2A, que no es A2A y que el TCK no prueba: `agentnet-discovery/0`, las vistas del inventario, las consultas y ofertas de servicio y la búsqueda. Lo respaldan los tests y la [prueba de salida](exit-test.md);
- los contactos A2A corrientes en saliente. El TCK prueba servidores, y PALS no sirve nada nuevo por HTTPS. Los ejercita el caso 7 de la prueba de salida, con un agente sobre el SDK oficial de Python.
