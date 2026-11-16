# PALS hito 3: descubrimiento con Kademlia — Implementation Plan (fase 1: comparación del backend; fase 2: Kademlia sobre Iroh en el daemon)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Un plan, dos fases.** La fase 1, las tareas 1 a 6, compara los backends y está hecha: su cabecera, sus Global Constraints y su Review Focus son los que siguen. La [fase 2](#fase-2-kademlia-sobre-iroh-en-el-daemon), las tareas 7 a 15, lleva al daemon el backend elegido, y tiene su propia cabecera, sus Global Constraints y su Review Focus. Los de una fase no rigen la otra.

**Goal:** Elegir, con medidas y no con suposiciones, el backend con el que PALS encuentra proveedores de una capacidad sin contacto previo (spec §5, §6), y dejarlo justificado en un registro de decisión que cumpla la primera parte del criterio de salida del hito 3: «justificar la implementación elegida».

**Architecture:** Esta fase no toca el daemon. Cada candidato se construye como un binario de prueba pequeño, en un workspace aparte (`spikes/hito-3/`), que cumple un mismo contrato de nodo: anunciar capacidades, buscarlas y dar estadísticas por una API de control local. Los registros que viajan son los de `pals-core` sin cambios: `Signed<ContactRecord>` y `Signed<AnnounceRecord>`, del hito 2. Un único arnés Docker monta la misma topología para todos, con dos redes internas, routers de dos operadores, proveedores y clientes. Ejecuta los mismos escenarios con los mismos parámetros y escribe un JSON de resultados por candidato. El registro de decisión toma sus números de esos JSON. El resto del plan del hito 3 (tareas 2 a 9 de la hoja de ruta) se escribe después, sobre el backend elegido.

**Tech Stack:** Rust 2024 con MSRV 1.91, `pals-core` del workspace como dependencia por ruta y Docker. Dependencias de prueba, todas con `=`, comprobadas al escribir este plan (2026-11-16) con `cargo info`:
- `libp2p =0.57.1` y `libp2p-kad =0.49.0` (MSRV 1.88);
- `mainline =8.0.1` (MSRV 1.85);
- `pkarr =9.0.0` (MSRV 1.85);
- `iroh =1.4.1`, la del daemon.

**Spec:** [`docs/plans/initial.md`](initial.md) (revisión 3): §5 (Kademlia como directorio), §6 (implementación de la DHT), §13 y §14 (hito 3, escenarios y métricas). La [hoja de ruta](2026-11-14-hoja-de-ruta-hitos-2-4.md#hito-3-descubrimiento-con-kademlia) fija el método de la comparación, qué descarta a un candidato y qué pesa en contra. Las decisiones H3-D1 a H3-D5 están delegadas desde el 2026-11-16 ([registro](../decisions/2026-11-16-cierre-hitos-1-y-2.md#decisiones-del-hito-3)).

## Candidatos

La hoja de ruta compara tres alternativas (§6.1). Este plan mide dos de ellas y una variante de la primera. La tercera se evalúa sobre el papel.

| Id | Candidato | Qué se construye | Duda concreta que resuelve |
|-|-|-|-|
| `kad-libp2p` | Iroh más `libp2p-kad` en su propia pila libp2p, el candidato inicial del spec | Swarm libp2p con QUIC, Kademlia `/agentnet/kad/1.0.0` y un vínculo de identidades firmado por las dos claves (§6.2) | Lo que cuestan dos pilas en memoria, conexiones y tráfico en reposo, y si el vínculo de identidades se valida en los routers |
| `kad-iroh` | `libp2p-kad` sobre un `Transport` de libp2p hecho con conexiones Iroh: una sola pila de red y un solo `PeerId`, derivado de la clave Ed25519 del agente | Un adaptador `Transport` y `StreamMuxer` sobre `iroh::Endpoint` con un ALPN propio, más el mismo Kademlia | Si el coste de las dos pilas se evita sin reimplementar Kademlia (§6.3) y sin vínculo de identidades aparte (§6.2) |
| `mainline` | Iroh más una DHT Mainline privada (BEP 5 y BEP 44), con el contacto resuelto por pkarr | Índice de capacidades sobre Mainline y resolución del contacto con la clave del agente | Si Mainline puede dar un índice de capacidades con identidades verificables y caducidad, o qué haría falta definir (§6.1) |
| `libp2p-total` | Toda la red en libp2p | Nada: evaluación sobre el papel en la tarea 6, con las medidas de Kademlia de `kad-libp2p` | Lo que costaría replantear el transporte de tareas y de ficheros de los hitos 1 y 2 |

Hay además una línea base, `baseline`: solo contactos, la situación del hito 2. No encuentra a nadie sin contacto previo, y así debe aparecer. Sirve para validar el arnés y como referencia de los costes en reposo.

## Global Constraints

- **El daemon no cambia en esta fase.** No se toca `crates/`. Lo único fuera de `spikes/` y `docs/` es `exclude = ["spikes"]` en el `[workspace]` del `Cargo.toml` raíz. La CI no compila `spikes/`.
- **Versiones fijadas.** Cada dependencia de `spikes/hito-3/Cargo.toml` va con `=`, y el workspace de pruebas tiene su propio `Cargo.lock` versionado. Nunca `latest` (§14). Cada binario se comprueba también con `cargo +1.91 check`. Si no compila con la MSRV, se registra como dato de la comparación, no se arregla subiendo la MSRV.
- **No se reimplementa Kademlia** (§6.3). Los candidatos usan una implementación existente.
- **Redes privadas.** Ninguna prueba depende de una DHT pública ni de los relays de n0 (§6.2). Las dos redes Docker se crean con `--internal`. Ningún candidato lleva nodos de arranque por defecto: ni los de IPFS ni `router.bittorrent.com`. La búsqueda de direcciones y los relays de Iroh quedan desactivados.
- **Formatos congelados.** Los registros `agentnet.contact/1` y `agentnet.announce/1` se usan tal cual, con `pals_core::records` y `pals_core::signed`. Las capacidades son cadenas `agentnet:capability:v1:<id>`. Un formato nuevo que solo necesite un candidato, como el vínculo de identidades de `kad-libp2p`, se llama `agentnet.spike-<nombre>/0` y queda documentado como provisional.
- **Protocolo Kademlia:** `/agentnet/kad/1.0.0` (§6.2). La clave de un provider record en libp2p es el multihash SHA2-256 (código `0x12`) de los bytes UTF-8 de la cadena completa de la capacidad (§5.1).
- **Docker en un anfitrión compartido.** Todo recurso se llama `pals-spike-*` y lleva la etiqueta `pals-spike.dir=<directorio de trabajo>`. El arnés solo borra lo que tiene su prefijo y su etiqueta, como `tests/exit/hito-2.sh`. Nunca se ejecuta `docker system prune` ni `docker network prune`, ni `iptables`, `nft`, `tc` o `sysctl` en el anfitrión. Dentro de un contenedor sí se puede, con `NET_ADMIN`. Nunca se copia ni se monta una credencial (`~/.claude`, claves de API) en un contenedor.
- **Repositorio público.** Antes de cada commit se revisa el diff en busca de rutas del anfitrión (`/root`, `/workspace`, `/tmp/claude`), nombres de usuario, correos y tokens. Los JSON de resultados guardan rutas relativas.
- **Idioma.** Los documentos van en español y los commits en inglés, convencionales, terminados en `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Lo que no se mide se dice.** Un escenario que un candidato no puede ejecutar queda como `not-applicable` y no como superado, con su motivo.

## Review Focus

1. **Resultados no comparables.** Un candidato medido con otra topología, otros plazos u otros límites invalida la tabla. Los parámetros van fijos en `spikes/hito-3/harness/params.env`, cada JSON los copia, y `check_results.py` rechaza un JSON cuyos parámetros difieran de los del resto (tarea 2).
2. **Fugas a redes públicas.** Una prueba que «funciona» porque llegó a una DHT pública no prueba nada. El arnés comprueba que cada red es `Internal=true`. Además, en cada candidato, un nodo con la lista de arranque vacía no encuentra a nadie (escenario S0 en la tarea 2 y en cada candidato).
3. **Validación en el router y no solo en el cliente.** Si un registro inválido se guarda en los routers y solo lo filtra quien busca, la DHT almacena basura ajena. S5 exige que los routers lo cuenten en `records_rejected` y que su `records_stored` no lo incluya (tareas 3 a 5).
4. **Caducidad real.** Emular la caducidad filtrando en el cliente no prueba que los anuncios caduquen. S3 exige que el contador de los routers baje a 0 para el proveedor parado dentro del plazo, con el mecanismo de caducidad de la propia biblioteca (tareas 3 a 5).
5. **Números de una ejecución fallida.** Si un escenario falla por un error del arnés y no por diseño, su cifra no vale. Cada escenario lleva `status`, el arnés sale con código distinto de 0 si alguno queda en `error`, y la tabla del registro muestra el estado junto a cada cifra (tareas 2 y 6).

## Topología y escenarios comunes

Todo lo que sigue es igual para todos los candidatos y lo fija `params.env`.

**Topología.** Dos redes, `pals-spike-net-a` y `pals-spike-net-b`, las dos `--internal`, con 12 contenedores, uno por nodo, cada uno con `--cpus 1` y `--memory 512m`:
- 4 routers, en las dos redes, en modo servidor de la DHT: `r1` y `r2` del operador 1, y `r3` y `r4` del operador 2;
- 4 proveedores, en modo cliente: `p1` y `p2` en la red A, `p3` y `p4` en la B;
- 4 clientes, también en modo cliente: `c1` y `c2` en la A, `c3` y `c4` en la B.

Todos arrancan con `r1` como único punto de arranque, el arranque original. Los clientes tienen además `r3` como contacto guardado. Tres nodos más se arrancan solo para su escenario: `c0` (S0, en la red A), `c5` (S2, en la A) y `rogue` (S5, en la B).

**Capacidades.** Cuatro, del ejemplo del §5.1:
- `vision.ocr`, anunciada por `p1` y `p3`;
- `code.rust.review`, por `p2` y `p4`;
- `knowledge.documentation`, por `p1` y `p4`;
- `vision.defect-detection`, por `p2` y `p3`.

Todas llevan el prefijo `agentnet:capability:v1:`. Cada capacidad tiene así un proveedor en cada red.

**Plazos de prueba.** `TTL_S=120` es la vida de anuncios y contactos en la DHT. Los registros firmados de `pals-core` conservan su caducidad de 24 h, y la DHT aplica la suya. `REPUBLISH_S=40`, `IDLE_S=300`, `SEARCHES=10` por cliente y capacidad, y `SEARCH_LIMIT=8` candidatos, el de la hoja de ruta.

| Id | Escenario | Qué mide | Superado si |
|-|-|-|-|
| S0 | Nodo aislado: un cliente nuevo con la lista de arranque vacía | Que no hay fuga a redes públicas | No encuentra ningún candidato |
| S1 | Búsqueda sin contacto previo: cada cliente busca cada capacidad `SEARCHES` veces | Tiempo hasta el primer candidato válido (p50 y p95), porcentaje de búsquedas con candidato, peticiones y bytes por búsqueda y candidatos devueltos | Cada búsqueda devuelve los dos proveedores de la capacidad, con contacto y anuncio verificados. La línea base queda como `by-design` |
| S2 | Corte del arranque original: se para `r1` | Si los clientes conectados siguen resolviendo, y el tiempo hasta el primer candidato de un cliente nuevo, `c5`, que arranca con `[r1, r3]` | Los clientes siguen encontrando a los dos proveedores, y `c5` entra por `r3` |
| S3 | Caducidad: se para `p1` sin aviso | Tiempo hasta que los routers dejan de guardar sus registros, y hasta que ninguna búsqueda lo devuelve | Las dos cosas ocurren en menos de `TTL_S + REPUBLISH_S` segundos |
| S4 | Caída de routers: se paran `r2` y `r4` | Cobertura inmediata (fracción de los proveedores esperados que se encuentran) y tiempo hasta recuperar la cobertura completa | Cobertura completa en menos de `2 × REPUBLISH_S` segundos |
| S5 | Registro inválido: un nodo `rogue` anuncia `vision.ocr` con un contacto de firma inválida | Rechazo en los routers y en los clientes | `records_rejected > 0` en algún router, ningún router lo guarda y ningún cliente lo devuelve como válido |
| S6 | Reposo: `IDLE_S` segundos sin búsquedas | Memoria (RSS), conexiones abiertas, entradas en la tabla de rutas y bytes por minuto en la interfaz, por papel (p50 y máximo) | Se mide siempre: `measured` |
| S7 | Cambio de red: `p3` sale de la red B y entra en la A, con otra IP | Tiempo hasta que los clientes resuelven su contacto nuevo | Lo resuelven en menos de `2 × REPUBLISH_S` segundos |

Las peticiones por búsqueda salen del candidato cuando la biblioteca las cuenta (por ejemplo, `QueryStats` en `libp2p-kad`). Los bytes por búsqueda son la diferencia de contadores de la interfaz del cliente durante una búsqueda aislada, menos su tráfico en reposo medido en S6 para la misma duración.

---

### Task 1: Workspace de pruebas y compatibilidad de dependencias

**Files:**
- Modify: `Cargo.toml`: `exclude = ["spikes"]` en `[workspace]`
- Create: `spikes/hito-3/Cargo.toml` (workspace con `resolver = "3"`, `edition = "2024"`, `rust-version = "1.91"`; miembros `common`, `baseline`, `kad-libp2p`, `kad-iroh` y `mainline`) y `spikes/hito-3/Cargo.lock`
- Create: `spikes/hito-3/{common,baseline,kad-libp2p,kad-iroh,mainline}/Cargo.toml` y un `src/main.rs` (o `src/lib.rs` en `common`) mínimo que solo enlaza sus dependencias
- Create: `spikes/hito-3/compat.sh`
- Create: `spikes/hito-3/README.md`
- Create: `docs/decisions/hito-3-backend/compat.json`

**Interfaces:**
- Produces: el workspace de pruebas y los paquetes en los que escriben las tareas 2 a 5:
  - `spike-common` (biblioteca);
  - `spike-baseline`, `spike-kad-libp2p`, `spike-kad-iroh` y `spike-mainline` (binarios);
  - `compat.json`, con este esquema: `{"date": "...", "rustc": "...", "commit": "...", "candidates": {"<id>": {"msrv_1_91": "pass|fail", "msrv_error": "...", "crates": n, "duplicates": ["name vA, vB", ...], "release_build_s": n, "binary_bytes": n, "binary_bytes_over_baseline": n}}}`.
- Dependencias: `spike-common` depende de `pals-core = { path = "../../../crates/pals-core" }`. Cada binario depende de `spike-common` y `iroh =1.4.1`, como el daemon. Además:
  - `kad-libp2p`: `libp2p =0.57.1`, con las features `kad`, `quic`, `tokio`, `macros` e `identify`;
  - `kad-iroh`: `libp2p =0.57.1`, sin transportes propios (`kad`, `tokio`, `macros`);
  - `mainline`: `mainline =8.0.1` y `pkarr =9.0.0`. Si existe un crate de Iroh 1.x que hace la búsqueda de direcciones por pkarr sobre Mainline y es compatible con `iroh =1.4.1`, se añade también, fijado, y se anota.

- [ ] **Step 1:** Crear el workspace y los cinco paquetes, con sus dependencias fijadas. Ejecutar `cargo build --release --locked` en `spikes/hito-3/`. Esperado: compila. Desde la raíz, `cargo test --workspace --locked` sigue igual: los 620 tests pasan y `spikes/` no entra.
- [ ] **Step 2:** Escribir `compat.sh`. Por cada binario mide:
  - `cargo +1.91 check -p <paquete>`, y guarda la primera línea de error si falla;
  - el número de crates de `cargo tree -p <paquete> -e normal --prefix none | sort -u`;
  - los duplicados de `cargo tree -p <paquete> -d -e normal`;
  - el tiempo de `cargo build --release -p <paquete>` desde limpio, con `CARGO_TARGET_DIR` propio;
  - el tamaño del binario, y su diferencia con el de `spike-baseline`.

  Escribe `docs/decisions/hito-3-backend/compat.json` con el esquema de arriba.
- [ ] **Step 3:** Ejecutar `bash spikes/hito-3/compat.sh`. Esperado: `compat.json` con los cuatro candidatos, y `python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert set(d["candidates"])=={"baseline","kad-libp2p","kad-iroh","mainline"}' docs/decisions/hito-3-backend/compat.json` sale con 0.
- [ ] **Step 4:** Escribir `spikes/hito-3/README.md`: qué es, que no es código del producto, cómo ejecutar `compat.sh` y el arnés, y el enlace a la hoja de ruta.
- [ ] **Step 5:** Commit: `chore(spikes): hito 3 backend comparison workspace and dependency check`.

### Task 2: Contrato de nodo, arnés Docker y línea base

**Files:**
- Create: `spikes/hito-3/common/src/{lib.rs,args.rs,control.rs,records.rs,stats.rs}`
- Create: `spikes/hito-3/baseline/src/main.rs`
- Create: `spikes/hito-3/harness/{Dockerfile,harness.sh,params.env,check_results.py,summarize.py}`
- Create: `docs/decisions/hito-3-backend/results/baseline.json`
- Test: `spikes/hito-3/common/tests/contract.rs`

**Interfaces:**
- Produces: lo que implementan las tareas 3 a 5.

```rust
// spike_common
pub enum Role { Router, Provider, Client }
pub struct NodeConfig {
    pub name: String, pub role: Role,
    pub listen: std::net::SocketAddr,
    pub bootstrap: Vec<String>,       // forma propia de cada backend, la que da su `bootstrap_addr`
    pub ttl: std::time::Duration, pub republish: std::time::Duration,
}
pub struct RawCandidate {
    pub contact: Option<Vec<u8>>,     // JSON de un Signed<ContactRecord>
    pub announce: Option<Vec<u8>>,    // JSON de un Signed<AnnounceRecord>
    pub found_ms: Option<u64>,        // ms desde el inicio de la búsqueda hasta que el backend obtuvo sus registros
}
pub struct RawSearch { pub candidates: Vec<RawCandidate>, pub requests: Option<u64> }
pub struct Candidate {
    pub agent_id: Option<pals_core::identity::AgentId>,
    pub valid: bool,
    pub reason: Option<&'static str>,  // None si es válido
}
pub fn verify(capability: &str, raw: RawCandidate, now_unix: i64) -> Candidate;
pub fn dedupe(candidates: Vec<Candidate>) -> Vec<Candidate>;   // por agent_id, conservando la copia válida de mayor `seq` del contacto
pub struct BackendStats {
    pub connections: u64, pub routing_peers: u64,
    pub records_stored: u64, pub provider_records_stored: u64, pub records_rejected: u64,
    pub bootstrap_addr: Option<String>,
}
#[allow(async_fn_in_trait)]
pub trait Backend: Sized + Send + 'static {
    async fn start(cfg: &NodeConfig, key: &pals_core::identity::AgentKey) -> anyhow::Result<Self>;
    /// Publica en la DHT el anuncio de cada capacidad y el contacto, y los renueva cada `cfg.republish`.
    async fn announce(&mut self, capabilities: &[String],
        contact: &pals_core::signed::Signed<pals_core::records::ContactRecord>,
        announce: &pals_core::signed::Signed<pals_core::records::AnnounceRecord>) -> anyhow::Result<()>;
    async fn unannounce(&mut self) -> anyhow::Result<()>;
    async fn search(&mut self, capability: &str, limit: usize) -> anyhow::Result<RawSearch>;
    fn stats(&self) -> BackendStats;
}
pub async fn run_node<B: Backend>() -> anyhow::Result<()>;   // el `main` de cada binario
```

- **`run_node`** lee la CLI, genera la clave y emite los registros. Después sirve la API de control y verifica lo que devuelve `search`.
- **CLI:** `--name <n> --role router|provider|client --listen <ip:puerto> [--bootstrap <s>]… [--announce <capacidad>]… --ttl-s <n> --republish-s <n> --key-seed <64 hex>`.
- **Verificación de cada candidato:**
  - el contacto y el anuncio pasan `Signed::from_json` y su firma verifica;
  - los dos son del mismo `agent_id`;
  - el anuncio lista la capacidad buscada;
  - los dos están en plazo.

  Si falla, el motivo es `bad_contact`, `bad_announce`, `agent_mismatch`, `capability_missing` o `expired`. Los candidatos se deduplican por `agent_id` (§5.3, paso 4).
- **API de control,** en `127.0.0.1:7600` dentro del contenedor, con `axum` y `tokio`, que ya usa el daemon:
  - `POST /search {"capability": s, "limit": n}` devuelve `{"elapsed_ms", "first_valid_ms", "candidates": [{"agent_id", "valid", "reason"}], "requests"}`;
  - `POST /announce {"capabilities": [..]}`, que emite los registros con las direcciones actuales y llama a `announce`;
  - `POST /unannounce`;
  - `GET /stats`, con los campos de `BackendStats` más `rss_kib`, leído de `/proc/self/status`;
  - `GET /healthz`.
- **`rogue`:** con `--role provider --rogue`, `run_node` emite un contacto con la firma alterada en un byte, para S5.
- **Resultados:** `results/<id>.json` = `{"backend", "commit", "date", "params": {…params.env…}, "scenarios": {"S0".."S7": {"status": "pass|fail|by-design|not-applicable|error|measured", "metrics": {…}, "notes": "…"}}}`.

- [ ] **Step 1: Write the failing test** `contract.rs`:
  - `verify_accepts_a_matching_pair`: un contacto y un anuncio de la misma clave, en plazo y con la capacidad dan `valid` sin motivo;
  - `verify_rejects_a_tampered_contact`: el contacto que emite `--rogue` da `bad_contact`;
  - `verify_rejects_mixed_agents`: un contacto de A con un anuncio de B da `agent_mismatch`;
  - `verify_rejects_a_missing_capability`: `capability_missing`;
  - `candidates_dedupe_by_agent`: dos `RawCandidate` del mismo agente dan un solo candidato.
- [ ] **Step 2:** `cargo test -p spike-common` en `spikes/hito-3/`. Esperado: FAIL, porque no existen las funciones.
- [ ] **Step 3:** Implementar `spike_common`: `run_node`, `verify`, `dedupe`, la API de control y `stats`.
- [ ] **Step 4:** `cargo test -p spike-common`. Esperado: PASS.
- [ ] **Step 5:** Implementar `spike-baseline`. Su `Backend` solo conoce los contactos que le pasa `--bootstrap`: `search` pregunta a esos nodos, por HTTP en su puerto de escucha, si anuncian la capacidad. Así, sin contacto previo, no encuentra a nadie.
- [ ] **Step 6:** Escribir el arnés, siguiendo las protecciones de `tests/exit/hito-2.sh`: prefijo y etiqueta, negarse a seguir si hay recursos `pals-spike-*` de otra ejecución, y desmontar con `trap` en `EXIT` e `INT`.
  - `harness.sh <id>` construye la imagen `pals-spike:latest` (Debian trixie-slim fijado por digest, como `tests/exit/Dockerfile`, con `curl` y `iproute2`). Copia el binario `--release` del candidato y crea las dos redes con `--internal`, comprobando `docker network inspect -f '{{.Internal}}'`. Arranca los nodos de la topología y ejecuta S0 a S7 en orden, con `docker exec <c> curl -s 127.0.0.1:7600/...`. Lee los bytes de la interfaz en `/sys/class/net/*/statistics`, dentro de cada contenedor. Escribe `docs/decisions/hito-3-backend/results/<id>.json` y los registros de cada nodo en `spikes/hito-3/harness/logs/<id>/`, que van en `.gitignore`.
  - `check_results.py <json>…` valida el esquema, rechaza parámetros distintos entre ficheros y sale con 1 si algún escenario está en `error`.
  - `summarize.py --compat <compat.json> <json>…` imprime la tabla Markdown que irá en el registro de decisión: una fila por métrica, una columna por candidato, y en cada celda la cifra con su `status`. Las filas de compatibilidad (MSRV, crates, duplicados, compilación, tamaño) salen de `compat.json`, que los resultados no copian, porque se vuelve a medir en la tarea 6.
- [ ] **Step 7:** Ejecutar `bash spikes/hito-3/harness/harness.sh baseline`. Esperado:
  - S0 `pass`;
  - S1 `by-design`, sin candidatos;
  - S2, S3, S4, S5 y S7 `not-applicable`, con su motivo;
  - S6 `measured`;
  - `check_results.py docs/decisions/hito-3-backend/results/baseline.json` sale con 0;
  - `docker ps -a --filter label=pals-spike.dir` queda vacío al terminar.
- [ ] **Step 8:** Commit: `feat(spikes): common node contract, Docker harness and contacts-only baseline`.

### Task 3: Candidato `kad-libp2p`: Iroh más `libp2p-kad` en su propia pila

**Files:**
- Create: `spikes/hito-3/kad-libp2p/src/{main.rs,behaviour.rs,link.rs,validate.rs}`
- Create: `docs/decisions/hito-3-backend/kad-libp2p.md`
- Create: `docs/decisions/hito-3-backend/results/kad-libp2p.json`
- Test: `spikes/hito-3/kad-libp2p/tests/link.rs`

**Interfaces:**
- Consumes: `spike_common::{Backend, NodeConfig, RawSearch, BackendStats, run_node}` (tarea 2).
- Produces: el registro provisional `agentnet.spike-kadlink/0`, el vínculo de identidades del §6.2:
  - `{"schema", "peer_id", "agent_id", "contact": <Signed<ContactRecord> en JSON>, "issued_at", "expires_at"}`;
  - firmado con la clave libp2p (dominio `agentnet.spike-kadlink/0\nlibp2p\n`) y con la del agente (dominio `agentnet.spike-kadlink/0\nagent\n`);
  - guardado como valor de Kademlia bajo la clave `/agentnet/link/<peer_id>`.

  El provider record de cada capacidad lo pone el `PeerId`, y el anuncio firmado va en otro valor, `/agentnet/announce/<peer_id>`.

- [ ] **Step 1: Write the failing test** `link.rs`:
  - `link_verifies_with_both_keys`;
  - `link_rejects_a_peer_id_not_matching_its_key`: validar la firma libp2p incluye comprobar que su clave corresponde al `PeerId` anunciado (§6.2);
  - `link_rejects_a_bad_agent_signature`;
  - `validator_rejects_a_record_under_the_wrong_key`: un vínculo de `P` guardado bajo `/agentnet/link/<Q>`.
- [ ] **Step 2:** `cargo test -p spike-kad-libp2p`. Esperado: FAIL.
- [ ] **Step 3:** Implementar el swarm y el `Backend`:
  - transporte QUIC de libp2p y protocolo `kad::Config::new(StreamProtocol::new("/agentnet/kad/1.0.0"))`;
  - modo `Server` en los routers y `Client` en el resto;
  - `MemoryStore` con los límites por nodo puestos y anotados;
  - `StoreInserts::FilterBoth`, de modo que cada `PutRecord` y `AddProvider` entrante pase por `validate.rs` antes de `store_mut().put`; los rechazados suman a `records_rejected`;
  - caducidad con `set_record_ttl` y `set_provider_record_ttl` a `TTL_S`, y republicación con `set_provider_publication_interval` y `set_publication_interval` a `REPUBLISH_S`;
  - `search` hace `get_providers`, después `get_record` del vínculo y del anuncio de cada `PeerId`, hasta `limit`, y suma `QueryStats::num_requests` en `requests`;
  - Iroh se arranca como en el daemon, con relays y búsqueda de direcciones desactivados, para medir las dos pilas juntas.
- [ ] **Step 4:** `cargo test -p spike-kad-libp2p`. Esperado: PASS.
- [ ] **Step 5:** `bash spikes/hito-3/harness/harness.sh kad-libp2p`. Esperado: S0, S1, S2, S3, S4, S5 y S7 en `pass` o en `fail` con su causa (un `fail` es un resultado de la comparación, no algo que corregir a toda costa), S6 en `measured`, ninguno en `error`, y `check_results.py` sobre `baseline.json` y `kad-libp2p.json` con salida 0.
- [ ] **Step 6:** Escribir `kad-libp2p.md`:
  - las API de `libp2p-kad` usadas, con sus versiones;
  - la duda que resuelve y su respuesta;
  - los límites por nodo puestos;
  - las líneas de código del candidato, con `tokei` o `wc -l` sobre `src/` y sin contar tests;
  - lo que no se pudo medir;
  - qué filtraciones de metadatos ve un router: claves buscadas, `PeerId` e IP de quien busca.
- [ ] **Step 7:** Commit: `feat(spikes): libp2p-kad on its own libp2p stack, measured`.

### Task 4: Candidato `kad-iroh`: `libp2p-kad` sobre conexiones Iroh

**Files:**
- Create: `spikes/hito-3/kad-iroh/src/{main.rs,transport.rs,muxer.rs}`
- Create: `docs/decisions/hito-3-backend/kad-iroh.md`
- Create: `docs/decisions/hito-3-backend/results/kad-iroh.json`
- Test: `spikes/hito-3/kad-iroh/tests/transport.rs`

**Interfaces:**
- Consumes: el contrato de la tarea 2, y de la tarea 3 su configuración de Kademlia (protocolo, store, validación de anuncio y contacto, plazos). El código común entre las dos se mueve a `spike-common` o se copia con una nota, lo que quede más corto.
- Produces:
  - `IrohTransport`: `impl libp2p::core::Transport<Output = (PeerId, StreamMuxerBox)>` sobre un `iroh::Endpoint`, con el ALPN `agentnet-kad/0`;
  - `IrohMuxer`: `impl StreamMuxer`, en el que cada substream es un stream bidireccional QUIC de la conexión Iroh;
  - el `PeerId`, que sale de la clave pública Ed25519 del agente (`identity::PublicKey::from(ed25519::PublicKey::try_from_bytes(agent_id_bytes))`), así que `PeerId` y `AgentId` son la misma clave y no hace falta vínculo aparte;
  - las direcciones, que son `/ip4/<ip>/udp/<puerto>/quic-v1/p2p/<peer_id>` (o `ip6`): el transporte las traduce a un `EndpointAddr` de Iroh con ese id y esa dirección directa.

- [ ] **Step 1:** Antes de escribir el adaptador, buscar en crates.io un `Transport` de libp2p sobre Iroh ya publicado y compatible con `iroh =1.4.1` y `libp2p =0.57.1`. Si hay uno mantenido, se usa, fijado, y `kad-iroh.md` lo dice. Si no, se escribe el adaptador.
- [ ] **Step 2: Write the failing test** `transport.rs`:
  - `two_nodes_open_a_substream_both_ways`: dos `IrohTransport` en loopback, un substream en cada sentido, bytes de ida y vuelta;
  - `peer_id_is_the_agent_key`: el `PeerId` remoto que da el transporte coincide con el `AgentId` del otro;
  - `dial_with_a_peer_id_not_matching_the_endpoint_fails`.
- [ ] **Step 3:** `cargo test -p spike-kad-iroh`. Esperado: FAIL.
- [ ] **Step 4:** Implementar `IrohTransport` e `IrohMuxer`, y el `Backend` con el mismo Kademlia de la tarea 3, sin el vínculo de identidades: el contacto firmado va en `/agentnet/contact/<peer_id>`.
- [ ] **Step 5:** `cargo test -p spike-kad-iroh`. Esperado: PASS.
- [ ] **Step 6:** `bash spikes/hito-3/harness/harness.sh kad-iroh`, con lo mismo que espera la tarea 3 en su paso 5.
- [ ] **Step 7:** **Límite de la prueba.** Si el adaptador no consigue que Kademlia funcione (por ejemplo, porque `libp2p-kad` exige algo que el transporte no puede dar), o pasa de unas 1000 líneas sin tests, se para. `kad-iroh.md` registra hasta dónde llegó, el obstáculo concreto y la estimación de lo que faltaría, y los escenarios quedan en `not-applicable` con ese motivo. Ese resultado es válido para la comparación.
- [ ] **Step 8:** Escribir `kad-iroh.md`, con los mismos apartados que `kad-libp2p.md`. Añadir:
  - lo que supone reutilizar la clave del agente como identidad libp2p: en este candidato libp2p no firma nada con ella, porque la autenticación es el TLS de Iroh, y hay que comprobarlo y decirlo;
  - el ALPN nuevo que el daemon tendría que servir.
- [ ] **Step 9:** Commit: `feat(spikes): libp2p-kad over an Iroh-backed libp2p transport, measured`.

### Task 5: Candidato `mainline`: Iroh más una DHT Mainline privada

**Files:**
- Create: `spikes/hito-3/mainline/src/{main.rs,index.rs}`
- Create: `docs/decisions/hito-3-backend/mainline.md`
- Create: `docs/decisions/hito-3-backend/results/mainline.json`
- Test: `spikes/hito-3/mainline/tests/index.rs`

**Interfaces:**
- Consumes: el contrato de la tarea 2.
- Produces: el diseño del índice de capacidades sobre Mainline, que la DHT no trae (§6.1). El de partida, que la tarea puede cambiar si la biblioteca ofrece algo mejor y lo justifica en `mainline.md`, tiene tres partes:
  - **Índice:** BEP 5 `announce_peer` en el *infohash* SHA-1 de la cadena de la capacidad, que devuelve direcciones `ip:puerto`. Si `mainline =8.0.1` trae un anuncio firmado de claves Ed25519 bajo un *infohash* (por ejemplo, *signed peers*), se prefiere, porque devuelve identidades y no direcciones.
  - **Anuncio firmado:** item mutable BEP 44 de la clave del agente, con *salt* `agentnet:announce:v1`, que guarda el `Signed<AnnounceRecord>`.
  - **Contacto:** con pkarr, que publica con la propia clave Ed25519 del agente, o con un item mutable BEP 44 con *salt* `agentnet:contact:v1`, que guarda el `Signed<ContactRecord>`.

  Lo que tenga que traducir direcciones a identidades se documenta como coste de definir el índice.

- [ ] **Step 1: Write the failing test** `index.rs`, sobre una red Mainline local de tres nodos con arranque explícito (por ejemplo, `mainline::Testnet` si existe en esta versión):
  - `announced_capability_is_found`;
  - `found_entries_lead_to_a_verified_contact`;
  - `a_node_without_bootstrap_finds_nothing`.
- [ ] **Step 2:** `cargo test -p spike-mainline`. Esperado: FAIL.
- [ ] **Step 3:** Implementar el `Backend`:
  - arranque solo por `--bootstrap`, nunca con los nodos por defecto, y modo servidor en los routers;
  - la caducidad de Mainline es la del protocolo (BEP 5 y BEP 44 no tienen TTL configurable), así que se documenta la que da la versión y se ajusta la republicación a `REPUBLISH_S`;
  - si no se puede caducar en `TTL_S`, S3 se mide con el plazo real y su `notes` lo dice;
  - validación: los items BEP 44 los firma la propia DHT, y la del `Signed<…>` interior la hace el cliente. Lo que los routers pueden o no rechazar va a `records_rejected` y a `mainline.md`.
- [ ] **Step 4:** `cargo test -p spike-mainline`. Esperado: PASS.
- [ ] **Step 5:** `bash spikes/hito-3/harness/harness.sh mainline`, con lo mismo que espera la tarea 3 en su paso 5.
- [ ] **Step 6:** Escribir `mainline.md`, con los mismos apartados que `kad-libp2p.md`. Añadir:
  - qué hubo que definir para tener un índice de capacidades;
  - qué controles de abuso da Mainline (tokens de BEP 5, límites de almacenamiento);
  - qué metadatos ve un nodo intermedio.
- [ ] **Step 7:** Commit: `feat(spikes): private Mainline DHT with a capability index, measured`.

### Task 6: Registro de decisión del backend

**Files:**
- Create: `docs/decisions/2026-MM-DD-hito-3-backend-dht.md`, con la fecha en que se escribe
- Modify: `docs/plans/2026-11-14-hoja-de-ruta-hitos-2-4.md`: la fila del hito 3 en «Estado» y un enlace al registro en «Comparación del backend»
- Modify: `README.md`: la línea de estado del hito 3 en «Roadmap»

**Interfaces:**
- Consumes: `compat.json`, `results/*.json` y `docs/decisions/hito-3-backend/*.md` (tareas 1 a 5).
- Produces: el backend elegido y lo que la fase 2 hereda de él:
  - la API de la DHT que usará el daemon;
  - las dependencias nuevas y sus versiones;
  - si hace falta vínculo de identidades (tarea 3 de la hoja de ruta);
  - el modo por defecto, router o cliente (H3-D3).

- [ ] **Step 1:** Volver a medir la compatibilidad con los binarios reales: commit de todo lo anterior, `bash spikes/hito-3/compat.sh --repeat 3` sobre ese árbol limpio (sin `provisional`, con `commit` igual a ese hash) y commit de `compat.json` solo. Después, `python3 spikes/hito-3/harness/check_results.py docs/decisions/hito-3-backend/results/*.json`. Esperado: `compat.json` sin `provisional` y con tamaños de binario reales, y `check_results.py` con salida 0, con los cuatro ficheros y los mismos parámetros.
- [ ] **Step 2:** Escribir el registro con los apartados que pide la hoja de ruta:
  - candidatos y versiones;
  - las pruebas y la duda de cada una;
  - el montaje: topología, parámetros y anfitrión;
  - las métricas, con la tabla que imprime `summarize.py`, pegada tal cual;
  - lo no medido;
  - la evaluación sobre el papel de `libp2p-total`: qué partes de `crates/pals` dependen de Iroh y de iroh-blobs (túnel A2A, blobs, `agentnet-discovery/0`, gateway), con sus líneas y lo que haría falta para cada una sobre libp2p;
  - la aplicación, una por una, de las reglas de la hoja de ruta (qué descarta a un candidato, qué pesa en contra, que reducir dependencias no basta para ganar);
  - la recomendación, justificada frente al criterio de salida;
  - qué la haría cambiar.

  El apartado «Decisión» queda con la recomendación y la marca `Pendiente de ratificar (H3-D1)`, que el controlador sustituye.
- [ ] **Step 3:** Actualizar la fila del hito 3 en la hoja de ruta («Comparación hecha; plan de la fase 2 pendiente») y el estado del hito 3 en el README.
- [ ] **Step 4:** Commit: `docs(decisions): hito 3 DHT backend comparison and recommendation`.

## Después de la fase 1

La fase 1 está hecha. El controlador ratificó la recomendación con condiciones (H3-D1: `kad-iroh`, [registro del backend](../decisions/2026-11-16-hito-3-backend-dht.md#decisión)) y decidió H3-D2 a H3-D5 ([registro](../decisions/2026-11-16-hito-3-decisiones.md)). La fase 2, a continuación, lleva las tareas 2 a 9 de la hoja de ruta al daemon, sobre ese backend, y empieza por las condiciones de la ratificación (tarea 7).

---

# Fase 2: Kademlia sobre Iroh en el daemon

**Goal:** Un daemon PALS encuentra proveedores de una capacidad sin contacto previo ni directorio central (§1, §5): publica en una DHT Kademlia privada, por sus propias conexiones Iroh, los registros firmados del hito 2 y un provider record por capacidad pública; los busca como una fuente más de la selección del hito 2; sigue funcionando sin el punto de arranque original, y los anuncios caducan. La tarea elegida se sigue enviando con A2A v1.0 estándar.

**Architecture:** `libp2p-kad` corre dentro del daemon sobre un transporte de libp2p hecho con las conexiones del endpoint de Iroh que ya sirve A2A, blobs y descubrimiento. El `Router` de `node.rs` sirve un ALPN más, `agentnet-kad/0`, y el `PeerId` es la clave del agente. Todo va en un módulo nuevo, `crates/pals/src/dht/`:
- `keys.rs`: el `PeerId` del agente y las claves de la DHT, con vectores;
- `transport.rs` y `muxer.rs`: el adaptador, con sus límites;
- `validate.rs`: lo que un router guarda;
- `swarm.rs`: el swarm y la tarea que lo mueve;
- `publish.rs`: las vueltas de publicación;
- `mod.rs`: `Dht`, la API que usan el nodo, la búsqueda y el gateway.

La DHT está apagada salvo que se configure (`[dht]`). Un nodo es cliente salvo que se configure router, y nunca cambia de modo solo. El `Searcher` del hito 2 la usa como fuente de candidatos detrás de los contactos (§5.3), y cada candidato sigue después el camino del hito 2: card verificada, `ServiceQuery` y selección en dos etapas. El código de [`spikes/hito-3/kad-iroh/`](../../spikes/hito-3/kad-iroh/) es el punto de partida, no código del producto: cada tarea dice qué conserva y qué cambia.

**Tech Stack:** Lo de los hitos 1 y 2, sin cambios de versión: Rust 2024 con MSRV 1.91, `iroh =1.4.1`, `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9`, `a2a-pb =0.3.2`, `rmcp =3.10.0` y `prost =0.14.6`. Una sola dependencia directa nueva, `libp2p =0.57.1`, sin transportes propios:
- features `kad`, `tokio`, `macros` y `ed25519`, e `identify` solo si la tarea 11 la conserva;
- trae `libp2p-kad 0.49.0`, `libp2p-swarm 0.48.0`, `libp2p-core 0.44.0`, `libp2p-identity 0.3.1` y `libp2p-connection-limits 0.7.0`, y `libp2p-identify 0.48.0` si se conserva;
- comprobado al escribir este plan (2026-11-16): `cargo info` da `libp2p` 0.57.1 y `libp2p-kad` 0.49.0 como últimas versiones, las dos con `rust-version` 1.88, las mismas del `Cargo.lock` de `spikes/hito-3/`, y `spike-kad-iroh` compila con la MSRV ([`compat.json`](../decisions/hito-3-backend/compat.json)).

`sha2 =0.11.0`, que ya está en el lock, pasa de dependencia de pruebas a dependencia normal de `pals`: hace la clave de los provider records. Pruebas: las de los hitos 1 y 2, Docker para la prueba de salida y el arnés de `spikes/hito-3/` para la tarea 7.

**Spec:** [`docs/plans/initial.md`](initial.md) (revisión 3): §3, §5, §6, §7.3, §8, §10, §13 y §14. La [hoja de ruta](2026-11-14-hoja-de-ruta-hitos-2-4.md#hito-3-descubrimiento-con-kademlia) fija las tareas 2 a 9 y «Cómo se cierra un hito». Decisiones vinculantes:
- H3-D1, en el [registro del backend](../decisions/2026-11-16-hito-3-backend-dht.md#decisión): ratificada con condiciones, con lo que dice «Lo que hereda la fase 2»;
- H3-D2 a H3-D5, en su [registro](../decisions/2026-11-16-hito-3-decisiones.md).

Este plan no las revisa. Lo que la revisión final de la fase 1 dejó para esta fase está en [Notas heredadas](#notas-heredadas-de-la-fase-1).

## Cambios respecto al spec y a la hoja de ruta

Se mantienen los de los hitos 1 y 2. Esta fase añade:

| Spec o hoja de ruta | Este plan | Motivo |
|-|-|-|
| §6.2: dos identidades de red (la clave del agente y de su endpoint Iroh, y el `PeerId` de libp2p) y un registro de contacto firmado por las dos | Una sola identidad: el `PeerId` es la clave pública Ed25519 del agente. No hay registro de vínculo | H3-D1. El TLS de Iroh autentica esa clave en cada conexión, y libp2p no firma nada |
| Hoja de ruta, tarea 3: vínculo de identidades «si el backend lo pide» | No se planifica | Con `kad-iroh` no hay nada que vincular (registro del backend, «Lo que hereda la fase 2») |
| §5.2, fila «Contacto»: «relación verificable entre identidad libp2p, agente y endpoint Iroh» | El `agentnet.contact/1` del hito 2, tal cual, bajo `/agentnet/contact/<peer_id>` | Agente, endpoint y `PeerId` son la misma clave |
| §6.1: «no se presupone un adaptador inmediato entre ambos transportes» | Un adaptador: `Transport` y `StreamMuxer` de libp2p sobre `iroh::Endpoint`, con el ALPN `agentnet-kad/0`, en su propio módulo y con sus pruebas de transporte como puerta | H3-D1, condición 3 |
| §6.2: «un identificador de protocolo Kademlia propio, por ejemplo `/agentnet/kad/1.0.0`» | Dos nombres: el ALPN de la conexión Iroh, `agentnet-kad/0`, y dentro de ella el protocolo de libp2p `/agentnet/kad/1.0.0` | El ALPN elige la conexión, y libp2p negocia cada stream |
| §6.3: invitaciones «con datos de conexión» | Una invitación es el ticket de Iroh de un router (`pals id --ticket`). Se pone en `[dht] bootstrap` o se guarda como contacto | El formato existe desde el hito 1 |
| Hoja de ruta, tarea 5: «pruebas con el reloj inyectable del hito 2» | La validez de los registros firmados sigue el `Clock` del nodo. La vida en la DHT es la de `libp2p-kad`, que usa `Instant`, y se prueba con plazos cortos, esperando al estado de la tienda | `libp2p-kad` no admite otro reloj |

**Cómo se reparten las tareas de la hoja de ruta:**

| Hoja de ruta | Fase 2 | Por qué |
|-|-|-|
| Condiciones de H3-D1 | 7 | La ratificación manda medir antes de congelar nada |
| 2. Vocabulario, su clave y alias locales | 8 y 13 | Los alias son el paso 1 de la búsqueda (§5.3) y se prueban con ella |
| 3. Vínculo de identidades | — | No aplica |
| 4. Red propia con validadores; modos | 9, 10 y 11 | Adaptador, validadores puros y nodo: un revisor puede aceptar uno y rechazar otro |
| 5. Anuncio, renovación y caducidad; límites por nodo | 12, con los límites en 9 y 10 | |
| 6. Entrada | 11 | Entrar en la red es arrancar el swarm, con las mismas pruebas de varios nodos |
| 7. Búsqueda | 13 | |
| 8. Escenarios y métricas | 14 | |
| 9. Cierre | 15 | |

## Valores que el spec no fija y que decide este plan

| Valor | Decisión |
|-|-|
| Vida en la DHT de valores y provider records | 3600 s por defecto (`[dht] record_ttl_s`, de 60 a 86 400). Los registros firmados conservan su vida de 24 h, y un router no sirve uno caducado por cualquiera de las dos |
| Republicación | 1200 s por defecto (`[dht] republish_s`, al menos 20 y como mucho la mitad de `record_ttl_s`): se pueden perder dos vueltas antes de que caduque nada. Son los «intervalos de producción» de la condición 1 de H3-D1 |
| Modo | Cliente por defecto. Router solo con `[dht] mode = "router"`. Siempre `set_mode(Some(..))`, nunca automático (H3-D3) |
| Tienda de un router | 1024 valores de 8 KiB como mucho, 20 proveedores por clave (`K`) y 1024 claves de proveedores: los de la prueba. Un contacto o un anuncio, 4 KiB como mucho (`MAX_RECORD_BYTES`) |
| Provider records de un agente en un router | Solo de capacidades que liste el anuncio de ese agente que el router guarda, y por tanto 32 como mucho (`MAX_ANNOUNCED_CAPABILITIES`) |
| Conexiones de `agentnet-kad/0` | Una cola de 32 conexiones entrantes que nunca espera: llena, la conexión se cierra. 2 conexiones establecidas por par y 256 entrantes en total |
| Streams de `agentnet-kad/0` | Por par, 8 a la vez y 120 por minuto del `Clock` del nodo; 256 a la vez en total. Un stream de más se resetea al aceptarlo, sin leerlo |
| Plazos | Marcado, 5 s. Consulta de Kademlia, 20 s. Conexión ociosa, 10 s (el de `libp2p-swarm`). Arranque periódico, 5 min (el de `libp2p-kad`). Una vuelta de publicación que no llegó a ningún router se repite a los 30 s |
| La DHT en una búsqueda | Como mucho `per_candidate_timeout` (3 s) y lo que quede del presupuesto. Sus candidatos cuentan en `max_candidates` (8), detrás de los contactos |
| Entrada | Los tickets de `[dht] bootstrap`. Si después de marcarlos la tabla de rutas sigue vacía, hasta 8 contactos guardados, por nombre |
| Direcciones que declara un nodo | Un router: `[dht] external_addrs` o, si no hay, las directas de su endpoint (`dialable_addr`). Un cliente: solo `/p2p/<peer_id>`, sin IP, así que sus provider records no llevan direcciones de su red |
| Puerto de un router | `[net] listen`, nuevo: el endpoint se liga a esa dirección. Sin él, todo sigue como en el hito 2 |

## Notas heredadas de la fase 1

La revisión final de la fase 1 dejó doce notas sobre el código de `kad-iroh`. Cada una va a una tarea, con la prueba que la fija, o a [Fuera de alcance](#fuera-de-alcance), con su motivo:

| Nota | Dónde va |
|-|-|
| 1. La forma de dirección no llega a un relay | Fuera de alcance. La forma sin IP, `/p2p/<id>`, sí se usa: la llevan los provider records de los clientes (tarea 12) |
| 2. Una llamada cada vez: el estado de la prueba no admite búsquedas a la vez | Tarea 12, `concurrent_lookups_and_a_round_all_finish` |
| 3. Tienda agotable, sin cuota por agente, y provider records de capacidades sin anuncio | Tarea 10 (regla del anuncio y cuota por agente), `provider_without_its_announce_is_refused`; tarea 12 (el anuncio viaja con cada provider record), `a_restarted_router_gets_the_announce_before_the_providers` y `a_full_router_store_is_counted`. El acaparamiento por 20 identidades vivas, a Fuera de alcance |
| 4. Sin límites de conexiones en `agentnet-kad/0` | Tarea 9, `a_full_accept_queue_refuses_instead_of_waiting` y `a_peer_beyond_its_stream_quota_is_reset`; tarea 11, `connections_beyond_the_per_peer_limit_are_refused` |
| 5. Marcaciones dobles | Tarea 9, `a_dial_carries_every_known_address`; tarea 11, `a_router_with_two_addresses_is_dialed_once` |
| 6. Direcciones locales declaradas externas: un provider record tras NAT lleva las de la LAN | Tarea 12, `a_client_providers_record_carries_no_ip_address` |
| 7. Reconexión en cada operación por la conexión ociosa de 10 s | Tarea 11: se queda en 10 s, `idle_connections_close_after_the_idle_timeout`. La tarea 14 mide su coste en el tiempo hasta el primer candidato |
| 8. Los `put` rechazados se contestan como guardados | Fuera de alcance |
| 9. Los dos caminos raros del anuncio | Tarea 12, `a_capability_that_does_not_fit_fails_alone` y `a_round_that_reaches_no_router_is_retried` |
| 10. La instrumentación de la prueba | Tarea 11: sin barrido ni estadísticas cada segundo, y `status` al pedirlo. Tarea 12, `records_expire_at_the_routers` y `a_lookup_ends_at_its_deadline` |
| 11. Si hace falta `identify` | Tarea 11: se prueba sin ella, `a_router_with_no_bootstrap_learns_the_others` y `clients_never_enter_a_routing_table` |
| 12. Los routers ven qué agente busca | Fuera de alcance (H3-D4). Se documenta en la plantilla de configuración (tarea 11) y en el README (tarea 15) |

## Fuera de alcance

- **Una forma de dirección con relay (nota 1).** Con H3-D3, los routers son alcanzables directamente y los clientes solo marcan hacia fuera. A un proveedor no se le marca por Kademlia (`NoDial`): se le llega por su contacto, que lleva su `relay_url`, por los ALPN de los hitos 1 y 2. Con `relay = true`, Iroh puede buscar en el DNS de n0 el endpoint de un router al marcarlo. Es la búsqueda de direcciones que el daemon ya usa desde el hito 1, no una DHT, y la prueba de salida va con `relay = false`. Un router tras NAT es lo que el registro del backend pone en «Qué la haría cambiar».
- **Los `put` rechazados que `libp2p-kad` contesta como guardados (nota 8).** Es el protocolo de `libp2p-kad` 0.49 con `StoreInserts::FilterBoth`, y cambiarlo pide un fork. El router cuenta y registra cada rechazo con su motivo (`pals dht status`).
- **Buscar sin identificarse ante los routers (nota 12).** H3-D4 acepta que cada búsqueda diga a los routers qué agente busca qué capacidad, y manda decirlo en el README y en la configuración. El segundo endpoint que lo evitaría se pesa si aparece el caso de uso que dice H3-D4.
- **Sybil y acaparamiento de una clave.** 20 identidades vivas que republican pueden ocupar los 20 huecos de proveedores de una capacidad en un router (`MemoryStore::add_provider`). Las firmas no lo evitan (§5.5, §13). Las políticas de admisión quedan para cuando haya una red abierta. La prueba de salida no prueba ataques (§14).
- **Escala.** Encaminamiento por distancia con muchos routers, rutas disjuntas y particiones de categorías o muestreo de proveedores (§5.5: «cuando las mediciones lo justifiquen»). La prueba de salida valida integración y fallos básicos (§14).
- **Persistencia de la tabla de rutas y de la tienda de un router.** Un router arranca vacío y se rellena con la republicación y la replicación.
- **Búsqueda semántica y embeddings (§5.4).** Solo alias locales.
- **Repetir la prueba de salida en máquinas físicas.** Es un extra opcional, como en los hitos 1 y 2.

## Global Constraints (fase 2)

- **Versiones fijadas.** Rust 2024, MSRV 1.91, `iroh =1.4.1` como ahora. `libp2p =0.57.1` con `=` en el `Cargo.toml` raíz, y el `Cargo.lock` raíz actualizado. Features: `kad`, `tokio`, `macros` y `ed25519`, más `identify` solo si la tarea 11 la conserva. Nunca `latest` (§14). `cargo +1.91 check --workspace --all-targets --locked` antes del commit de cada tarea que toque `Cargo.toml`.
- **El adaptador** (`dht/transport.rs` y `dht/muxer.rs`) va en su propio módulo. Sus pruebas (`crates/pals/tests/dht_transport.rs`) son la puerta de cualquier cambio de versión de `libp2p` o de `iroh` (H3-D1, condición 3).
- **Formatos congelados.** `agentnet.contact/1` y `agentnet.announce/1` se usan tal cual, con `pals_core::records` y `pals_core::signed`. Lo nuevo se congela con el vector `crates/pals-core/tests/vectors/kad-v0.json` (tarea 8):
  - cómo sale el `PeerId` de la clave del agente: la clave pública Ed25519 en protobuf de libp2p, con multihash identidad;
  - las claves de los valores: `/agentnet/contact/<peer_id>` y `/agentnet/announce/<peer_id>`, con el `PeerId` en base58;
  - la clave de un provider record: el multihash SHA2-256 (código `0x12`) de los bytes UTF-8 de la cadena completa de la capacidad (§5.1). Para una capacidad común, `agentnet:capability:v1:<id>`; para una con espacio de nombres, `<espacio>:<id>` tal cual. Es lo que da `pals_core::inventory::capability_tag`;
  - el ALPN `agentnet-kad/0`, el protocolo `/agentnet/kad/1.0.0` y las dos formas de dirección: `/ip4|ip6/<ip>/udp/<puerto>/quic-v1/p2p/<peer_id>`, un endpoint de Iroh en esa dirección UDP, y `/p2p/<peer_id>`, ninguna ubicación.
- **Sin redes públicas.**
  - El binario no lleva puntos de arranque por defecto.
  - Ningún tráfico de la DHT va a una DHT pública: nunca `/ipfs/kad/1.0.0`, ni mDNS, relay, AutoNAT, UPnP o DCUtR de libp2p.
  - La DHT está apagada salvo que se configure: sin `[dht] enabled = true`, el nodo no sirve `agentnet-kad/0` ni abre ninguna conexión con él.
- **Contratos sin cambios.** Los de A2A, `agentnet-discovery/0`, el inventario y el gateway de los hitos 1 y 2 no cambian. Lo que el hito 3 añade a una petición o una respuesta del gateway y de `pals_discover` son campos opcionales. Con la DHT apagada, una petición del hito 2 recibe lo mismo que en el hito 2.
- **Consultar no ejecuta trabajo**, también en la DHT. Un router contesta desde su tienda, y el módulo `dht` no tiene worker, adaptador MCP ni cliente de tareas. `libp2p-kad` es iterativo: un router nunca reenvía una consulta (§7.3).
- **Lo que llega de la DHT son datos que no son de fiar,** nunca instrucciones: registros, direcciones y claves se verifican antes de usarlos. A la DHT solo va la clave de la capacidad, y los ids del inventario siguen yendo solo a quien da un grant del proyecto (H3-D4, D3 del cierre de los hitos 1 y 2).
- **Pruebas de integración:** nodos reales sobre Iroh en loopback, sincronizadas con el estado y nunca con el tiempo que pasa. Un plazo solo sirve para fallar. Lo que tiene tiempos de `libp2p-kad` (vida en la DHT, republicación, conexión ociosa) se prueba con plazos cortos de `NodeOptions`.
- **Docker para las pruebas, en un anfitrión compartido.** Prefijo `pals-exit-` y etiqueta `pals-exit.dir=<PALS_EXIT_DIR>`, como en los hitos 1 y 2. Nunca `prune`. Nunca `iptables`, `nft`, `tc` ni `sysctl` en el anfitrión; dentro de los contenedores sí, con `NET_ADMIN`. Nunca se copia ni se monta una credencial. La tarea 7 usa el arnés de `spikes/hito-3/`, con su prefijo `pals-spike-` y sus protecciones.
- **Repositorio público.** Antes de cada commit se revisa el diff en busca de rutas del anfitrión (`/root`, `/workspace`, `/tmp/claude`), nombres de usuario, correos y tokens.
- **Idioma.** Los documentos van en español. Los commits, en inglés y convencionales, terminan con estas dos líneas:
  ```text
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_011fjJHFfnWGvJikRKK6ifSY
  ```
- **Lo que no se mide se dice,** en el documento de cada medida y en `exit-test.md`.
- Rama `feat/hito-3`. La implementación y las revisiones las hacen subagentes con Opus (decisión del responsable para PALS, que prevalece sobre la norma general del equipo). `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace --locked` en verde antes de cada commit.

## Review Focus (fase 2)

Lo que el spec implica y es más probable que muerda a quien use el daemon, con la prueba que lo fija:

1. **DHT configurada sin ningún router alcanzable,** como un portátil sin red o con el arranque caído. La búsqueda sigue con los contactos, dentro de su presupuesto, con cobertura parcial `dht unavailable`, nunca con un error. Prueba `dht_without_routers_leaves_the_contacts_search_intact` (13).
2. **Un proveedor que cambia de red.** Su contacto nuevo llega a los routers en cuanto se emite, no en la vuelta siguiente, que puede tardar 20 min. Prueba `an_address_change_republishes_the_contact_at_once` (12).
3. **Relojes desajustados entre máquinas.** Un router guarda un registro emitido 59 s en el futuro y rechaza uno de 61 s, como el resto de PALS (`CLOCK_SKEW_S`). Prueba `records_follow_the_clock_skew` (10).
4. **Servicios privados o de proyecto.** Con `[dht] announce = true`, ninguno llega a la DHT: solo las capacidades públicas (H3-D4, §5.2, §9.8). Prueba `only_public_capabilities_reach_the_dht` (12).
5. **Un proveedor que vuelve a arrancar con otro directorio de datos,** y su `seq` vuelve a 1. Los routers se quedan con el registro de `seq` mayor hasta que caduca, y el README lo dice. Prueba `a_lower_seq_waits_for_the_held_one` (10).

## Estructura de ficheros

```text
crates/pals/src/dht/
  mod.rs          Dht, DhtParts, DhtTiming, DhtStatus, DhtLookup               (8, 11, 12)
  keys.rs         PeerId del agente, claves de valores y de proveedores        (8)
  transport.rs    IrohTransport, IrohListener, AddressBook, direcciones         (9)
  muxer.rs        IrohMuxer y las cuotas de streams                            (9)
  validate.rs     lo que guarda un router                                      (10)
  swarm.rs        el swarm, NoDial, la entrada y la tarea que lo mueve         (11, 12)
  publish.rs      las vueltas de publicación y la búsqueda en la DHT            (12)
crates/pals/src/{config.rs,node.rs,gateway/mod.rs,main.rs,mcp.rs}               (9, 11, 12, 13)
crates/pals/src/discovery/{mod.rs,search.rs}, crates/pals-core/src/discovery/selection.rs   (12, 13)
crates/pals/examples/dht_rogue.rs                                               (14)
crates/pals/tests/{dht_keys,dht_transport,dht_validate,dht,dht_publish,dht_search}.rs
crates/pals-core/tests/vectors/kad-v0.json                                       (8)
docs/extensions/capabilities-v1.md                                               (8)
docs/protocols/agentnet-kad-0.md                                                 (8 a 12)
docs/decisions/hito-3-backend/repeticion.md, results/runs/                       (7)
tests/exit/hito-3.sh, tests/exit/hito-3/                                         (14)
docs/runbooks/hito-3-descubrimiento-kademlia.md, docs/conformance/hito-3/        (14, 15)
```

---

### Task 7: Repetir las medidas de la fase 1 (condiciones de H3-D1)

**Depende de:** la fase 1. Va antes de congelar nada: ni el ALPN, ni las claves de la DHT, ni cómo sale el `PeerId`.

**Files:**
- Modify: `spikes/hito-3/harness/harness.sh`: opciones `--tag <t>`, `--params <fichero>` y `--only <S…>`, y `NODE_ENV` para los contenedores
- Modify: `spikes/hito-3/harness/evaluate.py` (un escenario no ejecutado queda `not-applicable`) y `test_evaluate.py`
- Modify: `spikes/hito-3/kad-iroh/src/behaviour.rs`: el factor de marcado sale de `SPIKE_DIAL_CONCURRENCY`
- Create: `spikes/hito-3/harness/params-production.env`, `spikes/hito-3/harness/repeat.py`, `spikes/hito-3/harness/test_repeat.py`
- Create: `docs/decisions/hito-3-backend/results/runs/*.json` y `docs/decisions/hito-3-backend/repeticion.md`
- Modify: `docs/decisions/2026-11-16-hito-3-backend-dht.md`: una línea con el veredicto en «Condiciones»

**Interfaces:**
- Consumes: el arnés y los candidatos de la fase 1 (tareas 2 a 4), sin cambiar lo medido. Sin `SPIKE_DIAL_CONCURRENCY`, `spike-kad-iroh` marca como en la fase 1, con el factor por defecto de libp2p (8).
- Produces:
  - `harness.sh [--tag <t>] [--params <fichero>] [--only <S…>] <id>`. Con `--tag`, el JSON va a `docs/decisions/hito-3-backend/results/runs/<id>-<t>.json` y los registros, a `logs/<id>-<t>/`. `--params` se lee después de `params.env`, y lo que cambia va al `params` del JSON. Con `--only`, los demás escenarios quedan `not-applicable`, con la nota «no ejecutado en esta ejecución». `NODE_ENV` (por ejemplo, `SPIKE_DIAL_CONCURRENCY=1`) se pasa a cada contenedor con `docker run -e`;
  - `repeat.py --runs <json>… --production <kad-iroh.json> <kad-libp2p.json>`, que imprime las tablas de `repeticion.md` y sale con 0 si no se da ninguna condición de cambio, con 3 si se da alguna, y con 1 si algún JSON es inválido o tiene un escenario en `error`;
  - el veredicto: o sigue `kad-iroh` y las tareas 8 a 15 siguen, o se da una condición de cambio y el plan se para.

Las condiciones son las de la ratificación, tal cual:
- **S3 y S7:** `kad-iroh` queda en `fail` en S3 o en S7 en alguna de sus ejecuciones, y `kad-libp2p` en ninguna. Cuentan las cuatro de cada uno: la de la fase 1 y las tres de esta tarea;
- **reposo con intervalos de producción:** el tráfico en reposo de un router de `kad-iroh` (`router_bytes_per_min_p50` de S6), marcando una sola vez a cada par, pasa del doble del de `kad-libp2p`.

- [ ] **Step 1: Write the failing test** `test_repeat.py`, con JSON sintéticos:
  - `test_s3_fail_only_in_kad_iroh_switches`: un `fail` de S3 en una ejecución de `kad-iroh` y ninguno en `kad-libp2p` da 3, y la salida nombra S3;
  - `test_a_fail_in_both_does_not_switch`: un `fail` de S7 en los dos da 0;
  - `test_idle_traffic_over_double_switches`: 500 001 frente a 250 000 bytes por minuto da 3; 500 000 frente a 250 000 da 0, porque «pasa del doble» es estrictamente más;
  - `test_a_run_with_an_error_is_refused`: un escenario en `error` da 1, y la salida dice que esa ejecución se repite;
  - `test_production_runs_need_production_params`: un JSON de `--production` con `REPUBLISH_S` distinto de 1200, o con S6 sin `measured`, da 1.
- [ ] **Step 2:** `python3 -m unittest` en `spikes/hito-3/harness/`. Esperado: FAIL, porque `repeat.py` no existe.
- [ ] **Step 3:** Implementar `repeat.py` y las opciones del arnés. `params-production.env` lleva `TTL_S=3600`, `REPUBLISH_S=1200` e `IDLE_S=2400`: el reposo cubre dos vueltas de republicación.
- [ ] **Step 4:** `python3 -m unittest` en `spikes/hito-3/harness/` y `bash -n harness.sh`. Esperado: PASS, también `test_evaluate.py`.
- [ ] **Step 5:** Commit de las herramientas, para que cada ejecución anote un commit sin `-dirty`: `test(spikes): repeated runs, production intervals and single dials in the hito 3 harness`.
- [ ] **Step 6:** Seis ejecuciones, alternando los candidatos para repartir la carga del anfitrión: `harness.sh --tag 2 kad-iroh`, `--tag 2 kad-libp2p`, y así con las etiquetas 3 y 4. Esperado: cada una sale con 0, y `check_results.py` sobre las seis y los dos JSON de la fase 1 (`results/kad-iroh.json` y `results/kad-libp2p.json`) sale con 0, con los mismos parámetros. Una ejecución con un escenario en `error` se repite con la misma etiqueta. No cuenta.
- [ ] **Step 7:** Las dos medidas de S6 con intervalos de producción:
  - `harness.sh --tag production --params spikes/hito-3/harness/params-production.env --only S6 kad-libp2p`;
  - la misma con `kad-iroh`, `--tag production-dial-once` y `NODE_ENV=SPIKE_DIAL_CONCURRENCY=1`.

  Esperado: S6 en `measured` y el resto en `not-applicable`, y `check_results.py` sobre los dos sale con 0. En los registros de los routers de `kad-iroh` no aparece ningún `aborted by peer`, la huella de las marcaciones dobles: así se comprueba que marcaba una sola vez.
- [ ] **Step 8:** `python3 spikes/hito-3/harness/repeat.py --runs <los ocho JSON> --production <los dos de producción>`. Escribir `repeticion.md`:
  - el anfitrión, la carga, las fechas y el commit de cada ejecución;
  - el estado de S0 a S7 por ejecución;
  - el mínimo, la mediana y el máximo de S1 (`first_valid_ms_p50`), S3 (`routers_forget_s`), S7 (`resolved_s_max`) y S6 (`router_bytes_per_min_p50`);
  - la tabla de S6 con intervalos de producción, por papel;
  - el veredicto, condición por condición;
  - lo que no se midió.

  Añadir el veredicto, en una línea con el enlace, a «Condiciones» del registro del backend.
- [ ] **Step 9: Si `repeat.py` sale con 3,** escribir un registro nuevo, `docs/decisions/<fecha>-hito-3-cambio-de-backend.md`, con la condición que se dio y sus cifras. Hacer el commit del paso 10 y **parar**: las tareas 8 a 15 dan por hecho `kad-iroh`, y el controlador vuelve a planificar. Con 0, seguir.
- [ ] **Step 10:** Commit: `docs(decisions): hito 3 repeated backend measurements`.

### Task 8: Vocabulario común, claves de la DHT y `PeerId`, congelados

**Depende de:** 7, con el veredicto `kad-iroh`.

**Files:**
- Modify: `Cargo.toml` (`libp2p = { version = "=0.57.1", features = ["kad", "tokio", "macros", "ed25519"] }`; `sha2` sale de «Test-only»), `crates/pals/Cargo.toml`, `Cargo.lock`
- Create: `crates/pals/src/dht/mod.rs` (cabecera del módulo y `pub mod keys`), `crates/pals/src/dht/keys.rs`
- Modify: `crates/pals/src/lib.rs` (`pub mod dht`)
- Create: `crates/pals-core/tests/vectors/kad-v0.json`, `crates/pals/tests/dht_keys.rs`
- Create: `docs/extensions/capabilities-v1.md` (H3-D5)
- Create: `docs/protocols/agentnet-kad-0.md`: identidad, claves y direcciones (las tareas 9 a 12 añaden sus apartados)

**Interfaces:**
- Consumes: `pals_core::identity::AgentId`, `pals_core::inventory::{capability_tag, InventoryError}`.
- Produces:
  ```rust
  // pals::dht::keys
  pub const CONTACT_KEY_PREFIX: &str = "/agentnet/contact/";
  pub const ANNOUNCE_KEY_PREFIX: &str = "/agentnet/announce/";
  pub fn peer_id(agent: &AgentId) -> PeerId;
  pub fn agent_of(peer: &PeerId) -> Option<AgentId>;            // None: no es una clave Ed25519
  pub fn contact_key(agent: &AgentId) -> RecordKey;
  pub fn announce_key(agent: &AgentId) -> RecordKey;
  pub fn provider_key(capability: &str) -> Result<RecordKey, InventoryError>;   // de capability_tag(capability)
  pub enum Slot { Contact(AgentId), Announce(AgentId) }
  pub fn slot(key: &RecordKey) -> Option<Slot>;                 // solo el base58 canónico de un PeerId Ed25519
  ```

**El vector `kad-v0.json`.** La clave del agente es la de los vectores del hito 2: 32 bytes `0x01` como clave secreta. Lleva:
- `agent_id`: `8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c`;
- `peer_id`: `12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5`, con bytes `0024080112208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c`;
- `contact_key` y `announce_key`, las dos cadenas UTF-8 con ese `PeerId`;
- `provider_keys`, cada una con su capacidad, su cadena completa y su clave en hexadecimal:

  | Capacidad | Cadena completa | Clave |
  |-|-|-|
  | `vision.ocr` | `agentnet:capability:v1:vision.ocr` | `122092485626d851a455cd10cd7ae3800eb76ff67220722638435d3ae050e92c8463` |
  | `vision.defect-detection` | `agentnet:capability:v1:vision.defect-detection` | `1220451ff690aedc29578775b84d01a50c84babb625def0ef4c1546f409e08078f23` |
  | `code.rust.review` | `agentnet:capability:v1:code.rust.review` | `122081ce3ea1d76d68090da37bd3df0a4562e5b50e6e7daa7c5c95dbed7728dfc934` |
  | `knowledge.documentation` | `agentnet:capability:v1:knowledge.documentation` | `1220ab4d4ea5320beba8673732049203d56cac1fdae3720699a4fef8c47215345235` |
  | `example:metal-plates` | `example:metal-plates` | `12202a561a2441f0cc4eba6d49bc3d437b467d69cb6c640d213337a81b4826b173b6` |

- `addresses`: `router`, `/ip4/192.0.2.10/udp/4433/quic-v1/p2p/<peer_id>`, y `client`, `/p2p/<peer_id>`.

Las cifras se calcularon al escribir este plan con un script independiente: `hashlib` de Python y base58 hecho a mano. El código tiene que dar las mismas.

- [ ] **Step 1: Write the failing test** `dht_keys.rs`, contra el vector:
  - `peer_id_is_the_agent_key`: `peer_id(&agent).to_base58()` y `to_bytes()` son los del vector, y `agent_of(&peer_id(&agent)) == Some(agent)`;
  - `a_peer_id_of_another_kind_is_no_agent`: `agent_of(&PeerId::random())` es `None`;
  - `record_keys_match_the_vector`;
  - `provider_keys_hash_the_full_capability_string`: las cinco claves de la tabla, y `provider_key("agentnet:capability:v1:vision.ocr")` es un error, para que cada capacidad tenga una sola escritura;
  - `slot_reads_only_canonical_keys`: `/agentnet/contact/<peer_id>` da `Slot::Contact(agent)`; `/agentnet/link/<peer_id>`, `/agentnet/contact/<peer_id>/`, un `PeerId` que no es Ed25519 y una clave de proveedor dan `None`;
  - `capabilities_doc_quotes_the_vector`: cada clave del vector aparece en `docs/extensions/capabilities-v1.md`.
- [ ] **Step 2:** `cargo test -p pals --test dht_keys`. Esperado: FAIL, porque el módulo no existe.
- [ ] **Step 3:** Implementar `keys.rs`. El `PeerId` sale de `identity::PublicKey::from(ed25519::PublicKey::try_from_bytes(agent.as_bytes()))`. La clave de un proveedor es `0x12 0x20` seguido del SHA-256 de `capability_tag(capability)`.
- [ ] **Step 4:** Escribir `capabilities-v1.md`. Lleva:
  - la gramática del hito 2: una capacidad común es de 1 a 128 caracteres `[a-z0-9._-]`, sin `:`; una con espacio de nombres es `<espacio>:<id>`;
  - las cuatro capacidades comunes del §5.1, con su definición y sus ejemplos:
    - `vision.ocr`: sacar el texto de imágenes o de documentos escaneados, como matrículas o facturas;
    - `vision.defect-detection`: encontrar defectos en imágenes de piezas fabricadas;
    - `code.rust.review`: revisar código Rust y decir qué falla o qué mejorar;
    - `knowledge.documentation`: contestar preguntas con un cuerpo de documentación;
  - la cadena completa y la clave en la DHT de cada una, con la tabla del vector;
  - cómo se amplía (H3-D5): un cambio revisado en este documento, sin solaparse con una capacidad existente y con definición y ejemplos. Las capacidades propias van en su espacio de nombres, sin aprobación;
  - que los alias son del buscador (`[discovery.aliases]`, tarea 13).

  Escribir en `agentnet-kad-0.md` la identidad, las claves y las dos formas de dirección.
- [ ] **Step 5:** `cargo test -p pals --test dht_keys` y `cargo +1.91 check --workspace --all-targets --locked`. Esperado: PASS. `cargo tree -p pals -e normal --prefix none | sort -u` solo gana crates de la lista del registro del backend («Las dependencias nuevas y sus versiones»), y ninguno en otra versión de uno que ya estuviera.
- [ ] **Step 6:** Commit: `feat(dht): capability vocabulary, DHT keys and the agent's PeerId, frozen with vectors`.

### Task 9: Transporte de libp2p sobre Iroh, `agentnet-kad/0` y sus límites

**Depende de:** 8.

**Files:**
- Create: `crates/pals/src/dht/transport.rs` y `crates/pals/src/dht/muxer.rs`, a partir de los de la prueba
- Modify: `crates/pals/src/dht/mod.rs`, `crates/pals/src/config.rs` (`[net] listen` e `INIT_TEMPLATE`), `crates/pals/src/node.rs` (`bind_endpoint`)
- Create: `crates/pals/tests/dht_transport.rs`
- Modify: `docs/protocols/agentnet-kad-0.md`: la conexión y sus límites

**Interfaces:**
- Consumes: `keys::{peer_id, agent_of}` (tarea 8), `pals_core::clock::Clock`.
- Produces:
  ```rust
  pub const ALPN_KAD: &[u8] = b"agentnet-kad/0";
  pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
  pub const REFUSED: u32 = 1;                         // código con que se cierra una conexión rechazada
  pub struct KadQuotas { pub accept_queue: usize /*32*/, pub peer_streams: usize /*8*/,
      pub peer_streams_per_minute: u32 /*120*/, pub global_streams: usize /*256*/ }
  #[derive(Clone, Default)] pub struct AddressBook;   // las direcciones directas conocidas de cada par
  impl AddressBook { pub fn learn(&self, peer: PeerId, addr: SocketAddr); pub fn of(&self, peer: &PeerId) -> Vec<SocketAddr>; }
  pub struct IrohTransport;                           // impl Transport<Output = (PeerId, StreamMuxerBox)>
  #[derive(Clone)] pub struct IrohListener;           // impl iroh::protocol::ProtocolHandler
  #[derive(Clone)] pub struct Refusals;               // lo que el listener y el muxer rechazan, desde el arranque
  impl Refusals { pub fn counts(&self) -> RefusedCounts }
  #[derive(Serialize)] pub struct RefusedCounts { pub connections: u64, pub streams: u64 }
  impl IrohTransport {
      pub fn new(endpoint: Endpoint, quotas: KadQuotas, clock: Arc<dyn Clock>, book: AddressBook)
          -> (Self, IrohListener, Refusals);
  }
  pub fn multiaddr_of(peer: &PeerId, addr: SocketAddr) -> Multiaddr;   // /ip4|ip6/<ip>/udp/<puerto>/quic-v1/p2p/<peer>
  pub fn located_nowhere(peer: &PeerId) -> Multiaddr;                  // /p2p/<peer>
  pub fn dial_target(addr: &Multiaddr) -> Option<(PeerId, SocketAddr)>; // solo la primera forma
  pub struct IrohMuxer;                               // impl StreamMuxer; streams entrantes bajo KadQuotas
  // config
  pub struct NetCfg { pub relay: bool, pub listen: Option<SocketAddr> }
  ```

**Qué se conserva de la prueba y qué cambia:**
- Se conservan el `PeerId` del `EndpointId` autenticado (nunca el de la dirección), el marcado con `DIAL_TIMEOUT`, `listen_on` sobre el socket ya ligado y un substream por stream bidireccional de QUIC.
- **El listener nunca espera** (nota 4). Con la cola llena, `accept` cierra la conexión con `REFUSED` y vuelve.
- **Un marcado lleva todas las direcciones conocidas del par** (nota 5): el `EndpointAddr` de `Endpoint::connect` lleva la dirección marcada y las del `AddressBook`, e Iroh elige el camino.
- **Cuotas de streams.** `IrohMuxer` comprueba las de `KadQuotas` antes de entregar a libp2p un stream entrante. El minuto es el del `Clock` del nodo. Lo que pasa de la cuota se resetea sin leerlo y se cuenta en `Refusals`. Por par cuentan todas sus conexiones.
- **`[net] listen`.** El endpoint se liga ahí, con `relay = true` o sin él. Sin `listen` todo queda como en el hito 2: `N0` con relay, o 127.0.0.1 en un puerto libre sin él.

- [ ] **Step 1: Write the failing test** `dht_transport.rs`, con dos endpoints reales en loopback:
  - los cinco de la prueba, con los mismos nombres: `two_nodes_open_a_substream_both_ways`, `peer_id_is_the_agent_key`, `dial_with_a_peer_id_not_matching_the_endpoint_fails`, `a_closed_connection_ends_on_both_sides` y `listen_on_wakes_a_waiting_swarm`;
  - `addresses_follow_the_frozen_forms`: `multiaddr_of` da la forma `router` del vector con `192.0.2.10:4433`, y `dial_target` de la forma `client` o de una dirección `/tcp/` es `None`;
  - `a_full_accept_queue_refuses_instead_of_waiting`: con `accept_queue: 1` y el transporte sin sondear, la segunda conexión se cierra con el código `REFUSED`, el `accept` del listener vuelve y `refusals.counts().connections == 1`;
  - `a_dial_carries_every_known_address`: el `AddressBook` conoce `192.0.2.1:9`, que no contesta, y la dirección real del otro. Marcar la primera conecta, y el otro lado cuenta una sola conexión entrante;
  - `a_peer_beyond_its_stream_quota_is_reset`: con `peer_streams: 2`, el tercer stream abierto a la vez se resetea y los dos primeros funcionan. Con `peer_streams_per_minute: 3` y un `ManualClock`, el cuarto del minuto se resetea, y tras `advance(60)` se acepta otro. `refusals.counts().streams` los cuenta;
  - `global_stream_cap_refuses_beyond_it`: dos pares con `global_streams: 2`;
  - `relay_off_with_listen_binds_there`: un nodo con `relay = false` y `listen = "127.0.0.1:<puerto libre>"` tiene esa dirección en `node.addr()`;
  - `relay_off_without_listen_stays_on_loopback`, como en el hito 2;
  - `listen_that_is_not_a_socket_address_is_refused`, al leer la configuración.
- [ ] **Step 2:** `cargo test -p pals --test dht_transport`. Esperado: FAIL.
- [ ] **Step 3:** Implementar el transporte, el muxer, `[net] listen` en `bind_endpoint` y su comentario en `INIT_TEMPLATE`. Escribir en `agentnet-kad-0.md` el ALPN, qué va dentro de cada conexión (multistream-select de libp2p) y los límites.
- [ ] **Step 4:** `cargo test -p pals --test dht_transport`, cinco veces seguidas. Esperado: PASS las cinco.
- [ ] **Step 5:** Commit: `feat(dht): libp2p transport over the node's Iroh endpoint, with its limits`.

### Task 10: Lo que guarda un router

**Depende de:** 8.

**Files:**
- Create: `crates/pals/src/dht/validate.rs`, `crates/pals/tests/dht_validate.rs`
- Modify: `docs/protocols/agentnet-kad-0.md`: lo que valida un router

**Interfaces:**
- Consumes: `keys::{slot, Slot, provider_key, agent_of}` (8); `pals_core::signed::{Signed, check_validity}`; `pals_core::records::{ContactRecord, AnnounceRecord, MAX_RECORD_BYTES}`.
- Produces:
  ```rust
  pub enum Verdict { Store, Stale, Reject(Rejection) }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
  pub enum Rejection { UnknownKey, TooLarge, WrongKey, BadContact, BadAnnounce, Expired, NotYetValid,
      BadProviderKey, NotAnAgent, ProviderWithoutAnnounce, CapabilityNotAnnounced, StoreFull }
  impl Rejection { pub fn as_str(self) -> &'static str }     // "unknown_key", "too_large", "wrong_key", …
  pub fn check_put(key: &RecordKey, value: &[u8], held: Option<&[u8]>, now_unix: i64) -> Verdict;
  pub fn check_provider(key: &RecordKey, provider: &PeerId, held_announce: Option<&[u8]>, now_unix: i64) -> Verdict;
  pub fn contact_for(agent: &AgentId, value: &[u8], now_unix: i64) -> Result<Signed<ContactRecord>, Rejection>;
  pub fn announce_for(agent: &AgentId, value: &[u8], now_unix: i64) -> Result<Signed<AnnounceRecord>, Rejection>;
  ```
  `StoreFull` no lo da ningún validador: lo cuenta el swarm cuando la tienda no admite más (tarea 11).

**Reglas, en este orden:**
1. Una clave de valor que `slot` no lee es `UnknownKey`. Un valor de más de `MAX_RECORD_BYTES` es `TooLarge`, antes de leerlo.
2. Un contacto o un anuncio tiene que pasar `Signed::from_json`. Si no, es `BadContact` o `BadAnnounce`. Si lo firma otro agente que el de su clave, es `WrongKey`. Fuera de su ventana, con `CLOCK_SKEW_S`, es `Expired` o `NotYetValid`.
3. Si el router guarda uno válido de la misma clave con mayor `seq`, o con el mismo `seq` y otros bytes, el nuevo es `Stale`: no se guarda ni cuenta como rechazado.
4. **Provider record** (nota 3):
   - su clave tiene que ser un multihash SHA2-256 de 34 bytes, o es `BadProviderKey`;
   - su `PeerId`, el de un agente, o es `NotAnAgent`;
   - el router tiene que guardar un anuncio válido de ese agente, o es `ProviderWithoutAnnounce`;
   - ese anuncio tiene que listar una capacidad `c` con `provider_key(c)` igual a la clave, o es `CapabilityNotAnnounced`.

   Así, un agente tiene en un router como mucho 32 provider records, los de su anuncio firmado. Que el anuncio llegue antes lo resuelve el publicador (tarea 12).

- [ ] **Step 1: Write the failing test** `dht_validate.rs`:
  - `vectors_are_stored_under_their_keys`: `contact-v1` y `announce-v1` del hito 2, bajo las claves de su agente, a las 2026-11-14T12:00:00Z, dan `Store`;
  - `a_record_under_another_agents_key_is_wrong_key`;
  - `a_tampered_contact_is_bad_contact`: el contacto con un byte de la firma cambiado, como el de `rogue` en la fase 1;
  - `a_value_over_4_kib_is_too_large_before_it_is_read`: 4097 bytes que no son JSON dan `TooLarge`, no `BadContact`;
  - `records_follow_the_clock_skew` (RF3): `issued_at` 59 s en el futuro da `Store`, 61 s da `NotYetValid`; `expires_at` 59 s en el pasado da `Store`, 61 s da `Expired`;
  - `a_lower_seq_waits_for_the_held_one` (RF5): con el `seq` 5 guardado, el 1 da `Stale`; sin nada guardado, `Store`;
  - `provider_without_its_announce_is_refused`;
  - `provider_of_a_capability_its_announce_does_not_list_is_refused`. Con un anuncio que lista `vision.ocr` y `example:metal-plates`:
    - la clave de `code.rust.review` da `CapabilityNotAnnounced`;
    - el SHA-256 de `vision.ocr` sin el prefijo da `CapabilityNotAnnounced`;
    - las claves del vector de `vision.ocr` y de `example:metal-plates` dan `Store`;
  - `a_provider_key_that_is_not_a_sha256_multihash_is_refused`;
  - `unknown_keys_are_refused`: `/agentnet/link/<peer_id>` y `/ipfs/x`;
  - `rejections_have_stable_names`: la lista de `as_str`, que va a los registros y a `pals dht status`.
- [ ] **Step 2:** `cargo test -p pals --test dht_validate`. Esperado: FAIL.
- [ ] **Step 3:** Implementar `validate.rs`, a partir del de la prueba. Escribir el apartado en `agentnet-kad-0.md`.
- [ ] **Step 4:** `cargo test -p pals --test dht_validate`. Esperado: PASS.
- [ ] **Step 5:** Commit: `feat(dht): what a router stores, and why it refuses the rest`.

### Task 11: El swarm en el daemon: red propia, modos, entrada y límites

**Depende de:** 9 y 10.

**Files:**
- Create: `crates/pals/src/dht/swarm.rs`, `crates/pals/tests/dht.rs`
- Modify: `crates/pals/src/dht/mod.rs`
- Modify: `crates/pals/src/config.rs`: `[dht]` y su bloque comentado en `INIT_TEMPLATE`
- Modify: `crates/pals/src/node.rs`: arranca el `Dht` si está activo y sirve `ALPN_KAD`; `Node::dht()`; `NodeOptions::{dht_timing, kad_quotas, kad_store}`
- Modify: `crates/pals/src/gateway/mod.rs` (`GET /dht`), `crates/pals/src/main.rs` (`pals dht status [--json]`), `crates/pals/tests/common/mod.rs` (`spawn_dht_node`)
- Modify: `Cargo.toml` y `Cargo.lock`, solo si se conserva `identify`
- Modify: `docs/protocols/agentnet-kad-0.md`: protocolo, modos y entrada

**Interfaces:**
- Consumes: `IrohTransport`, `IrohListener`, `AddressBook`, `KadQuotas`, `located_nowhere`, `multiaddr_of` (9); `validate::*` (10); `keys::*` (8).
- Produces:
  ```rust
  // config.rs
  pub enum DhtMode { Client, Router }
  pub struct DhtCfg { pub enabled: bool, pub mode: DhtMode, pub bootstrap: Vec<String> /* tickets */,
      pub external_addrs: Vec<SocketAddr>, pub announce: bool, pub record_ttl_s: u64, pub republish_s: u64 }
  // dht/mod.rs
  pub struct DhtTiming { pub record_ttl: Duration, pub republish: Duration, pub query_timeout: Duration,
      pub idle_timeout: Duration, pub retry_after: Duration }
  impl DhtTiming { pub fn from_config(cfg: &DhtCfg) -> Self }       // 20 s, 10 s y 30 s fijos
  pub struct DhtParts { pub endpoint: Endpoint, pub key: Arc<AgentKey>, pub cfg: DhtCfg, pub timing: DhtTiming,
      pub quotas: KadQuotas, pub store_limits: MemoryStoreConfig, pub clock: Arc<dyn Clock>, pub store: Arc<Store> }
  #[derive(Clone)] pub struct Dht;
  impl Dht {
      pub fn spawn(parts: DhtParts) -> anyhow::Result<(Dht, IrohListener)>;
      pub async fn status(&self) -> Result<DhtStatus, DhtError>;
      pub async fn held_providers(&self, capability: &str) -> Result<Vec<(AgentId, Vec<Multiaddr>)>, DhtError>;
  }
  #[derive(Serialize)] pub struct DhtStatus { pub mode: DhtMode, pub routing_peers: usize, pub connections: usize,
      pub connections_accepted: u64, pub refused: RefusedCounts,
      pub records_stored: usize, pub provider_records_stored: usize,      // solo lo que no ha caducado
      pub stored_by_agent: BTreeMap<AgentId, u32>,                         // ídem, valores y provider records
      pub rejected: BTreeMap<&'static str, u64> }                          // por Rejection::as_str, desde el arranque
  pub enum DhtError { NoRouters, Stopped }
  impl Node { pub fn dht(&self) -> Option<&Dht> }
  pub struct GatewayParts { …, pub dht: Option<Dht> }   // GET /dht y, en la tarea 13, la búsqueda
  ```
  `held_providers` es lo que el nodo guarda como router para una capacidad: sus pruebas y la prueba de salida lo leen.

**El swarm:**
- `Swarm::new` con el `PeerId` del agente, sin `Keypair`, y `Config::with_tokio_executor().with_dial_concurrency_factor(1).with_idle_connection_timeout(idle_timeout)`.
- Comportamientos:
  - `kad::Behaviour<MemoryStore>`;
  - `connection_limits::Behaviour`, con 2 conexiones establecidas por par y 256 entrantes (nota 4);
  - `NoDial`, de la prueba: una búsqueda no marca a los proveedores que encuentra;
  - `identify`, solo si el paso 4 lo pide.
- `kad::Config::new("/agentnet/kad/1.0.0")`:
  - `set_record_ttl` y `set_provider_record_ttl` a `record_ttl`;
  - `set_publication_interval(None)` y `set_provider_publication_interval(None)`: publica la tarea 12, en sus vueltas;
  - `set_replication_interval(Some(republish))`;
  - `StoreInserts::FilterBoth`, `Caching::Disabled` y `query_timeout`.

  `MemoryStore` con `store_limits`. El modo, siempre fijado: `Some(Mode::Server)` en un router y `Some(Mode::Client)` en un cliente (H3-D3).
- **Lo que llega.** Cada `PutRecord` y `AddProvider` pasa por `check_put` o `check_provider`, con el `Clock` del nodo. Lo guardado va a la tienda; si la tienda no admite más, cuenta `StoreFull`. Cada rechazo se cuenta por nombre y se registra en `debug` con quien lo envió.
- **Entrada.**
  - Cada ticket de `bootstrap` da sus direcciones IP en la forma de `multiaddr_of`, que van a `add_address` y al `AddressBook`. Después, `bootstrap()`.
  - Si al terminar la tabla de rutas sigue vacía, se intenta igual con los contactos guardados que tienen ticket: 8 como mucho, por nombre.
  - Un router sin `bootstrap` arranca solo.
  - Tras un marcado fallido a un servidor, se le devuelven a Kademlia sus direcciones, como en la prueba: `libp2p-kad` las quitaría.
- **Direcciones externas.** `libp2p-kad` no devuelve un provider record sin direcciones, así que todo nodo declara alguna. Un router declara `external_addrs` o, si no hay, las directas de su endpoint. `dialable_addr` (`node.rs`) añade las `external_addrs` de un router, así que su ticket, la invitación, las lleva aunque el endpoint esté ligado a `0.0.0.0`. Un cliente solo declara `located_nowhere(propio)`, que no es ninguna ubicación.
- **Sin instrumentación de la prueba** (nota 10): no hay barrido ni estadísticas cada segundo. `status` lo calcula al pedirlo, y solo cuenta lo que no ha caducado (`is_expired(Instant::now())`).
- **Conexión ociosa** (nota 7): 10 s, y nada mantiene vivas las conexiones. Un cliente no gasta tráfico en reposo, y cada búsqueda tras un reposo paga un apretón de manos de Iroh con los routers.
- **Configuración.**
  - Los tickets de `bootstrap` tienen que leerse, nombrar otro endpoint que el propio y llevar al menos una dirección IP directa.
  - `external_addrs` solo vale en un router.
  - `record_ttl_s` y `republish_s`, en sus márgenes.
  - `INIT_TEMPLATE` lleva `[dht]` comentado. Dice que la DHT está apagada por defecto y sin puntos de arranque, que el nodo es cliente salvo que se diga, que no anuncia nada salvo `announce = true` y entonces solo lo público, y que **cada búsqueda en la DHT dice a los routers qué agente busca qué capacidad, porque la conexión va autenticada con la clave del agente** (H3-D4).

**Montaje de las pruebas** (`spawn_dht_node`): R1, un router sin `bootstrap`; R2 y R3, routers con R1; clientes con R1. Todo en loopback, con `NodeOptions::dht_timing`.

- [ ] **Step 1: Write the failing test** `dht.rs`:
  - `dht_off_serves_no_kad_alpn`: con la configuración por defecto, `connect(addr, ALPN_KAD)` falla porque el ALPN no se sirve, `node.dht()` es `None` y `GET /dht` da 404;
  - `a_node_with_no_bootstrap_knows_no_one`: `routing_peers == 0` y `connections == 0`;
  - `nodes_join_through_the_bootstrap`: R2 y el cliente entran por R1, y R1 y R2 se tienen en su tabla;
  - `a_router_with_no_bootstrap_learns_the_others`: R1 llega a tener a R2 y a R3 en su tabla;
  - `clients_never_enter_a_routing_table`: la tabla de cada router solo tiene routers;
  - `a_client_stays_a_client`: el `mode` de su estado, que es `kad.mode()`, sigue en `Client` aunque declara una dirección externa, y ningún router lo tiene en su tabla. Con el modo automático, `libp2p-kad` lo pasaría a servidor;
  - `a_newcomer_joins_through_a_saved_contact_when_the_bootstrap_is_down`: con R1 parado, un nodo con `bootstrap = [R1]` y R2 como contacto guardado entra por R2;
  - `a_router_with_two_addresses_is_dialed_once` (nota 5): con R1 y un cliente solos, el cliente arranca con un ticket de R1 que lleva `192.0.2.1` primero y su dirección real después. Tras entrar, R1 cuenta `connections_accepted == 1`;
  - `connections_beyond_the_per_peer_limit_are_refused` (nota 4): tres `connect(R1, ALPN_KAD)` desde el mismo endpoint, y R1 cierra la tercera;
  - `idle_connections_close_after_the_idle_timeout` (nota 7): con `idle_timeout` de 1 s, las conexiones del cliente llegan a 0;
  - `a_routers_ticket_names_its_external_addrs`: el ticket de un router (`pals id --ticket`) lleva sus `external_addrs`, también con `[net] listen = "0.0.0.0:<puerto>"`;
  - `status_through_the_gateway_and_the_cli`: `GET /dht` y `pals dht status --json` (con `assert_cmd`) dan el mismo `DhtStatus`;
  - `bad_dht_sections_are_refused`: un ticket que no se lee, el propio, uno sin direcciones IP, `republish_s` mayor que la mitad de `record_ttl_s` y `external_addrs` en un cliente.
- [ ] **Step 2:** `cargo test -p pals --test dht`. Esperado: FAIL.
- [ ] **Step 3:** Implementar `swarm.rs`, `Dht::spawn`, `status` y `held_providers`, `[dht]`, el arranque en `node.rs`, `GET /dht` y `pals dht status`. Sin `identify`.
- [ ] **Step 4: `identify`** (nota 11). `cargo test -p pals --test dht`.
  - Si pasan `a_router_with_no_bootstrap_learns_the_others` y `clients_never_enter_a_routing_table`, `identify` no hace falta: ni comportamiento ni feature.
  - Si alguna falla y la causa es la que da el registro del backend (la lista de protocolos del par, o las direcciones de una conexión entrante), se añade `identify` con `with_hide_listen_addrs(true)` y `with_cache_size(0)`, con la feature en el `Cargo.toml` raíz. La cabecera de `swarm.rs` nombra la prueba que lo pidió.
- [ ] **Step 5:** `cargo test -p pals --test dht` y `cargo +1.91 check --workspace --all-targets --locked`. Esperado: PASS. Escribir el apartado en `agentnet-kad-0.md`, con la decisión sobre `identify`.
- [ ] **Step 6:** Commit: `feat(dht): the daemon's Kademlia swarm over Iroh, its modes, entry and limits`.

### Task 12: Publicar y encontrar: anuncio, renovación, caducidad y búsqueda en la DHT

**Depende de:** 11.

**Files:**
- Create: `crates/pals/src/dht/publish.rs`, `crates/pals/tests/dht_publish.rs`
- Modify: `crates/pals/src/dht/{mod.rs,swarm.rs}`
- Modify: `crates/pals/src/discovery/mod.rs`: `Records::{changes, capabilities}` e `impl PublishedRecords for Records`
- Modify: `crates/pals/src/node.rs`: publica cuando `[dht] announce`
- Modify: `docs/protocols/agentnet-kad-0.md`: publicación y caducidad

**Interfaces:**
- Consumes: `Dht`, `DhtParts` (11); `keys`, `validate` (8, 10); `discovery::{Records, announced_capabilities}`.
- Produces:
  ```rust
  pub trait PublishedRecords: Send + Sync + 'static {
      fn current(&self) -> (Vec<u8>, Vec<u8>);         // contacto y anuncio en vigor, JSON firmado
      fn capabilities(&self) -> Vec<String>;           // las públicas, como las escribe el inventario
      fn changes(&self) -> tokio::sync::watch::Receiver<u64>;   // sube con cada registro que se vuelve a emitir
  }
  impl PublishedRecords for Records {}
  impl Records { pub fn changes(&self) -> watch::Receiver<u64>; pub fn capabilities(&self) -> Vec<String>; }
  // DhtParts gana: pub published: Option<Arc<dyn PublishedRecords>>   // Some con [dht] announce
  impl Dht {
      pub async fn providers(&self, capability: &str, limit: usize, deadline: tokio::time::Instant)
          -> Result<DhtLookup, DhtError>;
  }
  pub struct DhtLookup { pub found: Vec<DhtCandidate>, pub discarded: Vec<(AgentId, String)>,
      pub requests: u64, pub cut: bool }
  pub struct DhtCandidate { pub agent_id: AgentId, pub contact: Signed<ContactRecord>, pub contact_json: Vec<u8>,
      pub announce: Signed<AnnounceRecord>, pub announce_json: Vec<u8> }
  #[derive(Serialize)] pub struct PublicationStatus { pub capabilities: Vec<String>,
      pub last_round_unix: Option<i64>, pub last_error: Option<String>, pub failed_capabilities: Vec<String> }
  // DhtStatus gana: pub publication: Option<PublicationStatus>
  ```

**Una vuelta de publicación.** Hay una al entrar en la red, cada `republish`, cada vez que sube `changes()` (un registro nuevo: cambio de direcciones o renovación de las 12 h) y, si una vuelta no llegó a ningún router, a los `retry_after`:
1. Pone el contacto y el anuncio con `Quorum::All`. Un `put` cuenta como hecho cuando al menos un router lo guardó: `QuorumFailed` con `success` no vacío.
2. **El anuncio viaja con cada provider record** (nota 3). Para cada capacidad pública, con su `provider_key`: `get_closest_peers` de la clave, después `put_record_to` del anuncio a esos pares con `Quorum::One`, y después `start_providing`. Así, el router que guarda el provider record guarda también el anuncio que la regla de la tarea 10 le pide, aunque con muchos routers los más cercanos a la clave del anuncio y a la de la capacidad no sean los mismos.
3. Cada capacidad va por su cuenta (nota 9). Si una falla, por ejemplo con `store::Error::MaxProvidedKeys` en un router que también anuncia, va a `failed_capabilities` y se intenta en la vuelta siguiente, y las demás siguen. No se deshace nada. Una vuelta nueva cierra con `query_mut(id).finish()` las consultas que quedaban de la anterior.
4. Deja de anunciar (`stop_providing`) las capacidades que el anuncio nuevo ya no lista.

Solo se anuncian las capacidades de `announced_capabilities`, es decir, de los servicios públicos (H3-D4). Sin `announce`, no se publica nada, tampoco el contacto. Al parar el nodo no se manda nada: las copias de los routers caducan solas.

Dos ajustes de la prueba desaparecen, porque publican las vueltas, en segundo plano y sin el plazo de una API de control: la republicación de los valores atada al intervalo de replicación, y el plazo del anuncio (`Task::end_announce`).

**La búsqueda en la DHT** (`providers`). El estado va por `QueryId`, así que admite cualquier número de búsquedas y vueltas a la vez (nota 2). Sigue estos pasos:
- pide los proveedores de `provider_key(capability)`;
- según llega cada `PeerId`, hasta `limit`, pide su contacto y su anuncio, y verifica cada copia con `contact_for` y `announce_for` al `Clock` del nodo. Se queda con la válida de mayor `seq`;
- un proveedor es candidato cuando tiene contacto y anuncio válidos y el anuncio lista la capacidad;
- los que no lo consiguen antes de `deadline` van a `discarded`, con el motivo de su último rechazo o `no valid contact`;
- en `deadline` devuelve lo que tiene, con `cut: true`, y cierra sus consultas.

`requests` suma `QueryStats::num_requests` menos los marcados que deniega `NoDial`.

**Montaje de las pruebas:** R1 y R2 routers; P1 con `vision.ocr` y `knowledge.documentation` públicos; P2 con `code.rust.review` público; C cliente. Plazos cortos (`record_ttl` de 6 s y `republish` de 2 s), salvo donde se dice.

- [ ] **Step 1: Write the failing test** `dht_publish.rs`:
  - `a_published_capability_is_found`: `C.providers("vision.ocr", 8, …)` da P1, con su contacto y su anuncio verificados, y `requests > 0`;
  - `only_public_capabilities_reach_the_dht` (RF4): con un servicio de proyecto y otro privado en el inventario de P1, sus capacidades no se encuentran, `held_providers` de los routers no las tiene y el anuncio solo lista las públicas;
  - `nothing_is_published_without_announce`: un proveedor con `announce = false` no está en el `stored_by_agent` de ningún router;
  - `a_client_providers_record_carries_no_ip_address` (nota 6): `held_providers("vision.ocr")` de R1 da P1 con `[/p2p/<P1>]`, la forma `client` del vector, y C lo encuentra;
  - `concurrent_lookups_and_a_round_all_finish` (nota 2): cuatro búsquedas a la vez, más una vuelta forzada con `changes()` en medio. Todas devuelven a P1 y la vuelta acaba sin `last_error`;
  - `a_restarted_router_gets_the_announce_before_the_providers` (nota 3): R2 vuelve a arrancar vacío. Tras la vuelta siguiente de P1, R2 guarda su anuncio y su provider record, y su `rejected["provider_without_announce"]` es 0;
  - `an_address_change_republishes_the_contact_at_once` (RF2): con `republish` de 3600 s y un `PublishedRecords` de prueba, emitir un contacto con `seq` 2 y subir `changes()` lleva los routers al `seq` 2. Con esa republicación, solo el cambio lo explica;
  - `records_expire_at_the_routers` (nota 10): P1 parado sin aviso. Los `stored_by_agent` y `records_stored` de los routers dejan de contarlo, sin barrido, y `C.providers` deja de devolverlo, con el mecanismo de caducidad de `libp2p-kad`;
  - `a_router_back_after_a_stop_is_dialed_again`: R1 vuelve en el mismo puerto (`[net] listen`, con `common::shut_down` antes de reutilizar su directorio), y la búsqueda siguiente de C vuelve a conectar con él: R1 cuenta `connections_accepted`;
  - `a_capability_that_does_not_fit_fails_alone` (nota 9): en una red de R3 y C solos, R3 es router, anuncia `vision.ocr` y `code.rust.review` y tiene `max_provided_keys: 1`. `failed_capabilities == ["code.rust.review"]`, y C encuentra `vision.ocr`;
  - `a_round_that_reaches_no_router_is_retried` (nota 9): con `republish` de 3600 s y `retry_after` de 1 s, P1 arranca con R1 parado. Cuando R1 arranca en su puerto, recibe los registros de P1;
  - `a_full_router_store_is_counted` (nota 3): con `max_records: 2` en R1, el segundo proveedor suma `rejected["store_full"]` en R1;
  - `a_lookup_ends_at_its_deadline` (nota 10): con un router mudo, un endpoint que sirve `ALPN_KAD`, acepta y nunca contesta, la búsqueda vuelve con `cut: true`;
  - `a_provider_with_a_tampered_contact_is_discarded` (§14): un `PublishedRecords` con un byte de la firma del contacto cambiado. Los routers cuentan `bad_contact` y `C.providers` no lo da como candidato y lo pone en `discarded`.
- [ ] **Step 2:** `cargo test -p pals --test dht_publish`. Esperado: FAIL.
- [ ] **Step 3:** Implementar `publish.rs`, `providers`, `Records::changes` y la publicación en `node.rs`. `renew_on_change` y `renew_records_hourly` ya llaman a `Records::renew`: `changes()` sube cuando emite algo.
- [ ] **Step 4:** Si `a_client_providers_record_carries_no_ip_address` muestra que `libp2p-kad` 0.49 no conserva `/p2p/<peer_id>` como dirección de un provider record, parar e informar. La forma `client` está en el vector congelado, y otra salida se decide fuera de esta tarea.
- [ ] **Step 5:** `cargo test -p pals --test dht_publish`, tres veces seguidas. Esperado: PASS las tres. Escribir el apartado en `agentnet-kad-0.md`.
- [ ] **Step 6:** Commit: `feat(dht): publish public capabilities, renew, expire and look them up`.

### Task 13: La DHT como fuente de la búsqueda (§5.3)

**Depende de:** 12.

**Files:**
- Modify: `crates/pals-core/src/discovery/selection.rs`: `SearchRequest::exclude_dht`, `Candidate::from_dht` y `SearchMetrics::dht_requests`
- Modify: `crates/pals/src/discovery/search.rs`: `SearcherParts::dht`, los candidatos de la DHT y los alias
- Modify: `crates/pals/src/config.rs` (`[discovery.aliases]`), `crates/pals/src/gateway/mod.rs`, `crates/pals/src/net/mod.rs` (`contact_addr`), `crates/pals/src/main.rs` (`pals discover --no-dht`), `crates/pals/src/mcp.rs` (descripción de `pals_discover`)
- Create: `crates/pals/tests/dht_search.rs`

**Interfaces:**
- Consumes: `Dht::providers`, `DhtLookup`, `DhtCandidate` (12); `Store::peer_record_accept` (hito 2).
- Produces:
  ```rust
  // pals-core, campos opcionales: una petición o una respuesta del hito 2 se lee igual
  pub struct SearchRequest { …, #[serde(default, skip_serializing_if = "is_false")] pub exclude_dht: bool }
  pub struct Candidate { …, #[serde(default, skip_serializing_if = "is_false")] pub from_dht: bool }
  pub struct SearchMetrics { …, #[serde(default)] pub dht_requests: u64 }
  // pals
  pub struct SearcherParts { …, pub dht: Option<Dht> }
  pub struct DiscoveryCfg { …, pub aliases: BTreeMap<String, String> }   // alias -> capacidad
  pub fn contact_addr(contact: &IrohContact) -> EndpointAddr;              // net/mod.rs
  ```

**Los seis pasos del §5.3:**
1. **Traducir.** `req.capability` pasa por `[discovery.aliases]`, un paso y sin cadenas. Un alias se dirige a una capacidad válida (`capability_tag`), o la configuración no se lee. 256 alias como mucho.
2. **Contactos.** Los del hito 2, como hasta ahora.
3. **DHT,** solo si está activa, la petición no nombra `peers` y no lleva `exclude_dht` (H3-D4). La búsqueda tiene como mucho `per_candidate_timeout` y lo que quede del presupuesto. A la DHT solo va la clave de la capacidad.
4. **Un conjunto limitado.** Primero los contactos y después los de la DHT, en su orden de llegada, cada agente una vez y nunca este. Hasta `max_candidates`; los que sobran dan cobertura parcial `candidate limit`. La cobertura parcial gana además `dht timeout` si la búsqueda en la DHT se cortó, y `dht unavailable` si no había routers.
5. **Verificar.** El contacto y el anuncio de cada candidato de la DHT pasan por `peer_record_accept`, y uno en regresión se descarta (`record regression`). Al candidato se le marca en `contact_addr` de su contacto: sus direcciones directas y su relay. Su card se verifica como la de cualquier par, y su anuncio, en la respuesta a la consulta, tiene que nombrar la card leída, como en el hito 2. Lo que descarta la DHT va a `discarded`, en la etapa `Card`, con el motivo `dht: <motivo>`.
6. **Filtros baratos y consulta.** Los del hito 2. Un candidato de la DHT sin grant del proyecto no recibe ids del inventario ni proyecto (H3-D4, D3), y queda en el ranking con `grant_held: false` y `from_dht: true`.

**Preguntar a un candidato de la DHT.** Cuando el gateway resuelve un id de agente sin contacto guardado, lo marca en las direcciones del contacto más nuevo que conoce de él (`Store::peer_record`), si sigue en vigor por el `Clock` del nodo; si no tiene ninguno, solo por su id, como antes. Así, `pals ask <id>` y `choose_and_ask` llegan a un proveedor encontrado en la DHT, una vez que se tiene un grant suyo.

**Interfaz.** `pals discover --no-dht`. El parámetro `exclude_dht` de `pals_discover` sale solo, porque son los campos de `SearchRequest`, y siguen siendo ocho herramientas. La descripción de `pals_discover` dice que una búsqueda en la DHT identifica al agente ante los routers.

- [ ] **Step 1: Write the failing test** `dht_search.rs`, con dos routers, P1 y P2 proveedores con el worker `CountingExecutor`, y A, el que busca, sin contactos salvo donde se dice:
  - `a_provider_with_no_prior_contact_is_found` (criterio de salida): `search(vision.ocr)` pone a P1 en el ranking con `from_dht`, la oferta verificada del ámbito público y `grant_held: false`;
  - `contacts_come_first_and_each_agent_once` (§5.3, pasos 2 y 4): P2 es contacto de A y está también en la DHT. Con `max_candidates: 1` solo se evalúa P2, con cobertura parcial `candidate limit`. Con 8, P2 sale una vez y sin `from_dht`;
  - `exclude_dht_and_named_peers_never_touch_the_dht` (H3-D4): con `exclude_dht` o con `peers`, `metrics.dht_requests == 0` y P1 no sale;
  - `dht_without_routers_leaves_the_contacts_search_intact` (RF1): con P2 como contacto de A y los routers parados, la búsqueda devuelve a P2 con cobertura parcial `dht unavailable`, sin error;
  - `a_tampered_card_is_discarded_even_with_a_valid_dht_announce` (§14): un par de prueba del hito 2 (`stub_discovery_peer_with_card`) sirve una card alterada, y sus registros válidos están en la DHT, publicados con un `Dht` y un `PublishedRecords` de prueba con su clave. Queda descartado en la etapa `Card`;
  - `a_dht_record_that_goes_back_is_discarded`: A conoce el contacto de P1 con `seq` 3, la DHT sirve el 2, y el motivo es `record regression`;
  - `inventory_ids_never_go_to_a_dht_candidate_without_a_grant` (H3-D4, D3): el `DiscoveryHook` de P1 ve la consulta sin proyecto y sin ids;
  - `an_alias_searches_its_capability` (§5.4): con `ocr = "vision.ocr"`, buscar `ocr` encuentra a P1;
  - `aliases_to_invalid_capabilities_are_refused`, al leer la configuración;
  - `a_dht_candidate_is_asked_once_a_grant_is_held`: con un grant de P1 importado en A, `choose_and_ask` pregunta a P1, que no es contacto, y la respuesta es `echo: hola`;
  - `querying_the_dht_runs_no_work`: tras 20 búsquedas, `runs() == 0` en P1, P2 y los routers, y ningún `task.accepted`;
  - `discover_cli_and_mcp_take_exclude_dht`: `pals discover --no-dht` (con `assert_cmd`) y `pals_discover` con `exclude_dht`; la lista de herramientas MCP sigue con ocho.
- [ ] **Step 2:** `cargo test -p pals --test dht_search`. Esperado: FAIL.
- [ ] **Step 3:** Implementar. Las baterías del hito 2 (`selection`, `service_query`, `discovery`, `mcp`, `gateway`) no cambian ninguna aserción: los campos nuevos son opcionales y la DHT está apagada en ellas. Solo los literales de `SearchRequest`, `Candidate` y `SearchMetrics` de las pruebas ganan `exclude_dht: false`, `from_dht: false` y `dht_requests: 0`.
- [ ] **Step 4:** `cargo test --workspace --locked`. Esperado: PASS.
- [ ] **Step 5:** Commit: `feat(discovery): the DHT as a source of candidates, behind the contacts`.

### Task 14: Prueba de salida del hito 3

**Depende de:** 13.

**Files:**
- Create: `crates/pals/examples/dht_rogue.rs`: entra en la DHT y publica `vision.ocr` con un contacto de firma alterada, con un `PublishedRecords` propio
- Create: `tests/exit/hito-3.sh` y `tests/exit/hito-3/{Dockerfile,metricas.py}`. La imagen es Debian trixie-slim fijada por digest, como la de `tests/exit/Dockerfile`, con `curl`, `iproute2` y `python3`
- Create: `docs/runbooks/hito-3-descubrimiento-kademlia.md`, `docs/conformance/hito-3/exit-test.md`
- Modify: `tests/exit/README.md`

**Interfaces:**
- Consumes: el binario `pals` (`cargo build --release --locked -p pals`), `pals dht status --json`, `pals discover`, `pals grant issue` e `import`, `pals id --ticket` y `pals contact add`.
- Produces: `exit-test.md`, con el resultado de cada caso y las métricas del §14.

**Montaje** (H3-D2). Dos redes Docker `--internal`, `10.213.73.0/24` (A) y `10.213.74.0/24` (B), con un contenedor por agente, `pals-exit-h3-*`, y un `PALS_HOME` por agente. Todos los daemons llevan `relay = false`, `[net] listen = "0.0.0.0:4433"`, `[dht] enabled = true`, `record_ttl_s = 120` y `republish_s = 40`: los plazos de la fase 1, para que los casos de caducidad duren minutos. Un worker `echo`.
- **Dos operadores,** cada uno con sus identidades, su configuración (`tests/exit/hito-3/operador-1/` y `operador-2/`) y sus puntos de arranque:
  - operador 1: `r1`, sin arranque y el arranque original, y `r2`, con `r1`;
  - operador 2: `r3`, que entra en la red por `r1`, y `r4`, con `r3`.

  Los cuatro son routers, en las dos redes, con sus dos IP en `external_addrs`.
- **Proveedores**, con `announce = true` y las capacidades de la fase 1: `p1` y `p2` en la A, con `r1`; `p3` y `p4` en la B, con `r3`. `p2` tiene además un servicio privado y otro de proyecto.
- **Clientes:** `c1` y `c2` en la A, con `r1`; `c3` y `c4` en la B, con `r3`.
- **Los que solo arrancan para su caso:**
  - `c0`, aislado, sin arranque ni contactos;
  - `c5` en la A, con `bootstrap = [r1]` y el ticket de `r3` guardado como contacto: la invitación del operador 2;
  - `rogue` en la B, con `dht_rogue`;
  - `cx`, sin `[dht]`.

**Casos** (runbook y arnés; cada uno imprime `RESULTADO <caso>: superado|FALLIDO`):

| Caso | Comprobación |
|-|-|
| 1. Sin fuga | Las dos redes tienen `Internal=true`. `c0` no encuentra a nadie y su `GET /dht` da 0 pares y 0 conexiones. `cx` no abre ninguna conexión `agentnet-kad/0` y su búsqueda no hace peticiones a la DHT |
| 2. Encontrar un proveedor sin contacto previo | Cada cliente busca 10 veces cada capacidad: los dos proveedores de cada una, `from_dht`, con oferta verificada. Después, `p1` da un grant a `c1`, y `pals discover --capability vision.ocr --ask "hola"` desde `c1` obtiene `echo: hola` de `p1` por A2A estándar |
| 3. Sin el arranque original | Se para `r1`. Los clientes conectados siguen encontrando los dos proveedores de cada capacidad. `c5` arranca, entra por `r3` y los encuentra |
| 4. Caducar anuncios | Se para `p1` sin aviso. Los routers dejan de guardar sus registros (`stored_by_agent`) y las búsquedas dejan de devolverlo antes de `record_ttl_s + republish_s` (160 s) |
| 5. Caída de routers y renovación | Se paran `r2` y `r4`, uno de cada operador. Se miden la cobertura inmediata y el tiempo hasta recuperarla entera, que tiene que ser menos de `2 × republish_s` |
| 6. Contacto con firma inválida | `rogue` anuncia `vision.ocr`. Los routers cuentan `bad_contact` y ninguno guarda su contacto. Ningún cliente lo pone en su ranking, y su `discarded` lo nombra |
| 7. Búsquedas repetidas o maliciosamente amplias | Ráfaga de 200 búsquedas desde `c1`: las 4 capacidades y 20 que no existen, 4 a la vez. Las de más dan `too many searches`, ningún router se cae y las búsquedas de `c3` durante la ráfaga terminan. Se anota el `refused.streams` de cada router |
| 8. Política | Las capacidades privadas y de proyecto de `p2` no están en el `held_providers` de ningún router. Con `--no-dht`, `dht_requests` es 0 |
| 9. Cambio de red | `p3` pasa de la red B a la A. Su daemon emite un contacto nuevo y lo publica, y antes de `2 × republish_s` los clientes reciben la dirección nueva y consultan a `p3` en ella: card y oferta verificadas |

- [ ] **Step 1:** Escribir `dht_rogue.rs` y comprobar que compila (`cargo build --release --locked -p pals --examples`).
- [ ] **Step 2:** Escribir el runbook y el arnés, con las protecciones de `tests/exit/hito-2.sh`:
  - prefijo y etiqueta;
  - negarse a seguir si hay recursos `pals-exit-h3-*` de otra ejecución;
  - desmontar con `trap` en `EXIT` e `INT`;
  - las fases `preparar`, `caso1` a `caso9` y `desmontar`.

  `metricas.py` lee lo que el arnés recoge:
  - la respuesta de `POST /discovery/search` de cada búsqueda, como en el hito 2;
  - `GET /dht` de cada nodo;
  - los contadores de `/sys/class/net/*/statistics` y el RSS de `/proc/<pid>/status`, dentro de cada contenedor.
- [ ] **Step 3:** Ejecutar `PALS_EXIT_DIR=<dir nuevo> tests/exit/hito-3.sh`. Esperado: los nueve casos superados, y `docker ps -a --filter label=pals-exit.dir=<dir>` vacío al terminar. Un caso que falla se corrige y se repite. No se cierra con casos abiertos.
- [ ] **Step 4:** Escribir `exit-test.md`:
  - fecha, anfitrión y carga, versiones, commit y topología;
  - el resultado de cada caso;
  - las métricas del §14: tiempo hasta el primer candidato válido (p50 y p95), porcentaje de búsquedas con candidato, peticiones a la DHT y bytes por búsqueda, candidatos contactados, anuncios caducados y tiempo de caducidad, tiempos de recuperación (casos 3, 5 y 9), coste del matching (`matching_us`), y memoria, conexiones y tráfico en reposo por papel durante 300 s;
  - el coste de mantener el adaptador: líneas de `transport.rs` y `muxer.rs`;
  - las filtraciones de metadatos observadas: lo que un router registra de cada búsqueda, el agente que busca y la clave que pide (H3-D4);
  - lo que no cubre: escala, Sybil, NAT de operador, máquinas físicas y la búsqueda con `relay = true`.
- [ ] **Step 5:** Commit: `test(exit): hito 3 exit test in Docker, two operators on two networks`.

### Task 15: Conformidad sin regresión y cierre del hito 3

**Depende de:** 14.

**Files:**
- Create: `docs/conformance/hito-3/README.md` y `docs/conformance/hito-3/compatibility*.{json,html}`, que se generan
- Modify: `README.md`, `CLAUDE.md`, `docs/plans/2026-11-14-hoja-de-ruta-hitos-2-4.md`

**Interfaces:**
- Consumes: todo lo anterior.
- Produces: el hito 3 cerrado según «Cómo se cierra un hito» de la hoja de ruta.

- [ ] **Step 1: Repetir la conformidad**

```bash
PALS_CONFORMANCE_DIR=docs/conformance/hito-3 tests/interop/run_tck.sh
target/interop-venv/bin/pip install -r tests/interop/requirements.txt
target/interop-venv/bin/python tests/interop/python_client.py
target/interop-venv/bin/python tests/interop/check_schemas.py
target/interop-venv/bin/python tests/interop/card_signatures.py --vector crates/pals-core/tests/vectors/card-jws-v1.json
```

Esperado:
- el TCK sale con 0, sin ningún MUST fallido en `jsonrpc` ni en `http_json`: 72 MUST superados, 58 en `jsonrpc` y 57 en `http_json`, como en el hito 2;
- `4 checks passed`;
- todos los documentos válidos;
- el vector de la card verifica.

La card no cambia en este hito. Cualquier diferencia con el hito 2 se explica requisito a requisito en `docs/conformance/hito-3/README.md`.
- [ ] **Step 2: Documentación.**
  - **`README.md`:** qué añade el hito 3, con `[dht]`, `pals dht status`, `pals discover --no-dht` y los alias. Sus limitaciones:
    - cada búsqueda en la DHT dice a los routers qué agente busca qué capacidad (H3-D4);
    - los routers tienen que ser alcanzables directamente, y no hay forma de dirección con relay;
    - un proveedor que vuelve con otro directorio de datos no se encuentra en su dirección nueva hasta que caduca su registro anterior (RF5);
    - ni Sybil ni la escala están probados;
    - una invitación es un ticket de Iroh;
    - para preguntar a un proveedor encontrado en la DHT sigue haciendo falta un grant suyo.
  - **`CLAUDE.md`:**
    - el módulo `dht/` en «Layout»;
    - en «Constraints», los identificadores congelados del hito 3: ALPN `agentnet-kad/0`, protocolo `/agentnet/kad/1.0.0`, las claves, las dos formas de dirección y el vector `kad-v0`;
    - que el adaptador tiene sus pruebas como puerta de una subida de `libp2p` o `iroh`;
    - la orden de la prueba de salida.
  - **Hoja de ruta:** el hito 3 pasa a implementado, con su prueba de salida.
- [ ] **Step 3:** Revisión de toda la rama, que pide el controlador, con sus correcciones, hasta que no quede nada crítico ni importante.
- [ ] **Step 4: Commit y PR.**

```bash
git add -A && git commit -m "docs: hito 3 exit test, conformance evidence and repo guide"
git push origin feat/hito-3
gh pr create --draft --base dev --head feat/hito-3 --body-file docs/conformance/hito-3/README.md   # si no existe; si existe, gh pr edit
```

El push va bajo la instrucción permanente del responsable de subir cada PR en cuanto existe. Unirla con `dev` es decisión del responsable.

---

## Cobertura del spec (fase 2)

| Requisito | Tarea |
|-|-|
| §3: A2A como base; el intercambio final no cambia | 13, 14 (caso 2), 15 |
| §5.1: identificadores de capacidad versionados; vocabulario publicado; espacios de nombres sin aprobación; clave multihash de la cadena canónica | 8 |
| §5.2: provider records y contactos verificables en la DHT; nada privado | 10, 12 |
| §5.3: los seis pasos; un resultado es un candidato; una búsqueda vacía no prueba nada (cobertura parcial) | 12, 13 |
| §5.4: alias locales en el buscador | 13 |
| §5.5: caducidad y renovación; secuencia y regresiones; límites de registros, consultas y tamaño por nodo | 9, 10, 12 |
| §6.1: `libp2p-kad` sin reimplementar Kademlia; comparación hecha | Fase 1, 7 |
| §6.2: protocolo propio y validadores; sin DHT pública | 8, 10, 11 (vínculo: «Cambios») |
| §6.3: modos router y cliente; varios puntos de arranque, contactos guardados e invitaciones; sin el arranque inicial | 11, 14 (casos 3 y 5) |
| §7.3: límites de la búsqueda; sin reenvío por el receptor | 13; Global Constraints |
| §8: selección en dos etapas con candidatos de la DHT | 13 |
| §10: `agentnet-discovery/0` sin cambios | 13 |
| §13: suplantación, anuncios obsoletos, consultas masivas, contenido como datos, resultados sesgados | 9, 10, 12, 13, 14 |
| §14: escenarios del hito 3 (provider record que lleva a un contacto con firma inválida, corte del arranque, caída de routers, búsquedas amplias, card alterada) | 12, 13, 14 |
| §14: criterio de salida (justificar, encontrar sin contacto previo, sin el arranque original, caducar anuncios) | Fase 1, 7, 13, 14 |
| §14: métricas | 7, 14 |
| §14: versiones fijadas, MSRV, nunca `latest` | 8, 11, Global Constraints |
| H3-D2 a H3-D5 | 14 (D2), 11 (D3), 12 y 13 (D4), 8 (D5) |
