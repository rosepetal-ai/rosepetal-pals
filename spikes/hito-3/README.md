# Hito 3: comparación del backend de descubrimiento

Este directorio es la fase 1 del [plan del hito 3](../../docs/plans/2026-11-16-hito-3-descubrimiento-kademlia.md): una comparación acotada de los backends con los que PALS podría encontrar proveedores de una capacidad sin contacto previo. Cada candidato es un binario de prueba pequeño, y todos cumplen el mismo contrato de nodo. El método, qué descarta a un candidato y qué pesa en contra están en la [hoja de ruta](../../docs/plans/2026-11-14-hoja-de-ruta-hitos-2-4.md#hito-3-descubrimiento-con-kademlia).

## Qué no es

No es código del producto. El daemon no usa nada de aquí, y esta fase no toca `crates/`. Es un workspace de Cargo aparte, con su propio `Cargo.lock`, excluido del workspace raíz (`exclude = ["spikes"]`). La CI no lo compila, y sus comprobaciones no cuentan para las de la raíz. Un formato que solo necesite un candidato se llama `agentnet.spike-<nombre>/0` y es provisional.

## Paquetes

| Paquete | Directorio | Qué es |
|-|-|-|
| `spike-common` | `common/` | Biblioteca común: el contrato de nodo (tarea 2). Depende de `pals-core` por ruta, sin cambios: los registros que viajan son sus `agentnet.contact/1` y `agentnet.announce/1` |
| `spike-baseline` | `baseline/` | Línea base: solo contactos, la situación del hito 2 (tarea 2) |
| `spike-kad-libp2p` | `kad-libp2p/` | Iroh más `libp2p-kad` en su propia pila libp2p (tarea 3) |
| `spike-kad-iroh` | `kad-iroh/` | `libp2p-kad` sobre un transporte de libp2p hecho con conexiones Iroh (tarea 4) |
| `spike-mainline` | `mainline/` | Iroh más una DHT Mainline privada, con el contacto resuelto por pkarr (tarea 5) |

`spike-common` y `spike-baseline` están escritos (tarea 2), y `spike-kad-libp2p` también (tarea 3). Los otros dos binarios todavía solo nombran un elemento de cada dependencia para enlazarlas; las tareas 4 y 5 escriben sus nodos sobre el contrato de [más abajo](#el-contrato-de-nodo).

## Dependencias

Todas van fijadas con `=` en [`Cargo.toml`](Cargo.toml):

| Crate | Versión | Quién la usa |
|-|-|-|
| `iroh` | `=1.4.1`, la del daemon | Todos los binarios |
| `libp2p` | `=0.57.1`, que trae `libp2p-kad 0.49.0` | `kad-libp2p`, con `kad`, `quic`, `tokio`, `macros`, `identify` y `ed25519`; `kad-iroh`, sin transportes propios: `kad`, `tokio` y `macros` |
| `mainline` | `=8.0.1` | `mainline` |
| `pkarr` | `=9.0.0`, con sus features por defecto (`full-client`: `dht` y `relays`) | `mainline` |
| `iroh-mainline-address-lookup` | `=0.6.1` | `mainline` |

El contrato de nodo usa además, con las versiones que ya fija el `Cargo.lock` de la raíz: `axum =0.8.9` y `tokio =1.54.1` (la API de control, como la pasarela del daemon), `clap =4.6.7` (la línea de órdenes), `serde =1.0.229`, `serde_json =1.0.154`, `anyhow =1.0.104`, `base64 =0.23.1` (la firma que altera `--rogue`), `tracing =0.1.44` y `tracing-subscriber =0.3.23` (el registro de cada nodo). `kad-libp2p` usa también `sha2 =0.11.0`, para la clave de los provider records y la clave libp2p que deriva de la semilla del agente.

El `Cargo.lock` de aquí partió de una copia del de la raíz. Así, los crates que el daemon ya usa conservan sus versiones, y solo se resolvieron los nuevos.

`libp2p =0.57.1` e `iroh =1.4.1` se resuelven juntos y compilan en el mismo binario.

### Búsqueda de direcciones por pkarr sobre Mainline en Iroh 1.x

El plan pide añadir, si existe, un crate de Iroh 1.x que haga la búsqueda de direcciones por pkarr sobre Mainline y sea compatible con `iroh =1.4.1`. Lo que había en crates.io el 2026-11-16:

- **`iroh-mainline-address-lookup 0.6.1`**, de n0 ([n0-computer/iroh-address-lookups](https://github.com/n0-computer/iroh-address-lookups)). Es la búsqueda de direcciones por pkarr sobre Mainline de Iroh 1.x (`DhtAddressLookup`, que implementa `iroh::address_lookup::AddressLookup`). Tiene MSRV 1.91 y depende de `iroh ^1.0.0`. **Se añade**, fijado a `=0.6.1`. Ojo: no usa `mainline 8.0.1` ni `pkarr 9.0.0`. Su DHT es `n0-mainline 0.7.2`, el fork de n0 de `mainline`, y sus paquetes pkarr son los de `iroh-dns 1.4.1`. Si la tarea 5 lo usa junto a `mainline` y `pkarr`, el binario lleva dos implementaciones de Mainline.
- `iroh-mainline-endpoint-discovery 0.1.6`, también de n0. Sigue otro camino: Mainline lleva a un servidor de índice de direcciones (`udp-addr-index-proto`), que da la identidad firmada del endpoint. No depende de `iroh`, solo de `iroh-base`, así que no implementa su `AddressLookup`. No se añade.
- `iroh-pkarr-node-discovery 0.6.0` depende de `iroh-net 0.22`, de antes de Iroh 1.x. No se añade.

## Compatibilidad de dependencias: `compat.sh`

```bash
bash spikes/hito-3/compat.sh [--repeat N] [--provisional "<motivo>"]    # desde cualquier directorio
```

Los scripts de este directorio necesitan Linux con las coreutils de GNU. `compat.sh` necesita además la toolchain de la MSRV (`rustup toolchain install 1.91`) y `python3`. Tarda varios minutos, porque compila cada binario en release desde cero. Para cada candidato mide:

- si `cargo +1.91 check --locked -p <paquete>` pasa. Si falla, guarda la primera línea de error y la que nombra el crate culpable: la primera que dice `requires rustc` o, si no hay ninguna, la siguiente línea no vacía;
- cuántos crates trae: las líneas distintas de `cargo tree -p <paquete> -e normal --prefix none`, quitada la marca ` (*)` de los repetidos para que cada nombre y versión cuente una vez;
- qué crates trae en más de una versión, según `cargo tree -p <paquete> -d -e normal`. Una misma versión compilada dos veces, para el anfitrión y para el destino, no cuenta como duplicado;
- cuánto tarda `cargo build --release --locked -p <paquete>` desde cero, con su propio `CARGO_TARGET_DIR` en `target/compat/build/<id>/`;
- cuánto ocupa el binario, y cuánto más que el de `baseline`.

Las opciones:

- `--repeat N` compila cada candidato `N` veces desde cero (1 por defecto). En cada vuelta el orden empieza un candidato más tarde, para que ninguno vaya siempre primero. `release_build_s` es el mínimo, y `release_build_samples_s` guarda todas las muestras.
- `--provisional "<motivo>"` es para cuando los binarios todavía no son los reales. El JSON lleva el motivo en `provisional`, y `binary_bytes` y `binary_bytes_over_baseline` salen como `null`. El resto de las medidas sí vale. Sin la opción, el JSON lleva los tamaños reales y no tiene `provisional`.

Escribe [`docs/decisions/hito-3-backend/compat.json`](../../docs/decisions/hito-3-backend/compat.json) con este esquema:

```json
{"date": "...", "rustc": "...", "rustc_msrv": "...", "commit": "...", "host_cpus": n, "provisional": "...",
 "candidates": {"<id>": {"msrv_1_91": "pass|fail", "msrv_error": "...", "crates": n,
   "duplicates": ["name vA, vB", ...], "release_build_s": n, "release_build_samples_s": [n, ...],
   "release_build_load_1m": [[inicio, fin], ...], "binary_bytes": n, "binary_bytes_over_baseline": n}}}
```

Notas sobre los campos:

- `msrv_error` es `null` cuando la comprobación pasa. Las rutas del anfitrión que pueda llevar salen como `.` (el repositorio), `$CARGO_HOME`, `$RUSTUP_HOME` o `~`.
- `commit` es el `HEAD` del repositorio al medir. Lleva `-dirty` si lo medido (`Cargo.toml`, `Cargo.lock`, `crates/` o `spikes/hito-3/`) tenía cambios sin commit.
- `rustc_msrv`, `host_cpus`, `provisional`, `release_build_samples_s` y `release_build_load_1m` no están en el esquema del plan. Los tiempos de compilación dependen de la carga del anfitrión, que suele estar muy cargado. Por eso cada muestra de `release_build_samples_s` va con la carga media de un minuto al empezar y al acabar, en el mismo orden en `release_build_load_1m`, y con el número de CPU. Los tiempos solo se comparan dentro de una misma ejecución.
- Mientras los binarios solo enlazan sus dependencias, el enlazador quita el código que nadie usa, y el tamaño no mide nada. Por eso la medida de la tarea 1 lleva `--provisional`. La medida que vale es la de la tarea 6, que ejecuta `compat.sh --repeat 3` sobre un árbol limpio, con los nodos ya escritos.

`compat.sh` deja sus compilaciones en `target/compat/`, que git ignora.

## El contrato de nodo

Cada candidato es un binario cuyo `main` es `spike_common::run_node::<B>()` sobre su propio `B: spike_common::Backend` ([`common/src/lib.rs`](common/src/lib.rs)). El backend solo pone la DHT: arrancar, anunciar, retirar el anuncio, buscar y dar sus contadores. Lo demás es común, para que lo único que cambie entre candidatos sea la DHT:

- **Línea de órdenes:** `--name <n> --role router|provider|client --listen <ip:puerto> [--bootstrap <s>]… [--announce <capacidad>]… --ttl-s <n> --republish-s <n> --key-seed <64 hex> [--rogue]`. La clave del agente sale de `--key-seed`. `--announce` y `--rogue` son solo para proveedores.
- **Registros:** `run_node` emite los `agentnet.contact/1` y `agentnet.announce/1` de `pals-core`, sin cambios, con su vida de 24 h; la DHT aplica la suya, `--ttl-s`. El contacto lista las direcciones del nodo en ese momento: con `--listen 0.0.0.0:<puerto>`, todas sus IPv4 menos la de bucle, leídas de `/proc/net/fib_trie`. El `seq` sale del reloj, para que nunca retroceda aunque el arnés reinicie el nodo. No hay Agent Card, y el anuncio lleva un `agent_card_hash` fijo y bien formado.
- **Verificación:** todo lo que devuelve `search` es ajeno. `verify` comprueba cada candidato (los dos registros pasan `Signed::from_json` y su firma verifica, son del mismo agente, el anuncio lista la capacidad buscada y los dos están en plazo), con el primer motivo que falle: `bad_contact`, `bad_announce`, `agent_mismatch`, `capability_missing` o `expired`. Después, `dedupe` deja un candidato por agente: de sus copias válidas, la de mayor `seq` del contacto (la primera, si empatan), o la primera si ninguna es válida (spec §5.3, paso 4). Los válidos van delante antes de recortar a `limit`, para que los inválidos nunca dejen fuera a uno válido.
- **`--rogue`:** el contacto que se publica lleva la firma con su primer byte alterado (`tamper_signature`, la misma función que usa la prueba `verify_rejects_a_tampered_contact`).
- **API de control,** en `127.0.0.1:7600` dentro del contenedor, con `axum`:
  - `POST /search {"capability", "limit"}` devuelve `{"elapsed_ms", "first_valid_ms", "first_valid_source", "candidates": [{"agent_id", "valid", "reason", "direct_addrs", "contact_seq"}], "requests"}`. El backend da en cada `RawCandidate` su `found_ms`, cuándo tuvo sus dos registros desde que empezó la búsqueda, si lo sabe. `first_valid_ms` es el menor `found_ms` de los candidatos válidos, y entonces `first_valid_source` es `"backend"`; si ningún válido lo trae, es `elapsed_ms`, cuando terminó la búsqueda, y la fuente es `"completion"`. Sin candidatos válidos, los dos son `null`;
  - `POST /announce {"capabilities": [..]}` vuelve a emitir los registros, con las direcciones de ese momento y el `seq` siguiente, y llama a `announce`;
  - `POST /unannounce`;
  - `GET /stats` da los campos de `BackendStats` más `rss_kib`, de `/proc/self/status`;
  - `GET /healthz` da `{"name", "role", "agent_id", "announced", "announce_error", "declared"}`, y solo contesta cuando el backend ha arrancado y se ha intentado el anuncio de `--announce`. `announced` es lo que listó el último anuncio que salió bien, vacío tras `/unannounce`; `announce_error`, por qué falló el último, `null` cuando uno sale bien o tras `/unannounce`.

  `run_node` llama a los métodos del backend de uno en uno, desde una sola tarea. Por eso sus futuros no necesitan ser `Send`, y el backend mueve su red en tareas propias.

Tres cosas van más allá de lo que fija el plan, porque sin ellas el arnés no puede comprobar un escenario:

- `direct_addrs` en cada candidato: las direcciones del contacto verificado. S7 ve con ellas que un cliente resolvió el contacto nuevo.
- `stored_by_agent` en `BackendStats`: cuántos contactos, anuncios, otros registros (como un vínculo de identidades) y provider records guarda el nodo de cada agente. S3 comprueba con él que los routers dejan de guardar lo del proveedor parado, y S5 que ninguno guarda el contacto inválido. Un backend cuyos routers guardan registros tiene que llenarlo, o S5 queda en `error`.
- `Backend::DECLARED`, que sale en `declared` de `/healthz`: los escenarios que el backend no puede ejecutar tal cual, con su estado (`by-design` o `not-applicable`) y su motivo. Así el arnés no sabe qué candidato maneja. Un candidato real no declara ninguno; la línea base declara S1 `by-design` y S2 a S5 y S7 `not-applicable`.

## Arnés

```bash
bash spikes/hito-3/harness/harness.sh <id>               # p. ej. baseline; desde cualquier directorio
bash spikes/hito-3/harness/harness.sh --teardown <id>    # lo que dejó una ejecución interrumpida
```

Necesita Linux, Docker, `python3`, `cargo` y `git`, y tarda unos 10 minutos con la línea base: el reposo de S6 son 5. Los parámetros, comunes a todos los candidatos, están en [`harness/params.env`](harness/params.env): los plazos y límites del plan, la topología con la IP fija de cada nodo, las capacidades y cómo espera y sondea el arnés.

Qué hace `harness.sh <id>`:

1. Compila `spike-<id>` (`cargo build --release --locked`) y la imagen `pals-spike:latest` ([`harness/Dockerfile`](harness/Dockerfile)): Debian trixie-slim fijado por digest, con `curl` e `iproute2`. El binario se copia en cada contenedor antes de arrancarlo.
2. Comprueba, solo leyendo, que las subredes de `params.env` (`NET_A_SUBNET` y `NET_B_SUBNET`, fuera de los rangos que Docker reparte por defecto) no se solapan con ninguna red de Docker del anfitrión, y se niega a seguir si lo hacen. Crea `pals-spike-net-a` y `pals-spike-net-b` con `--internal` y esas subredes, y comprueba que `docker network inspect -f '{{.Internal}}'` dice `true`.
3. Arranca la topología del plan, un contenedor por nodo con `--cpus 1 --memory 512m` y sus IP fijas (`IP_<nodo>`, `--ip` en cada red): `r1`; luego `r2` a `r4`, con `--bootstrap` el `bootstrap_addr` de `r1`; los proveedores, con sus capacidades, y los clientes, que tienen además el de `r3`. Espera a que cada uno conteste `/healthz` y comprueba que está en sus IP. Así, el `bootstrap_addr` de `r1` sigue valiendo cuando `r1` vuelve a arrancar, y Docker no puede dar su IP a otro mientras está parado.
4. Espera a que la topología se asiente (ver abajo) y ejecuta S0 a S7 en orden, con `docker exec <contenedor> curl` sobre la API de control. Los bytes salen de `/sys/class/net/*/statistics` de cada contenedor, todas las interfaces menos `lo`.
5. Escribe `docs/decisions/hito-3-backend/results/<id>.json` y lo pasa por `check_results.py`, cuyo código de salida es el de la ejecución.

Los registros quedan en `harness/logs/<id>/`, que git ignora: `harness.log`, un `<nodo>.log` por contenedor y `raw/`, con cada observación en una línea JSON. Todo el JSON lo lee y lo escribe Python: [`harness/scenarios.py`](harness/scenarios.py) hace las observaciones y [`harness/evaluate.py`](harness/evaluate.py), sin tocar Docker, juzga cada escenario y dice cuándo ha ocurrido lo que espera cada bucle. [`harness/test_evaluate.py`](harness/test_evaluate.py) lo prueba con observaciones sintéticas.

Lo que el arnés necesita del candidato sale de su binario: cómo nombra un nodo a otro en `--bootstrap` (`bootstrap_addr` de `/stats`), el `agent_id` de cada nodo y los escenarios declarados (`/healthz`). Así, las tareas 3 a 5 lo ejecutan sin cambiarlo.

**Seguridad en un anfitrión compartido,** como en `tests/exit/hito-2.sh`. Cada contenedor y red se llama `pals-spike-*` y lleva la etiqueta `pals-spike.dir=<directorio de registros de la ejecución>`. Una ejecución se niega a empezar si existe cualquier contenedor o red `pals-spike-*`. Al salir, y también con Ctrl-C, desmonta solo lo que tiene su prefijo y su etiqueta; la imagen se queda. No toca la red del anfitrión (ni `iptables`, `nft`, `tc` o `sysctl`, ni `prune`), y en los contenedores solo entra el binario del candidato.

### Cómo se ejecuta cada escenario

Entre escenarios, la topología vuelve a su estado. Los nodos que para un escenario se arrancan de nuevo, con el mismo contenedor y clave, y el arnés comprueba que siguen en sus IP fijas; si no, la topología queda rota y los escenarios que faltan, en `error`. Los que solo arrancan para su escenario (`c0`, `c5` y `rogue`) se paran. Después, el arnés espera a que la topología se asiente: lo que guardan los routers no cambia en `SETTLE_STABLE_POLLS` sondeos seguidos y, salvo que el backend declare S1 `by-design`, todos los proveedores tienen algo guardado en algún router; como mucho, `SETTLE_MAX_S`. Si no se asienta a tiempo, queda un evento en `raw/settle.jsonl` y una nota en el escenario siguiente. Las búsquedas van de una en una, y el cliente no hace nada más mientras tanto.

| Id | Cómo | Superado si |
|-|-|-|
| S0 | Arranca `c0` en la red A sin lista de arranque. Espera `S0_GRACE_S`, por si un descubrimiento local (mDNS, difusión) llegara a los proveedores de la red A, y busca cada capacidad | No hay ningún candidato. Un error del backend cuenta como búsqueda sin candidatos |
| S1 | Cada cliente busca cada capacidad, `SEARCHES` rondas | Cada búsqueda devuelve los dos proveedores, verificados. Declarado `by-design`, no debe haber ningún candidato válido, y si lo hay el escenario queda en `error` |
| S2 | Para `r1` (`docker kill`). Cada cliente busca cada capacidad una vez. Arranca `c5` con `[r1, r3]` y busca hasta encontrar a los dos proveedores de cada capacidad, `2 × REPUBLISH_S` como mucho | Las búsquedas de los clientes están completas y `c5` encuentra a todos. El tiempo de `c5` cuenta desde que arranca su contenedor |
| S3 | Mide lo que los routers guardan de `p1` y lo para (`docker kill`). Cada `POLL_S`, las estadísticas de los routers y una búsqueda por cliente de una capacidad de `p1`, hasta que el último sondeo de los routers está a 0 y la última ronda está limpia (todas sus búsquedas contestan 200 y ninguna lo devuelve), `TTL_S + 2 × REPUBLISH_S` como mucho | Antes de pararlo los routers guardaban algo de `p1`, y en menos de `TTL_S + REPUBLISH_S` dejan de guardarlo y ninguna búsqueda lo devuelve, ni como inválido. Los routers cuentan en el primer sondeo a 0 después del último que guardaba algo; las búsquedas, en la primera contestada con 200 después de la última que lo devolvió, en toda la observación |
| S4 | Para `r2` y `r4`. Rondas seguidas en que cada cliente busca cada capacidad, hasta una ronda completa, `3 × REPUBLISH_S` como mucho | La cobertura (proveedores esperados encontrados) vuelve a 1 en menos de `2 × REPUBLISH_S`. Cuenta la primera búsqueda completa, del mismo cliente y capacidad, después de la última incompleta, y es 0 si ninguna lo fue |
| S5 | Arranca `rogue` en la red B con `--rogue`, que anuncia `vision.ocr`. Sondea los routers hasta que alguno cuente un rechazo, `2 × REPUBLISH_S` como mucho; después, cada cliente busca `vision.ocr` | Algún router sube `records_rejected`, ninguno atribuye a `rogue` un contacto (`stored_by_agent`), `records_stored` no crece en ningún router más de lo que explica `stored_by_agent` (los registros de los demás agentes y los de `rogue` que no son un contacto: lo que sobra cuenta como su contacto) y ningún cliente lo devuelve como válido. Si las estadísticas de los routers no atribuyen registros a los cuatro proveedores honrados, no prueban nada sobre `rogue` y S5 queda en `error` |
| S6 | `IDLE_S` sin búsquedas, con las estadísticas de los 12 nodos muestreadas cada `IDLE_SAMPLE_S` y los contadores al principio y al final | Siempre `measured`. Se dan los bytes recibidos más enviados y, aparte, los enviados, porque un contenedor que no envía nada recibe igualmente el tráfico de fondo del puente de Docker |
| S7 | Saca `p3` de la red B y lo mete en la A con la IP `S7_IP` (`docker network disconnect` y `connect --ip`), comprueba que está ahí, y le pide `POST /announce`. Cada cliente busca una capacidad de `p3` por ronda, hasta `3 × REPUBLISH_S` | Todos los clientes reciben un contacto válido de `p3` con su IP nueva en menos de `2 × REPUBLISH_S`, contando desde la desconexión |

Un escenario queda en `error`, y la ejecución sale con 1, cuando el arnés no obtuvo una respuesta que sirva: una búsqueda sin respuesta de la API o con un código que no sea 200 ni 500 (400, 422, 503…), un nodo que no arranca o no está en sus IP, o una topología que no se pudo restablecer. Sus cifras no valen. Un 500 es un error del propio backend: cuenta como búsqueda sin candidatos. Los bytes por búsqueda de S1 son los de la interfaz del cliente durante la búsqueda, menos su tráfico en reposo de S6 en el mismo tiempo. Los percentiles son de rango más cercano.

### Resultados

`results/<id>.json` = `{"backend", "commit", "date", "params": {…params.env…}, "scenarios": {"S0".."S7": {"status", "metrics": {…}, "notes"}}}`, con `status` uno de `pass`, `fail`, `by-design`, `not-applicable`, `error` o `measured`. `commit` lleva `-dirty` si `crates/`, `spikes/hito-3/` o los `Cargo` de la raíz tenían cambios sin commit.

```bash
python3 spikes/hito-3/harness/check_results.py docs/decisions/hito-3-backend/results/*.json
python3 spikes/hito-3/harness/summarize.py --compat docs/decisions/hito-3-backend/compat.json docs/decisions/hito-3-backend/results/*.json
```

- `check_results.py` valida el esquema, rechaza ficheros con `params` distintos entre sí o con el mismo `backend`, y rutas del anfitrión. Sale con 1 si algún escenario está en `error`.
- `summarize.py` imprime la tabla Markdown del registro de decisión: una columna por candidato y una fila por métrica, con el `status` del escenario en cada celda. Las filas de compatibilidad salen de `compat.json`, que los resultados no copian. Mientras `compat.json` es provisional, las celdas de tamaño dicen `provisional`.

### Línea base

`baseline` solo conoce los contactos de `--bootstrap`. Su búsqueda pregunta a cada uno, por HTTP en su puerto de escucha (`POST /lookup`), si anuncia la capacidad, y un contacto que la anuncia contesta con sus dos registros. Ningún cliente tiene a un proveedor como contacto, así que sin contacto previo no encuentra a nadie. Como el daemon del hito 2, tiene un endpoint de Iroh con la clave del agente, en UDP en el mismo puerto, sin relay ni búsqueda de direcciones. No lo usa nadie, pero así su coste en reposo es la referencia a la que cada candidato suma el suyo. Su `bootstrap_addr` son todas sus direcciones, separadas por comas.

Resultado esperado: S0 `pass`, S1 `by-design` sin candidatos, S2 a S5 y S7 `not-applicable` con su motivo, y S6 `measured`.

### `kad-libp2p`

Iroh, como en la línea base, más `libp2p-kad` sobre QUIC de libp2p en el puerto siguiente, con un vínculo de identidades `agentnet.spike-kadlink/0` que lleva el contacto. Su `bootstrap_addr` son las multiaddrs de libp2p del router, terminadas en `/p2p/<PeerId>` y separadas por comas. Cómo está hecho, qué valida cada router y sus resultados están en [`docs/decisions/hito-3-backend/kad-libp2p.md`](../../docs/decisions/hito-3-backend/kad-libp2p.md).

## Comprobaciones

Dentro de `spikes/hito-3/`:

```bash
cargo build --release --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -p spike-common -p spike-baseline -p spike-kad-libp2p
(cd harness && python3 -m unittest -v)    # evaluate.py, con observaciones sintéticas
```

Las comprobaciones de la raíz (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace --locked`) no ven este directorio.
