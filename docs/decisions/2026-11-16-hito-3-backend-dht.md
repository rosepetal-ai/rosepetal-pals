# Registro de decisión: backend de la DHT del hito 3

**Fecha:** 2026-11-16 · **Quién decide:** la decisión H3-D1 está delegada desde el 2026-11-16 ([registro](2026-11-16-cierre-hitos-1-y-2.md#decisiones-del-hito-3)). Este registro lo escribe el agente que hizo la comparación, en la tarea 6 de la [fase 1 del plan del hito 3](../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). Recomienda, pero no decide: el controlador ratifica o cambia la recomendación. · **Afecta a:** la fase 2 del plan del hito 3, es decir, las tareas 2 a 9 de la [hoja de ruta](../plans/2026-11-14-hoja-de-ruta-hitos-2-4.md#hito-3-descubrimiento-con-kademlia).

Es el registro de la comparación acotada del backend, la tarea 1 del hito 3 en la hoja de ruta. Tiene los apartados que pide la hoja de ruta: candidatos y versiones, pruebas y la duda de cada una, montaje, métricas y lo no medido, el resultado justificado frente al criterio de salida y qué lo haría cambiar. Cierra, como en el [registro anterior](2026-11-16-cierre-hitos-1-y-2.md), con qué cuesta si la decisión es errónea.

**De dónde salen las cifras.** Cada cifra sale de un JSON versionado de [`hito-3-backend/`](hito-3-backend/) ([`compat.json`](hito-3-backend/compat.json) o [`results/*.json`](hito-3-backend/results/)) o de un documento que dice de dónde la saca: los de cada candidato ([`kad-libp2p.md`](hito-3-backend/kad-libp2p.md), [`kad-iroh.md`](hito-3-backend/kad-iroh.md) y [`mainline.md`](hito-3-backend/mainline.md)) o este mismo. Junto a cada una se dice cuál. Las de los resultados son de **una sola ejecución del arnés por candidato**. Donde se sabe que una cifra varía entre ejecuciones, se dice.

## Decisión

**Ratificada el 2026-11-16 (H3-D1): `kad-iroh`.** La ratifica el agente que dirige el hito 3, con la delegación del [registro de cierre de los hitos 1 y 2](2026-11-16-cierre-hitos-1-y-2.md#decisiones-del-hito-3). Es una elección deliberada, no la regla de H3-D1 aplicada: tres revisiones del registro coinciden en que la regla, leída al pie de la letra, deja `kad-libp2p`, y en que `kad-iroh` solo se sostiene como un cambio respecto al spec decidido a sabiendas. Se ratifica así por estos motivos:

- **Una sola identidad en toda la red.** El `PeerId` es la clave del agente, la misma que autentica Iroh y que nombran los grants de los hitos 1 y 2. Un router puede admitir, limitar y cortar por agente, con la misma clave y antes de leer un mensaje de Kademlia. Son las cuotas por par que ya tiene `agentnet-discovery/0` y las políticas de admisión que el §5.5 y el §13 piden contra identidades falsas en masa. Con `kad-libp2p`, el `PeerId` de quien solo busca no lleva a ningún agente, así que una admisión por agente pediría que también los clientes publicaran su vínculo.
- **Lo que se ahorra**, ya dicho abajo: un esquema de vínculo nuevo y congelado, una segunda pila con su puerto en cada router y su peso medido, y un 12 % de los bytes por búsqueda.
- **Lo que se acepta a cambio:**
  - el adaptador de 524 líneas;
  - más tráfico en reposo, del que una parte son marcaciones dobles con causa conocida;
  - y que los routers sepan qué agente busca. Es la otra cara de la admisión por agente, y H3-D4 lo trata.

**Condiciones.** La fase 2 empieza por ellas, antes de congelar `agentnet-kad/0`, las claves de la DHT y cómo sale el `PeerId` de la clave del agente:

1. **Repetir las medidas de la fase 1.** Se ejecuta el arnés de `spikes/hito-3/` tres veces más con `kad-iroh` y tres más con `kad-libp2p`, con los mismos parámetros. Además, una medida de S6 de cada uno con intervalos de republicación de producción y con `kad-iroh` marcando una sola vez a cada par.
2. **Cuándo se cambia a `kad-libp2p`.** Se cambia antes de congelar nada, con un registro nuevo, si se da alguna de estas condiciones:
   - `kad-iroh` queda en `fail` en S3 o en S7 en alguna de sus ejecuciones y `kad-libp2p` en ninguna;
   - con intervalos de producción, el tráfico en reposo de un router de `kad-iroh` pasa del doble del de `kad-libp2p`.
3. **Las versiones fijadas.** El adaptador va en su propio módulo, con las pruebas de transporte de la prueba como puerta, y `libp2p` e `iroh` fijados con `=`. Si una versión que haya que adoptar lo rompe y arreglarlo cuesta más que la segunda pila, se vuelve a decidir.
4. **El cambio respecto al spec.** El plan de la fase 2 lo anota en su tabla de cambios: el §6.2 sin vínculo de dos identidades, el contacto del §5.2 y el adaptador que el §6.1 no presuponía.

**Veredicto de la repetición (tarea 7 de la fase 2): sigue `kad-iroh`.** No se da ninguna condición de cambio: ningún `fail` en S3 ni S7 en las cuatro ejecuciones de cada candidato, y en reposo con intervalos de producción un router de `kad-iroh` está a 1,34 veces uno de `kad-libp2p`, aunque esa medida no marcó del todo una sola vez a cada par. Los apretones abandonados solo suman bytes, así que 1,34 es una cota superior de la cifra que pide la condición ([repetición](hito-3-backend/repeticion.md)).

**Coste si es errónea**, además del que dice [Coste si es errónea](#coste-si-es-errónea): si la red ya está desplegada cuando se descubre, cambiar de backend obliga a arrancar una DHT nueva.

Lo que sigue es la recomendación tal como la escribió la tarea 6, con su razonamiento.

**Recomendación: Iroh e iroh-blobs más `libp2p-kad`, con `libp2p-kad` sobre las conexiones Iroh del propio daemon (el candidato `kad-iroh`).** Es un módulo de `rust-libp2p` para Kademlia dentro del daemon, con Iroh para consultas, tareas y ficheros, como pide la decisión provisional del spec (§6.1). Pero no es esa decisión al pie de la letra: el §6.1 y el §6.2 describen `kad-libp2p`, con su propia pila y dos identidades vinculadas.

En pocas líneas:

- `mainline` queda descartado: no caduca anuncios (S3 en `fail`), una parte del criterio de salida, y eso basta. S2 también queda en `fail`, pero en esta topología, con routers en dos redes sin ruta entre ellas: Mainline da una sola dirección a cada nodo, y un router solo sirve a una de sus dos redes.
- `libp2p-total` no se descarta, pero cambiaría el transporte de tareas y de ficheros, y lo que aporta a la DHT ya lo dan los dos candidatos de Kademlia.
- `kad-libp2p` y `kad-iroh` pasan los mismos escenarios, y ningún coste medido descarta a ninguno. En los recursos medidos se reparten: la memoria en reposo va a favor de `kad-iroh`; las conexiones abiertas de los routers y el tráfico en reposo, a favor de `kad-libp2p`.
- **Recomendar `kad-iroh` es un juicio, no la regla de H3-D1 aplicada.** Esa regla («salvo un coste medido que la descarte») deja `kad-libp2p`. El juicio se apoya en lo que `kad-iroh` se ahorra:
  - un esquema de vínculo nuevo y congelado, con sus vectores (la tarea 3 de la hoja de ruta);
  - una segunda pila de red, con su peso medido (33 crates más, un binario 6,3 MB mayor y unos 2 MiB más de memoria por nodo) y un segundo puerto UDP, con sus conexiones, que abrir y operar en cada router;
  - un 12 % de los bytes de cada búsqueda.

  En contra pesan, medidos, contados o leídos en el código: un adaptador de 524 líneas que mantener frente a las versiones de libp2p y de Iroh, más tráfico en reposo y más conexiones abiertas en los routers, y que los routers sepan la clave de agente de quien busca.

  Atravesar NAT y una segunda clave no cuentan como motivos. Con el modo por defecto que se recomienda (H3-D3), los clientes solo marcan hacia fuera, así que ninguno de los dos candidatos necesita relays ni atravesar NAT para ellos, y los routers tienen que ser alcanzables directamente en los dos. Además, el adaptador de `kad-iroh` solo marca direcciones IP, así que solo llega a los relays de Iroh por la búsqueda de direcciones de n0, que el daemon enciende con `relay = true`. Llegar a ellos sin depender de n0 pediría una forma de dirección nueva, que queda para la fase 2. Y la clave libp2p se puede derivar de la semilla del agente, como hace la prueba, sin guardar nada más. Sin esos dos motivos, el margen del juicio es estrecho.
- **Ratificarlo es aprobar un cambio respecto al spec.** Desaparece el vínculo de dos identidades del §6.2. Y el contacto del §5.2, «relación verificable entre identidad libp2p, agente y endpoint Iroh», pasa a ser el `agentnet.contact/1` tal cual, porque la identidad libp2p es la del agente.

El razonamiento completo está en [Aplicación de las reglas](#aplicación-de-las-reglas-de-la-hoja-de-ruta) y en [Recomendación](#recomendación-frente-al-criterio-de-salida). Lo que hereda la fase 2 (API, lo que el código de la prueba no resuelve, dependencias, vínculo de identidades y modo por defecto) está en [Lo que hereda la fase 2](#lo-que-hereda-la-fase-2).

## Candidatos y versiones

Las versiones son las del [`Cargo.lock`](../../spikes/hito-3/Cargo.lock) de `spikes/hito-3/`, todas fijadas con `=` en su [`Cargo.toml`](../../spikes/hito-3/Cargo.toml). Las líneas de código son las de `src/`, sin pruebas, según el documento de cada candidato: todas, y entre paréntesis sin comentarios ni líneas vacías.

| Id | Qué es | Dependencias del backend | Código |
|-|-|-|-|
| `baseline` | Línea base: solo contactos, como el hito 2. Lleva un endpoint de Iroh sin uso, para que su coste en reposo sea la referencia | `iroh 1.4.1` | — |
| `kad-libp2p` | Iroh más `libp2p-kad` en su propia pila libp2p (QUIC en otro puerto UDP), con un vínculo de identidades `agentnet.spike-kadlink/0` firmado por las dos claves (§6.2) | `iroh 1.4.1`; `libp2p 0.57.1` con `kad`, `quic`, `tokio`, `macros`, `identify` y `ed25519`, que trae `libp2p-kad 0.49.0`, `libp2p-quic 0.14.0`, `libp2p-identify 0.48.0`, `libp2p-swarm 0.48.0` y `libp2p-identity 0.3.1` | 1992 (1547) |
| `kad-iroh` | `libp2p-kad` sobre un transporte de libp2p hecho con conexiones Iroh, con el ALPN `agentnet-kad/0`: una sola pila, un solo puerto y el `PeerId` igual a la clave del agente | `iroh 1.4.1`; `libp2p 0.57.1` sin transportes, con `kad`, `tokio`, `macros`, `identify` y `ed25519`, que trae `libp2p-kad 0.49.0`, `libp2p-identify 0.48.0`, `libp2p-swarm 0.48.0`, `libp2p-core 0.44.0` y `libp2p-identity 0.3.1` | 2304 (1756), de las que el adaptador son 524 (390) |
| `mainline` | Iroh más una DHT Mainline privada (BEP 5 y BEP 44), con un índice de capacidades de *signed peers* y el contacto y el anuncio en items BEP 44 | `iroh 1.4.1`; `n0-mainline 0.7.2` con `unstable_signed_peers` | 1255 (901), de las que el índice y su validación son 692 (468) |
| `libp2p-total` | Toda la red en libp2p | Nada: se evalúa [sobre el papel](#libp2p-total-evaluación-sobre-el-papel) | — |

Compilador: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` y, para la MSRV, `rustc 1.91.1 (ed61e7d7e 2025-11-07)` ([`compat.json`](hito-3-backend/compat.json)).

`mainline` empezó con dos pilas Mainline en el `Cargo.toml` de la tarea 1: `mainline 8.0.1` con `pkarr 9.0.0`, y la de n0 con `iroh-mainline-address-lookup 0.6.1`. La tarea 5 se quedó con `n0-mainline` y quitó las demás. El porqué está en [`mainline.md`](hito-3-backend/mainline.md#la-pila-mainline).

## Las pruebas y la duda de cada una

La comparación no implementa tres sistemas completos (§6.1). Cada candidato es un binario de prueba pequeño sobre el mismo [contrato de nodo](../../spikes/hito-3/README.md#el-contrato-de-nodo), y los registros que viajan son los `agentnet.contact/1` y `agentnet.announce/1` de `pals-core`, sin cambios. Ninguno es código del producto.

| Candidato | Duda que resuelve | Respuesta |
|-|-|-|
| `baseline` | Que el arnés funciona, y cuánto cuesta en reposo un nodo con Iroh y sin DHT | No encuentra a nadie sin contacto previo, como debe: S1 en `by-design`, S2 a S5 y S7 en `not-applicable` con su motivo |
| `kad-libp2p` | Si las dos pilas conviven en un nodo, con dos identidades vinculadas (§6.2), y qué cuesta | Sí, con cinco ajustes de `libp2p-kad` y libp2p para una red privada con dos subredes. Los routers validan el vínculo ([`kad-libp2p.md`](hito-3-backend/kad-libp2p.md#la-duda-que-resuelve)) |
| `kad-iroh` | Si `libp2p-kad` puede ir sobre las conexiones Iroh del daemon, sin reimplementar Kademlia (§6.3) ni vínculo de identidades (§6.2), y qué cuesta | Sí, con un adaptador de 524 líneas y un ajuste más del plazo del anuncio. No hay transporte publicado que sirva ([`kad-iroh.md`](hito-3-backend/kad-iroh.md#paso-1-un-transporte-ya-publicado)) |
| `mainline` | Si Mainline puede dar un índice de capacidades con identidades verificables y caducidad, o qué haría falta definir (§6.1) | Sirve para buscar, pero no caduca nada y un router en dos redes solo sirve a una ([`mainline.md`](hito-3-backend/mainline.md#la-duda-que-resuelve)) |
| `libp2p-total` | Qué costaría replantear el transporte de tareas y de ficheros de los hitos 1 y 2 | [Evaluación sobre el papel](#libp2p-total-evaluación-sobre-el-papel) |

## Montaje

**Topología** ([`params.env`](../../spikes/hito-3/harness/params.env), copiado en el `params` de cada JSON de resultados). Dos redes Docker `--internal`, `10.213.71.0/24` (A) y `10.213.72.0/24` (B), y 12 contenedores, uno por nodo, con `--cpus 1` y `--memory 512m`:

- 4 routers en las dos redes y en modo servidor de la DHT: `r1` y `r2` del operador 1, `r3` y `r4` del operador 2;
- 4 proveedores en modo cliente: `p1` y `p2` en la A, `p3` y `p4` en la B;
- 4 clientes en modo cliente: `c1` y `c2` en la A, `c3` y `c4` en la B.

Todos arrancan con `r1` como único punto de arranque, y los clientes tienen además `r3` como contacto guardado. Tres nodos más arrancan solo para su escenario: `c0` (S0), `c5` (S2) y `rogue` (S5). Cuatro capacidades del §5.1, cada una con un proveedor en cada red. Los escenarios S0 a S7 están en el [README de las pruebas](../../spikes/hito-3/README.md#cómo-se-ejecuta-cada-escenario).

**Parámetros**, los mismos para todos (`check_results.py` rechaza ficheros con `params` distintos): `TTL_S` = 120 s de vida en la DHT, `REPUBLISH_S` = 40 s, `IDLE_S` = 300 s de reposo en S6, `SEARCHES` = 10 búsquedas por cliente y capacidad en S1 y `SEARCH_LIMIT` = 8 candidatos. Los registros de `pals-core` conservan su vida de 24 h, y la DHT aplica la suya.

**Anfitrión.** Uno solo, Linux, de 32 CPU, compartido con otros trabajos y muy cargado. Cada candidato se midió una vez:

| Candidato | JSON de resultados | Commit medido | Fecha (UTC) | Duración y carga media al empezar |
|-|-|-|-|-|
| `baseline` | [`results/baseline.json`](hito-3-backend/results/baseline.json) | `6aaa318` | 2026-11-16 03:30 | 6 min 3 s; 16,3 |
| `kad-libp2p` | [`results/kad-libp2p.json`](hito-3-backend/results/kad-libp2p.json) | `420a7e1` | 2026-11-16 09:25 | 13 min 43 s; 22,6 |
| `kad-iroh` | [`results/kad-iroh.json`](hito-3-backend/results/kad-iroh.json) | `c954836` | 2026-11-16 10:31 | 12 min 56 s; 25,0 |
| `mainline` | [`results/mainline.json`](hito-3-backend/results/mainline.json) | `970f71a` | 2026-11-16 12:26 | 15 min 10 s; 16,8 |

El commit y la fecha son los del JSON. La duración y la carga salen del `harness.log` de cada ejecución, que git ignora: las de los candidatos las da su documento, y la de la línea base se leyó para este registro. `mainline` se midió dos veces: la primera ejecución, sobre `37ada0e`, quedó sustituida por la de `970f71a`, tras la primera revisión. El commit posterior a la ejecución de `kad-iroh` (`e17558f`, su primera revisión) solo tocó un camino del transporte que el arnés no ejerce, y la ejecución no se repitió ([`kad-iroh.md`](hito-3-backend/kad-iroh.md#resultados)).

**Compatibilidad.** [`compat.json`](hito-3-backend/compat.json) es de `compat.sh --repeat 3` sobre el árbol limpio del commit `3a5693a`, con los cuatro binarios ya escritos: `cargo +1.91 check`, `cargo tree` y tres compilaciones release desde cero de cada uno, en un orden que rota en cada vuelta. El JSON lleva ese commit sin `-dirty` y no lleva `provisional`, así que los tamaños de binario son reales. Según la salida de la ejecución, que no se versiona, empezó a las 12:35:45 UTC con una carga media de un minuto de 18,6 y terminó a las 12:51:23 con 32,2: 15 min 34 s. Cada compilación tiene en `release_build_load_1m` la carga al empezar y al acabar, de 20,4 a 35,0. Ningún `msrv_error` lleva texto, así que en el JSON no hay rutas del anfitrión, y el filtro de rutas de `compat.sh` no necesitó cambios.

## Métricas

La tabla es la que imprime `summarize.py`, pegada tal cual:

```bash
python3 spikes/hito-3/harness/summarize.py --compat docs/decisions/hito-3-backend/compat.json docs/decisions/hito-3-backend/results/*.json
```

Una columna por candidato, en el orden de los ficheros. Cada celda es la cifra con el estado de su escenario entre paréntesis, y `—` donde no hay cifra.

| Métrica | `baseline` | `kad-iroh` | `kad-libp2p` | `mainline` |
|-|-|-|-|-|
| MSRV 1.91 (`cargo +1.91 check`) | pass | pass | pass | pass |
| Crates | 300 | 337 | 370 | 306 |
| Crates en más de una versión | 8 | 8 | 12 | 9 |
| Compilación release desde cero (s) | 68.7 | 62.9 | 74.8 | 65.3 |
| Binario (bytes) | 25863520 | 29270056 | 35567696 | 27487136 |
| Binario sobre `baseline` (bytes) | 0 | 3406536 | 9704176 | 1623616 |
| **S0** estado | pass | pass | pass | pass |
| S0 `searches` | 4 (pass) | 4 (pass) | 4 (pass) | 4 (pass) |
| S0 `candidates` | 0 (pass) | 0 (pass) | 0 (pass) | 0 (pass) |
| S0 `backend_errors` | 0 (pass) | 0 (pass) | 0 (pass) | 0 (pass) |
| S0 `routing_peers` | 0 (pass) | 0 (pass) | 0 (pass) | 0 (pass) |
| S0 `connections` | 0 (pass) | 0 (pass) | 0 (pass) | 0 (pass) |
| **S1** estado | by-design | pass | pass | pass |
| S1 `searches` | 160 (by-design) | 160 (pass) | 160 (pass) | 160 (pass) |
| S1 `with_valid_pct` | 0 (by-design) | 100 (pass) | 100 (pass) | 100 (pass) |
| S1 `complete_pct` | 0 (by-design) | 100 (pass) | 100 (pass) | 100 (pass) |
| S1 `first_valid_ms_p50` | — (by-design) | 2 (pass) | 2 (pass) | 2 (pass) |
| S1 `first_valid_ms_p95` | — (by-design) | 5 (pass) | 5 (pass) | 2 (pass) |
| S1 `first_valid_backend_pct` | — (by-design) | 100 (pass) | 100 (pass) | 100 (pass) |
| S1 `elapsed_ms_p50` | 0 (by-design) | 5 (pass) | 7 (pass) | 3 (pass) |
| S1 `elapsed_ms_p95` | 0 (by-design) | 10 (pass) | 13 (pass) | 1240 (pass) |
| S1 `requests_p50` | 2 (by-design) | 20 (pass) | 20 (pass) | — (pass) |
| S1 `requests_p95` | 2 (by-design) | 20 (pass) | 20 (pass) | — (pass) |
| S1 `candidates_p50` | 0 (by-design) | 2 (pass) | 2 (pass) | 2 (pass) |
| S1 `candidates_max` | 0 (by-design) | 2 (pass) | 2 (pass) | 2 (pass) |
| S1 `bytes_raw_p50` | 2186 (by-design) | 27847 (pass) | 31603 (pass) | 13922 (pass) |
| S1 `bytes_p50` | 2186 (by-design) | 27841 (pass) | 31599 (pass) | 13922 (pass) |
| S1 `bytes_p95` | 2186 (by-design) | 28183 (pass) | 32195 (pass) | 15383 (pass) |
| S1 `backend_errors` | 0 (by-design) | 0 (pass) | 0 (pass) | 0 (pass) |
| **S2** estado | not-applicable | pass | pass | fail |
| S2 `connected_complete_pct` | — (not-applicable) | 100 (pass) | 100 (pass) | 0 (fail) |
| S2 `connected_first_valid_ms_p50` | — (not-applicable) | 2 (pass) | 2 (pass) | 1 (fail) |
| S2 `connected_first_valid_ms_p95` | — (not-applicable) | 11 (pass) | 16 (pass) | 1 (fail) |
| S2 `connected_first_valid_backend_pct` | — (not-applicable) | 100 (pass) | — (pass) | 100 (fail) |
| S2 `connected_elapsed_ms_p50` | — (not-applicable) | 5001 (pass) | 5000 (pass) | 914 (fail) |
| S2 `connected_elapsed_ms_p95` | — (not-applicable) | 10013 (pass) | 10000 (pass) | 1252 (fail) |
| S2 `newcomer_first_valid_s` | — (not-applicable) | 16 (pass) | 15.2 (pass) | 5.8 (fail) |
| S2 `newcomer_complete` | — (not-applicable) | sí (pass) | sí (pass) | no (fail) |
| S2 `newcomer_searches` | — (not-applicable) | 4 (pass) | 4 (pass) | 80 (fail) |
| **S3** estado | not-applicable | pass | pass | fail |
| S3 `stored_before` | — (not-applicable) | 12 (pass) | 16 (pass) | 4 (fail) |
| S3 `routers_forget_s` | — (not-applicable) | 106.6 (pass) | 111.4 (pass) | — (fail) |
| S3 `searches_forget_s` | — (not-applicable) | 99.3 (pass) | 107.9 (pass) | — (fail) |
| S3 `limit_s` | — (not-applicable) | 160 (pass) | 160 (pass) | 160 (fail) |
| S3 `polls` | — (not-applicable) | 28 (pass) | 29 (pass) | 34 (fail) |
| **S4** estado | not-applicable | pass | pass | pass |
| S4 `immediate_coverage` | — (not-applicable) | 1 (pass) | 1 (pass) | 1 (pass) |
| S4 `full_coverage_s` | — (not-applicable) | 0 (pass) | 0 (pass) | 0 (pass) |
| S4 `degraded_first_valid_ms_p50` | — (not-applicable) | 3 (pass) | 2 (pass) | 2 (pass) |
| S4 `degraded_first_valid_ms_p95` | — (not-applicable) | 7 (pass) | 6 (pass) | 4 (pass) |
| S4 `degraded_first_valid_backend_pct` | — (not-applicable) | 100 (pass) | — (pass) | 100 (pass) |
| S4 `degraded_elapsed_ms_p50` | — (not-applicable) | 5001 (pass) | 5001 (pass) | 905 (pass) |
| S4 `degraded_elapsed_ms_p95` | — (not-applicable) | 5006 (pass) | 8361 (pass) | 1252 (pass) |
| S4 `rounds` | — (not-applicable) | 1 (pass) | 1 (pass) | 1 (pass) |
| S4 `limit_s` | — (not-applicable) | 80 (pass) | 80 (pass) | 80 (pass) |
| **S5** estado | not-applicable | pass | pass | pass |
| S5 `routers_rejecting` | — (not-applicable) | 4 (pass) | 4 (pass) | 3 (pass) |
| S5 `records_rejected_delta` | — (not-applicable) | 4 (pass) | 4 (pass) | 6 (pass) |
| S5 `routers_storing_rogue_contact` | — (not-applicable) | 0 (pass) | 0 (pass) | 0 (pass) |
| S5 `records_stored_unexplained_max` | — (not-applicable) | 0 (pass) | 0 (pass) | 0 (pass) |
| S5 `clients_rogue_valid` | — (not-applicable) | 0 (pass) | 0 (pass) | 0 (pass) |
| S5 `clients_rogue_returned` | — (not-applicable) | 4 (pass) | 4 (pass) | 4 (pass) |
| S5 `searches` | — (not-applicable) | 4 (pass) | 4 (pass) | 4 (pass) |
| **S6** estado | measured | measured | measured | measured |
| S6 `router_rss_kib_p50` | 14396 (measured) | 21128 (measured) | 23240 (measured) | 16040 (measured) |
| S6 `router_rss_kib_max` | 14864 (measured) | 23540 (measured) | 25420 (measured) | 16116 (measured) |
| S6 `router_connections_p50` | 0 (measured) | 1 (measured) | 0 (measured) | 0 (measured) |
| S6 `router_connections_max` | 0 (measured) | 4 (measured) | 1 (measured) | 0 (measured) |
| S6 `router_routing_peers_p50` | 1 (measured) | 3 (measured) | 3 (measured) | 2 (measured) |
| S6 `router_routing_peers_max` | 1 (measured) | 3 (measured) | 3 (measured) | 2 (measured) |
| S6 `router_bytes_per_min_p50` | 444 (measured) | 355418 (measured) | 249735 (measured) | 10259 (measured) |
| S6 `router_bytes_per_min_max` | 444 (measured) | 402601 (measured) | 271964 (measured) | 23935 (measured) |
| S6 `router_sent_bytes_per_min_p50` | 0 (measured) | 174690 (measured) | 126338 (measured) | 2607 (measured) |
| S6 `router_sent_bytes_per_min_max` | 0 (measured) | 202527 (measured) | 139238 (measured) | 8340 (measured) |
| S6 `provider_rss_kib_p50` | 14612 (measured) | 20372 (measured) | 22764 (measured) | 15264 (measured) |
| S6 `provider_rss_kib_max` | 14688 (measured) | 20468 (measured) | 22912 (measured) | 15652 (measured) |
| S6 `provider_connections_p50` | 0 (measured) | 0 (measured) | 0 (measured) | 0 (measured) |
| S6 `provider_connections_max` | 0 (measured) | 4 (measured) | 4 (measured) | 0 (measured) |
| S6 `provider_routing_peers_p50` | 1 (measured) | 4 (measured) | 4 (measured) | 2 (measured) |
| S6 `provider_routing_peers_max` | 1 (measured) | 4 (measured) | 4 (measured) | 7 (measured) |
| S6 `provider_bytes_per_min_p50` | 222 (measured) | 218700 (measured) | 180386 (measured) | 5963 (measured) |
| S6 `provider_bytes_per_min_max` | 222 (measured) | 243477 (measured) | 199567 (measured) | 19301 (measured) |
| S6 `provider_sent_bytes_per_min_p50` | 0 (measured) | 112145 (measured) | 88376 (measured) | 3962 (measured) |
| S6 `provider_sent_bytes_per_min_max` | 0 (measured) | 123952 (measured) | 97707 (measured) | 13980 (measured) |
| S6 `client_rss_kib_p50` | 14824 (measured) | 20400 (measured) | 22748 (measured) | 15596 (measured) |
| S6 `client_rss_kib_max` | 14852 (measured) | 20556 (measured) | 24916 (measured) | 15856 (measured) |
| S6 `client_connections_p50` | 0 (measured) | 0 (measured) | 0 (measured) | 0 (measured) |
| S6 `client_connections_max` | 0 (measured) | 0 (measured) | 0 (measured) | 0 (measured) |
| S6 `client_routing_peers_p50` | 2 (measured) | 4 (measured) | 4 (measured) | 3 (measured) |
| S6 `client_routing_peers_max` | 2 (measured) | 4 (measured) | 4 (measured) | 7 (measured) |
| S6 `client_bytes_per_min_p50` | 222 (measured) | 27142 (measured) | 18262 (measured) | 361 (measured) |
| S6 `client_bytes_per_min_max` | 222 (measured) | 28874 (measured) | 20406 (measured) | 818 (measured) |
| S6 `client_sent_bytes_per_min_p50` | 0 (measured) | 14162 (measured) | 8997 (measured) | 172 (measured) |
| S6 `client_sent_bytes_per_min_max` | 0 (measured) | 14891 (measured) | 9551 (measured) | 390 (measured) |
| **S7** estado | not-applicable | pass | pass | pass |
| S7 `clients_resolved` | — (not-applicable) | 4 (pass) | 4 (pass) | 4 (pass) |
| S7 `resolved_s_p50` | — (not-applicable) | 1.5 (pass) | 1.9 (pass) | 6.6 (pass) |
| S7 `resolved_s_max` | — (not-applicable) | 1.8 (pass) | 2.1 (pass) | 6.8 (pass) |
| S7 `limit_s` | — (not-applicable) | 80 (pass) | 80 (pass) | 80 (pass) |
| S7 `searches` | — (not-applicable) | 4 (pass) | 4 (pass) | 4 (pass) |

**Cómo leerla:**

- **Una ejecución por candidato.** Las filas de S0 a S7 son de una sola ejecución del arnés. Las de compilación son el mínimo de tres muestras, y solo se comparan dentro de `compat.json`: dependen de la carga del anfitrión, que va junto a cada muestra en `release_build_load_1m`.
- **Tamaño del binario sobre la línea base.** `binary_bytes_over_baseline` no incluye la pila de Iroh, porque la línea base ya enlaza un endpoint de Iroh sin uso. La cifra de `kad-libp2p` es libp2p, con su QUIC, encima de Iroh; la de `kad-iroh`, libp2p sin transportes más el adaptador; la de `mainline`, `n0-mainline` y el índice.
- **`first_valid_backend_pct` de `kad-libp2p` en S2 y S4.** Su JSON es anterior a los campos `connected_first_valid_backend_pct` y `degraded_first_valid_backend_pct`, y por eso esas celdas dicen `—`. En la revisión de la tarea 3 se comprobó sobre los registros en bruto de su ejecución, que git ignora, que el 100 % de sus `first_valid` de S2 y S4 los fechó el backend. Es una comprobación sobre esos registros, no un campo del JSON.
- **S2 y S4 en los dos candidatos de Kademlia.** El primer candidato válido llega en milisegundos, pero la búsqueda no termina hasta el plazo de marcado, 5 s, mientras haya un router caído entre los más cercanos a la clave. Es de `libp2p-kad`, que no saca de la tabla de rutas a un router caído mientras su cubo no está lleno ([`kad-libp2p.md`](hito-3-backend/kad-libp2p.md#direcciones-de-los-routers)).
- **`newcomer_first_valid_s` de S2** cuenta desde que arranca el contenedor de `c5` hasta que **termina** su primera búsqueda con un candidato válido, no hasta que llega el candidato (`evaluate.py`). Según los `raw/S2.jsonl` de cada ejecución, que git ignora, el candidato llegó a los 12,2 s en `kad-iroh`, a los 10,4 s en `kad-libp2p` y a los 5,2 s en `mainline`. En los dos de Kademlia, el resto es la espera de esa búsqueda al `r1` parado. Afecta igual a los dos finalistas.
- **S3 de `kad-iroh`: 12 registros antes, no 16.** `r1`, arrancado de nuevo al final de S2, todavía no guardaba nada al empezar S3 ([`kad-iroh.md`](hito-3-backend/kad-iroh.md#resultados)). No cambia ningún estado.
- **S6, tráfico en reposo,** medido con la republicación de la prueba, cada 40 s. Con intervalos de producción, de horas, bajaría en proporción, pero eso no se ha medido. Los bytes son los de la interfaz del contenedor: Iroh y la DHT juntos.
- **S7** mide cuánto tardan los clientes en encontrar a un proveedor que ha cambiado de red y **se ha vuelto a anunciar de forma explícita** (`POST /announce`). No mide cuánto tarda el proveedor en darse cuenta de que ha cambiado de red.
- **`requests` de `mainline` es `null`:** `n0-mainline` no cuenta las peticiones de una consulta.

## Lo no medido

Lo que dicen los documentos de los candidatos, reunido. Nada de esto se midió en ningún candidato, salvo donde se dice:

- **Variación entre ejecuciones.** Cada candidato tiene una sola ejecución que cuenta. En `kad-iroh` se sabe que varía:
  - S7 tardó unos 34 s en una ejecución de prueba sin commit (el anuncio, 32,5 s), frente a 1,5 s (p50) en la que cuenta, donde `p3` se anunció a los 1,3 s del cambio;
  - S3 empezó con 12 registros de `p1` en los routers en la ejecución que cuenta, donde `kad-libp2p` tenía 16, porque el router vuelto a arrancar tardó más en recibirlos.

  [`kad-iroh.md`](hito-3-backend/kad-iroh.md#resultados) apunta como posible contribución, sin comprobarla, los plazos de par muerto de Iroh (15 s por ruta y 30 s por conexión, frente a 10 s de `libp2p-quic`). No se repitió ninguna ejecución para esta tarea.
- **Escala.** Con 4 routers, menos que `K` = 20, todos están entre los más cercanos a cualquier clave y guardan todo. No se ejercita el encaminamiento por distancia, la distribución de claves ni las rutas disjuntas.
- **Abuso.** No se prueban identidades falsas en masa (Sybil), inundaciones de provider records o de *signed peers*, una clave popular saturada ni un par que abre muchas conexiones o streams. El único registro inválido es el contacto de firma alterada de S5. Además, la política de los routers en los dos candidatos de Kademlia es concreta: guardan el anuncio y el provider record de `rogue`, que son válidos por sí mismos, y solo rechazan su contacto, que no pasa la comprobación de identidad. Los clientes lo marcan como inválido (`bad_contact`). Es lo que S5 comprueba, no algo más fuerte. En `mainline` pasa lo mismo con su anuncio y su *signed peer*.
- **El gancho del router de `mainline` como diseño.** Sirve para medir, pero no es un diseño de producto. Su conjunto de objetivos que se pregunta a sí mismo crece sin límite: cualquier `put` o `announce_signed_peer` que el gancho pueda leer añade uno, aunque no lleve firma válida, y su identificador de `u16` da la vuelta pasados 65 536 objetivos. Además, el gancho y la biblioteca podrían descodificar el mismo datagrama de forma distinta (el gancho lee un `HashMap` del diccionario bencode, donde gana la última clave repetida, y la biblioteca usa `serde` derive), y no se probó.
- **NAT y relays.** Las redes son privadas y sin NAT, los relays de Iroh están apagados y libp2p no lleva relay, AutoNAT ni DCUtR. Además, el adaptador de `kad-iroh` solo marca direcciones IP, así que, con los relays encendidos, solo llegaría a ellos por la búsqueda de direcciones de n0 del preset `N0`, que Iroh lanza al conectar con un par al que no tiene camino elegido ([Lo que hereda la fase 2](#lo-que-hereda-la-fase-2)).
- **El contacto en uso.** Nadie conecta al endpoint de Iroh que nombra el contacto: S7 comprueba que el cliente recibe la dirección nueva, no que conecte con ella.
- **La convivencia con el daemon.** `kad-iroh` sirve solo `agentnet-kad/0`. No se mide el endpoint compartido con A2A, blobs y descubrimiento, ni las cuotas que harían falta en ese ALPN.
- **Intervalos de producción.** Todo se mide con `TTL_S` = 120 s y `REPUBLISH_S` = 40 s.
- **De dónde sale el tráfico en reposo.** No se separan los bytes de Kademlia, de `identify` y de Iroh.
- **En `kad-iroh`:** si hacen falta `identify` y los dos últimos ajustes de `kad-libp2p` sobre Iroh.
- **En `kad-libp2p`:** generar la clave libp2p y guardarla aparte. La prueba la deriva de la semilla del agente, con separación de dominio, y el producto podría hacer lo mismo.
- **En `mainline`:** las peticiones por búsqueda; lo que la biblioteca rechaza después del gancho; el paso de los clientes a modo servidor, que no ocurre solo por un error de la 0.7.2 y que el arnés no ve, porque ningún nodo vive 15 minutos seguidos.
- **Filtraciones de metadatos.** Lo que ve un router sale de leer el protocolo y el código de cada candidato (el apartado «Metadatos» de cada documento), no de un observador instrumentado.
- **`libp2p-total`:** nada; es una evaluación sobre el papel.

## `libp2p-total`: evaluación sobre el papel

Concentrar la red en libp2p (§6.1) quitaría Iroh e iroh-blobs del daemon. Su Kademlia sería el de `kad-libp2p`, sin la pila de Iroh: las mismas API y los mismos ajustes, y presumiblemente el mismo comportamiento en S1 a S5 y S7. No se midió nada de esto, y en particular no se midieron la memoria ni el tráfico en reposo de S6 sin la pila de Iroh. Lo que cambia es todo lo demás. Estas son las partes de `crates/pals` que dependen de Iroh y de iroh-blobs, con sus líneas en el commit `3a5693a` (las mismas que en `dev`, porque esta rama no toca `crates/`), y lo que haría falta para cada una sobre libp2p:

| Parte | Dónde depende de Iroh o de iroh-blobs | Qué haría falta sobre libp2p |
|-|-|-|
| **Túnel A2A** (binding `http-over-iroh/v1`) | `net/mod.rs` (116 líneas): el ALPN `agentnet-a2a-http/1` (línea 28), el `AgentId` como `EndpointId` (42 a 51) y `peer_addr` con un `EndpointTicket` (60 a 78). `net/tunnel_client.rs` (184): `PeerHttp`, con `Endpoint::connect` (125) y una conexión guardada por `EndpointId`. `net/tunnel_server.rs` (173): un `ProtocolHandler` que acepta cada stream (64 a 97) y pone `x-agentnet-peer` con el id autenticado por Iroh. `a2a/card.rs`: las URI del binding (47 a 54) y las interfaces `iroh://<agente>/jsonrpc` y `/rest` de la card (163 y 164) | Un protocolo de streams de libp2p que lleve el mismo HTTP/1.1, un intercambio por stream, con el `PeerId` autenticado por Noise o TLS de libp2p en lugar del id de Iroh. Las URI de `v1` no cambian de significado, así que harían falta un binding nuevo y su documento, y otras interfaces en la card. Cambiar la card obliga a repetir el TCK, el cliente de Python y los verificadores de firma de la card |
| **Blobs** (iroh-blobs 0.105.0) | `blobs.rs` (946 líneas): el `FsStore` (275), el `BlobsProtocol` en `iroh_blobs::ALPN` (322; servido en `node.rs`, 557), la autorización por grant con los eventos del proveedor (`EventSender`, 282; `authorize`, de 776 a 829, donde `ClientConnected` da el `EndpointId`, y `may_fetch`, de 833 a 860, que lo compara con los grants), la descarga con `Endpoint::connect` (557 y 558), la prueba de posesión con un `MemStore` (707 a 738) y el error de permiso (763). Además, `gateway/rewrite.rs` (120 líneas), las URL `iroh-blob://<titular>/<hash>`, y el uso de `Hash` y `BlobService` en `worker/mod.rs` | El spec ya lo dice: iroh-blobs no pasa a funcionar sobre libp2p automáticamente. Leyendo el código de iroh-blobs 0.105.0, sin probarlo: tiene caminos genéricos sobre streams (`provider::handle_stream<R: RecvStream, W: SendStream>` y `api::remote::GetStreamPair`), y `provider::handle_connection`, que recibe la `Connection` de Iroh, es la que emite el `ClientConnected` con el `EndpointId` del que depende la autorización por grant. Pero `provider::StreamPair` lleva el `EventSender`, y `StreamPair::new(connection_id, reader, writer, events)` y `EventSender::client_connected` son públicos. Así que un adaptador de los streams de libp2p a los traits `RecvStream` y `SendStream` de iroh-blobs podría emitir él mismo el `ClientConnected`, con la clave Ed25519 del par, y conservar la autorización por grant sobre los eventos del proveedor (`authorize` y `may_fetch`). Habría que escribir ese adaptador y su bucle de aceptación, o sustituir iroh-blobs. Las pruebas de blobs del hito 1 (blob sin grant, transferencia grande interrumpida, por conexión directa y por relay) se repetirían |
| **`agentnet-discovery/0`** | `discovery/server.rs` (367 líneas): un `ProtocolHandler` (304 a 322), con las cuotas por par según el id que autentica Iroh. `discovery/client.rs` (560): `Endpoint::connect` con `ALPN_DISCOVERY` (470) y una conexión guardada por `EndpointId`. El ALPN, congelado, en `pals-core/src/discovery/mod.rs` (51). El contacto congelado `agentnet.contact/1` (`pals-core/src/records.rs`, de 57 a 90) lleva un campo `iroh` con `endpoint_id`, `relay_url` y `direct_addrs`, sin admitir campos desconocidos, y el anuncio nombra el ALPN (`discovery_alpn`, 108) | Los marcos (un `u32` big-endian y el mensaje protobuf) podrían ir tal cual en un stream de libp2p, con un identificador de protocolo en lugar del ALPN y las cuotas por `PeerId`. Pero el contacto tendría que nombrar multiaddrs y un `PeerId`: un esquema nuevo, `agentnet.contact/2`, con sus vectores, porque los `agentnet.*/1` están congelados |
| **Gateway** | `gateway/` no habla Iroh: es HTTP en loopback. Depende de lo que tiene debajo: `PeerHttp` (`gateway/mod.rs`, 1375), `peer_addr` (641) y `BlobService` (traer un blob de su titular, de 474 a 529; compartirlo, 1460 a 1471; subirlo, 1736; servirlo, 1777) | Su API HTTP para los clientes estándar no cambia. Cambia con el túnel y los blobs |
| **Arranque del nodo** | `node.rs`: `bind_endpoint` (835 a 854), con el preset `N0` y los relays públicos de n0 o sin relay (`net.relay`, `config.rs`, 444 a 447); el `Router` con los tres ALPN (555 a 558); `dialable_addr` y `ticket_of` (669 a 684). `main.rs`: los *tickets* de Iroh de `pals contact add` (26 y 1571) | Un swarm de libp2p con su transporte, relay v2, DCUtR y AutoNAT, y alguien que opere esos relays, que hoy son los de n0. Invitaciones con multiaddrs en lugar de *tickets* |

Las pruebas de integración también: 12 ficheros de `crates/pals/tests/` nombran Iroh (`grep -rli iroh`), porque arrancan nodos reales sobre él.

**Lo que ganaría:** una sola pila, como `kad-iroh`, y Kademlia sin adaptador, como `kad-libp2p`.

**Lo que costaría:** reescribir el túnel A2A y el transporte de `agentnet-discovery/0`, y adaptar los blobs; un binding nuevo y un contacto nuevo, con sus vectores; repetir la conformidad de los hitos 1 y 2 y sus pruebas de salida; y operar relays propios. Es un cambio del transporte de tareas y de ficheros, que el spec pide decidir de forma explícita (§6.1). Nada de lo medido en la DHT lo justifica: la DHT de `libp2p-total` sería la de `kad-libp2p`, que no mejora la de `kad-iroh` en nada que pida el criterio de salida.

## Aplicación de las reglas de la hoja de ruta

Las reglas son las de [«Comparación del backend»](../plans/2026-11-14-hoja-de-ruta-hitos-2-4.md#comparación-del-backend-tarea-1), y el punto de partida, la recomendación de H3-D1: la decisión provisional del spec, Iroh e iroh-blobs más `libp2p-kad`, salvo un coste medido que la descarte, y nunca solo por reducir dependencias (§6.1). Los dos candidatos de Kademlia llevan `libp2p-kad` dentro del daemon, con Iroh para lo demás. Pero, leídos al pie de la letra, el §6.1 («no se presupone un adaptador inmediato entre ambos transportes») y el §6.2 («Esto introduce dos identidades de red») describen `kad-libp2p`: una segunda pila QUIC y un vínculo de identidades. `kad-iroh` es una variante que el spec no describe: Kademlia sobre Iroh, con un adaptador de 524 líneas y una sola identidad.

### Qué descarta a un candidato

**1. No poder cumplir el criterio de salida.** Las partes medibles son encontrar un proveedor sin contacto previo (S1), funcionar sin el arranque original (S2) y caducar anuncios (S3).

| | S1 | S2 | S3 | Resultado |
|-|-|-|-|-|
| `kad-libp2p` | `pass` | `pass` | `pass` | Cumple |
| `kad-iroh` | `pass` | `pass` | `pass` | Cumple |
| `mainline` | `pass` | `fail` en esta topología | `fail` | **Descartado** por S3 |
| `libp2p-total` | Su Kademlia sería el de `kad-libp2p` | | | No se descarta por esta regla |

Por qué falla `mainline`, según [`mainline.md`](hito-3-backend/mainline.md):

- **S3, por la biblioteca.** `n0-mainline` 0.7.2 no caduca nada: guarda en cachés LRU acotadas por número de entradas. En el último sondeo, 202,2 s después de parar a `p1`, `r1` seguía guardando sus 4 registros, y todas las búsquedas lo devolvían como válido (según `raw/S3.jsonl`, que git ignora; en el JSON, `routers_forget_s` y `searches_forget_s` son `null`). Emular la caducidad en el cliente no cuenta (punto 4 de «Review Focus» del plan). Cumplir exigiría cambiar el almacén de la biblioteca.
- **S2, por el protocolo y la topología.** No muestra que `mainline` no funcione sin el arranque original, sino lo que pasa con routers en dos redes sin ruta entre ellas. Mainline nombra a cada nodo con una sola dirección (26 bytes en BEP 5), así que un router en dos redes solo sirve a una. S1 pasó solo porque `r1` lo guardaba todo. S3 basta para descartar `mainline`. Con `r1` parado, ninguna búsqueda de los clientes estuvo completa (`connected_complete_pct` 0) y `c5` no encontró nunca a todos los proveedores en sus 80 búsquedas. Un router como dos nodos, uno por red, partiría la DHT en dos, salvo que copiara lo guardado de uno a otro, y eso ya es reimplementar parte de la DHT. La prueba de salida del hito 3 se hace en al menos dos redes (§14; tarea 8 de la hoja de ruta).

**2. Exigir reimplementar Kademlia (§6.3).** Ninguno lo exige. `kad-iroh` usa `libp2p-kad` sin cambios, y solo escribe el transporte por debajo (524 líneas, lejos del límite de unas 1000 del plan). `mainline` usa la DHT de `n0-mainline` tal cual, pero arreglar sus dos fallos pediría cambiar el almacén de la biblioteca y salirse de su protocolo de una dirección por nodo, que ya es reimplementar parte de la DHT.

**3. Depender de que una DHT pública existente admita nuestros valores (§6.2).** Ninguno depende: todos van en redes privadas, sin nodos de arranque por defecto, y S0 queda en `pass` en todos. En `mainline` importa por otro motivo: la red pública de Mainline no serviría su índice. Los *signed peers* son una propuesta de BEP, tras la feature `unstable_signed_peers`, y solo los contestan los nodos de `n0-mainline`. Además, un item BEP 44 lleva 1000 bytes como mucho, y en ellos cabe un anuncio de unas 10 capacidades. Así que `mainline` también tendría que ser una red privada.

**4. No poder validar registros propios ni el vínculo entre identidad de red y agente (§6.2, §13).**

- `kad-libp2p`: sus routers validan cada valor y cada provider record antes de guardarlo (`StoreInserts::FilterBoth`). Comprueban el vínculo entero (las dos firmas y que la clave libp2p es la del `PeerId`), el anuncio y, si lo tienen, que el provider record es de una capacidad anunciada. S5 en `pass`, con 4 rechazos.
- `kad-iroh`: no hay vínculo que validar, porque el `PeerId` es la clave del agente y el TLS de Iroh lo autentica. Los routers comprueban cada contacto y cada anuncio contra el agente de su clave. S5 en `pass`, con 4 rechazos.
- `mainline`: valida, pero solo leyendo los mensajes en un gancho de datagramas, antes que la biblioteca, porque esta no da otro sitio. Ese gancho no es un diseño de producto ([Lo no medido](#lo-no-medido)). Un router no sabe si un *signed peer* es de una capacidad cuando no tiene el anuncio del agente. No lo descarta esta regla, pero pesa en contra.

En los tres, los routers guardan el anuncio y el provider record (o el *signed peer*) de `rogue`, y solo rechazan su contacto. Los clientes lo marcan como inválido (`bad_contact`).

### Qué pesa en contra

**Definir el índice de capacidades y sus mecanismos de anuncio, identificación y abuso.** Pesa contra `mainline`, que lo tuvo que definir entero ([`mainline.md`](hito-3-backend/mainline.md#qué-hubo-que-definir-para-tener-un-índice-de-capacidades)):

- el índice son *signed peers*, una propuesta de BEP que solo contesta `n0-mainline`;
- la validación es el gancho de datagramas, y la observación de lo guardado, preguntas del router a sí mismo por la dirección de bucle;
- hace falta un cliente de la DHT nuevo en cada cambio de red, y una búsqueda antes de repetir un `put` fallido;
- un anuncio de más de unas 10 capacidades no cabe en un item;
- no hay límite de ritmo ni cuota por emisor: los almacenes son LRU;
- un cliente sigue en modo cliente solo por un error de la 0.7.2;
- los clientes de la red A esperan unos 900 ms en S1 a direcciones de la B que no alcanzan: 917 y 914 ms (p50) de `c1` y `c2`, según `raw/S1.jsonl` en `mainline.md`, frente a 3 ms de los de la B;
- y `requests` es `null`.

Los dos candidatos de Kademlia usan el índice de `libp2p-kad`: provider records bajo el multihash de la capacidad, con su TTL, su republicación y sus límites de almacén. Lo propio son los validadores (`validate.rs`, de 193 y 234 líneas), que la tarea 4 de la hoja de ruta ya prevé, y los cinco ajustes de [`kad-libp2p.md`](hito-3-backend/kad-libp2p.md#api-de-libp2p-usadas).

**Cambiar el transporte de tareas o de ficheros, que exige una decisión explícita (§6.1).** Pesa contra `libp2p-total`, que cambia los dos. Ningún otro los cambia. `kad-libp2p` añade una segunda pila en otro puerto UDP. `kad-iroh` añade un ALPN, `agentnet-kad/0`, al endpoint de Iroh que ya sirve A2A, blobs y descubrimiento. `mainline` añade un socket UDP.

**Además, el coste que la hoja de ruta pone al candidato inicial,** aunque no está en su lista de lo que pesa en contra: dos pilas de conectividad y dos identidades que vincular (tabla de candidatos de la hoja de ruta, §6.1 y §6.2), y el riesgo «Dos pilas de red con `libp2p-kad`: más conexiones, configuración y mantenimiento». `kad-libp2p` lo tiene entero: dos pilas QUIC, dos puertos, una segunda clave (que la prueba deriva de la semilla del agente, sin guardar nada más) y un vínculo firmado por las dos claves que, con los `agentnet.*/1` congelados, pide un esquema nuevo. `kad-iroh` no tiene ninguna de las dos cosas. Lo que se midió de ese coste está repartido ([Entre los que pasan](#entre-los-que-pasan-deciden-las-medidas)).

### Reducir dependencias no basta para ganar

`compat.json` cuenta los crates de cada binario, con `cargo tree -e normal` y cada nombre y versión una vez: 300 la línea base, 306 `mainline`, 337 `kad-iroh` y 370 `kad-libp2p`. Y el binario sobre el de la línea base: 1 623 616 bytes `mainline`, 3 406 536 `kad-iroh` y 9 704 176 `kad-libp2p`, que lleva libp2p con su QUIC encima de Iroh.

- `mainline` es el que menos añade, y no por eso gana: el criterio de salida lo descarta.
- `libp2p-total` quitaría Iroh e iroh-blobs del daemon. Eso solo no justifica cambiar el transporte de tareas y de ficheros (§6.1).
- Entre los dos de Kademlia, `kad-iroh` trae 33 crates menos que `kad-libp2p`, 4 crates en más de una versión menos (8 frente a 12) y un binario 6 297 640 bytes menor. La recomendación no se apoya solo en eso, que la regla no admite: cuenta como parte del peso de la segunda pila, junto al esquema de vínculo que `kad-iroh` evita ([Entre los que pasan](#entre-los-que-pasan-deciden-las-medidas)).

### Entre los que pasan, deciden las medidas

Quedan `kad-libp2p` y `kad-iroh`. Los dos pasan S0 a S5 y S7, con S6 medido. Comparados escenario a escenario, con las cifras de sus JSON y de `compat.json`:

| Medida (fuente) | `kad-iroh` | `kad-libp2p` | A favor de |
|-|-|-|-|
| S0 a S5 y S7 (JSON) | `pass` | `pass` | Ninguno |
| S1: primer candidato válido, p50 / p95 (JSON) | 2 / 5 ms | 2 / 5 ms | Ninguno |
| S1: búsqueda entera, p50 / p95 (JSON) | 5 / 10 ms | 7 / 13 ms | Ninguno: son milisegundos |
| S1: peticiones por búsqueda, p50 (JSON) | 20 | 20 | Ninguno |
| S1: bytes netos por búsqueda, p50 (JSON) | 27 841 | 31 599 | `kad-iroh`, un 12 % menos |
| S2: búsqueda entera con `r1` parado, p50 / p95, y fin de la primera búsqueda de `c5` con candidato válido (JSON) | 5001 / 10 013 ms; 16,0 s | 5000 / 10 000 ms; 15,2 s | Ninguno |
| S3: los routers olvidan / las búsquedas olvidan (JSON) | 106,6 / 99,3 s | 111,4 / 107,9 s | Ninguno: una ejecución, y `stored_before` varía (12 frente a 16) |
| S4: búsqueda entera con `r2` y `r4` parados, p50 / p95 (JSON) | 5001 / 5006 ms | 5001 / 8361 ms | Ninguno |
| S6: memoria en reposo, p50 de router, proveedor y cliente (JSON) | 21 128, 20 372 y 20 400 KiB | 23 240, 22 764 y 22 748 KiB | `kad-iroh`, de 2,1 a 2,3 MiB menos por nodo |
| S6: bytes por minuto en reposo, p50 de router, proveedor y cliente (JSON) | 355 418, 218 700 y 27 142 | 249 735, 180 386 y 18 262 | `kad-libp2p`: `kad-iroh` gasta un 42, un 21 y un 49 % más |
| S6: conexiones de un router, p50 / máximo (JSON) | 1 / 4 | 0 / 1 | `kad-libp2p` |
| S7: clientes con el contacto nuevo, p50 / máximo (JSON) | 1,5 / 1,8 s | 1,9 / 2,1 s | Ninguno: una ejecución, y `kad-iroh` tardó unos 34 s en una de prueba |
| Crates y crates en más de una versión (`compat.json`) | 337 y 8 | 370 y 12 | `kad-iroh`, aunque eso solo no basta |
| Binario sobre la línea base (`compat.json`) | 3 406 536 bytes | 9 704 176 bytes | `kad-iroh` |
| Compilación release desde cero, mínimo de tres (`compat.json`) | 62,9 s | 74,8 s | Ninguno de forma fiable: la carga fue de 20 a 35, y el mínimo de `kad-iroh` queda incluso por debajo del de la línea base (68,7 s) |
| Código de `src/`, sin pruebas (documento de cada candidato) | 2304 líneas, 524 del adaptador | 1992 líneas, 265 del vínculo | `kad-libp2p`: menos código propio que mantener, 265 líneas de vínculo frente a 524 de adaptador. Su vínculo pide además un esquema congelado |
| Metadatos que ve un router: quién busca (lectura del código, [`kad-iroh.md`](hito-3-backend/kad-iroh.md#metadatos-que-ve-un-router)) | Lo identifica su clave de agente | Si no publica, su `PeerId` no lleva a su agente (el de un proveedor sí, por su vínculo); su IP lo identifica en una red pequeña | `kad-libp2p` |

Las medidas no separan a los dos en nada de lo que pide el criterio de salida, y ninguna diferencia medida descarta a ninguno. En el coste se reparten:

- de las medidas de recursos del §6.3, la memoria en reposo va a favor de `kad-iroh`, y las conexiones abiertas y el tráfico en reposo, a favor de `kad-libp2p`;
- `kad-iroh` gasta además menos bytes por búsqueda, trae menos dependencias y da un binario menor;
- `kad-libp2p` tiene menos código propio que mantener y deja ver menos de quien busca.

La diferencia de tráfico en reposo es, en un router, de unos 106 000 bytes por minuto (355 418 frente a 249 735, p50), con una republicación cada 40 s. Con los intervalos de producción bajaría en los dos, aunque eso no se ha medido.

**Así que la regla de H3-D1 no elige `kad-iroh`.** Leídos al pie de la letra, el §6.1 y el §6.2 describen `kad-libp2p`, y H3-D1 lo mantiene «salvo un coste medido que la descarte»: ninguno lo descarta. El riesgo de dos pilas, tal como lo dice la hoja de ruta, es «más conexiones, configuración y mantenimiento». De eso, lo medido (conexiones y tráfico) no favorece a `kad-iroh`, y el mantenimiento del código propio tampoco. Recomendar `kad-iroh` es un juicio, y se apoya en lo que `kad-iroh` se ahorra:

- **Un esquema nuevo y congelado.** El vínculo firmado por las dos claves, con sus vectores, junto al `agentnet.contact/1` congelado: la tarea 3 de la hoja de ruta. Es el coste de identidad que queda en `kad-libp2p`.
- **Una segunda pila de red.** Su peso está medido: 33 crates más (370 frente a 337, con una segunda implementación de QUIC y la capa TLS de libp2p sobre el mismo `rustls`: `libp2p-quic`, `quinn`, `libp2p-tls`, `rcgen`, `x509-parser` y `aws-lc-rs`, entre otros, según [`kad-iroh.md`](hito-3-backend/kad-iroh.md#código)), un binario 6 297 640 bytes mayor (unos 6,3 MB) y de 2,1 a 2,3 MiB más de memoria en reposo por nodo. Operarla no está medido: un segundo puerto UDP, con su juego de conexiones, que abrir y vigilar en cada router, porque los routers tienen que ser alcanzables en los dos puertos.
- **Menos bytes por búsqueda:** un 12 % menos en `kad-iroh` (27 841 frente a 31 599, p50), medido.

Dos costes que se le podrían atribuir a la segunda pila no cuentan, porque no se sostienen:

- **Atravesar NAT.** Con el modo por defecto que se recomienda (H3-D3, en [Lo que hereda la fase 2](#lo-que-hereda-la-fase-2)), quien no es router es cliente de Kademlia: solo marca hacia fuera, hacia los routers, y nadie lo marca por Kademlia, porque a un proveedor se llega por su contacto (`NoDial`). Así que ninguno de los dos candidatos necesita relays ni atravesar NAT para sus clientes: en `kad-libp2p` basta el QUIC de salida. En los dos, los routers tienen que ser alcanzables directamente, y los dos declaran externas sus direcciones locales, el mismo apaño ([`kad-libp2p.md`](hito-3-backend/kad-libp2p.md#solo-red-privada)). AutoNAT, relay v2 y DCUtR solo harían falta en `kad-libp2p` para un router tras NAT, que H3-D3 excluye. Y `kad-iroh`, tal como está escrito, solo llega a los relays y a la perforación de Iroh por la búsqueda de direcciones de n0, que el daemon enciende con `relay = true`: su adaptador solo marca direcciones IP, y Kademlia solo maneja direcciones IP ([`kad-iroh.md`](hito-3-backend/kad-iroh.md#lo-que-no-se-pudo-medir)). Usarlos sin depender de n0 pediría una forma de dirección nueva ([Lo que hereda la fase 2](#lo-que-hereda-la-fase-2)).
- **Una segunda clave que guardar y rotar.** La prueba deriva la clave libp2p de la semilla del agente, con separación de dominio (`libp2p_keypair` en [`link.rs`](../../spikes/hito-3/kad-libp2p/src/link.rs)): no hay nada más que guardar, y la clave cambia cuando cambia la del agente. El producto podría hacer lo mismo ([`kad-libp2p.md`](hito-3-backend/kad-libp2p.md#cómo-está-hecho)).

Lo que pesa en contra de `kad-iroh` está medido, contado o leído en el código:

- **un adaptador de 524 líneas** (390 sin comentarios ni vacías) que PALS mantiene frente a las versiones de libp2p y de Iroh. Es el «coste de mantener adaptadores» del §14. El único transporte publicado con el mismo fin, `libp2p-iroh` 0.1.3, se quedó en `iroh ^0.97` y `libp2p ^0.56` ([`kad-iroh.md`](hito-3-backend/kad-iroh.md#paso-1-un-transporte-ya-publicado)). Es una muestra de lo que pasa si nadie lo mantiene. Con las versiones fijadas con `=`, solo se rompe en una subida deliberada, que en este repositorio ya pide una revisión explícita;
- **más tráfico en reposo y más conexiones abiertas en los routers** (S6, arriba: 1 / 4 frente a 0 / 1, p50 / máximo). Del tráfico, al menos una parte son las marcaciones dobles a los routers con dos direcciones, unas 5 por minuto en el registro de `r2` según `kad-iroh.md`;
- **quien busca se identifica ante los routers con su clave de agente**, porque cada búsqueda va por una conexión Iroh autenticada. En `kad-libp2p`, el `PeerId` de un cliente que no publica no lleva a su agente, aunque su IP lo identifica en una red pequeña ([`kad-iroh.md`](hito-3-backend/kad-iroh.md#metadatos-que-ve-un-router)). Es una filtración de metadatos del §14 mayor que en `kad-libp2p`. Se podría atenuar con una clave aparte para buscar, que no se publica. Pero un endpoint de Iroh tiene una sola clave, así que eso es un segundo endpoint, con su socket, sus conexiones y su estado de direcciones y relays: cuesta menos que el vínculo, pero trae de vuelta parte de la segunda pila ([Qué la haría cambiar](#qué-la-haría-cambiar));
- **un ALPN más** que servir en el daemon, con sus cuotas por decidir.

Las medidas, que según el título de este apartado deciden entre los que pasan, se reparten y no deciden. Por eso la regla se queda en la decisión provisional del spec, que describe `kad-libp2p`, y la recomendación se aparta de ella como juicio. Pesando unos y otros, la recomendación es `kad-iroh`, por un margen estrecho, y no como resultado de las medidas.

## Recomendación frente al criterio de salida

**Iroh e iroh-blobs más `libp2p-kad` sobre las conexiones Iroh del daemon (`kad-iroh`)**, frente a las tres partes del criterio de salida del hito 3 (spec §14), con las cifras de [`results/kad-iroh.json`](hito-3-backend/results/kad-iroh.json), de una sola ejecución:

- **Justificar la implementación elegida:** este registro.
- **Encontrar un proveedor sin contacto previo (S1, `pass`).** Las 160 búsquedas devolvieron los dos proveedores, verificados. Primer candidato válido a los 2 ms (p50) y 5 ms (p95), búsqueda entera en 5 y 10 ms, 20 peticiones y 27 841 bytes netos (p50) por búsqueda.
- **Funcionar sin el arranque original (S2, `pass`).** Con `r1` parado, el 100 % de las búsquedas de los clientes conectados estuvieron completas, y `c5`, que arranca con `[r1, r3]`, entró por `r3`, el único de los dos en marcha, y terminó su primera búsqueda con candidato válido 16,0 s después de arrancar su contenedor (el candidato llegó a los 12,2 s; ver «Cómo leerla» en [Métricas](#métricas)). Mientras un router caído esté entre los más cercanos a la clave, la búsqueda tarda hasta el plazo de marcado: 5001 ms (p50) y 10 013 ms (p95).
- **Caducar anuncios (S3, `pass`).** Parado `p1` sin aviso, los routers dejaron de guardar sus registros a los 106,6 s y ninguna búsqueda lo devolvió desde los 99,3 s, con el mecanismo de caducidad de la propia biblioteca. El límite es 160 s (`TTL_S + REPUBLISH_S`).

Además: S4 en `pass` (cobertura 1,0 desde el principio con `r2` y `r4` parados), S5 en `pass` (4 routers rechazan el contacto de `rogue`, ninguno lo guarda y ningún cliente lo da por válido) y S7 en `pass` (los cuatro clientes, con el contacto nuevo de `p3` a los 1,5 s, p50; con la variación que dice [Lo no medido](#lo-no-medido)).

**Es un juicio, no la regla de H3-D1 aplicada.** Las cifras de arriba muestran que `kad-iroh` cumple el criterio de salida, igual que `kad-libp2p`. No muestran que sea mejor. La recomendación no se apoya en las cifras de S3 y S7 de `kad-iroh` frente a las de `kad-libp2p`, que son de una sola ejecución con variación conocida. Ningún coste medido descarta a `kad-libp2p`, que es lo que describen al pie de la letra el §6.1 y el §6.2, y los recursos medidos se reparten: la memoria en reposo va a favor de `kad-iroh`, y las conexiones abiertas y el tráfico en reposo, a favor de `kad-libp2p`. La recomendación descansa en lo que `kad-iroh` se ahorra:

- un esquema de vínculo nuevo, con sus vectores, junto a los `agentnet.*/1` congelados;
- una segunda pila de red, con 33 crates, 6,3 MB de binario y unos 2 MiB de memoria por nodo más, y un segundo puerto UDP, con sus conexiones, que abrir y operar en cada router. Las dependencias cuentan solo como parte de esa pila: solas no bastarían;
- un 12 % de los bytes de cada búsqueda.

Frente a eso pesan el adaptador de 524 líneas que mantener frente a las versiones de libp2p y de Iroh, más tráfico en reposo y más conexiones abiertas en los routers, y que los routers sepan la clave de agente de quien busca. Atravesar NAT y una segunda clave no cuentan: con H3-D3, ninguno de los dos candidatos necesita relays ni atravesar NAT para sus clientes; `kad-iroh`, tal como está escrito, solo llega a los relays de Iroh por la búsqueda de direcciones de n0; y la clave libp2p se puede derivar de la del agente ([Entre los que pasan](#entre-los-que-pasan-deciden-las-medidas)). El margen es estrecho.

**Ratificarla es aprobar un cambio respecto al spec,** no solo anotarlo:

- el §6.2 describe la variante Iroh más libp2p con dos identidades y un vínculo firmado por las dos, y `kad-iroh` la hace innecesaria;
- el §5.2 define el contacto como «relación verificable entre identidad libp2p, agente y endpoint Iroh», y con `kad-iroh` la identidad libp2p es la del agente, así que basta el `agentnet.contact/1`;
- el §6.1 dice que no se presupone un adaptador entre los dos transportes, y aquí se escribió y se midió.

La fase 2 lo anota en la tabla de cambios respecto al spec de su plan.

## Lo que hereda la fase 2

**La API de la DHT que usará el daemon.** `libp2p-kad` 0.49.0, por `libp2p` 0.57.1, sobre las conexiones Iroh del endpoint del daemon:

- **Transporte:** un `libp2p::core::Transport` con `Output = (PeerId, StreamMuxerBox)` y un `StreamMuxer` sobre `iroh::Endpoint` (`IrohTransport` e `IrohMuxer` de la prueba: `Endpoint::connect`, `Connection::{open_bi, accept_bi, remote_id}`), servido en el `iroh::protocol::Router` que ya monta `node.rs`, junto a `agentnet-a2a-http/1`, `iroh_blobs::ALPN` y `agentnet-discovery/0`. El ALPN `agentnet-kad/0` es provisional: congelarlo es una decisión de la fase 2.
- **Identidad:** el `PeerId` es la clave pública Ed25519 del agente (`identity::PublicKey::from(ed25519::PublicKey::try_from_bytes(agent_id))`). El swarm se crea con `Swarm::new` y ese `PeerId`, sin `Keypair`: libp2p no recibe la clave secreta del agente ni firma nada con ella.
- **Kademlia:** protocolo `/agentnet/kad/1.0.0`; `kad::Behaviour` con `kad::Config` (TTL de valores y de provider records, intervalos de publicación y de replicación, `StoreInserts::FilterBoth`, `Caching::Disabled`, plazo de consulta) y `MemoryStore` con límites; el modo fijado con `set_mode(Some(Mode::Server))` o `Some(Mode::Client)`, nunca automático; `get_providers`, `get_record`, `put_record` con `Quorum::All`, `start_providing`, `stop_providing` y `remove_record`; los eventos `InboundRequest` y `OutboundQueryProgressed`.
- **`identify`,** con la clave pública del agente, `with_hide_listen_addrs(true)` y `with_cache_size(0)`.
- **Lo que se publica:** `/agentnet/contact/<peer_id>` con el `Signed<ContactRecord>` y `/agentnet/announce/<peer_id>` con el `Signed<AnnounceRecord>`, los dos de `pals-core` sin cambios, y un provider record bajo el multihash SHA2-256 (código `0x12`) de la cadena completa de cada capacidad (§5.1).
- **Los ajustes** que la prueba necesitó: los cinco de [`kad-libp2p.md`](hito-3-backend/kad-libp2p.md#api-de-libp2p-usadas) (con `NoDial`) y el plazo del anuncio de [`kad-iroh.md`](hito-3-backend/kad-iroh.md#los-ajustes-de-kad-libp2p).

El código de [`spikes/hito-3/kad-iroh/`](../../spikes/hito-3/kad-iroh/) es un punto de partida, no código del producto. Esto es lo que no resuelve y la fase 2 tiene que resolver:

- **La forma de dirección no llega a un relay.** El transporte solo marca `/ip4|ip6/<ip>/udp/<puerto>/quic-v1/p2p/<id>` (`dial_target` en `transport.rs`), y conecta a un `EndpointAddr` con esa dirección IP y sin URL de relay (`dial`). Iroh solo usa un relay si conoce la URL del relay del par, por el `EndpointAddr` o por su búsqueda de direcciones. Con `relay = true`, el daemon arranca con el preset `N0` (`bind_endpoint` en `node.rs`), cuya búsqueda de direcciones es el DNS público de n0, y es lo que pasa por defecto: Iroh la lanza al conectar con un par al que no tiene camino elegido, aunque el `EndpointAddr` lleve direcciones IP. Con `relay = false`, no hay búsqueda. Y Kademlia solo maneja direcciones IP: `bootstrap_addr`, las de `identify` y las de los provider records. Para que un router o un proveedor tras NAT sea alcanzable por Kademlia, la fase 2 tiene que añadir la URL del relay a la forma de dirección (por ejemplo, sacándola del `relay_url` del contacto), o aceptar depender de la búsqueda de n0 y decidirlo frente al §6.2. Con H3-D3 no hace falta para los clientes, que solo marcan hacia fuera.
- **Una llamada cada vez.** La tarea de la prueba contesta «another call is in progress» a toda búsqueda o anuncio mientras otro está en marcha (`Task::on_command`), porque `run_node` espera a cada llamada antes de la siguiente. En el daemon habría búsquedas a la vez (pasarela y MCP) y un anuncio por un cambio de red en mitad de una búsqueda. El estado tiene que ir por `QueryId` y admitir búsquedas concurrentes.
- **Dos caminos del anuncio que la topología no ejerce,** porque solo se dan en un nodo que es a la vez router y proveedor y tiene la tienda llena. Son el mismo código en los dos candidatos. Cuando `provide` falla a medias, deja de anunciar las capacidades que había empezado solo en el propio nodo: las consultas `AddProvider` ya empezadas siguen y pueden llegar una vez a los routers, y habría que cerrarlas con `query_mut(id).finish()`. Y cuando un reintento de `retry_puts` no puede poner un registro, el anuncio contesta con un error sin anunciar las capacidades, al revés que al llegar el plazo, cuando las anuncia porque el nodo republica sus registros nuevos; tendría que hacer lo mismo que al llegar el plazo.

**Las dependencias nuevas y sus versiones.** Una sola directa: `libp2p =0.57.1`, sin transportes propios, con las features `kad`, `tokio`, `macros`, `identify` y `ed25519`. Con las versiones del `Cargo.lock` de las pruebas, entran en el árbol normal de `pals` 31 crates que hoy no tiene, todos traídos por libp2p y ninguno en otra versión de uno que ya tenga: `asynchronous-codec 0.7.0`, `base-x 0.2.11`, `base256emoji 1.0.2`, `base45 3.2.0`, `bs58 0.5.1`, `const-str 0.4.3`, `crunchy 0.2.4`, `futures-bounded 0.3.1`, `futures-timer 3.0.3`, `hkdf 0.13.0`, `hmac 0.13.0`, `libp2p 0.57.1`, `libp2p-allow-block-list 0.7.0`, `libp2p-connection-limits 0.7.0`, `libp2p-core 0.44.0`, `libp2p-identify 0.48.0`, `libp2p-identity 0.3.1`, `libp2p-kad 0.49.0`, `libp2p-swarm 0.48.0`, `libp2p-swarm-derive 0.36.0`, `match-lookup 0.1.3`, `multiaddr 0.19.1`, `multibase 0.9.3`, `multihash 0.19.5`, `multistream-select 0.14.0`, `prost-codec 0.4.0`, `rw-stream-sink 0.5.0`, `static_assertions 1.1.0`, `uint 0.10.2`, `unsigned-varint 0.8.0`, `web-time 1.1.0`. Es una cuenta hecha para este registro, no una cifra de un JSON: las líneas de `cargo tree --locked -e normal --prefix none -p spike-kad-iroh` en `spikes/hito-3/` que no salen en el de `-p pals` en la raíz, sin los dos paquetes de la prueba. `sha2 0.11.0`, que la prueba usa para la clave de los provider records, ya está en el árbol del daemon. `kad-iroh` compila con la MSRV 1.91 (`compat.json`). La fase 2 fija `libp2p` con `=` en el `Cargo.toml` raíz y vuelve a comprobar las versiones al escribir su plan, como pide «Cómo se cierra un hito».

**Si hace falta vínculo de identidades (tarea 3 de la hoja de ruta): no.** El `PeerId` es la clave del agente, y el `agentnet.contact/1` viaja tal cual, sin esquema nuevo. La tarea 3 no aplica. Lo que sí hay que congelar, con vectores, son las claves de la DHT y cómo sale el `PeerId` de la clave del agente: es parte de las tareas 2 y 4.

**El modo por defecto (H3-D3): cliente; router por configuración en nodos estables y alcanzables,** como recomienda la hoja de ruta (§6.3). Alcanzables directamente, por IP, mientras la forma de dirección no lleve relay (arriba). Lo que dicen las medidas de `kad-iroh` (S6, p50): un router mueve 355 418 bytes por minuto en reposo, frente a 27 142 de un cliente, con una memoria parecida (21 128 frente a 20 400 KiB). Los proveedores y los clientes de la prueba van en modo cliente y encuentran y son encontrados (S1). El modo se fija siempre (`Some(Mode::Client)`): la prueba declara externas las direcciones locales de cada nodo, y con el modo automático `libp2p-kad` pasa a servidor a un nodo con direcciones externas confirmadas (`determine_mode_from_external_addresses` en `libp2p-kad` 0.49.0, leído para este registro).

**Lo que dicen las medidas sobre H3-D2** (quién opera los puntos de arranque y los routers), que no decide este registro. La topología tiene dos operadores, `r1` y `r2` frente a `r3` y `r4`, todos en las dos redes. Sin el arranque original (S2), los clientes conectados siguen resolviendo, y un cliente nuevo entra por el router del otro operador que tiene como contacto guardado. Con un router de cada operador parado (S4), la cobertura sigue en 1,0.

## Coste si es errónea

Si `kad-iroh` resulta ser la elección equivocada y hay que pasar a `kad-libp2p`:

- **Antes de desplegar la DHT,** se tira el adaptador (unas 524 líneas y sus pruebas) y vuelve la tarea 3: la clave libp2p (derivada de la del agente, como en la prueba), el vínculo de identidades y su esquema congelado. La configuración de Kademlia, la búsqueda, el anuncio, la caducidad y los contadores son casi el mismo código en los dos candidatos: `behaviour.rs` de `kad-iroh` es una copia del de `kad-libp2p`, con el contacto en lugar del vínculo, `spawn` sobre Iroh, `stored_by_agent` atribuido por el `PeerId` de cada registro y el plazo del anuncio (`Task::end_announce`), que `kad-libp2p` no tiene. La validación es una adaptación: `validate.rs` tiene 193 líneas en `kad-libp2p`, que valida el vínculo, y 234 en `kad-iroh`, que valida el contacto contra la clave del agente.
- **Después de desplegarla,** los `PeerId` y las claves de los valores cambian (`/agentnet/contact/` frente a `/agentnet/link/`), así que los nodos de una y otra no forman la misma DHT: hay que arrancar una red nueva con todos los nodos.

## Qué la haría cambiar

- **El adaptador no aguanta una versión.** Si una versión de libp2p o de Iroh que haya que adoptar rompe el adaptador y arreglarlo cuesta más que la segunda pila, se pasa a `kad-libp2p`. Si aparece un transporte publicado y mantenido para `iroh` 1.x, se usa en lugar del propio.
- **La convivencia con el daemon.** Si servir `agentnet-kad/0` en el mismo endpoint que A2A, blobs y descubrimiento, con sus cuotas, interfiere con ellos de una forma que la prueba no vio.
- **La variación de `kad-iroh`.** Si, repetidas, las ejecuciones de S7 o de un router que vuelve a arrancar salen de forma sistemática peor que en `kad-libp2p` (como los unos 34 s de la ejecución de prueba de S7), y la causa son los plazos de Iroh, que no se pueden ajustar para el ALPN de Kademlia. Ni S7 ni S3 deciden la recomendación, pero los dos son de una sola ejecución con variación conocida.
- **El tráfico en reposo con intervalos de producción.** Si, medido con los intervalos del producto, el de `kad-iroh` sigue claramente por encima del de `kad-libp2p` después de quitar las marcaciones dobles.
- **La privacidad de quien busca.** Si la política de búsqueda pública (H3-D4), o lo que se decida sobre las filtraciones de metadatos del §14, pide que un router no sepa qué agente busca. Bastaría una clave aparte para las búsquedas en modo cliente, por ejemplo una clave de Iroh que no se publica. Un endpoint de Iroh tiene una sola clave, así que eso es un segundo endpoint, con su socket, sus conexiones y su estado de direcciones y relays: trae de vuelta parte de la segunda pila, aunque solo marque hacia fuera. No necesitaría vínculo ni esquema, ni guardar la clave si es efímera, así que cuesta menos que la segunda identidad de `kad-libp2p`, pero no es gratis. No se probó. Solo si no bastara, `kad-libp2p`.
- **NAT y relays.** Si la fase 2 necesita routers o proveedores tras NAT alcanzables por Kademlia, algo que H3-D3 no pide. `kad-iroh` necesitaría la forma de dirección con relay de [Lo que hereda la fase 2](#lo-que-hereda-la-fase-2), y `kad-libp2p`, AutoNAT, relay v2 y DCUtR y relays de libp2p que alguien opere. Si esa forma de dirección cuesta más de lo previsto, o solo se puede tener dependiendo de la búsqueda de direcciones de n0 contra el §6.2, se vuelve a pesar. Y si en la prueba de salida del hito 3 (tarea 8) la DHT sobre Iroh no funciona en redes reales con routers alcanzables directamente.
- **El peso de la segunda pila.** Si el daemon acabara llevando la pila QUIC de libp2p por otro motivo, el ahorro de `kad-iroh` en dependencias, binario, memoria y puertos desaparecería, y quedaría solo el esquema de vínculo frente al adaptador.
- **Para los descartados.** `mainline` volvería a contar con una biblioteca que caducara lo guardado y un protocolo con varias direcciones por nodo, y aun así pesaría en contra por el índice. `libp2p-total`, solo con una decisión explícita de cambiar el transporte de tareas y de ficheros por otros motivos.
