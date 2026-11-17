# Runbook: prueba de salida del hito 3, descubrimiento con Kademlia

Instrucciones para ejecutar la prueba que cierra el hito 3. Comprueba que un agente encuentra proveedores sin contacto previo a través de la DHT, `agentnet-kad/0`, entre dos operadores y en dos redes, y que lo hace sin depender del punto de arranque original ni de los routers de un solo operador. Comprueba también:
- que los anuncios caducan;
- que un registro mal firmado no entra;
- que las búsquedas tienen límites;
- que lo privado no llega a la DHT;
- que un proveedor que cambia de red vuelve a ser alcanzable.

Los resultados se anotan en [`docs/conformance/hito-3/exit-test.md`](../conformance/hito-3/exit-test.md).

La prueba se hace con Docker en una sola máquina, como decidió H3-D2 ([registro](../decisions/2026-11-16-hito-3-decisiones.md#h3-d2-quién-opera-los-puntos-de-arranque-y-los-routers-de-la-prueba)). El agente que implementa el hito opera los dos operadores, cada uno con sus identidades, su configuración y sus puntos de arranque. La ejecuta el arnés [`tests/exit/hito-3.sh`](../../tests/exit/README.md#prueba-de-salida-del-hito-3-con-docker), y este runbook dice qué hace cada caso y qué se espera de él.

| Caso | Qué se comprueba |
|-|-|
| 1. Sin fuga | Las dos redes son internas. `c0`, aislado, no encuentra a nadie, y su `GET /dht` da 0 pares y 0 conexiones. `cx`, sin `[dht]`, no abre ninguna conexión `agentnet-kad/0`, y su búsqueda no hace peticiones a la DHT |
| 2. Encontrar un proveedor sin contacto previo | Cada cliente busca 10 veces cada capacidad: los dos proveedores de cada una salen de la DHT. El de su red queda en el ranking `from_dht`, con su card y su oferta verificadas, y el de la otra red, descartado en la card por inalcanzable. Después, `p1` da un grant a `c1`, y `pals discover --capability vision.ocr --ask "hola"` desde `c1` obtiene `echo: hola` de `p1` por A2A estándar |
| 3. Sin el arranque original | Se para `r1`. Los clientes conectados siguen encontrando los dos proveedores de cada capacidad. `c5` arranca, entra por `r3` y los encuentra |
| 4. Caducar anuncios | Se para `p1` sin aviso. Los routers dejan de guardar sus registros y las búsquedas dejan de devolverlo antes de `record_ttl_s + republish_s` (160 s) |
| 5. Caída de routers y renovación | Se paran `r2` y `r4`, uno de cada operador. Se miden la cobertura inmediata y el tiempo hasta recuperarla entera, que tiene que ser menos de `2 × republish_s` (80 s). Y sigue entera cuando ya ha caducado todo lo publicado antes de pararlos |
| 6. Contacto con firma inválida | `rogue` anuncia `vision.ocr` con un contacto de firma alterada. Los routers cuentan `bad_contact` y ninguno guarda su contacto. Ningún cliente lo pone en su ranking, y su `discarded` lo nombra |
| 7. Búsquedas repetidas o maliciosamente amplias | Ráfaga de 200 búsquedas desde `c1`, 8 a la vez: las 4 capacidades y 20 que no existen. Al menos una da `too many searches`, y las demás, 200. Ningún router se cae, y las búsquedas de `c3` durante la ráfaga terminan. Se anota el `refused.streams` de cada router |
| 8. Política | Las capacidades privada y de proyecto de `p2` no están en ningún router. Con `--no-dht`, `dht_requests` es 0 |
| 9. Cambio de red | `p3` pasa de la red B a la A. Su daemon emite un contacto nuevo y lo publica, y antes de `2 × republish_s` los clientes de la red A reciben la dirección nueva y consultan a `p3` en ella: card y oferta verificadas |

Tiempo: unos 17 min, de los que 5 son el reposo medido, y uno o dos más la primera vez, para construir la imagen.

## Dónde se aparta de la redacción del plan

Tres desviaciones, en los casos 2, 3, 5, 7, 8 y 9, hacen que esos casos no comprueben exactamente lo que dice la tabla de la tarea 14 del [plan](../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). Lo decidió quien dirige el hito antes de la ejecución, porque tal como estaban escritas, esas comprobaciones no se podían cumplir o no se podían observar.

1. **Los proveedores de la otra red (casos 2, 3, 5 y 9).**
   - **El plan.** El caso 2 pide «los dos proveedores de cada una, `from_dht`, con oferta verificada».
   - **Por qué no se puede.** Con las capacidades de la fase 1, cada capacidad tiene un proveedor en cada red, y cada cliente está en una sola. Las redes son `--internal` y los daemons van sin relay, así que un cliente no puede conectar con el proveedor de la otra red: A2A entre redes es imposible por construcción. Lo que el caso prueba es el descubrimiento entre operadores y redes a través de la DHT.
   - **Lo que se comprueba.** Que los dos proveedores salen de la DHT. El de la red del cliente queda en el ranking con `from_dht`, su card y su oferta verificadas. El de la otra red queda descartado en la etapa `card` por inalcanzable: `timeout` o `card: peer unreachable`, nunca con un motivo `dht: …`. Que llegue a esa etapa prueba que la DHT dio su contacto y su anuncio válidos.
   - **Quién verifica qué.** Hay clientes en las dos redes, así que cada proveedor lo verifica algún cliente.
   - **El caso 5.** Su cobertura es la parte de las búsquedas en las que la DHT dio los dos proveedores de la capacidad, en el ranking o descartados por inalcanzables.
   - **El caso 9.** Usa los clientes de la red A. Antes del cambio descartan a `p3` por inalcanzable. Después, antes de `2 × republish_s`, verifican su card y su oferta en su dirección nueva.
2. **Lo que guardan los routers (caso 8).**
   - **El plan.** Pide mirar el `held_providers` de cada router.
   - **Por qué no se puede.** El daemon no lo expone fuera del proceso: `GET /dht` y `pals dht status --json` no lo llevan, y `Dht::held_providers` solo lo usan las pruebas en Rust.
   - **Lo que se comprueba.** Lo mismo, con lo que el daemon expone, sin añadir API:
     - en cada router, `stored_by_agent` de `p2` es 2 más sus capacidades públicas: su contacto, su anuncio y un provider record de cada una;
     - el contador `capability_not_announced` no crece durante una vuelta de publicación de `p2`;
     - las búsquedas de las capacidades privada y de proyecto de `p2`, desde cada cliente, preguntan a la DHT (`dht_requests > 0`) y no encuentran a `p2`. Un router contesta a esa petición, `GET_PROVIDERS`, con los proveedores que guarda de esa clave.

     Cubre los dos fallos posibles: que `p2` las publicara, y que las publicara y los routers las rechazaran.
3. **La ráfaga (caso 7).**
   - **El plan.** Dice «4 a la vez».
   - **Por qué no basta.** El daemon admite 4 búsquedas a la vez (`max_searches`), así que una ráfaga de 4 en 4 solo da `too many searches` por casualidad, cuando una respuesta adelanta a la liberación de su plaza.
   - **Lo que se comprueba.** La ráfaga va de 8 en 8, por encima del límite, y exige al menos un `429 too many searches`. Cada respuesta es un `200` o ese `429`.

## Topología

Dos redes Docker `--internal`, sin salida ni ruta entre ellas: A, `10.213.73.0/24`, y B, `10.213.74.0/24`. Cada agente va en su contenedor, `pals-exit-h3-<agente>`, con su `PALS_HOME`.

| Agente | Operador | Red y dirección | DHT | Entra por |
|-|-|-|-|-|
| `r1` | 1 | A `.11`, B `.11` | router | nadie: es el arranque original |
| `r2` | 1 | A `.12`, B `.12` | router | `r1` |
| `r3` | 2 | A `.13`, B `.13` | router | `r1` |
| `r4` | 2 | A `.14`, B `.14` | router | `r3` |
| `p1` | 1 | A `.21` | cliente, `announce = true` | `r1` |
| `p2` | 1 | A `.22` | cliente, `announce = true` | `r1` |
| `p3` | 2 | B `.23`; A `.23` desde el caso 9 | cliente, `announce = true` | `r3` |
| `p4` | 2 | B `.24` | cliente, `announce = true` | `r3` |
| `c1`, `c2` | 1 | A `.31`, `.32` | cliente | `r1` |
| `c3`, `c4` | 2 | B `.33`, `.34` | cliente | `r3` |
| `c0` | | A `.40` | cliente, aislado | nadie (caso 1) |
| `cx` | | A `.46` | sin `[dht]` | (caso 1) |
| `c5` | 1 | A `.45` | cliente | `r1`, parado, y el ticket de `r3` guardado como contacto (caso 3) |
| `rogue` | | B `.50` | `dht_rogue` | `r3` (caso 6) |

Todos los daemons llevan lo mismo:
- `relay = false`;
- `[net] listen = "0.0.0.0:4433"`;
- `[dht] enabled = true` (salvo `cx`), `record_ttl_s = 120` y `republish_s = 40`. Son los plazos de la fase 1, para que los casos de caducidad duren minutos;
- el worker `echo`.

Los routers declaran sus dos direcciones en `external_addrs`, y su ticket las lleva. La configuración de cada agente está en [`tests/exit/hito-3/operador-1/`](../../tests/exit/hito-3/operador-1) y [`operador-2/`](../../tests/exit/hito-3/operador-2). Los tickets de `r1` y `r3`, por los que se entra, se escriben en ella cuando esos routers ya están en marcha. Con el puerto fijo, un ticket sigue valiendo cuando su router vuelve a arrancar.

Las capacidades y los servicios son los de la fase 1, todos públicos. Cada capacidad tiene un proveedor en cada red:

| Proveedor | Capacidades públicas | Además |
|-|-|-|
| `p1` | `vision.ocr`, `knowledge.documentation` | |
| `p2` | `code.rust.review`, `vision.defect-detection` | `audio.transcribe`, privado, y `data.extraction`, del proyecto `demo` |
| `p3` | `vision.ocr`, `vision.defect-detection` | |
| `p4` | `code.rust.review`, `knowledge.documentation` | |

Ningún cliente tiene contactos guardados: todo lo que encuentra sale de la DHT. Las búsquedas usan los límites por defecto de `[discovery]`: 3 s por candidato y 15 s en total. El descarte de un proveedor de la otra red llega cuando se agota su plazo, así que cada búsqueda tarda unos 3 s.

## 0. Requisitos

- Linux con Docker, con permiso para crear imágenes, redes y contenedores, y `bash`, `git`, `python3`, `sha256sum` y `awk`.
- Las subredes `10.213.73.0/24` y `10.213.74.0/24` libres.
- Rust 1.91 o posterior, para compilar en el anfitrión.
- Salida a internet para construir la imagen (paquetes de Debian). La prueba no la usa.

## 1. Compilar y ejecutar

```bash
cargo build --release --locked -p pals
cargo build --release --locked -p pals --examples     # dht_rogue, el del caso 6
export PALS_EXIT_DIR=$(mktemp -d)                      # nuevo o vacío
tests/exit/hito-3.sh                                   # preparar, casos 1 a 9 y desmontar
```

Cada caso imprime `RESULTADO caso <n>: superado` o `FALLIDO`. Los resultados quedan en `$PALS_EXIT_DIR/registro/resultados.txt`, y las métricas en `registro/metricas.txt`. [`tests/exit/README.md`](../../tests/exit/README.md#prueba-de-salida-del-hito-3-con-docker) explica las fases sueltas, las variables, lo que crea y toca y cómo desmontar una ejecución interrumpida.

Para seguir un caso a mano, cada agente se alcanza con `docker exec`:

```bash
a() { docker exec -i -e PALS_HOME=/root/pals-h3/$1 pals-exit-h3-$1 "${@:2}"; }
a r1 pals dht status                 # el estado de la DHT de r1
a c1 pals discover --capability vision.ocr
a c1 curl -sS http://127.0.0.1:7420/dht
```

## 2. Preparación (`preparar`)

1. Comprueba que no hay recursos `pals-exit-h3-*`, que ninguna red de Docker se solapa con las dos subredes y que existen los binarios. Anota la fecha, el commit, las versiones y la carga del anfitrión.
2. Construye la imagen y crea las dos redes, con `--internal`. Comprueba que Docker las hizo internas.
3. Crea los doce contenedores de la topología en sus direcciones, y copia en cada uno el binario y `metricas.py`.
4. Arranca los agentes en este orden, cada uno con la configuración de su operador:
   1. `r1`, y con su ticket, `r2` y `r3`;
   2. con el ticket de `r3`, `r4`;
   3. los proveedores y los clientes.

   Cada daemon arranca con `pals init` y `pals daemon`, y el arnés espera a que su gateway dé el ticket.
5. **Asentamiento.** Espera a que cada router guarde, de cada proveedor, su contacto, su anuncio y un provider record de cada capacidad: 4 por proveedor en `stored_by_agent`.
6. **Reposo.** 300 s sin búsquedas. Cada 60 s, de cada agente:
   - los contadores de sus interfaces (`/sys/class/net/*/statistics`);
   - la memoria de su daemon (`VmRSS` de `/proc/<pid>/status`);
   - su `GET /dht`.

   Así se miden la memoria, las conexiones y el tráfico en reposo de cada papel.

**Esperado:** los cuatro routers con `routing_peers 3`, y cada uno con 4 registros de cada proveedor. Cada proveedor con `publication` y sus dos capacidades, sin `last_error`.

## 3. Casos

Las búsquedas van por `POST /discovery/search` del gateway de cada cliente, como las de `pals discover`, con `metricas.py busca`. Cada respuesta se guarda con su tiempo y con los bytes que pasaron por las interfaces del cliente mientras duró. Los estados son `GET /dht`.

Nada espera un tiempo fijo: el arnés sondea el estado, cada 3 s, hasta que se cumple lo que espera. Un techo generoso solo sirve para fallar. Los tiempos que tienen un límite (casos 4, 5 y 9) se miden desde el cambio de estado, y se anota la carga del anfitrión.

Cada caso juzga con un guion de Python sobre lo que guardó. Un caso solo se supera si el guion llega al final sin error y escribe `OK`: uno que se rompe lo hace fallar. Una búsqueda sin respuesta, o cuyo fichero no se puede leer, cuenta como un problema del caso. Si un caso que paró agentes no consigue que los routers vuelvan a guardarlo todo al restaurarlos, el que falla es ese caso, no el siguiente.

### Caso 1. Sin fuga

1. Las dos redes: `docker network inspect -f '{{.Internal}}'`.
2. `c0` arranca en la red A con la DHT encendida, sin arranque ni contactos. 10 s después busca cada capacidad, y se lee su `GET /dht`.
3. `cx` arranca en la red A sin `[dht]`, con `c0` como único contacto guardado (`pals contact add <ticket de c0> --name c0`). Se lee su `GET /dht`. 10 s después busca `vision.ocr`, y se lee otra vez el `GET /dht` de `c0`.

**Cómo se observa que `cx` no abre ninguna conexión `agentnet-kad/0`.** En `cx`, `GET /dht` da 404, y un router no puede atribuirle una conexión, porque todos los agentes se conectan a los routers. Por eso el único contacto de `cx` es `c0`: tiene la DHT encendida y nadie más se conecta con él. Un nodo con la DHT entra en la red marcando sus contactos guardados, así que una conexión `agentnet-kad/0` de `cx` aparecería en `connections_accepted` de `c0`.

**Esperado:**
- las dos redes, `Internal=true`;
- cada búsqueda de `c0`, sin nadie en el ranking ni en los descartes, con cobertura parcial `dht unavailable`;
- `GET /dht` de `c0`, antes y después de la búsqueda de `cx`, con `routing_peers 0`, `connections 0` y `connections_accepted 0`;
- `GET /dht` de `cx`, `404`;
- la búsqueda de `cx`, con `dht_requests 0`, ningún candidato `from_dht` y `c0` entre los descartes: llegó a él por A2A.

### Caso 2. Encontrar un proveedor sin contacto previo

1. Cada cliente busca 10 veces cada capacidad, una tras otra, los cuatro clientes a la vez: 160 búsquedas.
2. `p1` da a `c1` un grant del proyecto `demo` (`pals grant issue --to <id de c1>`), y `c1` lo importa.
3. Desde `c1`: `pals discover --capability vision.ocr --ask "hola"`.

**Esperado:**
- **En cada búsqueda,** los dos proveedores de la capacidad salen de la DHT, y nadie más. El de la red del cliente está en el ranking con `from_dht: true`, la card firmada por él (`card_signature: verified`, `kid` su id) y su oferta, de él y para el cliente. El de la otra red está en los descartes, en la etapa `card`, con `timeout` o `card: peer unreachable`. La cobertura es parcial, `timeout`, por ese descarte.
- **La pregunta:**
  - `p1` primero, `grant held, found in the DHT`, con su oferta;
  - `asked: <id de p1>` y `echo: hola`, con código 0;
  - la tarea `TASK_STATE_COMPLETED` en `pals task ls` de `c1`.

  La pregunta va por el outbox y el gateway de `c1`, con los bindings estándar de A2A sobre `http-over-iroh/v1`, como la de cualquier contacto.
- **`p1`**, con un `task.accepted` en su registro de eventos: el de esa tarea.

Tras las 160 búsquedas se cuentan además las líneas del registro de cada router, en su nivel por defecto, que nombran a un cliente por su id de agente o por su `PeerId`. Es una observación, no una comprobación (sección 4, filtraciones de metadatos).

### Caso 3. Sin el arranque original

1. Se para `r1` con `docker kill`, después de leer los tickets de `r1` y `r3`.
2. Cada cliente busca cada capacidad una vez.
3. `c5` arranca en la red A. Su arranque es solo `r1`, parado, y antes de arrancar guarda el ticket de `r3` como contacto: la invitación del operador 2. Busca las cuatro capacidades cada 3 s, hasta encontrar los dos proveedores de cada una.
4. `c5` se para. `r1` vuelve a arrancar, y se espera a que los routers vuelvan a guardarlo todo.

**Esperado:**
- **Los conectados.** Las 16 búsquedas, como en el caso 2. La cobertura puede decir `dht timeout` además de `timeout`: la búsqueda en la DHT también pregunta a `r1`, que no contesta. No es un fallo por sí sola.
- **El registro de `c5`:**
  - `no DHT bootstrap router answered: trying the saved contacts`;
  - `dialing the saved contacts, to find a DHT router among them`;
  - `a saved contact is a DHT router: entering through it peer=<PeerId de r3>`.
- **Sus búsquedas:** los dos proveedores de cada capacidad, como en el caso 2. `r3`, su contacto, sale además entre sus descartes en la card (`no skill of <capacidad>`): es un router, sin servicios. Basta con que cada capacidad salga así en alguna de sus búsquedas.

Se anota cuánto tarda desde que arranca. Mientras `r1` está parado, en cada búsqueda de `c5` se lee el estado de los proveedores, y después hasta que cada uno muestra una vuelta terminada con `r1` parado, como mucho 300 s. Cuentan solo las vueltas que terminaron después de pararlo (`last_round_unix`): cada proveedor tiene que terminar así al menos una, y ninguna puede terminar con error.

### Caso 4. Caducar anuncios

1. Se para `p1` sin aviso, con `docker kill`.
2. Cada 3 s, se lee el estado de los routers, y cada cliente busca una capacidad de `p1`: `vision.ocr` y `knowledge.documentation`, alternas. Sigue hasta que ningún router guarda nada de `p1` y una ronda entera contesta sin nombrarlo.
3. `p1` vuelve a arrancar con su directorio de datos, y se espera a que los routers vuelvan a guardarlo.

**Esperado:**
- antes, cada router guarda 4 registros de `p1`;
- los routers lo olvidan antes de 160 s desde que se paró: la primera lectura con 0 en todos, después de la última que guardaba algo;
- las búsquedas dejan de devolverlo antes de 160 s: la primera búsqueda contestada después de la última que lo nombraba, en el ranking o en los descartes.

El límite sale de la última vuelta de `p1`: como mucho `republish_s` antes de pararlo, y lo publicado dura `record_ttl_s`.

### Caso 5. Caída de routers y renovación

1. Se paran `r2` (operador 1) y `r4` (operador 2) con `docker kill`.
2. Rondas seguidas en las que cada cliente busca cada capacidad, hasta una ronda en la que las 16 búsquedas encuentran los dos proveedores.
3. **Renovación.** Con `r2` y `r4` todavía parados, se espera hasta `record_ttl_s + republish_s` (160 s) desde que se pararon, leyendo cada 15 s el estado de los proveedores. Entonces ha caducado todo lo que los routers guardaban antes, y lo que guardan salió de las vueltas de publicación hechas sin `r2` ni `r4`. Se hace otra ronda.
4. `r2` y `r4` vuelven a arrancar.

**Medidas:**
- **La cobertura de una ronda** es la parte de sus búsquedas en las que la DHT dio los dos proveedores de la capacidad, en el ranking o descartados por inalcanzables. La inmediata es la de la primera ronda.
- **La cobertura completa** cuenta, como en la fase 1, desde la primera búsqueda completa del mismo cliente y capacidad que la última incompleta. Es 0 si ninguna fue incompleta.

**Esperado:**
- la cobertura completa otra vez antes de 80 s desde que se pararon;
- la ronda de la renovación, también con cobertura completa.

Se anotan las etiquetas de cobertura que aparecen (`dht timeout` o `dht unavailable`, por los routers parados) y las vueltas de publicación de los proveedores que terminaron después de pararlos, con sus errores si los hay.

La comprobación de la renovación no está en la tabla del plan. Sin ella, el caso pasaría siempre en la primera ronda, porque los routers que quedan guardan todavía lo de antes, como pasó en la fase 1 (cobertura inmediata 1, recuperación 0 s). La renovación es lo que dice el título del caso. Sin ella, el caso no habría visto el fallo del daemon que encontraron los ensayos: con un router caído, las vueltas de publicación de los proveedores fallaban y sus registros caducaban en la DHT ([`exit-test.md`](../conformance/hito-3/exit-test.md#correcciones-antes-de-esta-ejecución)).

### Caso 6. Contacto con firma inválida

1. En la red B, con el ticket de `r3`:

   ```bash
   dht_rogue --bootstrap <ticket de r3> --addr 10.213.74.50:4433 --capability vision.ocr --record-ttl-s 120 --republish-s 40
   ```

   [`dht_rogue`](../../crates/pals/examples/dht_rogue.rs) usa el código del daemon, `Dht` y `PublishedRecords`, con un `PublishedRecords` propio:
   - un contacto firmado y después alterado (el primer carácter de la firma);
   - un anuncio válido de `vision.ocr`.
2. Se sondean los routers hasta que cada uno cuenta el contacto. Después cada cliente busca `vision.ocr`, y `rogue` se para.

**Esperado:**
- **Cada router:**
  - `rejected.bad_contact` crece;
  - `stored_by_agent` del `rogue` es 2: su anuncio y su provider record, sin contacto.
- **Cada cliente:**
  - el `rogue` fuera del ranking;
  - en los descartes, con su id, `card` y `dht: no valid contact`, porque ningún router le dio una copia de su contacto;
  - el resto, como en el caso 2.

### Caso 7. Búsquedas repetidas o maliciosamente amplias

1. `c3` empieza a buscar las cuatro capacidades, una tras otra. La ráfaga empieza cuando su primera búsqueda ha contestado y la siguiente está en marcha.
2. `c1` lanza 200 búsquedas, 8 a la vez, cada una de 24 capacidades por turno: las 4 y `example:inexistente-01` a `-20`.
3. Cuando acaba la ráfaga, `c3` deja de buscar.

Se leen antes y después el estado de los routers y su daemon: el PID y la hora a la que arrancó el proceso, de `/proc/<pid>/stat`, solo si sigue vivo.

**Esperado:**
- **La ráfaga.** Al menos un `429` con `{"error":"too many searches"}`, y el resto, `200`. El daemon admite 4 búsquedas a la vez y rechaza las demás en el acto.
- **`c3`.** Cada búsqueda que coincidió con la ráfaga termina, con `200`.
- **Los routers.** Los mismos PID y `healthz` `200`.

Se anota el `refused.streams` de cada router durante el caso. Un número distinto de cero es un dato, no un fallo: los routers admiten 8 streams a la vez y 600 por minuto de cada par.

### Caso 8. Política

1. Se lee el estado de los routers y de `p2`, y otra vez cuando `p2` ha terminado una vuelta de publicación entera dentro del caso.
2. Cada cliente busca `audio.transcribe` (privada de `p2`) y `data.extraction` (de su proyecto `demo`).
3. `c1` busca `vision.ocr` con `exclude_dht` (`pals discover --no-dht`) y sin él.

**Esperado:**
- `publication` de `p2`, solo con sus dos capacidades públicas;
- **en cada router**, antes y después, `stored_by_agent` de `p2` es 4, y `capability_not_announced` no cambia;
- **las búsquedas de las capacidades ocultas**, sin `p2` en el ranking ni en los descartes, y con `dht_requests` mayor que 0: la DHT se consultó;
- **con `exclude_dht`**, `dht_requests 0`, sin candidatos de la DHT. Sin él, `dht_requests` mayor que 0;
- **`pals discover --no-dht`**, nada `found in the DHT`, con código 0.

### Caso 9. Cambio de red

1. `c1` y `c2` buscan las capacidades de `p3`.
2. Se mueve `p3`:

   ```bash
   docker network disconnect pals-exit-h3-red-b pals-exit-h3-p3
   docker network connect --ip 10.213.73.23 pals-exit-h3-red-a pals-exit-h3-p3
   ```

   El daemon sigue en marcha. Su endpoint de Iroh, ligado a `0.0.0.0:4433`, ve cambiar sus direcciones.
3. `c1` y `c2` buscan las capacidades de `p3` cada 3 s, hasta que los dos verifican su card y su oferta.

**Esperado:**
- **Antes del cambio,** `p3` descartado en la card por inalcanzable.
- **Después,** el contacto de `p3` (`own_records` de su almacén) con un `seq` mayor y la dirección `10.213.73.23:4433`.
- **En el registro de `p3`,** `renewed this agent's records: its addresses changed`, y una vuelta de publicación después del cambio, sin error.
- **`c1` y `c2`** tienen `p3` en el ranking, desde la DHT, con su card y su oferta verificadas, antes de 80 s desde el cambio.
- **El contacto de `p3` que guarda `c1`** (`peer_records`) es el nuevo, con el mismo `seq` y la misma dirección. `c1` solo lo pudo recibir de la DHT: prueba que los routers guardan el contacto nuevo.

Los tiempos cuentan desde que empieza el cambio, antes del `disconnect`. El daemon de `p3` puede ver su dirección nueva, emitir el contacto y publicarlo antes de que `docker network connect` vuelva. `last_round_unix` y `issued_at` van en segundos enteros.

## 4. Registro

Anota en [`docs/conformance/hito-3/exit-test.md`](../conformance/hito-3/exit-test.md):
- la fecha, el anfitrión, su carga, las versiones, el commit y la topología;
- el resultado de cada caso;
- las métricas del §14, que da `metricas.py informe` con lo que guardó el arnés:
  - el tiempo hasta el primer candidato válido, p50 y p95;
  - el porcentaje de búsquedas con candidato;
  - las peticiones a la DHT y los bytes por búsqueda;
  - los candidatos contactados;
  - los anuncios caducados y su tiempo;
  - los tiempos de recuperación de los casos 3, 5 y 9;
  - el coste del matching;
  - la memoria, las conexiones y el tráfico en reposo de cada papel;
- el coste de mantener el adaptador: las líneas de `transport.rs` y `muxer.rs`;
- las filtraciones de metadatos observadas: lo que un router ve de cada búsqueda (H3-D4). El arnés cuenta, en el caso 2, las líneas del registro de cada router que nombran a un cliente. Con libp2p en `debug` se ve además lo que un router podría anotar. Se observa aparte, después de `preparar` y antes de `desmontar`:

  ```bash
  a() { docker exec -i -e PALS_HOME=/root/pals-h3/$1 pals-exit-h3-$1 "${@:2}"; }
  a r2 bash -c 'kill $(cat $PALS_HOME/daemon.pid); sleep 2
      RUST_LOG=info,libp2p_swarm=debug,libp2p_kad=trace nohup pals daemon > $PALS_HOME/debug.log 2>&1 < /dev/null &
      echo $! > $PALS_HOME/daemon.pid'
  a c1 python3 /root/pals-h3/metricas.py busca debug '{"search": {"capability": "vision.ocr"}}' > /dev/null
  a r2 grep -c <PeerId de c1> /root/pals-h3/r2/debug.log                 # el agente que busca
  a r2 grep -c 122092485626d851a455cd10cd7ae3800eb76ff67220722638435d3ae050e92c8463 /root/pals-h3/r2/debug.log   # la clave de vision.ocr
  ```

  El `PeerId` de `c1` está en su `daemon.log` (`the DHT started peer_id=…`);
- lo que no cubre.

No copies tickets ni tokens. Los ids de agente se escriben por su nombre en la topología.

## 5. Limpieza

`tests/exit/hito-3.sh desmontar`, que en `todo` va al final. Copia el `daemon.log` de cada agente a `registro/nodos/`, escribe las métricas y borra los contenedores y las redes con la etiqueta de este `PALS_EXIT_DIR`. Después, comprueba que no queda nada:

```bash
docker ps -a --filter "label=pals-exit.dir=$PALS_EXIT_DIR"
docker network ls --filter "label=pals-exit.dir=$PALS_EXIT_DIR"
```
