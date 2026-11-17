# Prueba de salida del hito 3

Este documento recoge la ejecución de los nueve casos del criterio de salida del hito 3, el descubrimiento con Kademlia (spec §14). Sigue el runbook [`docs/runbooks/hito-3-descubrimiento-kademlia.md`](../../runbooks/hito-3-descubrimiento-kademlia.md):
- dos operadores y dos redes;
- un contenedor Docker por agente;
- la DHT `agentnet-kad/0` del daemon, sin relay.

La ejecuta el arnés [`tests/exit/hito-3.sh`](../../../tests/exit/README.md#prueba-de-salida-del-hito-3-con-docker).

**No es una prueba en varias máquinas físicas.** Los agentes comparten kernel, reloj y máquina. Entre ellos solo hay dos redes de Docker `--internal` (ver [«Lo que no cubre»](#lo-que-no-cubre)). Lo decidió H3-D2 ([registro](../../decisions/2026-11-16-hito-3-decisiones.md#h3-d2-quién-opera-los-puntos-de-arranque-y-los-routers-de-la-prueba)): el agente que implementa el hito opera los dos operadores, cada uno con sus identidades, su configuración y sus puntos de arranque. Una prueba con Docker cierra un hito (D2 del cierre de los hitos 1 y 2).

## Ejecución tras reservar huecos a la DHT (2026-11-17)

Después del cierre del hito 3, el responsable decidió reservar huecos a la DHT entre los candidatos de una búsqueda ([registro de decisión](../../decisions/2026-11-17-huecos-dht.md)). El caso 2 ganó una comprobación: `c6`, con tantos contactos guardados como huecos, encuentra un proveedor en la DHT. La prueba entera se ejecutó una vez sobre el árbol con ese cambio. Las secciones siguientes, desde [«Resultado (2026-11-17)»](#resultado-2026-11-17), son la ejecución que cerró el hito 3, y quedan como estaban.

**Resultado.** Los nueve casos, superados en una sola ejecución completa, a la primera. Al acabar no quedó ningún contenedor ni red con la etiqueta de la ejecución, y se conservaron las imágenes `pals-exit-h3:latest` y `pals-exit-a:latest`.

| Dato | Valor |
|-|-|
| Fecha | 2026-11-17, de 10:39:40 a 10:56:36 UTC (16 min 56 s, con los 300 s de reposo) |
| Commit | `e3e5843`, con el árbol limpio: el reparto de los huecos (`3870709`), la comprobación de `c6` (`4d0baba`) y su documentación. Después solo cambia este registro |
| Binarios | `cargo build --release --locked -p pals` y `--examples` en el anfitrión: `pals 0.1.0` (sha256 `74c75029…`) y `dht_rogue` (sha256 `98d8c4f9…`) |
| Imagen | `pals-exit-h3:latest` (id `6fa4bcf19e77…`), la de la ejecución anterior |
| Anfitrión | El mismo, compartido con otros proyectos. Carga media al empezar: 21,4 / 37,6 / 76,2, tras la batería de pruebas del daemon y la carga de otros proyectos. La de un minuto, de 18,9 a 15,6 durante el reposo y de 15,6 a 5,8 durante los casos |
| Contenedores | Doce de la topología y cinco de sus casos (`c0`, `cx`, `c6`, `c5` y `rogue`) |

| Caso | Resultado | Lo medido |
|-|-|-|
| 1. Sin fuga | Superado | `c0`, 0 pares y 0 conexiones antes y después de la búsqueda de `cx`. `cx`: `GET /dht` 404 y `dht_requests 0` |
| 2. Encontrar un proveedor sin contacto previo | Superado | 160 de 160 búsquedas conformes, como en la ejecución anterior: `dht_requests` 20 en todas, 2 contactados y cobertura `timeout`. La pregunta de `c1` a `p1`: `echo: hola`. Y `c6`, más abajo |
| 3. Sin el arranque original | Superado | Con `r1` parado, 16 de 16 conformes. `c5` entra por `r3`: primer candidato a los 11,3 s, los dos proveedores de cada capacidad a los 33,5 s |
| 4. Caducar anuncios | Superado | Los routers olvidan a `p1` a los 116,1 s, y las búsquedas, a los 116,3 s. Límite: 160 s |
| 5. Caída de routers y renovación | Superado | Cobertura inmediata 1, entera a los 0 s (límite 80 s). A los 166 s, con todo lo de antes caducado, 1 en 16 búsquedas |
| 6. Contacto con firma inválida | Superado | Los 4 routers: `bad_contact` +1, y guardan 2 registros del `rogue`, sin contacto |
| 7. Búsquedas repetidas o maliciosamente amplias | Superado | Ráfaga de 200 desde `c1` en 6,2 s: 4 `200` y 196 `429 too many searches`. `refused.streams` 0 en los cuatro routers |
| 8. Política | Superado | Cada router guarda 4 registros de `p2` antes y después de una vuelta suya. `capability_not_announced` 0 → 0 |
| 9. Cambio de red | Superado | `c1` y `c2` verifican la card y la oferta de `p3` en la red A a los 6,5 s (límite 80 s) |

**La comprobación nueva, `c6` en el caso 2.** `c6` entra por `r1`. Tiene `[discovery] max_candidates = 3`, `dht_places` por defecto, 2, y tres contactos guardados, los routers `r1`, `r2` y `r3`. Su primera búsqueda de `vision.ocr` ya tenía a `p1`:

| Dato | Valor |
|-|-|
| Búsquedas hasta tener a `p1` | 1 |
| `p1` | En el ranking con `from_dht: true`, su card verificada y su oferta `available`, sin grant (`grant_held: false`) |
| Contactos evaluados | `r1`, el primero por nombre, descartado en la card: `no skill of vision.ocr`. `r2` y `r3` quedan fuera |
| De la DHT | `p1` y `p3`. `p3`, de la otra red, descartado en la card: `timeout` |
| Contactados | 3, su `max_candidates` |
| `dht_requests` | 20 |
| Cobertura | Parcial: `candidate limit`, por `r2` y `r3`; `dht timeout`, porque la búsqueda en la DHT llegó a su plazo de 3 s, con los dos proveedores ya encontrados; y `timeout`, por `p3` |
| Tiempo | 6,0 s: los 3 s de la búsqueda en la DHT y los 3 s de `p3`. Primer candidato válido a los 3031 ms |

Con `dht_places = 0`, como antes de reservar huecos, una búsqueda como la de `c6` no consulta la DHT: sus tres contactos llenan sus tres huecos, con `dht_requests 0` y `candidate limit`. Lo comprueba `contacts_that_fill_every_place_leave_the_dht_alone`, en `crates/pals/tests/dht_search.rs`.

Las líneas del arnés:

```text
RESULTADO caso 1: superado (c0 GET /dht (caso1-c0): routing_peers 0, connections 0, connections_accepted 0;c0 GET /dht (caso1-c0-despues): routing_peers 0, connections 0, connections_accepted 0;cx GET /dht: HTTP 404;cx vision.ocr: ranked [], discarded [('c0', 'card', 'no skill of vision.ocr')], complete, dht_requests 0, contacted 1;)
RESULTADO caso 2: superado (búsquedas 160, conformes 160; pregunta a p1: echo: hola; c6: 1 búsquedas; en caso2b-c6-01, contactos evaluados ['r1'] de ['r1', 'r2', 'r3'], de la DHT ['p1', 'p3'], contactados 3, dht_requests 20, partial (candidate limit, dht timeout, timeout), 6.0 s)
RESULTADO caso 3: superado (conectados: 16 búsquedas, conformes 16, coberturas ['partial (dht timeout, timeout)'];c5: 8 búsquedas; primer candidato a los 11.3 s de arrancar; los dos proveedores de cada capacidad a los 33.5 s; entra por r3: sí;)
RESULTADO caso 4: superado (los routers lo olvidan a los 116.1 s; las búsquedas dejan de devolverlo a los 116.3 s; límite 160 s; 18 sondeos, 72 búsquedas; carga 9.18 13.57 42.26)
RESULTADO caso 5: superado (cobertura inmediata 1.0; cobertura completa a los 0.0 s (límite 80 s); etiquetas {'partial (dht timeout, timeout)': 16}; carga 10.02 11.38 35.80;renovación: a los 166 s de pararlos (todo lo de antes ya caducado a los 160 s), cobertura 1.0 en 16 búsquedas; etiquetas {'partial (dht timeout, timeout)': 16};)
RESULTADO caso 6: superado (r1: bad_contact +1, guarda del rogue 2;r2: bad_contact +1, guarda del rogue 2;r3: bad_contact +1, guarda del rogue 2;r4: bad_contact +1, guarda del rogue 2;)
RESULTADO caso 7: superado (ráfaga de c1: 200 búsquedas, 8 a la vez, en 6.2 s; códigos {'200': 4, '429': 196};refused.streams de cada router durante el caso: {'r1': 0, 'r2': 0, 'r3': 0, 'r4': 0};)
RESULTADO caso 8: superado (r1: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r2: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r3: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r4: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;)
RESULTADO caso 9: superado (c1 y c2 verifican la card y la oferta de p3 en su dirección nueva a los {'c2': 6.5, 'c1': 6.5} s del cambio (límite 80 s); carga 5.83 9.69 32.71)
```

## Resultado (2026-11-17)

Los nueve casos, superados en una sola ejecución completa, a la primera, sobre `8c5ef66` tal como está en el repositorio: el daemon y el arnés con las correcciones de la revisión final de la fase 2 (ver [«Correcciones»](#correcciones-antes-de-esta-ejecución), punto 6). Al acabar no quedó ningún contenedor ni red con la etiqueta de la ejecución. La ejecución anterior, sobre `9ff835f`, antes de esas correcciones, también superó los nueve.

Antes, los ensayos encontraron un fallo del daemon, corregido en `db4d1ee` con su prueba: con un router caído, los proveedores dejaban de publicar y sus registros caducaban en la DHT. Los ensayos y la revisión de la tarea llevaron también a varias correcciones del arnés. La más importante: cinco casos daban por superado un caso cuando su propio guion de comprobación se rompía. Ahora fallan (ver [«Correcciones»](#correcciones-antes-de-esta-ejecución)).

Tres desviaciones, en los casos 2, 3, 5, 7, 8 y 9, comprueban lo que decidió quien dirige el hito y no la letra del plan, porque tal como estaba escrita no se podía cumplir ni observar: los proveedores de la otra red (casos 2, 3, 5 y 9), `held_providers` (caso 8) y la ráfaga (caso 7). El caso 5 comprueba además la renovación (ver [«Dónde se aparta de la redacción del plan»](#dónde-se-aparta-de-la-redacción-del-plan)).

| Caso | Resultado | Evidencia | Tiempo |
|-|-|-|-|
| 1. Sin fuga | Superado | Las dos redes, `Internal=true`. `c0`, aislado, no encuentra a nadie en las 4 capacidades (`dht unavailable`), y su `GET /dht` da 0 pares, 0 conexiones y 0 aceptadas, antes y después de la búsqueda de `cx`. `cx`, sin `[dht]`: `GET /dht` 404, `dht_requests 0`, y llega a `c0` por A2A sin abrirle ninguna conexión `agentnet-kad/0` | 27 s |
| 2. Encontrar un proveedor sin contacto previo | Superado | 160 de 160 búsquedas conformes. El proveedor de la red del cliente, en el ranking desde la DHT con card y oferta verificadas; el de la otra red, descartado en la card (`timeout`). `pals discover --capability vision.ocr --ask "hola"` desde `c1`, con el grant de `p1`: `echo: hola`, código 0, tarea `TASK_STATE_COMPLETED`, un `task.accepted` en `p1` | 2 min 15 s |
| 3. Sin el arranque original | Superado | Con `r1` parado, las 16 búsquedas de los clientes conectados, conformes (`dht timeout, timeout`). `c5` entra por `r3` (`a saved contact is a DHT router: entering through it`): primer candidato a los 18,2 s de arrancar, los dos proveedores de cada capacidad a los 33,2 s. Cada proveedor termina con `r1` parado una vuelta de publicación, las 4 sin error | 1 min 29 s, con la restauración |
| 4. Caducar anuncios | Superado | `p1` parado sin aviso. Los routers olvidan sus 16 registros (4 cada uno) a los 121,3 s, y las búsquedas dejan de devolverlo a los 121,5 s. Límite: 160 s | 2 min 14 s |
| 5. Caída de routers y renovación | Superado | `r2` y `r4` parados. Cobertura inmediata 1, entera a los 0 s (límite 80 s). A los 167 s, con todo lo de antes caducado, cobertura 1 en 16 búsquedas. Las 12 vueltas de publicación terminadas con `r2` y `r4` parados, sin error | 3 min 46 s |
| 6. Contacto con firma inválida | Superado | Los 4 routers: `bad_contact` +1, y guardan del `rogue` 2 registros, su anuncio y su provider record, sin contacto. Los 4 clientes: el `rogue` fuera del ranking y descartado `dht: no valid contact` | 14 s |
| 7. Búsquedas repetidas o maliciosamente amplias | Superado | Ráfaga de 200 desde `c1`, 8 a la vez, en 6,3 s: 8 `200` y 192 `429 too many searches`, ninguna anómala. Las 3 búsquedas de `c3` durante la ráfaga, `200`. Cada router, el mismo proceso en marcha (PID y hora de arranque), `healthz` 200, y `refused.streams` 0 en los cuatro | 14 s |
| 8. Política | Superado | Cada router guarda 4 registros de `p2` antes y después de una vuelta suya: 2 más sus 2 capacidades públicas. `capability_not_announced` 0 → 0. Las 8 búsquedas de `audio.transcribe` (privada) y `data.extraction` (de proyecto) preguntan a la DHT (`dht_requests 4`) y no encuentran a `p2`. Con `exclude_dht` (`--no-dht`), `dht_requests 0`; sin él, 28 | 16 s |
| 9. Cambio de red | Superado | `p3` emite su contacto `seq` 2 con `10.213.73.23:4433` en el segundo en que empieza a moverse, y termina una vuelta sin error. `c1` y `c2` verifican su card y su oferta en la red A a los 6,7 s (límite 80 s). El contacto que `c1` recibió de la DHT es el nuevo | 14 s |

Las líneas del arnés:

```text
RESULTADO caso 1: superado (c0 GET /dht (caso1-c0): routing_peers 0, connections 0, connections_accepted 0;c0 GET /dht (caso1-c0-despues): routing_peers 0, connections 0, connections_accepted 0;cx GET /dht: HTTP 404;cx vision.ocr: ranked [], discarded [('c0', 'card', 'no skill of vision.ocr')], complete, dht_requests 0, contacted 1;)
RESULTADO caso 2: superado (búsquedas 160, conformes 160; pregunta a p1: echo: hola)
RESULTADO caso 3: superado (conectados: 16 búsquedas, conformes 16, coberturas ['partial (dht timeout, timeout)'];c5: 8 búsquedas; primer candidato a los 18.2 s de arrancar; los dos proveedores de cada capacidad a los 33.2 s; entra por r3: sí;)
RESULTADO caso 4: superado (los routers lo olvidan a los 121.3 s; las búsquedas dejan de devolverlo a los 121.5 s; límite 160 s; 19 sondeos, 76 búsquedas; carga 11.42 15.05 35.48)
RESULTADO caso 5: superado (cobertura inmediata 1.0; cobertura completa a los 0.0 s (límite 80 s); etiquetas {'partial (dht timeout, timeout)': 16}; carga 15.67 15.29 31.60;renovación: a los 167 s de pararlos (todo lo de antes ya caducado a los 160 s), cobertura 1.0 en 16 búsquedas; etiquetas {'partial (dht timeout, timeout)': 16};)
RESULTADO caso 6: superado (r1: bad_contact +1, guarda del rogue 2;r2: bad_contact +1, guarda del rogue 2;r3: bad_contact +1, guarda del rogue 2;r4: bad_contact +1, guarda del rogue 2;)
RESULTADO caso 7: superado (ráfaga de c1: 200 búsquedas, 8 a la vez, en 6.3 s; códigos {'200': 8, '429': 192};refused.streams de cada router durante el caso: {'r1': 0, 'r2': 0, 'r3': 0, 'r4': 0};)
RESULTADO caso 8: superado (r1: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r2: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r3: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r4: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;)
RESULTADO caso 9: superado (c1 y c2 verifican la card y la oferta de p3 en su dirección nueva a los {'c1': 6.7, 'c2': 6.7} s del cambio (límite 80 s); carga 21.89 17.61 30.84)
```

### Entorno

| Dato | Valor |
|-|-|
| Fecha | 2026-11-17, de 07:51:18 a 08:08:14 UTC (16 min 56 s, con los 300 s de reposo) |
| Commit | `8c5ef66`, con el árbol limpio (`git status --porcelain` vacío): el daemon y el arnés de esa revisión, con las correcciones de la revisión final de la fase 2. Después solo cambia este registro |
| Binarios | `cargo build --release --locked -p pals` y `--examples` en el anfitrión: `pals 0.1.0` (sha256 `569220f4…`), el mismo fichero en todos los contenedores, y `dht_rogue` (sha256 `1812fad8…`) solo en el del caso 6 |
| Rust | 1.99.1 |
| Versiones fijadas | `iroh =1.4.1`, `libp2p =0.57.1` (`libp2p-kad` 0.49.0, `libp2p-swarm` 0.48.0), `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9` |
| Imagen | `pals-exit-h3:latest` (id `6fa4bcf19e77…`), de [`tests/exit/hito-3/Dockerfile`](../../../tests/exit/hito-3/Dockerfile) sobre `debian:trixie-slim@sha256:a72b4433…` (Debian 13, Python 3.13.5) |
| Anfitrión | Linux x86_64, kernel 6.12 de Debian 13, 32 CPU, compartido con otros proyectos. Carga media al empezar: 14,0 / 31,5 / 60,9, tras la batería de pruebas del daemon y la carga de otros proyectos; durante el reposo, de 10,2 a 21,1, y durante los casos, de 11,4 a 23,4 (la de un minuto). El arnés corre en un entorno Ubuntu 24.04 con Docker 29.4.1 |
| Redes | `pals-exit-h3-red-a` (`10.213.73.0/24`) y `pals-exit-h3-red-b` (`10.213.74.0/24`), las dos `--internal`. `preparar` comprobó que ninguna otra red de Docker se solapaba |
| Contenedores | Doce de la topología y cuatro de sus casos (`c0`, `cx`, `c5`, `rogue`), con una CPU y 512 MiB cada uno. Las direcciones, en el runbook |
| Configuración | La de cada operador, en [`operador-1/`](../../../tests/exit/hito-3/operador-1) y [`operador-2/`](../../../tests/exit/hito-3/operador-2): `relay = false`, `[net] listen = "0.0.0.0:4433"`, `[dht] record_ttl_s = 120` y `republish_s = 40`, el worker `echo` y `[discovery]` por defecto (3 s por candidato) |

El registro completo de la ejecución quedó en la máquina de quien la hizo, sin versionar. Aquí se copia lo que respalda cada resultado:
- los ids de agente se escriben por el nombre del agente, y los `PeerId` como `<PeerId de x>`;
- los ids de grant y de oferta se acortan;
- no se copian tickets ni tokens.

### Dónde se aparta de la redacción del plan

Lo decidió quien dirige el hito antes de la ejecución. El runbook lo explica caso a caso.

1. **Los proveedores de la otra red (casos 2, 3, 5 y 9).**
   - **El plan.** Pide los dos proveedores de cada capacidad «`from_dht`, con oferta verificada».
   - **Por qué no se puede.** Con las capacidades de la fase 1, cada capacidad tiene un proveedor en cada red, y cada cliente está en una sola. Con redes `--internal` y sin relay, A2A entre redes es imposible por construcción.
   - **Lo que se comprueba.** Que los dos salen de la DHT:
     - el de la red del cliente, en el ranking `from_dht`, con su card y su oferta verificadas;
     - el de la otra red, descartado en la etapa `card` por inalcanzable (`timeout` o `card: peer unreachable`), nunca con un motivo `dht: …`. Llegar a esa etapa prueba que la DHT dio su contacto y su anuncio válidos.

     Hay clientes en las dos redes, así que cada proveedor lo verifica algún cliente.
   - **El caso 5.** Su cobertura es la parte de las búsquedas en las que la DHT dio los dos proveedores.
   - **El caso 9.** Usa los clientes de la red A.
2. **`held_providers` (caso 8).**
   - **Por qué no se puede.** `GET /dht` y `pals dht status` no lo exponen.
   - **Lo que se comprueba,** con lo que el daemon expone, sin añadir API:
     - `stored_by_agent` de `p2` en cada router es 2 más sus capacidades públicas;
     - `capability_not_announced` no crece durante una vuelta de `p2`;
     - las búsquedas de sus capacidades privada y de proyecto preguntan a la DHT y no lo encuentran.
3. **La ráfaga (caso 7).**
   - **Por qué no basta.** A 4 a la vez, con el límite de 4 búsquedas del daemon, solo daría `too many searches` por casualidad.
   - **Lo que se comprueba.** Va a 8 a la vez, y exige al menos un `too many searches` y el resto `200`.
4. **La renovación (caso 5).**
   - **Lo que se añade.** Además de la letra del plan, el caso exige cobertura completa en una ronda hecha después de `record_ttl_s + republish_s` con `r2` y `r4` parados, cuando lo que guardan los routers salió de las vueltas hechas sin ellos.
   - **Por qué.** Sin esto, el caso pasa siempre en la primera ronda, porque los routers que quedan guardan aún lo de antes: así pasó en la fase 1 (cobertura inmediata 1, recuperación 0 s). Sin esto, el caso tampoco habría visto el fallo corregido en `db4d1ee`.

### Montaje

`preparar`:
1. Construye la imagen y crea las dos redes, internas.
2. Arranca los agentes en el orden de los operadores:
   1. `r1`, el arranque original, sin `bootstrap`;
   2. `r2` y `r3` por `r1`, con su ticket;
   3. `r4` por `r3`;
   4. los proveedores y los clientes de cada operador por su router de entrada, `r1` o `r3`.

   Los routers declaran sus dos direcciones en `external_addrs`, y su ticket las lleva.
3. A los 4 s cada router guardaba 4 registros de cada proveedor: contacto, anuncio y un provider record de cada una de sus dos capacidades. Estado:

   ```text
   r1 … r4: router, routing_peers 3, connections 8, records 8, provider records 8, por agente {p1: 4, p2: 4, p3: 4, p4: 4}
   p1: client, routing_peers 4, publicación ['vision.ocr', 'knowledge.documentation'] error None
   p2: client, routing_peers 4, publicación ['code.rust.review', 'vision.defect-detection'] error None
   p3: client, routing_peers 4, publicación ['vision.ocr', 'vision.defect-detection'] error None
   p4: client, routing_peers 4, publicación ['code.rust.review', 'knowledge.documentation'] error None
   c1 … c4: client, routing_peers 4
   ```

4. Después, 300 s de reposo medido (ver «Métricas»).

### Caso 1. Sin fuga: superado

```text
pals-exit-h3-red-a: Internal=true
pals-exit-h3-red-b: Internal=true
c0 code.rust.review: ranked 0, discarded 0, partial (dht unavailable), dht_requests 0
c0 knowledge.documentation: ranked 0, discarded 0, partial (dht unavailable), dht_requests 0
c0 vision.defect-detection: ranked 0, discarded 0, partial (dht unavailable), dht_requests 0
c0 vision.ocr: ranked 0, discarded 0, partial (dht unavailable), dht_requests 0
c0 GET /dht (antes de cx): routing_peers 0, connections 0, connections_accepted 0
c0 GET /dht (después de la búsqueda de cx): routing_peers 0, connections 0, connections_accepted 0
cx GET /dht: HTTP 404
cx vision.ocr: ranked [], discarded [('c0', 'card', 'no skill of vision.ocr')], complete, dht_requests 0, contacted 1
```

**`c0`.** Con la DHT encendida y sin arranque ni contactos, no conoce a ningún router. Busca 10 s después de arrancar, como en la fase 1, y no encuentra a nadie. La cobertura dice `dht unavailable`, y la búsqueda no es un error.

**`cx`.** Arranca sin `[dht]` y con `c0` como único contacto guardado. `c0` es el único nodo con la DHT al que nadie más se conecta, así que cualquier conexión `agentnet-kad/0` de `cx` aparecería en su `connections_accepted`. Un nodo con la DHT entra en la red marcando sus contactos guardados. `cx` llegó a `c0` por A2A, para leer su card (`no skill of vision.ocr`), y `c0` siguió con 0 conexiones aceptadas.

### Caso 2. Encontrar un proveedor sin contacto previo: superado

Cada cliente buscó 10 veces cada capacidad, una tras otra, y los cuatro clientes a la vez: 160 búsquedas, todas conformes (`problems` del arnés). Ningún cliente tiene contactos: todo sale de la DHT. La quinta de `c3` de `vision.ocr`, resumida:

```text
ranked:    p3  from_dht, card_signature verified kid <p3>, oferta de <p3> para <c3>, available, skill ocr
discarded: p1  card: timeout
coverage:  partial (timeout)
metrics:   dht_requests 20, contacted 2, messages 2, bytes sent 176, bytes received 3974, first valid 368 ms, matching 5 us, elapsed 3318 ms
```

Cada búsqueda tarda unos 3 s, el plazo por candidato: el proveedor de la otra red no contesta. Las 20 peticiones a la DHT son:
- `GET_PROVIDERS` a los 4 routers;
- el contacto y el anuncio de los 2 proveedores, a cada uno de los 4.

Después, `p1` da un grant del proyecto `demo` a `c1`, por su id de agente, y `c1` pregunta:

```text
$ pals discover --capability vision.ocr --ask "hola"
ranked:
  1. <p1> (agent <p1>): available, skill ocr, time unknown, preferred met 0, benchmark none, completed 0 failed 0, grant held, found in the DHT, card verified <p1>, offer offer-b2d0843b… until 2026-11-17T07:59:52Z
discarded:
  <p3> (card): timeout
coverage: partial (timeout)
metrics: contacted 2, messages 3, bytes sent 961, bytes received 6087, first valid 41 ms, matching 4 us, elapsed 3020 ms
asked: <p1>
echo: hola
código 0
$ pals task ls | tail -n 2
20817ff0-…	TASK_STATE_COMPLETED	<p1>	…
```

En `p1`, un `task.accepted` en su registro de eventos. La pregunta va por el outbox y el gateway de `c1`, con los bindings estándar de A2A sobre `http-over-iroh/v1`. A `p1` se le marca en la dirección de su contacto, que llegó de la DHT.

### Caso 3. Sin el arranque original: superado

Con `r1` parado (`docker kill`), cada cliente buscó cada capacidad una vez: 16 de 16 conformes, todas con cobertura `partial (dht timeout, timeout)`. La búsqueda en la DHT también pregunta a `r1`, que no contesta. Agota sus 3 s, y aun así encuentra los dos proveedores en los demás routers. Cada búsqueda tarda unos 6,0 s: 3 s de la DHT y 3 s del candidato de la otra red.

`c5` arranca con `bootstrap = [r1]` y el ticket de `r3` guardado como contacto. Su registro:

```text
INFO pals::dht::swarm: no DHT bootstrap router answered: trying the saved contacts
INFO pals::dht::swarm: dialing the saved contacts, to find a DHT router among them contacts=1
INFO pals::dht::swarm: a saved contact is a DHT router: entering through it peer=<PeerId de r3>
```

El arnés comprueba que ese `PeerId` es el de `r3`. `c5` tuvo su primer candidato a los 18,2 s de arrancar y los dos proveedores de cada capacidad a los 33,2 s, en 8 búsquedas. Las 4 primeras, mientras aún entraba en la red, no tuvieron candidato (`dht unavailable` o `dht timeout`). En todas, `r3`, su contacto, sale además descartado en la card (`no skill of <capacidad>`): es un router sin servicios.

Las vueltas de publicación cuentan solo si terminaron después de parar `r1` (`last_round_unix`): una de antes no dice nada del router parado. El arnés sigue leyendo el estado de los proveedores hasta que cada uno muestra una así, como mucho 300 s. Cada proveedor terminó una, y las 4 sin error; el caso exige al menos una de cada proveedor, y ninguna con error:

```text
vueltas de publicación terminadas con r1 parado: 4, sin error por proveedor: {'p1': 1, 'p2': 1, 'p3': 1, 'p4': 1}, con error: 0
```

Antes de la corrección `db4d1ee` no era así (ver «Correcciones»).

### Caso 4. Caducar anuncios: superado

`p1` parado sin aviso (`docker kill`). Antes, cada router guardaba de él 4 registros: contacto, anuncio y dos provider records. Con 3 s de pausa entre uno y otro, cada sondeo lee el estado de los routers y hace una búsqueda de una capacidad de `p1` desde cada cliente:

```text
los routers lo olvidan a los 121.3 s; las búsquedas dejan de devolverlo a los 121.5 s; límite 160 s; 19 sondeos, 76 búsquedas; carga 11.42 15.05 35.48
```

**Cómo se mide.**
- Los routers lo olvidan en el primer sondeo con 0 en todos, después del último que guardaba algo.
- Las búsquedas dejan de devolverlo en la primera búsqueda contestada después de la última que lo nombraba, en el ranking o en los descartes.

**Lo que da el tiempo.**
- Caducaron 16 registros.
- Lo publicado vive `record_ttl_s` (120 s) desde la última vuelta que llegó a los routers, como mucho 40 s antes de parar: de 80 a 120 s después.
- Los routers olvidan a `p1` entre el último sondeo que aún guardaba algo y el primero a 0, que fue a los 121,3 s. Los sondeos van cada 6,7 s, más o menos (de 6,6 a 6,8 en esta ejecución): el estado de los cuatro routers y una búsqueda desde cada cliente. La última vuelta de `p1` llegó a los routers justo antes de pararlo.
- Las búsquedas lo dejan de devolver a los 121,5 s, en la ronda de búsquedas de ese mismo sondeo. Las dos medidas caen en el mismo intervalo de 6,7 s: la diferencia entre ellas no dice qué caduca antes.

En la ejecución anterior fueron 121,2 s y 114,7 s; antes, 116,7 s y 116,9 s; en la fase 1, 106,6 s y 99,3 s.

### Caso 5. Caída de routers y renovación: superado

`r2` (operador 1) y `r4` (operador 2), parados. La primera ronda, de 16 búsquedas, ya tuvo cobertura completa, y la recuperación cuenta 0 s, como en la fase 1. Todas las búsquedas tienen cobertura `partial (dht timeout, timeout)` y tardan unos 6 s:
- `libp2p-kad` deja a un router caído en su tabla de rutas mientras no le haga falta el sitio;
- cada búsqueda en la DHT le pregunta y agota sus 3 s;
- aun así, encuentra a los dos proveedores en los demás routers.

La renovación: el arnés esperó, con `r2` y `r4` parados, hasta 160 s después de pararlos, leyendo cada 15 s el estado de los proveedores, e hizo otra ronda:

```text
renovación: a los 167 s de pararlos (todo lo de antes ya caducado a los 160 s), cobertura 1.0 en 16 búsquedas
vueltas de publicación terminadas con r2 y r4 parados: 12, con error: 0
```

Lo que guardaban entonces `r1` y `r3` salió de las vueltas hechas sin `r2` ni `r4`, y bastó. Las vueltas cuentan solo si terminaron después de parar los dos routers: 3 de cada proveedor, cada 40 s. Cuando volvieron a arrancar, los routers tuvieron otra vez todo en 33 s.

### Caso 6. Contacto con firma inválida: superado

[`dht_rogue`](../../../crates/pals/examples/dht_rogue.rs), en la red B y con el ticket de `r3`, publica `vision.ocr` con un contacto firmado y alterado después, y un anuncio válido:

```text
dht_rogue: agent <rogue>, announcing vision.ocr with an altered contact
dht_rogue: round at Some(…), error None, failed [], routing peers 4
r1: bad_contact +1, guarda del rogue 2 (su anuncio y su provider record, sin contacto)
r2: bad_contact +1, guarda del rogue 2
r3: bad_contact +1, guarda del rogue 2
r4: bad_contact +1, guarda del rogue 2
c1: rogue fuera del ranking, descartado card: dht: no valid contact; el resto conforme
c2: rogue fuera del ranking, descartado card: dht: no valid contact; el resto conforme
c3: rogue fuera del ranking, descartado card: dht: no valid contact; el resto conforme
c4: rogue fuera del ranking, descartado card: dht: no valid contact; el resto conforme
```

`dht_rogue` publica tres registros: su contacto, su anuncio y un provider record. Si cada router guarda 2 y cuenta un `bad_contact` más, ninguno guarda el contacto.

El motivo del descarte es `no valid contact`, y no `bad_contact`. Si algún router hubiera servido una copia del contacto alterado, la búsqueda la habría rechazado y dado ese motivo. Así que ningún router se la dio.

### Caso 7. Búsquedas repetidas o maliciosamente amplias: superado

```text
ráfaga de c1: 200 búsquedas, 8 a la vez, en 6.3 s; códigos {'200': 8, '429': 192}; 200 de capacidades que existen 4, con candidato 4; 200 de las que no existen 4, con candidato 0; anómalas ninguna
c3 durante la ráfaga: 3 búsquedas, códigos {'200': 3}; en total 4
refused.streams de cada router durante el caso: {'r1': 0, 'r2': 0, 'r3': 0, 'r4': 0}
daemons (PID:arranque) antes y después, iguales: r1=14:162263616 r2=15:162299045 r3=29:162209558 r4=15:162299128; healthz: 200 los cuatro
```

**Cómo se reparte la ráfaga.**
- Cada búsqueda es de una de 24 capacidades, por turno: las 4 y `example:inexistente-01` a `-20`.
- El daemon de `c1` admite 4 búsquedas a la vez y contesta a las demás, en el acto, `429` con `{"error":"too many searches"}`.
- Las que entraron fueron 8. Cuatro de capacidades que existen, de unos 3 s y 20 peticiones a la DHT cada una (28 la de `vision.ocr`, con los registros del `rogue`, aún vivos); una tardó 6 s porque esperó su sitio entre las consultas del daemon. Y cuatro de capacidades que no existen, sin candidato, de 8 a 56 ms y 4 peticiones cada una: al acabar tan pronto, dejaron sitio a otras.

**Lo que vieron los routers.** No rechazaron ningún stream: el límite de búsquedas del cliente corta la ráfaga antes de que llegue a sus cuotas (8 streams a la vez y 600 por minuto por par). Las búsquedas de `c3`, otro daemon, siguieron y terminaron con `200`. La ráfaga empezó cuando la primera de `c3` había contestado, con la siguiente en marcha.

**Ningún router se cae.** De cada router se lee, antes y después, el PID de su daemon y la hora a la que arrancó el proceso (`/proc/<pid>/stat`), solo si sigue vivo. Un daemon que muriera, o que se arrancara otra vez, no daría lo mismo.

### Caso 8. Política: superado

`p2` tiene, además de sus dos servicios públicos, `audio.transcribe` privado y `data.extraction` del proyecto `demo` ([`p2-inventario.toml`](../../../tests/exit/hito-3/operador-1/p2-inventario.toml)).

```text
p2 publica ['code.rust.review', 'vision.defect-detection']; última vuelta 9 s después de empezar el caso, error None
r1: guarda de p2 4 -> 4 (2 + sus 2 públicas); capability_not_announced 0 -> 0
r2: guarda de p2 4 -> 4; capability_not_announced 0 -> 0
r3: guarda de p2 4 -> 4; capability_not_announced 0 -> 0
r4: guarda de p2 4 -> 4; capability_not_announced 0 -> 0
c1 … c4 buscan audio.transcribe y data.extraction: p2 no, dht_requests 4, ranked 0, discarded 0   (las 8)
c1 vision.ocr con exclude_dht (--no-dht): dht_requests 0, de la DHT 0 en el ranking, 0 descartados
c1 vision.ocr sin exclude_dht: dht_requests 28, de la DHT 1 en el ranking, 2 descartados
$ pals discover --capability vision.ocr --no-dht
ranked:
discarded:
coverage: complete
código 0
```

**Lo que guardan los routers.** Los dos estados se leyeron antes y después de una vuelta entera de `p2`. En los dos, cada router guarda de `p2` justo su contacto, su anuncio y los provider records de sus dos capacidades públicas.

**Si `p2` hubiera publicado las otras.** Si publicara los provider records de las capacidades privada o de proyecto, los routers los rechazarían (`capability_not_announced`), y ese contador sigue en 0. Si los guardaran, las búsquedas de esas capacidades darían con `p2`, y no dan: `GET_PROVIDERS` a los 4 routers (`dht_requests 4`) no devuelve a nadie.

**Las 28 peticiones sin `exclude_dht`.** Incluyen el contacto y el anuncio del `rogue` del caso 6, que aún no habían caducado, descartado `dht: no valid contact`.

### Caso 9. Cambio de red: superado

`p3`, con su daemon en marcha, pasa de la red B a la A: `docker network disconnect` de `pals-exit-h3-red-b` y `docker network connect --ip 10.213.73.23` a `pals-exit-h3-red-a`, 0,2 s en total.

```text
contacto de p3 antes:   seq 1, direct_addrs ["10.213.74.23:4433"]
registro de p3:         08:08:01.983 issued a signed record kind="contact" seq=2
                        08:08:01.984 renewed this agent's records: its addresses changed
contacto de p3 después: seq 2, direct_addrs ["10.213.73.23:4433"], issued_at 08:08:01Z
última vuelta de publicación de p3: en el mismo segundo del cambio, error None
el contacto de p3 que guarda c1, de la DHT: seq 2, ["10.213.73.23:4433"]
antes del cambio, c1 y c2: p3 descartado por inalcanzable (las 4 búsquedas, conformes)
c1 y c2 verifican la card y la oferta de p3 en su dirección nueva a los 6.7 s del cambio (límite 80 s)
```

**El endpoint de Iroh de `p3`.** Está ligado a `0.0.0.0:4433` y vio su dirección nueva dentro del contenedor en el mismo segundo del cambio. `Records::renew` emitió el contacto, y el cambio lanzó una vuelta de publicación.

**`c1` y `c2`.**
- **La primera búsqueda.** Empezó 0,07 s antes de que `p3` emitiera su contacto nuevo, así que aún lo vio en la red B y lo descartó por inalcanzable al cabo de sus 3 s. Descartó también al `rogue` del caso 6 (`dht: no valid contact`), cuyos registros aún no habían caducado: el arnés lo admite solo con un motivo de la DHT.
- **La siguiente.** Empezó 3 s después de acabar la primera y ya tuvo a `p3` en el ranking, desde la DHT, con su card y su oferta verificadas, en unos 55 ms. De ahí los 6,7 s.
- **Los routers.** El contacto `seq` 2 que guarda `c1` solo pudo llegarle de la DHT, así que los routers tenían el nuevo.

## Métricas del §14

Salvo que se diga otra cosa, son las 160 búsquedas del caso 2, con la red entera y en reposo, que da `metricas.py informe`. Las de los demás casos están en su sección.

| Métrica | Valor | Cómo se midió |
|-|-|-|
| Tiempo hasta el primer candidato válido | p50 137 ms, p95 373 ms (de 33 a 498 ms). La búsqueda entera, p50 3087 ms, p95 3318 ms. La mediana baja a lo largo del caso: 170 ms en el primer cuarto de las búsquedas, 47 ms en el último (ver «Observaciones») | `time_to_first_valid_ms` de cada búsqueda, en el reloj monótono del cliente. Toda búsqueda espera los 3 s del proveedor de la otra red (`elapsed_ms`). Con routers caídos, unos 6 s (casos 3 y 5) |
| Porcentaje de búsquedas con candidato | 100 %: 160 de 160 | Respuestas con `ranked` no vacío. En el resto de la ejecución, las búsquedas de capacidades que existen sin candidato fueron solo las esperadas: las de `c0` y `cx` (caso 1), las 4 primeras de `c5`, antes de entrar en la red (caso 3), y las de los clientes de la red A que buscaban a `p1` parado, con el otro proveedor en la red B (caso 4) |
| Peticiones a la DHT por búsqueda | 20 en todas: `GET_PROVIDERS` a los 4 routers, y el contacto y el anuncio de 2 proveedores a cada uno | `metrics.dht_requests`. Una capacidad sin proveedores: 4 (caso 8) |
| Bytes por búsqueda | Por las interfaces del cliente, menos su tráfico en reposo en el mismo tiempo: p50 32 555 B, p95 35 470 B. Las dos primeras de cada cliente, con las conexiones por abrir, hasta 165 kB. De ellos, los mensajes de discovery: 182 B enviados y 3978 B recibidos (p50) | Contadores de `/sys/class/net/*/statistics` dentro del contenedor del cliente, antes y después de cada búsqueda (`metricas.py busca`), menos el ritmo del cliente en el reposo (unos 400 B/s). Incluyen la DHT, la card y la consulta del proveedor de su red, y la sobrecarga de QUIC. En la fase 1, sin cards ni consultas, 27 841 B |
| Candidatos contactados | 2 en todas | `contacted`: los dos proveedores de la capacidad, el de la otra red sin respuesta |
| Anuncios caducados y tiempo | 16 registros de `p1` (4 en cada router); olvidados a los 121,3 s y fuera de las búsquedas a los 121,5 s, límite 160 s | Caso 4 |
| Recuperación | Caso 3, `c5` sin el arranque original: primer candidato a los 18,2 s, todo a los 33,2 s. Caso 5: cobertura inmediata 1, entera a los 0 s, y entera aún a los 167 s con lo renovado. Caso 9: contacto nuevo en el mismo segundo, verificado por los clientes a los 6,7 s | Desde el arranque de `c5`, desde la parada de los routers y desde el inicio del cambio de red |
| Coste del matching | `matching_us` p50 4 µs, p95 5 µs, máximo 6 µs | El tiempo de las dos etapas en el cliente. **No medido:** el coste de la consulta en cada proveedor, que ninguno informa |
| Memoria en reposo, por papel | Routers: RSS medio 39,3 MiB (de 38,2 a 40,4). Proveedores: 37,7 MiB (de 36,8 a 39,0). Clientes: 37,3 MiB (de 36,4 a 39,2) | `VmRSS` del daemon en `/proc/<pid>/status`, 6 muestras en 300 s |
| Conexiones `agentnet-kad/0` en reposo | Routers: 3,2 de media, 8 como mucho. Proveedores: 1,6 de media, 4 como mucho. Clientes: 1,3 de media, 4 como mucho | `connections` de `GET /dht` en esas muestras. Se cierran a los 10 s sin streams. Un router mantiene las de la replicación cada 40 s, y un proveedor, las de su vuelta |
| Tráfico en reposo, por papel | Routers: 320,2 kB/min (de 303,5 a 327,3). Proveedores: 245,9 kB/min (de 242,8 a 250,8). Clientes: 24,5 kB/min (de 23,3 a 26,3) | Bytes por las interfaces, en los dos sentidos, en 300 s. Con `republish_s = 40`: los proveedores publican cada 40 s, y los routers replican cada 40 s y reciben las vueltas, que son casi todo el tráfico de un router o un proveedor. Con los valores por defecto (1200 s) serían 30 veces menos frecuentes. En la fase 1, con los mismos plazos y otro proceso: 355 418 B/min un router y 27 142 B/min un cliente |

## Coste de mantener el adaptador

El adaptador de libp2p sobre el endpoint de Iroh, en `8c5ef66`:

| Fichero | Líneas | De código | De comentario | En blanco |
|-|-|-|-|-|
| [`crates/pals/src/dht/transport.rs`](../../../crates/pals/src/dht/transport.rs) | 643 | 455, de ellas 42 de pruebas unitarias | 139 | 49 |
| [`crates/pals/src/dht/muxer.rs`](../../../crates/pals/src/dht/muxer.rs) | 540 | 383 | 119 | 38 |
| Juntos | 1183 | 838 | 258 | 87 |
| Sus pruebas, [`crates/pals/tests/dht_transport.rs`](../../../crates/pals/tests/dht_transport.rs), la puerta de cualquier cambio de `libp2p` o de `iroh` (H3-D1) | 830 | 688 | 81 | 61 |

La tarea 14 añadió al adaptador 50 líneas de `transport.rs`, la corrección, y 80 de pruebas; la revisión final solo cambió un comentario de `muxer.rs`. Es el tipo de coste que H3-D1 aceptó: el comportamiento de `libp2p-swarm` al marcar (una dirección tras otra) y el de Iroh (un marcado con todas) no encajaban, y solo se vio con un router caído de dos direcciones.

## Filtraciones de metadatos observadas

H3-D4 acepta que cada búsqueda en la DHT diga a los routers qué agente busca qué capacidad. Lo observado:

- **Lo que un router registra, en su nivel por defecto (INFO): nada de las búsquedas.** Tras las 160 búsquedas del caso 2, ninguna línea del registro de ningún router nombra a un cliente, ni por su id de agente ni por su `PeerId` (`r1 0 r2 0 r3 0 r4 0`). `GET /dht` de un router tampoco: `stored_by_agent` nombra a los que publican, no a los que buscan.
- **Con libp2p en `debug`: el agente que busca.** Observación aparte, con el mismo binario, en una ejecución de `preparar` hecha para ello (las órdenes, en el runbook). `r2` volvió a arrancar con `RUST_LOG=info,libp2p_swarm=debug,libp2p_kad=trace`, y `c1` buscó `vision.ocr`. Su registro escribió dos líneas por la conexión de `c1`:

  ```text
  DEBUG libp2p_kad::handler: New inbound connection peer=<PeerId de c1> mode=server
  DEBUG libp2p_swarm: Connection established peer=<PeerId de c1> endpoint=Listener { local_addr: /ip4/0.0.0.0/udp/4433/quic-v1, send_back_addr: /ip4/<ip de c1>/udp/4433/quic-v1 } total_peers=1
  ```

  El `PeerId` es la clave del agente, así que da su id de agente, y además su dirección IP.
- **La clave que pide: no se registra, pero el router la tiene.** Ningún nivel de registro de `libp2p-kad` 0.49, ni `trace`, ni del daemon escribe la clave de una petición `GET_PROVIDERS`: 0 líneas con la de `vision.ocr`. Pero el router la recibe en claro dentro de la conexión, y la usa para contestar.
  - La clave es el SHA-256 de la etiqueta de la capacidad, y [`capabilities-v1.md`](../../extensions/capabilities-v1.md) publica la tabla de claves: se invierte con un diccionario.
  - Después, las peticiones del contacto y del anuncio de cada proveedor encontrado dicen a quién va a consultar el cliente.
  - Un router modificado puede anotar, de cada búsqueda, el agente, la capacidad y la hora.

  Es la filtración que H3-D4 aceptó, y que la plantilla de `pals init` y la descripción de `pals_discover` advierten.
- **Lo que no se filtra.** A la DHT no va nada del inventario más que la clave de las capacidades públicas. En el caso 8, las capacidades privada y de proyecto de `p2` no llegaron a ningún router. Las consultas a los candidatos siguen las reglas del hito 2: un candidato de la DHT sin grant no recibe ni proyecto ni ids.

## Correcciones antes de esta ejecución

Cuatro ensayos, una primera ejecución completa, la revisión de la tarea y la revisión final de la fase 2 llevaron a esta ejecución, cada una en un `PALS_EXIT_DIR` nuevo. Un caso que falló se corrigió y se repitió.

1. **Producto: un router caído hacía fallar las vueltas de publicación (`db4d1ee`).**
   - **El síntoma.** En el primer ensayo, sobre `ad9b9e3`, con `r1` parado en el caso 3, las vueltas de publicación de los proveedores empezaron a fallar con `2 capabilities not provided`, y alguna con `no DHT router answered`. Sus registros caducaron en los routers: `p4` dejó de salir en las búsquedas de `c5` unos 100 s después de parar `r1`, y `p2` poco después. Con `r1` parado, uno de cuatro routers, los proveedores iban saliendo de la DHT.
   - **La causa.** Los registros en `debug` y `trace` de `libp2p` de un proveedor la dieron:
     - libp2p marca las direcciones de un par una tras otra (`with_dial_concurrency_factor(1)`), y cada marcado del transporte lleva ya todas las direcciones del par;
     - un router caído con dos direcciones costaba así 2 × 5 s = 10 s;
     - 10 s es justo lo que libp2p deja ociosa una conexión antes de cerrarla.

     Mientras la búsqueda de los routers más cercanos esperaba a `r1`, las conexiones con los demás quedaban ociosas. Libp2p las cerraba (`KeepAliveTimeout`) en el mismo instante en que el `put` que sigue iba a usarlas, y las peticiones fallaban (`ConnectionLost`). El `put` del anuncio de una capacidad no llegaba a nadie, y la capacidad no se anunciaba.
   - **La corrección.** Un marcado falla en el acto (`Error::Covered`) cuando otro marcado del mismo par, que llevaba también su dirección, falló después de hacerse. Un par caído cuesta un plazo de marcado, no uno por dirección. [`agentnet-kad-0.md`](../../protocols/agentnet-kad-0.md#marcar) lo explica.
   - **La prueba.** `dht_transport::a_peer_down_costs_one_dial_whatever_its_addresses` falla sin la corrección, porque el segundo marcado espera su propio plazo. Comprueba también que un marcado nuevo vuelve a intentarlo, y que uno de una dirección que el fallido no llevaba sigue intentándolo.
   - **Los checks.** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace --locked`, en verde.

   En esta ejecución, las 4 vueltas de los proveedores terminadas con `r1` parado y las 12 con `r2` y `r4` parados terminaron sin error.
2. **Producto, la primera versión de esa corrección.** Hacía fallar cualquier marcado posterior a un fallo del mismo par. En el segundo ensayo, la red no llegó a asentarse: los routers no tenían los registros de los proveedores del otro operador. Un marcado puede llevar menos direcciones de las que libp2p prueba, cuando la libreta de direcciones aún no las conoce todas. Desde entonces solo cuenta el fallo de un marcado que llevaba esa dirección, y la prueba lo cubre.
3. **Arnés.**
   - **Caso 3.** `c5` tiene a `r3` como contacto, y su búsqueda lo descarta en la card (`no skill of …`); el arnés lo contaba como un candidato de más. Además, al recrear el contenedor de `c5` el arnés seguía usando el id de agente del anterior.
   - **Caso 5.** Se añadió la comprobación de la renovación y la lectura de las vueltas de publicación de los proveedores, también en el caso 3.
   - **Caso 9.** En el cuarto ensayo el caso falló porque el arnés tomaba la hora del cambio cuando volvía `docker network connect`, y para entonces `p3` ya había emitido y publicado su contacto nuevo. Ahora el cambio cuenta desde antes del `disconnect`, y el caso comprueba también que `c1` recibe de la DHT el contacto nuevo.
   - **Las búsquedas de prueba.** Se guardan con el nodo y el cuerpo, y los registros de los routers se revisan tras el caso 2.
4. **Una ejecución interrumpida.** El tercer ensayo coincidió con la batería de pruebas del daemon y con otra carga del anfitrión: la carga llegó a 224, y un `docker cp` se quedó parado en `preparar`. Se interrumpió con `SIGTERM`. El arnés desmontó todo lo suyo (`quedan: 0 contenedores, 0 redes`), y comprobó así la protección de las ejecuciones interrumpidas.
5. **Arnés, tras la revisión de la tarea (`9ff835f`).**
   - **La primera ejecución completa.** Se hizo con el arnés de `29f8c5b`, aún sin commit, sobre `db4d1ee`, entre las 02:41 y las 02:57 UTC, y superó los nueve casos. Sus líneas `RESULTADO` muestran que cada guion de comprobación llegó al final.
   - **El fallo.** La revisión encontró que cinco casos (1, 3, 6, 8 y 9) daban por superado un caso cuando su propio guion de Python se rompía. Juzgaban por la ausencia de líneas `PROBLEMA`, y un guion roto no escribe ninguna. Una búsqueda guardada vacía, por un `docker exec` fallido, rompía el guion. Lo reprodujo con el caso 6 y el caso 9.
   - **Fallar cerrado.** Ahora cada caso se supera solo si su guion termina con estado 0 y escribe `OK`. Un fichero de búsqueda vacío o que no es una búsqueda queda como una búsqueda sin respuesta: con su nodo, sacado del nombre, y ordenada la última. Una búsqueda sin respuesta es un problema del caso. Comprobado aparte, con los ficheros de la primera ejecución:
     - con un fichero vacío, el caso 6 y el caso 9 fallan;
     - con una línea rota en `estados.jsonl`, el guion del caso 6 se rompe, y el caso falla.
   - **Las demás correcciones de la revisión:**
     - en el caso 7, el daemon de cada router se reconoce por su PID y la hora de arranque de un proceso vivo, no por el contenido de `daemon.pid`, y la ráfaga empieza cuando `c3` ya busca;
     - una restauración que no consigue que los routers vuelvan a guardarlo todo hace fallar el caso que paró a los agentes;
     - un `PeerId` que no se lee hace fallar el caso, en lugar de valer para cualquier línea;
     - el caso 3 exige que ninguna vuelta de publicación de los proveedores falle con `r1` parado;
     - la topología se define una vez en el arnés y llega a los guiones como JSON;
     - la nota de procedencia de `preparar` usa `git status --porcelain`, que también ve los ficheros sin seguimiento.

     La ejecución anterior, sobre `9ff835f`, fue la primera con el arnés en un commit, sin cambios en el árbol. Superó los nueve casos, entre las 03:27 y las 03:43 UTC.
6. **Producto y arnés, tras la revisión final de la fase 2 (de `e8376be` a `8c5ef66`).** Esta ejecución es la primera tras esas correcciones. Las del daemon:
   - un cliente ya no entra en la tabla de rutas de un router que lo marcó (`BucketInserts::Manual`);
   - una vuelta que toca mientras otra sigue espera a que termine, y una sustituida dice lo que dejó sin anunciar;
   - una búsqueda cuyos contactos llenan `max_candidates` no consulta la DHT;
   - `dht_requests` se omite cuando es 0;
   - un router guarda 8 direcciones como mucho por provider record, y ninguna copia más allá de su firma;
   - el contacto de un proveedor se marca solo en direcciones que se pueden marcar y con un relay `https`;
   - se borran los registros de pares olvidados;
   - la clave de un registro sale acortada y escapada en el registro.

   Las del arnés: los casos 3 y 5 cuentan solo las vueltas terminadas después de parar los routers, y el 3 espera, como mucho 300 s, a que cada proveedor muestre una sin error. Cada caso borra al empezar las búsquedas de una ejecución anterior suya. Los casos 1 y 8 leen `dht_requests` con `.get(…, 0)`.

## Observaciones

1. **Cada búsqueda espera al proveedor de la otra red.**
   - Con los 3 s por candidato por defecto, toda búsqueda tarda 3 s y dice `partial (timeout)`, aunque el primer candidato válido llegue antes: en esta ejecución, p50 137 ms. Es la topología: el contacto de un proveedor de la otra red lleva una dirección que el cliente no puede alcanzar.
   - En redes reales, con `relay = true` o con rutas entre las redes, ese candidato contestaría. Con direcciones privadas de otras redes en los contactos, el plazo por candidato es el coste.
2. **Un router caído retrasa cada búsqueda 3 s.** `libp2p-kad` 0.49 no quita de la tabla de rutas a un par que no contesta mientras no le haga falta el sitio, y con 4 routers nunca le hace falta. Mientras el router está parado, cada búsqueda en la DHT le pregunta y agota sus 3 s (`dht timeout`). Aun así, encuentra a los proveedores en los demás (casos 3 y 5).
3. **La ráfaga no llega a los routers.** El límite de 4 búsquedas de un daemon recorta una ráfaga antes de que pese en las cuotas de los routers, y `refused.streams` se quedó en 0. Lo que esas cuotas aguantan lo prueban las pruebas de integración (`a_round_of_32_capabilities_fits_the_routers_quotas` y los límites de `dht_transport`), no esta ráfaga.
4. **El cambio de red se ve en el acto.** Dentro del contenedor, Iroh vio la interfaz nueva y la dirección nueva en el mismo segundo. El contacto nuevo llegó a los routers antes de que los clientes buscaran.
5. **El tráfico en reposo depende de `republish_s`.** Con los plazos de la fase 1 (40 s), un router mueve unos 320 kB/min y un proveedor 246 kB/min. Con los valores por defecto (`republish_s = 1200`), las vueltas y la replicación serían 30 veces menos frecuentes.
6. **La carga del anfitrión**, de 11,4 a 23,4 durante los casos, no cortó ninguna búsqueda por plazos más allá de los esperados. El primer candidato válido tardó más que en las ejecuciones anteriores, p50 137 ms y p95 373 ms frente a 42 ms y 178 ms (con la carga de 3,4 a 9,3) y 44 ms y 67 ms (de 6,6 a 27,4). No es un coste fijo: la mediana bajó durante el caso 2 de 170 ms en el primer cuarto de las búsquedas a 47 ms en el último, con la misma red y las mismas peticiones, como en las ejecuciones anteriores. Los tiempos varían de una ejecución a otra sin relación clara con la carga.

## Lo que no cubre

- **Escala.** Hay cuatro routers y cuatro proveedores. Con las 20 réplicas por clave de `libp2p-kad`, cada router guarda todo, y no se ejercita ni el encaminamiento en una red grande ni la elección de los routers más cercanos.
- **Sybil.** No hay muchas identidades de un mismo atacante, ni rutas envenenadas, ni un router que mienta en sus respuestas. El caso 6 es un solo agente con un registro mal firmado.
- **NAT de operador.** Las redes son puentes de Docker sin NAT entre ellas, y los routers son alcanzables directamente desde las dos. No hay operadores distintos de verdad: relojes, versiones y políticas de red son los mismos (H3-D2).
- **Máquinas físicas.** Todos los agentes comparten kernel, reloj y máquina, y los relojes nunca difieren. El desfase de 60 s de los registros lo prueban los tests (`records_follow_the_clock_skew`).
- **La búsqueda con `relay = true`.** Todos los daemons van sin relay, así que un cliente no alcanza a un proveedor de la otra red. No se mide cómo cambia el plazo por candidato, ni la búsqueda, con relays.
- **`held_providers`.** Se observa a través del estado y del protocolo, no directamente (ver «Dónde se aparta…»).
- **Una vuelta de publicación más larga que `republish_s`.** La revisión final la encontró, y `09f266b` la corrigió: el temporizador ya no corta una vuelta en curso. La prueban dos pruebas unitarias (`publish.rs`); esta ejecución no provoca ninguna vuelta así, y todas las vueltas observadas terminaron sin error.
