# Prueba de salida del hito 3

Este documento recoge la ejecución de los nueve casos del criterio de salida del hito 3, el descubrimiento con Kademlia (spec §14). Sigue el runbook [`docs/runbooks/hito-3-descubrimiento-kademlia.md`](../../runbooks/hito-3-descubrimiento-kademlia.md):
- dos operadores y dos redes;
- un contenedor Docker por agente;
- la DHT `agentnet-kad/0` del daemon, sin relay.

La ejecuta el arnés [`tests/exit/hito-3.sh`](../../../tests/exit/README.md#prueba-de-salida-del-hito-3-con-docker).

**No es una prueba en varias máquinas físicas.** Los agentes comparten kernel, reloj y máquina. Entre ellos solo hay dos redes de Docker `--internal` (ver [«Lo que no cubre»](#lo-que-no-cubre)). Lo decidió H3-D2 ([registro](../../decisions/2026-11-16-hito-3-decisiones.md#h3-d2-quién-opera-los-puntos-de-arranque-y-los-routers-de-la-prueba)): el agente que implementa el hito opera los dos operadores, cada uno con sus identidades, su configuración y sus puntos de arranque. Una prueba con Docker cierra un hito (D2 del cierre de los hitos 1 y 2).

## Resultado (2026-11-17)

Los nueve casos, superados en una sola ejecución completa, a la primera. Al acabar no quedó ningún contenedor ni red con la etiqueta de la ejecución.

Antes, los ensayos encontraron un fallo del daemon, corregido en `db4d1ee` con su prueba: con un router caído, los proveedores dejaban de publicar y sus registros caducaban en la DHT. También llevaron a varias correcciones del arnés (ver [«Correcciones»](#correcciones-antes-de-esta-ejecución)).

Tres desviaciones, en los casos 2, 3, 5, 7, 8 y 9, comprueban lo que decidió quien dirige el hito y no la letra del plan, porque tal como estaba escrita no se podía cumplir ni observar: los proveedores de la otra red (casos 2, 3, 5 y 9), `held_providers` (caso 8) y la ráfaga (caso 7). El caso 5 comprueba además la renovación (ver [«Dónde se aparta de la redacción del plan»](#dónde-se-aparta-de-la-redacción-del-plan)).

| Caso | Resultado | Evidencia | Tiempo |
|-|-|-|-|
| 1. Sin fuga | Superado | Las dos redes, `Internal=true`. `c0`, aislado, no encuentra a nadie en las 4 capacidades (`dht unavailable`), y su `GET /dht` da 0 pares, 0 conexiones y 0 aceptadas, antes y después de la búsqueda de `cx`. `cx`, sin `[dht]`: `GET /dht` 404, `dht_requests 0`, y llega a `c0` por A2A sin abrirle ninguna conexión `agentnet-kad/0` | 25 s |
| 2. Encontrar un proveedor sin contacto previo | Superado | 160 de 160 búsquedas conformes. El proveedor de la red del cliente, en el ranking desde la DHT con card y oferta verificadas; el de la otra red, descartado en la card (`timeout`). `pals discover --capability vision.ocr --ask "hola"` desde `c1`, con el grant de `p1`: `echo: hola`, código 0, tarea `TASK_STATE_COMPLETED`, un `task.accepted` en `p1` | 2 min 11 s |
| 3. Sin el arranque original | Superado | Con `r1` parado, las 16 búsquedas de los clientes conectados, conformes (`dht timeout, timeout`). `c5` entra por `r3` (`a saved contact is a DHT router: entering through it`): primer candidato a los 12,7 s de arrancar, los dos proveedores de cada capacidad a los 34,5 s. Las 4 vueltas de publicación de los proveedores con `r1` parado, sin error | 1 min 38 s, con la restauración |
| 4. Caducar anuncios | Superado | `p1` parado sin aviso. Los routers olvidan sus 16 registros (4 cada uno) a los 116,7 s, y las búsquedas dejan de devolverlo a los 116,9 s. Límite: 160 s | 2 min 2 s |
| 5. Caída de routers y renovación | Superado | `r2` y `r4` parados. Cobertura inmediata 1, entera a los 0 s (límite 80 s). A los 166 s, con todo lo de antes caducado, cobertura 1 en 16 búsquedas. 16 vueltas de publicación sin error | 3 min 20 s |
| 6. Contacto con firma inválida | Superado | Los 4 routers: `bad_contact` +1, y guardan del `rogue` 2 registros, su anuncio y su provider record, sin contacto. Los 4 clientes: el `rogue` fuera del ranking y descartado `dht: no valid contact` | 13 s |
| 7. Búsquedas repetidas o maliciosamente amplias | Superado | Ráfaga de 200 desde `c1`, 8 a la vez, en 3,4 s: 5 `200` y 195 `429 too many searches`, ninguna anómala. Las 2 búsquedas de `c3` durante la ráfaga, `200`. Los mismos PID de los routers, `healthz` 200, y `refused.streams` 0 en los cuatro | 8 s |
| 8. Política | Superado | Cada router guarda 4 registros de `p2` antes y después de una vuelta suya: 2 más sus 2 capacidades públicas. `capability_not_announced` 0 → 0. Las 8 búsquedas de `audio.transcribe` (privada) y `data.extraction` (de proyecto) preguntan a la DHT (`dht_requests 4`) y no encuentran a `p2`. Con `exclude_dht` (`--no-dht`), `dht_requests 0`; sin él, 28 | 18 s |
| 9. Cambio de red | Superado | `p3` emite su contacto `seq` 2 con `10.213.73.23:4433` en el segundo en que empieza a moverse, y termina una vuelta sin error. `c1` y `c2` verifican su card y su oferta en la red A a los 6,8 s (límite 80 s). El contacto que `c1` recibió de la DHT es el nuevo | 14 s |

Las líneas del arnés:

```text
RESULTADO caso 1: superado (c0 GET /dht (caso1-c0): routing_peers 0, connections 0, connections_accepted 0;c0 GET /dht (caso1-c0-despues): routing_peers 0, connections 0, connections_accepted 0;cx GET /dht: HTTP 404;cx vision.ocr: ranked [], discarded [('c0', 'card', 'no skill of vision.ocr')], complete, dht_requests 0, contacted 1;)
RESULTADO caso 2: superado (búsquedas 160, conformes 160; pregunta a p1: echo: hola)
RESULTADO caso 3: superado (conectados: 16 búsquedas, conformes 16, coberturas ['partial (dht timeout, timeout)'];c5: 8 búsquedas; primer candidato a los 12.7 s de arrancar; los dos proveedores de cada capacidad a los 34.5 s; entra por r3: sí;)
RESULTADO caso 4: superado (los routers lo olvidan a los 116.7 s; las búsquedas dejan de devolverlo a los 116.9 s; límite 160 s; 18 sondeos, 72 búsquedas; carga 10.97 12.76 20.03)
RESULTADO caso 5: superado (cobertura inmediata 1.0; cobertura completa a los 0.0 s (límite 80 s); etiquetas {'partial (dht timeout, timeout)': 16}; carga 9.17 11.34 18.12;renovación: a los 166 s de pararlos (todo lo de antes ya caducado a los 160 s), cobertura 1.0 en 16 búsquedas; etiquetas {'partial (dht timeout, timeout)': 16};)
RESULTADO caso 6: superado (r1: bad_contact +1, guarda del rogue 2;r2: bad_contact +1, guarda del rogue 2;r3: bad_contact +1, guarda del rogue 2;r4: bad_contact +1, guarda del rogue 2;)
RESULTADO caso 7: superado (ráfaga de c1: 200 búsquedas, 8 a la vez, en 3.4 s; códigos {'200': 5, '429': 195};refused.streams de cada router durante el caso: {'r1': 0, 'r2': 0, 'r3': 0, 'r4': 0};)
RESULTADO caso 8: superado (r1: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r2: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r3: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;r4: guarda de p2 4 -> 4; capability_not_announced 0 -> 0;)
RESULTADO caso 9: superado (c1 y c2 verifican la card y la oferta de p3 en su dirección nueva a los {'c1': 6.8, 'c2': 6.8} s del cambio (límite 80 s); carga 6.56 10.34 17.36)
```

### Entorno

| Dato | Valor |
|-|-|
| Fecha | 2026-11-17, de 02:41:29 a 02:57:42 UTC (16 min 13 s, con los 300 s de reposo) |
| Commit | `db4d1ee`, con la corrección del daemon, y el arnés y los documentos de esta tarea aún sin commit: son los del commit que añade este registro. `preparar` lo anota como «con cambios sin commit». El daemon que corrió es el de `db4d1ee` |
| Binarios | `cargo build --release --locked -p pals` y `--examples` en el anfitrión: `pals 0.1.0` (sha256 `06c3b9e2…`), el mismo fichero en todos los contenedores, y `dht_rogue` (sha256 `c9fc0924…`) solo en el del caso 6 |
| Rust | 1.99.1 |
| Versiones fijadas | `iroh =1.4.1`, `libp2p =0.57.1` (`libp2p-kad` 0.49.0, `libp2p-swarm` 0.48.0), `iroh-blobs =0.105.0`, `a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, `a2a-client-lf =0.2.9` |
| Imagen | `pals-exit-h3:latest` (id `6fa4bcf19e77…`), de [`tests/exit/hito-3/Dockerfile`](../../../tests/exit/hito-3/Dockerfile) sobre `debian:trixie-slim@sha256:a72b4433…` (Debian 13, Python 3.13.5) |
| Anfitrión | Linux x86_64, kernel 6.12 de Debian 13, 32 CPU, compartido con otros proyectos. Carga media al empezar: 27,4 / 26,5 / 28,9; al empezar los casos, 15,0, y durante ellos, de 6,6 a 15,6. El arnés corre en un entorno Ubuntu 24.04 con Docker 29.4.1 |
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
metrics:   dht_requests 20, contacted 2, messages 2, bytes sent 176, bytes received 3974, first valid 45 ms, matching 3 us, elapsed 3033 ms
```

Cada búsqueda tarda unos 3 s, el plazo por candidato: el proveedor de la otra red no contesta. Las 20 peticiones a la DHT son:
- `GET_PROVIDERS` a los 4 routers;
- el contacto y el anuncio de los 2 proveedores, a cada uno de los 4.

Después, `p1` da un grant del proyecto `demo` a `c1`, por su id de agente, y `c1` pregunta:

```text
$ pals discover --capability vision.ocr --ask "hola"
ranked:
  1. <p1> (agent <p1>): available, skill ocr, time unknown, preferred met 0, benchmark none, completed 0 failed 0, grant held, found in the DHT, card verified <p1>, offer offer-124b5734… until 2026-11-17T02:49:50Z
discarded:
  <p3> (card): timeout
coverage: partial (timeout)
metrics: contacted 2, messages 3, bytes sent 961, bytes received 6087, first valid 52 ms, matching 3 us, elapsed 3032 ms
asked: <p1>
echo: hola
código 0
$ pals task ls | tail -n 2
965257d0-…	TASK_STATE_COMPLETED	<p1>	…
```

En `p1`, un `task.accepted` en su registro de eventos. La pregunta va por el outbox y el gateway de `c1`, con los bindings estándar de A2A sobre `http-over-iroh/v1`. A `p1` se le marca en la dirección de su contacto, que llegó de la DHT.

### Caso 3. Sin el arranque original: superado

Con `r1` parado (`docker kill`), cada cliente buscó cada capacidad una vez: 16 de 16 conformes, todas con cobertura `partial (dht timeout, timeout)`. La búsqueda en la DHT también pregunta a `r1`, que no contesta. Agota sus 3 s, y aun así encuentra los dos proveedores en los demás routers. Cada búsqueda tarda unos 6,8 s: 3 s de la DHT y 3 s del candidato de la otra red.

`c5` arranca con `bootstrap = [r1]` y el ticket de `r3` guardado como contacto. Su registro:

```text
INFO pals::dht::swarm: no DHT bootstrap router answered: trying the saved contacts
INFO pals::dht::swarm: dialing the saved contacts, to find a DHT router among them contacts=1
INFO pals::dht::swarm: a saved contact is a DHT router: entering through it peer=<PeerId de r3>
```

El arnés comprueba que ese `PeerId` es el de `r3`. `c5` tuvo su primer candidato a los 12,7 s de arrancar y los dos proveedores de cada capacidad a los 34,5 s, en 8 búsquedas. En ellas, `r3`, su contacto, sale además descartado en la card (`no skill of <capacidad>`): es un router sin servicios.

Mientras `r1` estuvo parado, las 4 vueltas de publicación de los proveedores terminaron sin error. Antes de la corrección `db4d1ee` no era así (ver «Correcciones»).

### Caso 4. Caducar anuncios: superado

`p1` parado sin aviso (`docker kill`). Antes, cada router guardaba de él 4 registros: contacto, anuncio y dos provider records. Cada 3 s, el estado de los routers y una búsqueda de una capacidad de `p1` desde cada cliente:

```text
los routers lo olvidan a los 116.7 s; las búsquedas dejan de devolverlo a los 116.9 s; límite 160 s; 18 sondeos, 72 búsquedas; carga 10.97 12.76 20.03
```

**Cómo se mide.**
- Los routers lo olvidan en el primer sondeo con 0 en todos, después del último que guardaba algo.
- Las búsquedas dejan de devolverlo en la primera búsqueda contestada después de la última que lo nombraba, en el ranking o en los descartes.

**Lo que da el tiempo.**
- Caducaron 16 registros.
- Lo publicado vive `record_ttl_s` (120 s) desde la última vuelta que llegó a los routers, como mucho 40 s antes de parar.
- 116,7 s cae dentro de esa ventana, de 80 a 120 s.

En la fase 1 fueron 106,6 s y 99,3 s.

### Caso 5. Caída de routers y renovación: superado

`r2` (operador 1) y `r4` (operador 2), parados. La primera ronda, de 16 búsquedas, ya tuvo cobertura completa, y la recuperación cuenta 0 s, como en la fase 1. Todas las búsquedas tienen cobertura `partial (dht timeout, timeout)` y tardan unos 6 s:
- `libp2p-kad` deja a un router caído en su tabla de rutas mientras no le haga falta el sitio;
- cada búsqueda en la DHT le pregunta y agota sus 3 s;
- aun así, encuentra a los dos proveedores en los demás routers.

La renovación: el arnés esperó, con `r2` y `r4` parados, hasta 160 s después de pararlos, leyendo cada 15 s el estado de los proveedores, e hizo otra ronda:

```text
renovación: a los 166 s de pararlos (todo lo de antes ya caducado a los 160 s), cobertura 1.0 en 16 búsquedas
vueltas de publicación vistas con r2 y r4 parados: 16, con error: 0
```

Lo que guardaban entonces `r1` y `r3` salió de las vueltas hechas sin `r2` ni `r4`, y bastó. Cuando volvieron a arrancar, los routers tuvieron otra vez todo en 8 s.

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
ráfaga de c1: 200 búsquedas, 8 a la vez, en 3.4 s; códigos {'200': 5, '429': 195}; 200 de capacidades que existen 3, con candidato 3; 200 de las que no existen 2, con candidato 0; anómalas ninguna
c3 durante la ráfaga: 2 búsquedas, códigos {'200': 2}
refused.streams de cada router durante el caso: {'r1': 0, 'r2': 0, 'r3': 0, 'r4': 0}
PID antes: r1=15 r2=15 r3=29 r4=15; después: los mismos; healthz: 200 los cuatro
```

**Cómo se reparte la ráfaga.**
- Cada búsqueda es de una de 24 capacidades, por turno: las 4 y `example:inexistente-01` a `-20`.
- El daemon de `c1` admite 4 búsquedas a la vez y contesta a las demás, en el acto, `429` con `{"error":"too many searches"}`.
- Las que entraron fueron 3 de capacidades que existen (unos 3,2 s y 20 peticiones a la DHT cada una) y 2 de las que no existen (9 y 106 ms y 4 peticiones).

**Lo que vieron los routers.** No rechazaron ningún stream: el límite de búsquedas del cliente corta la ráfaga antes de que llegue a sus cuotas (8 streams a la vez y 600 por minuto por par). Las búsquedas de `c3`, otro daemon, siguieron y terminaron con `200`.

### Caso 8. Política: superado

`p2` tiene, además de sus dos servicios públicos, `audio.transcribe` privado y `data.extraction` del proyecto `demo` ([`p2-inventario.toml`](../../../tests/exit/hito-3/operador-1/p2-inventario.toml)).

```text
p2 publica ['code.rust.review', 'vision.defect-detection']; última vuelta 11 s después de empezar el caso, error None
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

`p3`, con su daemon en marcha, pasa de la red B a la A: `docker network disconnect` de `pals-exit-h3-red-b` y `docker network connect --ip 10.213.73.23` a `pals-exit-h3-red-a`, 0,3 s en total.

```text
contacto de p3 antes:   seq 1, direct_addrs ["10.213.74.23:4433"]
registro de p3:         02:57:24.666 issued a signed record kind="contact" seq=2
                        02:57:24.667 renewed this agent's records: its addresses changed
contacto de p3 después: seq 2, direct_addrs ["10.213.73.23:4433"], issued_at 02:57:24Z
última vuelta de publicación de p3: en el mismo segundo del cambio, error None
el contacto de p3 que guarda c1, de la DHT: seq 2, ["10.213.73.23:4433"]
antes del cambio, c1 y c2: p3 descartado por inalcanzable (las 4 búsquedas, conformes)
c1 y c2 verifican la card y la oferta de p3 en su dirección nueva a los 6.8 s del cambio (límite 80 s)
```

**El endpoint de Iroh de `p3`.** Está ligado a `0.0.0.0:4433` y vio su dirección nueva dentro del contenedor en el mismo segundo del cambio. `Records::renew` emitió el contacto, y el cambio lanzó una vuelta de publicación.

**`c1` y `c2`.**
- **La primera búsqueda.** Empezó 0,1 s antes de que `p3` emitiera su contacto nuevo, así que aún lo vio en la red B y lo descartó por inalcanzable al cabo de sus 3 s. Descartó también al `rogue` del caso 6 (`dht: no valid contact`), cuyos registros aún no habían caducado: el arnés lo admite solo con un motivo de la DHT.
- **La siguiente.** Empezó 3 s después y ya tuvo a `p3` en el ranking, desde la DHT, con su card y su oferta verificadas, en 0,18 s. De ahí los 6,8 s.
- **Los routers.** El contacto `seq` 2 que guarda `c1` solo pudo llegarle de la DHT, así que los routers tenían el nuevo.

## Métricas del §14

Salvo que se diga otra cosa, son las 160 búsquedas del caso 2, con la red entera y en reposo, que da `metricas.py informe`. Las de los demás casos están en su sección.

| Métrica | Valor | Cómo se midió |
|-|-|-|
| Tiempo hasta el primer candidato válido | p50 44 ms, p95 67 ms (de 30 a 134 ms). La búsqueda entera, p50 3032 ms, p95 3055 ms | `time_to_first_valid_ms` de cada búsqueda, en el reloj monótono del cliente. Toda búsqueda espera los 3 s del proveedor de la otra red (`elapsed_ms`). Con routers caídos, unos 6 s (casos 3 y 5) |
| Porcentaje de búsquedas con candidato | 100 %: 160 de 160 | Respuestas con `ranked` no vacío. En el resto de la ejecución, las búsquedas de capacidades que existen sin candidato fueron solo las esperadas: las de `c0` y `cx` (caso 1), las dos primeras de `c5`, antes de entrar en la red (caso 3), y las de los clientes de la red A que buscaban a `p1` parado, con el otro proveedor en la red B (caso 4) |
| Peticiones a la DHT por búsqueda | 20 en todas: `GET_PROVIDERS` a los 4 routers, y el contacto y el anuncio de 2 proveedores a cada uno | `metrics.dht_requests`. Una capacidad sin proveedores: 4 (caso 8) |
| Bytes por búsqueda | Por las interfaces del cliente, menos su tráfico en reposo en el mismo tiempo: p50 32 747 B, p95 35 158 B. Las dos primeras de cada cliente, con las conexiones por abrir, de 87 a 166 kB. De ellos, los mensajes de discovery: 182 B enviados y 3978 B recibidos (p50) | Contadores de `/sys/class/net/*/statistics` dentro del contenedor del cliente, antes y después de cada búsqueda (`metricas.py busca`), menos el ritmo del cliente en el reposo (unos 400 B/s). Incluyen la DHT, la card y la consulta del proveedor de su red, y la sobrecarga de QUIC. En la fase 1, sin cards ni consultas, 27 841 B |
| Candidatos contactados | 2 en todas | `contacted`: los dos proveedores de la capacidad, el de la otra red sin respuesta |
| Anuncios caducados y tiempo | 16 registros de `p1` (4 en cada router); olvidados a los 116,7 s y fuera de las búsquedas a los 116,9 s, límite 160 s | Caso 4 |
| Recuperación | Caso 3, `c5` sin el arranque original: primer candidato a los 12,7 s, todo a los 34,5 s. Caso 5: cobertura inmediata 1, entera a los 0 s, y entera aún a los 166 s con lo renovado. Caso 9: contacto nuevo en el mismo segundo, verificado por los clientes a los 6,8 s | Desde el arranque de `c5`, desde la parada de los routers y desde el inicio del cambio de red |
| Coste del matching | `matching_us` p50 3 µs, p95 4 µs, máximo 5 µs | El tiempo de las dos etapas en el cliente. **No medido:** el coste de la consulta en cada proveedor, que ninguno informa |
| Memoria en reposo, por papel | Routers: RSS medio 40,0 MiB (de 38,7 a 40,5). Proveedores: 37,7 MiB (de 37,6 a 37,8). Clientes: 37,2 MiB (de 36,9 a 37,4) | `VmRSS` del daemon en `/proc/<pid>/status`, 6 muestras en 300 s |
| Conexiones `agentnet-kad/0` en reposo | Routers: 2,8 de media, 8 como mucho. Proveedores y clientes: 1,3 de media, 4 como mucho | `connections` de `GET /dht` en esas muestras. Se cierran a los 10 s sin streams. Un router mantiene las de la replicación cada 40 s, y un proveedor, las de su vuelta |
| Tráfico en reposo, por papel | Routers: 314,8 kB/min (de 297,7 a 324,6). Proveedores: 242,6 kB/min (de 236,9 a 248,0). Clientes: 24,2 kB/min (de 22,7 a 26,3) | Bytes por las interfaces, en los dos sentidos, en 300 s. Con `republish_s = 40`: los proveedores publican cada 40 s, y los routers replican cada 40 s y reciben las vueltas, que son casi todo el tráfico de un router o un proveedor. Con los valores por defecto (1200 s) serían 30 veces menos frecuentes. En la fase 1, con los mismos plazos y otro proceso: 355 418 B/min un router y 27 142 B/min un cliente |

## Coste de mantener el adaptador

El adaptador de libp2p sobre el endpoint de Iroh, después de la corrección `db4d1ee`:

| Fichero | Líneas | De código | De comentario | En blanco |
|-|-|-|-|-|
| [`crates/pals/src/dht/transport.rs`](../../../crates/pals/src/dht/transport.rs) | 643 | 455, de ellas 42 de pruebas unitarias | 139 | 49 |
| [`crates/pals/src/dht/muxer.rs`](../../../crates/pals/src/dht/muxer.rs) | 538 | 383 | 117 | 38 |
| Juntos | 1181 | 838 | 256 | 87 |
| Sus pruebas, [`crates/pals/tests/dht_transport.rs`](../../../crates/pals/tests/dht_transport.rs), la puerta de cualquier cambio de `libp2p` o de `iroh` (H3-D1) | 830 | 688 | 81 | 61 |

Esta tarea añadió al adaptador 50 líneas de `transport.rs`, la corrección, y 80 de pruebas. Es el tipo de coste que H3-D1 aceptó: el comportamiento de `libp2p-swarm` al marcar (una dirección tras otra) y el de Iroh (un marcado con todas) no encajaban, y solo se vio con un router caído de dos direcciones.

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

Cuatro ensayos, cada uno en un `PALS_EXIT_DIR` nuevo, llevaron a esta ejecución. Un caso que falló se corrigió y se repitió.

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

   En esta ejecución, las 4 vueltas de los proveedores con `r1` parado y las 16 con `r2` y `r4` parados terminaron sin error.
2. **Producto, la primera versión de esa corrección.** Hacía fallar cualquier marcado posterior a un fallo del mismo par. En el segundo ensayo, la red no llegó a asentarse: los routers no tenían los registros de los proveedores del otro operador. Un marcado puede llevar menos direcciones de las que libp2p prueba, cuando la libreta de direcciones aún no las conoce todas. Desde entonces solo cuenta el fallo de un marcado que llevaba esa dirección, y la prueba lo cubre.
3. **Arnés.**
   - **Caso 3.** `c5` tiene a `r3` como contacto, y su búsqueda lo descarta en la card (`no skill of …`); el arnés lo contaba como un candidato de más. Además, al recrear el contenedor de `c5` el arnés seguía usando el id de agente del anterior.
   - **Caso 5.** Se añadió la comprobación de la renovación y la lectura de las vueltas de publicación de los proveedores, también en el caso 3.
   - **Caso 9.** En el cuarto ensayo el caso falló porque el arnés tomaba la hora del cambio cuando volvía `docker network connect`, y para entonces `p3` ya había emitido y publicado su contacto nuevo. Ahora el cambio cuenta desde antes del `disconnect`, y el caso comprueba también que `c1` recibe de la DHT el contacto nuevo.
   - **Las búsquedas de prueba.** Se guardan con el nodo y el cuerpo, y los registros de los routers se revisan tras el caso 2.
4. **Una ejecución interrumpida.** El tercer ensayo coincidió con la batería de pruebas del daemon y con otra carga del anfitrión: la carga llegó a 224, y un `docker cp` se quedó parado en `preparar`. Se interrumpió con `SIGTERM`. El arnés desmontó todo lo suyo (`quedan: 0 contenedores, 0 redes`), y comprobó así la protección de las ejecuciones interrumpidas.

## Observaciones

1. **Cada búsqueda espera al proveedor de la otra red.**
   - Con los 3 s por candidato por defecto, toda búsqueda tarda 3 s y dice `partial (timeout)`, aunque el primer candidato válido llegue en unos 44 ms. Es la topología: el contacto de un proveedor de la otra red lleva una dirección que el cliente no puede alcanzar.
   - En redes reales, con `relay = true` o con rutas entre las redes, ese candidato contestaría. Con direcciones privadas de otras redes en los contactos, el plazo por candidato es el coste.
2. **Un router caído retrasa cada búsqueda 3 s.** `libp2p-kad` 0.49 no quita de la tabla de rutas a un par que no contesta mientras no le haga falta el sitio, y con 4 routers nunca le hace falta. Mientras el router está parado, cada búsqueda en la DHT le pregunta y agota sus 3 s (`dht timeout`). Aun así, encuentra a los proveedores en los demás (casos 3 y 5).
3. **La ráfaga no llega a los routers.** El límite de 4 búsquedas de un daemon recorta una ráfaga antes de que pese en las cuotas de los routers, y `refused.streams` se quedó en 0. Lo que esas cuotas aguantan lo prueban las pruebas de integración (`a_round_of_32_capabilities_fits_the_routers_quotas` y los límites de `dht_transport`), no esta ráfaga.
4. **El cambio de red se ve en el acto.** Dentro del contenedor, Iroh vio la interfaz nueva y la dirección nueva en el mismo segundo. El contacto nuevo llegó a los routers antes de que los clientes buscaran.
5. **El tráfico en reposo depende de `republish_s`.** Con los plazos de la fase 1 (40 s), un router mueve unos 315 kB/min y un proveedor 243 kB/min. Con los valores por defecto (`republish_s = 1200`), las vueltas y la replicación serían 30 veces menos frecuentes.
6. **La carga del anfitrión**, de 6,6 a 27,4 durante la ejecución, no cortó ninguna búsqueda por plazos más allá de los esperados. En el cuarto ensayo, con la carga entre 10 y 34, el primer candidato válido tuvo p50 41 ms y p95 181 ms; aquí, 44 ms y 67 ms.

## Lo que no cubre

- **Escala.** Hay cuatro routers y cuatro proveedores. Con las 20 réplicas por clave de `libp2p-kad`, cada router guarda todo, y no se ejercita ni el encaminamiento en una red grande ni la elección de los routers más cercanos.
- **Sybil.** No hay muchas identidades de un mismo atacante, ni rutas envenenadas, ni un router que mienta en sus respuestas. El caso 6 es un solo agente con un registro mal firmado.
- **NAT de operador.** Las redes son puentes de Docker sin NAT entre ellas, y los routers son alcanzables directamente desde las dos. No hay operadores distintos de verdad: relojes, versiones y políticas de red son los mismos (H3-D2).
- **Máquinas físicas.** Todos los agentes comparten kernel, reloj y máquina, y los relojes nunca difieren. El desfase de 60 s de los registros lo prueban los tests (`records_follow_the_clock_skew`).
- **La búsqueda con `relay = true`.** Todos los daemons van sin relay, así que un cliente no alcanza a un proveedor de la otra red. No se mide cómo cambia el plazo por candidato, ni la búsqueda, con relays.
- **`held_providers`.** Se observa a través del estado y del protocolo, no directamente (ver «Dónde se aparta…»).
- **Una vuelta de publicación más larga que `republish_s`.** Está anotada para la revisión final. En esta ejecución, todas las vueltas observadas terminaron sin error.
