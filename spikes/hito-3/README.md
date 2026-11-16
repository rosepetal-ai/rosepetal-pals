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

Por ahora, cada binario solo nombra un elemento de cada dependencia para enlazarlas. Las tareas 2 a 5 escriben los nodos.

## Dependencias

Todas van fijadas con `=` en [`Cargo.toml`](Cargo.toml):

| Crate | Versión | Quién la usa |
|-|-|-|
| `iroh` | `=1.4.1`, la del daemon | Todos los binarios |
| `libp2p` | `=0.57.1`, que trae `libp2p-kad 0.49.0` | `kad-libp2p`, con `kad`, `quic`, `tokio`, `macros` e `identify`; `kad-iroh`, sin transportes propios: `kad`, `tokio` y `macros` |
| `mainline` | `=8.0.1` | `mainline` |
| `pkarr` | `=9.0.0`, con sus features por defecto (`full-client`: `dht` y `relays`) | `mainline` |
| `iroh-mainline-address-lookup` | `=0.6.1` | `mainline` |

El `Cargo.lock` de aquí partió de una copia del de la raíz. Así, los crates que el daemon ya usa conservan sus versiones, y solo se resolvieron los nuevos.

`libp2p =0.57.1` e `iroh =1.4.1` se resuelven juntos y compilan en el mismo binario.

### Búsqueda de direcciones por pkarr sobre Mainline en Iroh 1.x

El plan pide añadir, si existe, un crate de Iroh 1.x que haga la búsqueda de direcciones por pkarr sobre Mainline y sea compatible con `iroh =1.4.1`. Lo que había en crates.io el 2026-11-16:

- **`iroh-mainline-address-lookup 0.6.1`**, de n0 ([n0-computer/iroh-address-lookups](https://github.com/n0-computer/iroh-address-lookups)). Es la búsqueda de direcciones por pkarr sobre Mainline de Iroh 1.x (`DhtAddressLookup`, que implementa `iroh::address_lookup::AddressLookup`). Tiene MSRV 1.91 y depende de `iroh ^1.0.0`. **Se añade**, fijado a `=0.6.1`. Ojo: no usa `mainline 8.0.1` ni `pkarr 9.0.0`. Su DHT es `n0-mainline 0.7.2`, el fork de n0 de `mainline`, y sus paquetes pkarr son los de `iroh-dns 1.4.1`. Si la tarea 5 lo usa junto a `mainline` y `pkarr`, el binario lleva dos implementaciones de Mainline.
- `iroh-mainline-endpoint-discovery 0.1.6`, también de n0. Sigue otro camino: Mainline lleva a un servidor de índice de direcciones (`udp-addr-index-proto`), que da la identidad firmada del endpoint. No depende de `iroh`, solo de `iroh-base`, así que no implementa su `AddressLookup`. No se añade.
- `iroh-pkarr-node-discovery 0.6.0` depende de `iroh-net 0.22`, de antes de Iroh 1.x. No se añade.

## Compatibilidad de dependencias: `compat.sh`

```bash
bash spikes/hito-3/compat.sh    # desde cualquier directorio
```

Necesita la toolchain de la MSRV (`rustup toolchain install 1.91`) y `python3`. Tarda varios minutos: compila cada binario en release desde cero. Para cada candidato mide:

- si `cargo +1.91 check --locked -p <paquete>` pasa y, si falla, la primera línea de error;
- cuántos crates trae: las líneas distintas de `cargo tree -p <paquete> -e normal --prefix none`, quitada la marca ` (*)` de los repetidos para que cada nombre y versión cuente una vez;
- qué crates trae en más de una versión, según `cargo tree -p <paquete> -d -e normal`. Una misma versión compilada dos veces, para el anfitrión y para el destino, no cuenta como duplicado;
- cuánto tarda `cargo build --release --locked -p <paquete>` desde cero, con su propio `CARGO_TARGET_DIR` en `target/compat/build/<id>/`;
- cuánto ocupa el binario, y cuánto más que el de `baseline`.

Escribe [`docs/decisions/hito-3-backend/compat.json`](../../docs/decisions/hito-3-backend/compat.json) con este esquema:

```json
{"date": "...", "rustc": "...", "rustc_msrv": "...", "commit": "...", "host_cpus": n,
 "candidates": {"<id>": {"msrv_1_91": "pass|fail", "msrv_error": "...", "crates": n,
   "duplicates": ["name vA, vB", ...], "release_build_s": n, "release_build_load_1m": [inicio, fin],
   "binary_bytes": n, "binary_bytes_over_baseline": n}}}
```

Notas sobre los campos:

- `msrv_error` es `null` cuando la comprobación pasa.
- `commit` es el `HEAD` del repositorio al medir. Lleva `-dirty` si lo medido (`Cargo.toml`, `Cargo.lock`, `crates/` o `spikes/hito-3/`) tenía cambios sin commit.
- `rustc_msrv`, `host_cpus` y `release_build_load_1m` no están en el esquema del plan. Los tiempos de compilación dependen de la carga del anfitrión, que suele estar muy cargado. Por eso cada tiempo va con la carga media de un minuto al empezar y al acabar, y con el número de CPU. Los tiempos solo se comparan dentro de una misma ejecución.
- Mientras los binarios solo enlazan sus dependencias, el tamaño mide poco, porque el enlazador quita el código que nadie usa. Hay que volver a ejecutar `compat.sh` cuando cada candidato tenga su nodo.

`compat.sh` deja sus compilaciones en `target/compat/`, que git ignora.

## Arnés

El arnés Docker (`harness/`) llega con la tarea 2 del plan. Monta la misma topología para todos los candidatos, con redes internas y sin DHT públicas, y la tarea 2 añadirá aquí cómo ejecutarlo.

## Comprobaciones

Dentro de `spikes/hito-3/`:

```bash
cargo build --release --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

Las comprobaciones de la raíz (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace --locked`) no ven este directorio.
