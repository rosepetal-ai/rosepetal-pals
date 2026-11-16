# PALS hito 3: descubrimiento con Kademlia — Implementation Plan (fase 1: comparación del backend)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

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

## Después de esta fase

El controlador ratifica o cambia la recomendación (H3-D1) en el registro, decide H3-D2 y H3-D3 con sus datos, y escribe la fase 2 de este plan: las tareas 2 a 9 de la hoja de ruta, sobre el backend elegido. La fase 2 añade sus tareas a este mismo documento.
