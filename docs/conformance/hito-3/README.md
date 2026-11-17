# Conformidad A2A del hito 3

Evidencia de que el hito 3 no cambia la compatibilidad con A2A v1.0 del intercambio final de PALS. Como en los hitos [1](../hito-1/README.md) y [2](../hito-2/README.md), se repiten el TCK oficial, ejecutado a través del gateway local, y el cliente independiente hecho con el SDK oficial de Python. Los vectores congelados se validan otra vez contra los esquemas JSON de la extensión de inventario, y el vector de la card firmada se verifica con el SDK oficial de Python. La DHT del hito, `agentnet-kad/0`, la prueba su [prueba de salida](exit-test.md). Este documento dice qué queda respaldado, qué no y en qué difiere del hito 2, requisito a requisito.

**Resultado en una línea:** el TCK sigue sin dejar **ningún requisito MUST automatizado fallido** en `jsonrpc` ni en `http_json`, con las cifras del hito 2: 72 requisitos MUST superados; por transporte, 58 en `jsonrpc` y 57 en `http_json`. **Ningún requisito cambia de estado ni de transportes**, en el nivel MUST ni en el SHOULD. La card que ve el TCK es la del hito 2. El SDK oficial de Python completa sus 4 comprobaciones por los dos bindings, los 7 documentos de los vectores son válidos contra sus esquemas, y el vector de la card verifica (`3 checks passed`).

## Ejecución

| Dato | Valor |
|-|-|
| Fecha | 2026-11-17. `run_tck.sh`, de 04:01:21 a 04:02:45 UTC, con la compilación, la clonación del TCK y `uv sync`; después, a las 04:03, el cliente de Python, los esquemas y la firma de la card |
| Commit de `pals` | `fd0de8b`, con el árbol limpio: el código final del hito, con la corrección `db4d1ee` y la prueba de salida registrada. Desde `db4d1ee` solo cambian el arnés de la prueba de salida, el ejemplo `dht_rogue` y la documentación |
| TCK | `a2aproject/a2a-tck` en `104a7b4ae0d83f65c075e3793e322953a830eaba`, el mismo commit que en los hitos 1 y 2. Python 3.15.2 y las dependencias de su `uv.lock` (`uv sync --frozen`, uv 0.13.8) |
| Transportes | `jsonrpc` y `http_json`. El TCK no prueba gRPC, que PALS no ofrece |
| `TCK_STREAMING_TIMEOUT` | `2.0` (el valor por defecto). Una sola ejecución de cada nivel, sin repetir ninguna prueba |
| SDK de Rust | `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9` y `a2a-pb =0.3.2`, los mismos que en el hito 2, con `prost =0.14.6` |
| Resto | `iroh =1.4.1`, `iroh-blobs =0.105.0`, `rmcp =3.10.0`, Rust 1.99.1. Nuevas en el hito 3: `libp2p =0.57.1` (`libp2p-kad` 0.49.0), y `sha2 =0.11.0`, que pasa de dependencia de pruebas a dependencia normal |
| Cliente independiente | `a2a-sdk==1.3.0` (SDK oficial de Python, protocolo 1.0), Python 3.12.3, dependencias fijadas en [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt), sin cambios desde el hito 2 |
| Validador de esquemas | `jsonschema==4.26.0` (Draft 2020-12), fijado en el mismo fichero |
| Verificador de la card | `a2a-sdk==1.3.0` con `PyJWT==2.15.1`, fijados en el mismo fichero |
| Anfitrión | Linux x86_64, 32 CPU, compartido con otros proyectos. Carga media al empezar: 11,33 / 12,99 / 10,92; al terminar el TCK: 8,63 / 11,91 / 10,72 |

**Montaje.** Es el de los hitos 1 y 2: dos daemons en la misma máquina, en directorios temporales, con Iroh solo en 127.0.0.1 y sin relay. Uno es el proveedor, con el adaptador `tck`; el otro, el solicitante, con su gateway. Intercambian contactos y un grant (`task.send,task.read,task.cancel` sobre `skill:ask`) con la CLI `pals`. El TCK habla con el gateway del solicitante, en `http://127.0.0.1:<puerto del gateway>/peers/<id del proveedor>`, y cada petición viaja por Iroh con los bindings propios [`http-over-iroh/v1`](../../bindings/http-over-iroh-v1.md).

La configuración que escribe `run_tck.sh` no tiene `[dht]` ni `[net] listen`, así que los dos daemons llevan la DHT apagada, que es el valor por defecto, y ligan su endpoint como en el hito 2. El proveedor es un contacto guardado del solicitante, y su gateway lo marca en el ticket de ese contacto, como en el hito 2. Su inventario está vacío, como en el hito 2.

**Reproducir** (desde la raíz del repo, con Rust, Python 3 y acceso a GitHub y PyPI):

```bash
PALS_CONFORMANCE_DIR=docs/conformance/hito-3 tests/interop/run_tck.sh
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/python_client.py
target/interop-venv/bin/python tests/interop/check_schemas.py
target/interop-venv/bin/python tests/interop/card_signatures.py --vector crates/pals-core/tests/vectors/card-jws-v1.json
```

`run_tck.sh` copió los informes a este directorio: [`compatibility.json`](compatibility.json) y [`compatibility.html`](compatibility.html) (MUST), y [`compatibility-should.json`](compatibility-should.json) y [`compatibility-should.html`](compatibility-should.html) (SHOULD). Los de los hitos 1 y 2 no se tocaron: `git status` no muestra ningún cambio en `docs/conformance/hito-1` ni en `docs/conformance/hito-2`. La ejecución salió con el código `0` de la ejecución MUST; la SHOULD salió con `1`, por `HTTP_JSON-SVC-001`, como en los hitos 1 y 2.

Los cuatro informes copiados no contienen rutas del anfitrión, comprobado con `grep`: solo la URL del gateway, en loopback, y el id del proveedor temporal. El `tck_report.html` de pytest-html, que sí las lleva, se queda en `target/tck-run/<hora UTC>/reports-<nivel>/`.

Salida del TCK, nivel MUST:

```text
│ MUST   │ 72     │ 0      │ 20      │ 7      │ 16         │ 115   │
  agent_card:    6/6 ✓
  grpc:          0/65 (65 skipped) ✓
  jsonrpc:       75/90 (15 skipped) ✓
  http_json:     71/85 (14 skipped) ✓
134 passed, 95 skipped, 34 deselected in 67.31s (0:01:07)
[run_tck] TCK --level must exited 0
```

Esas columnas son superados, fallidos, saltados, `MANUAL` y sin prueba. Las líneas por transporte cuentan pruebas, no requisitos, y son las del hito 2.

## Resultado

Requisitos MUST por transporte, según `per_requirement` de [`compatibility.json`](compatibility.json), con el hito 2 entre paréntesis:

| Estado | `jsonrpc` | `http_json` |
|-|-|-|
| Superado | 58 (58) | 57 (57) |
| **Fallido** | **0** (0) | **0** (0) |
| Saltado por una capacidad que PALS no declara, o cuya precondición no se da | 14 (14) | 12 (12) |

La card (`CARD-DISC-001`, `CARD-STRUCT-001`, `CARD-PROTO-001`, `CARD-PROTO-002` y `BIND-FIELD-001`) pasa entera. En total, de los 115 requisitos MUST hay 72 superados, 0 fallidos, 20 saltados, 7 `MANUAL` y 16 sin prueba automática en el TCK, como en el hito 2.

**Saltados, todos por el propio TCK y los mismos 20 que en el hito 2:**

- **Notificaciones *push*:** `PUSH-CREATE-001`, `PUSH-CREATE-002`, `PUSH-GET-001`, `PUSH-GET-002`, `PUSH-LIST-001`, `PUSH-DEL-001`, `PUSH-DEL-002` y `PUSH-DELIVER-001` a `003`.
- **Card extendida:** `CARD-EXT-002`, porque la card extendida está configurada, y `CORE-CAP-003`, porque la card la declara (ver el [hito 2](../hito-2/README.md#diferencias-con-el-hito-1-requisito-a-requisito)).
- **Extensión obligatoria:** `CORE-CAP-004`. La card no declara ninguna como obligatoria.
- **Streaming no soportado:** `CORE-CAP-002`. PALS sí lo soporta, así que la prueba no aplica.
- **gRPC:** `GRPC-SVC-001`, `GRPC-SVC-002`, `GRPC-META-001` y `GRPC-ERR-001` a `003`.

**Sin prueba automática en el TCK** (`NOT TESTED`), los mismos 16: `AUTH-SERVER-002`, `AUTH-INTASK-001` a `004`, `AUTH-SCOPE-001` a `003`, `VER-CLIENT-001`, `VER-CLIENT-002`, `VER-SERVER-001`, `BIND-EQUIV-001` a `004` y `GRPC-SVC-003`. No cuentan como superados.

**Nivel SHOULD** ([`compatibility-should.json`](compatibility-should.json)), igual que en el hito 2: 8 superados, 1 fallido, 1 saltado, 2 `MANUAL` y 2 sin prueba automática. El fallido sigue siendo `HTTP_JSON-SVC-001`, que no se puede cumplir a la vez que la MUST `HTTP_JSON-ERR-001` (ver el [hito 1](../hito-1/README.md#resultado)), y el saltado, `BIND-CUSTOM-001`.

## Diferencias con el hito 2, requisito a requisito

**Ninguna.** Se compararon los `compatibility.json` y los `compatibility-should.json` de los dos hitos, requisito a requisito: estado, nivel, estado por transporte, errores y pruebas del TCK que lo cubren. En los dos niveles están los mismos 133 requisitos, y ninguno cambia en nada. `per_transport` es idéntico. Fuera de `per_requirement` solo cambian la URL del gateway (`sut_url`: su puerto y el id del proveedor temporal) y la hora.

La card que ve el TCK (la vista del gateway, clave `agent_card`) es la del hito 2, byte a byte una vez cambiados el puerto y los ids en las URL de sus interfaces. El hito 3 no cambia nada de lo que la produce: ni `crates/pals/src/a2a/` ni `crates/pals-core/src/card_jws.rs` ni el vector `card-jws-v1` cambian respecto a `dev`.

Lo que el hito 3 cambia en el camino de A2A, y por qué el TCK no lo ve:

| Cambio | Por qué no lo ve el TCK | Qué lo prueba |
|-|-|-|
| `[dht] enabled = true` sirve el ALPN `agentnet-kad/0` en el mismo endpoint de Iroh y mueve un swarm de `libp2p-kad` sobre él | La DHT está apagada sin `[dht]`, y `run_tck.sh` no la enciende. El servidor A2A y su ALPN, `agentnet-a2a-http/1`, no cambian | Caso 2 de la [prueba de salida](exit-test.md#caso-2-encontrar-un-proveedor-sin-contacto-previo-superado): `pals discover --capability vision.ocr --ask "hola"`, con la DHT encendida y un proveedor encontrado en ella, recibe `echo: hola` por los bindings estándar de A2A sobre `http-over-iroh/v1` |
| `[net] listen` liga el endpoint a una dirección fija | `run_tck.sh` no lo pone: el endpoint se liga como en el hito 2 | Los nueve casos de la prueba de salida, con `listen = "0.0.0.0:4433"` |
| El gateway marca a un agente sin contacto guardado, como un proveedor encontrado en la DHT, en el contacto firmado más nuevo y en vigor que conoce de él | El proveedor del TCK es un contacto guardado, y se marca en su ticket, como en el hito 2 | Caso 2 de la prueba de salida y `crates/pals/tests/dht_search.rs` |
| Ruta nueva `GET /dht` en el gateway, y el ticket de `GET /healthz` de un router lleva sus `[dht] external_addrs` | Están fuera de `/peers/`, que es lo que prueba el TCK | `crates/pals/tests/dht.rs` y la prueba de salida |

### Requisitos `MANUAL` del TCK

Son los mismos siete MUST y dos SHOULD que en el hito 2, con el mismo estado (ver su [tabla](../hito-2/README.md#requisitos-manual-del-tck)). La firma de la card (`CARD-SIGN-001` a `004`) no cambia: el vector sigue verificando con el SDK oficial de Python (más abajo).

`AUTH-TLS-001` se aceptó para los hitos 1 y 2 ([registro de decisión](../../decisions/2026-11-16-cierre-hitos-1-y-2.md#d1-auth-tls-001-se-acepta)). El hito 3 no cambia nada de aquello en lo que se basa: `agentnet-kad/0` va por el mismo endpoint de Iroh, con QUIC y TLS 1.3, y `GET /dht` está en el gateway, que solo escucha en loopback. Extender la aceptación al hito 3 lo decide el responsable; ningún registro lo hace todavía.

## Cliente independiente con el SDK oficial de Python

`tests/interop/python_client.py` y `requirements.txt` no han cambiado desde el hito 2. Su salida, igual en las dos ejecuciones que se hicieron:

```text
card resolved from the gateway: 'pals-interop-echo', interfaces JSONRPC, HTTP+JSON
check 1 passed: SendMessage answers "echo: hola" (JSONRPC, HTTP+JSON)
check 2 passed: SendStreamingMessage ends TASK_STATE_COMPLETED (JSONRPC, HTTP+JSON)
check 3 passed: GetTask returns the completed task (JSONRPC, HTTP+JSON)
check 4 passed: CancelTask leaves a long task TASK_STATE_CANCELED (JSONRPC, HTTP+JSON)
4 checks passed
```

Salió con el código `0`. Sus daemons, como los del TCK, llevan la DHT apagada.

## Esquemas de la extensión de inventario

`tests/interop/check_schemas.py` comprueba, con un validador independiente (`jsonschema`, Draft 2020-12), que cada esquema de [`docs/extensions/inventory-v1/`](../../extensions/inventory-v1/) es válido y que el documento de cada vector congelado del hito 2 es válido contra el suyo. Su salida:

```text
7 documents valid
```

El hito 3 no añade ningún documento con esquema. Su vector, [`kad-v0.json`](../../../crates/pals-core/tests/vectors/kad-v0.json), congela la identidad, las claves y las formas de dirección de la DHT, no un documento JSON firmado. Lo comprueban las pruebas de Rust: `crates/pals/tests/dht_keys.rs`, `dht_validate.rs` y `dht.rs`.

## Firma de la card

`tests/interop/card_signatures.py` verifica el vector congelado [`card-jws-v1.json`](../../../crates/pals-core/tests/vectors/card-jws-v1.json) con el verificador del propio `a2a-sdk` (ver [`card-signatures.md`](../hito-2/card-signatures.md) del hito 2). Su salida:

```text
check 1 passed: the card verifies with kid 8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c
check 2 passed: a changed skills[0].description is InvalidSignaturesError
check 3 passed: with 8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394 expected instead, InvalidSignaturesError
3 checks passed
```

Salió con el código `0`. Las verificaciones con los SDK de Go y JS no se repitieron: ni el vector ni el código que firma la card cambian en este hito, y la del hito 2 sigue valiendo.

## Lo que queda respaldado

> **A2A v1.0 con los bindings JSON-RPC y HTTP+JSON a través del gateway local; entre pares, el binding propio `http-over-iroh/v1` (`…/v1/jsonrpc` y `…/v1/http-json`), que transporta esos mismos bindings. Sin regresión respecto al hito 2: ningún requisito cambia. La card que el agente sirve a sus pares va firmada con JWS (A2A §8.4).**

En concreto: todos los requisitos MUST que el TCK en `104a7b4ae0d8` comprueba de forma automática para esos dos bindings se superan, como en el hito 2. Quedan fuera los de capacidades que la card no declara (notificaciones *push*), los dos cuya precondición no se da con la card extendida (`CORE-CAP-003` y `CARD-EXT-002`) y los de gRPC, que PALS no ofrece. El SDK oficial de Python opera con PALS por los dos bindings.

No queda respaldado:

- los 16 requisitos MUST sin prueba automática en el TCK;
- los `MANUAL`: `AUTH-TLS-001` está aceptado para los hitos 1 y 2, y lo que pasa con él en el hito 3 queda dicho más arriba;
- el SHOULD `HTTP_JSON-SVC-001`;
- el TCK con la DHT encendida, que no se ejecutó. El intercambio A2A después de encontrar a un proveedor en la DHT lo ejercita el caso 2 de la [prueba de salida](exit-test.md);
- lo que el hito 3 añade antes del intercambio A2A, que no es A2A y que el TCK no prueba: la DHT [`agentnet-kad/0`](../../protocols/agentnet-kad-0.md) y su uso como fuente de la búsqueda de servicios. Lo respaldan las pruebas (`crates/pals/tests/dht*.rs`) y la [prueba de salida](exit-test.md);
- lo que ya quedaba fuera en el hito 2: `agentnet-discovery/0`, las vistas del inventario, las consultas y ofertas y los contactos A2A corrientes en saliente ([hito 2](../hito-2/README.md#lo-que-queda-respaldado)).
