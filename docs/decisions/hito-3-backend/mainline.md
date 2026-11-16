# Candidato `mainline`: Iroh más una DHT Mainline privada

Tarea 5 de la [fase 1 del hito 3](../../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). Es el tercero y último de los candidatos que la tarea 6 compara con la línea base. El código está en [`spikes/hito-3/mainline/`](../../../spikes/hito-3/mainline/) y cumple el contrato de nodo de [`spikes/hito-3/README.md`](../../../spikes/hito-3/README.md#el-contrato-de-nodo). Los resultados de su ejecución del arnés están en [`results/mainline.json`](results/mainline.json).

No es código del producto. Las *salts* `agentnet:contact:v1` y `agentnet:announce:v1` y el *infohash* de una capacidad son provisionales.

## La duda que resuelve

Mainline, la DHT de BitTorrent (BEP 5 y BEP 44), guarda pares por *infohash* e items firmados por clave, pero no tiene un índice de capacidades (§6.1). La duda es si una DHT Mainline privada puede servir de backend de descubrimiento de PALS, junto a Iroh, y qué cuesta definir ese índice.

**Sirve para buscar, pero no como backend de PALS tal cual: la DHT no caduca nada, y un router en dos redes solo sirve a una de ellas. Definir el índice cuesta poco código, pero obliga a los routers a leer el protocolo por su cuenta.**

- **Integración.** `n0-mainline` 0.7.2 compila junto a Iroh 1.4.1, también con la MSRV 1.91, y solo añade seis crates a la línea base. Con *signed peers* como índice y los registros en items BEP 44, cada búsqueda de S1 devuelve los dos proveedores verificados. En la ejecución del arnés, S0, S1, S4, S5 y S7 quedan en `pass` y S6 en `measured`, sin ningún `error`.
- **Lo que Mainline no da.** S3 queda en `fail`: la biblioteca no caduca nada, y los routers seguían guardando lo del proveedor parado, y las búsquedas devolviéndolo, al acabar la observación ([Caducidad](#caducidad-y-republicación)). S2 queda en `fail`: Mainline nombra a cada nodo con una sola dirección, así que con `r1` parado nadie llega a lo que solo guardaba él, los registros de los proveedores de la red A ([Routers en dos redes](#routers-en-dos-redes)).
- **Ajustes.** La biblioteca no da cuatro cosas que hubo que hacer en el candidato: leer cada `put` en el gancho de datagramas para validarlo, preguntarse a sí mismo para ver lo que guarda, repetir tras una búsqueda un `put` fallido, porque un router que vuelve a arrancar rechaza los *tokens* viejos, y arrancar un cliente de la DHT nuevo al cambiar de red. Sin el tercero, una ejecución de prueba dejó S3 sin nada que medir ([Un router que vuelve a arrancar](#un-router-que-vuelve-a-arrancar)).
- **Coste, frente a la línea base y a los dos candidatos de Kademlia.** Las cifras son las de `results/mainline.json`, `results/baseline.json`, `results/kad-libp2p.json` y `results/kad-iroh.json`, salvo las dependencias:
  - memoria en reposo (S6, p50 por papel): de 15 584 a 16 020 KiB por nodo, frente a 14 396 a 14 824 de la línea base, 22 748 a 23 240 de `kad-libp2p` y 20 372 a 21 128 de `kad-iroh`;
  - tráfico en reposo (S6, p50), con la republicación de la prueba cada 40 s: 11 364 bytes por minuto en un router, 5992 en un proveedor y 359 en un cliente, frente a 444, 222 y 222 de la línea base, 249 735, 180 386 y 18 262 de `kad-libp2p` y 355 418, 218 700 y 27 142 de `kad-iroh`;
  - por búsqueda (S1, p50): 13 922 bytes netos, frente a 31 599 de `kad-libp2p` y 27 841 de `kad-iroh`. Las peticiones no se cuentan;
  - el primer candidato válido llega a los 2 ms (S1, p50 y p95), pero la búsqueda entera tarda 3 ms (p50) y 1102 ms (p95): los clientes de la red A esperan a routers que no alcanzan;
  - dependencias: 306 crates, frente a 300 de la línea base, 370 de `kad-libp2p` y 337 de `kad-iroh`. Es una observación, no una cifra del JSON ([Código](#código)).

El detalle está en [Resultados](#resultados).

## La pila Mainline

El `Cargo.toml` de la tarea 1 traía dos pilas Mainline: `mainline =8.0.1` con `pkarr =9.0.0`, y la de n0, `iroh-mainline-address-lookup =0.6.1`, que trae `n0-mainline 0.7.2` y los paquetes pkarr de `iroh-dns`. La prueba usa una sola. Se eligió leyendo el código fuente de las versiones fijadas el 2026-11-16, en unos 25 minutos. Esto es lo que se comprobó:

| Qué | `mainline 8.0.1` (+ `pkarr 9.0.0`) | `n0-mainline 0.7.2` |
|-|-|-|
| Items BEP 44 con *salt* | Sí | Sí (`put_mutable`, `get_mutable`) |
| BEP 5 `announce_peer` | Sí | Sí |
| Un anuncio firmado con una clave Ed25519 bajo un *infohash* | No | Sí: *signed peers* (`announce_signed_peer`, `get_signed_peers`), tras la feature `unstable_signed_peers` |
| Nodos de arranque | Por defecto los públicos; `bootstrap` los sustituye | Igual (`DEFAULT_BOOTSTRAP_NODES`: `router.bittorrent.com`, `dht.transmissionbt.com`, `dht.libtorrent.org` y `relay.pkarr.org`) |
| Modo servidor explícito | Sí | Sí (`server_mode`) |
| Filtro de peticiones del servidor | `RequestFilter`, que recibe un `RequestSpecific` cuyas variantes el crate no exporta | El mismo `RequestFilter`, con el mismo problema, y además `DatagramHook`, que ve cada datagrama antes de que la biblioteca lo decodifique, y `send_datagram` |
| Leer lo que guarda el servidor | No | No |
| Caducidad de lo guardado | No: LRU por número de entradas | No: LRU por número de entradas |
| Red de prueba | `Testnet` | `Testnet` |
| Relays | `pkarr` los trae por defecto (`relays`, con `reqwest`), y llegan a redes públicas | Ninguno |

`iroh-mainline-address-lookup` es la búsqueda de direcciones de Iroh sobre Mainline (`DhtAddressLookup`): publica la dirección del endpoint como paquete pkarr, en el formato de Iroh. Usarlo exigiría encender la búsqueda de direcciones de Iroh, que la prueba lleva desactivada, y no publica el `agentnet.contact/1`, que es el formato congelado.

**Se usa `n0-mainline =0.7.2` directamente,** con la feature `unstable_signed_peers`. Lleva el contacto y el anuncio como items BEP 44 y el índice como *signed peers*. Pesaron tres cosas:

- Los *signed peers* devuelven claves de agente, no direcciones, que es lo que el plan prefiere. `mainline 8.0.1` no los tiene.
- Sin `DatagramHook`, un router no podría leer un `put` antes de guardarlo: en las dos bibliotecas, `RequestFilter` recibe la petición, pero no puede nombrar sus variantes para leer el valor.
- `n0-mainline` comparte con Iroh 1.4.1 su capa UDP (`noq-udp`), y no trae relays.

Se quitaron `mainline`, `pkarr` e `iroh-mainline-address-lookup` del `Cargo.toml` del workspace y del candidato. Del `Cargo.lock` salieron ellos y lo que solo traían ellos: `async-compat`, `base32`, `document-features`, `flume`, `litrs`, `self_cell` y `spin 0.9.9`. Además, `reqwest`, que sigue en el lock por otros, dejó de listar `quinn` entre sus dependencias, que solo aparecía por la feature `rustls` que le pedía `pkarr`. No entró ningún crate nuevo: `serde_bencode`, `sha1_smol` y `futures-core`, que usa el candidato, ya los traía `n0-mainline`. Sin `pkarr`, no hay ningún relay que apagar.

## Cómo está hecho

Cada nodo lleva:

- **Iroh**, arrancado como en el daemon con `relay = false` y como en la línea base: un endpoint con la clave del agente, en UDP en el puerto de `--listen` (4433), sin relays, sin mapeo de puertos y sin búsqueda de direcciones. El contacto lo nombra; en la prueba nadie lo marca.
- **Un nodo de la DHT** (`n0-mainline`), en UDP en el puerto siguiente (4434). La biblioteca abre su propio socket en `0.0.0.0`, y no puede compartir el de Iroh.

**Una sola clave.** Todo va bajo la clave Ed25519 del agente, que también es su `EndpointId` de Iroh y su `AgentId`. La DHT tiene además su propio *node id*, de 20 bytes, aleatorio en cada arranque, que no dice nada del agente.

**Lo que se publica:**

| Dónde | Qué |
|-|-|
| Item mutable BEP 44 de la clave del agente, *salt* `agentnet:contact:v1`, en `SHA-1(clave ‖ salt)` | El JSON del `Signed<ContactRecord>` de `pals-core`, sin cambios. El `seq` de BEP 44 es el del registro |
| Item mutable BEP 44 de la clave del agente, *salt* `agentnet:announce:v1` | El JSON del `Signed<AnnounceRecord>`, con el `seq` del registro |
| Un *signed peer* por capacidad, bajo el SHA-1 de la cadena completa de la capacidad | La clave del agente, una marca de tiempo en microsegundos y la firma de la clave sobre el *infohash* y esa marca |

[`index.rs`](../../../spikes/hito-3/mainline/src/index.rs) define el índice y lo que un router acepta en él.

**Búsqueda.** El nodo pide los *signed peers* del *infohash* de la capacidad (`get_signed_peers`). Según llega cada clave, hasta `limit`, pide a la vez su contacto y su anuncio (`get_mutable`). De cada uno se queda con la copia de mayor `seq` que pasa la comprobación del router ([Validación en el router](#validación-en-el-router)), y `spike_common::verify` la vuelve a comprobar. El `found_ms` de cada candidato es cuándo la búsqueda tuvo por primera vez un contacto y un anuncio que pasan esa comprobación. Cada consulta iterativa termina cuando no le queda ninguna petición en vuelo, contestada o caducada. El plazo de una petición lo ajusta la biblioteca al tiempo de ida y vuelta, y es de 500 ms como mínimo. Una búsqueda dura 30 s como mucho, y entonces devuelve lo que tiene. `requests` es `null`: `n0-mainline` no cuenta las peticiones de una consulta.

**Anuncio.** El nodo pone a la vez el contacto y el anuncio (`put_mutable`). Cuando terminan, pone a la vez un *signed peer* por capacidad (`announce_signed_peer`). Así, un router ya tiene el anuncio cuando le llegan los *signed peers*. Un `put` termina cuando han contestado los nodos más cercanos o ha vencido el plazo. Uno que no llega a ningún nodo se repite una vez, tras buscar de nuevo su objetivo ([Un router que vuelve a arrancar](#un-router-que-vuelve-a-arrancar)). El anuncio falla, nombrándolos, si alguno sigue sin llegar. Antes de cambiar nada, comprueba que cada registro cabe en un item ([Límites](#límites-por-nodo)). Cada `REPUBLISH_S` lo pone todo otra vez: los mismos items, y *signed peers* firmados de nuevo, porque un router solo acepta una marca de tiempo con 45 s de diferencia como mucho. `unannounce` deja de republicar. Mainline no borra nada, así que lo guardado se queda mientras la biblioteca lo guarde ([Caducidad](#caducidad-y-republicación)).

**Cambio de red.** Cuando un proveedor anuncia un contacto con otras direcciones (S7), primero arranca un cliente de la DHT nuevo, en el mismo puerto y con la misma lista de arranque. La tabla de rutas de la biblioteca guarda cada nodo con la dirección por la que lo vio, de la red que el proveedor acaba de dejar. Solo saca un nodo tras 15 minutos sin verlo (`STALE_TIME`), y no ofrece forma de vaciarla. Sale de leer el código de `n0-mainline`: no se probó S7 sin el cliente nuevo.

### Routers en dos redes

Mainline nombra a cada nodo con una sola dirección. En las respuestas, un nodo va como 26 bytes: 20 de *node id* y 6 de IPv4 y puerto (BEP 5). La tabla de rutas de `n0-mainline` guarda una dirección por *node id*: la última por la que lo vio (`KBucket::add`). Los routers están en las dos redes, así que cada uno conoce a los demás por una sola de sus dos direcciones, y es la que da a quien le pregunta. Un nodo de la otra red no llega a esa dirección: no le pide nada ni le guarda nada.

Se vio en la primera ejecución de prueba del arnés, la misma del apartado siguiente, según sus ficheros `raw/`, que git ignora. Tras el arranque, `r1` guardaba los registros de los cuatro proveedores, y `r2`, `r3` y `r4` solo los de `p3` y `p4`, los de la red B. Toda la DHT conocía a esos tres routers por su dirección de la B. Los proveedores de la A, `p1` y `p2`, solo llegaban a `r1`, del que tienen las dos direcciones por `--bootstrap`. Los clientes llegan además a `r3`, su contacto guardado.

Con esta biblioteca no hay ajuste posible. El protocolo no lleva varias direcciones por nodo, y `n0-mainline` liga su socket a `0.0.0.0`, así que un router tampoco puede ser dos nodos, uno por red. Lo que mide el arnés con `r1` parado (S2) sale de aquí ([Resultados](#resultados)).

### Un router que vuelve a arrancar

`n0-mainline` no hace un `put` con una búsqueda nueva cada vez. Lo manda a los nodos, y con los *tokens*, que guardó de la última búsqueda de ese objetivo, mientras alguno de esos *tokens* tenga menos de 5 minutos (`get_cached_closest_nodes` y `Node::valid_token`). Un router que vuelve a arrancar tiene otro secreto de *tokens*, y rechaza los viejos con el error 203. La biblioteca da cualquier rechazo que no sea 301 o 302 como si el `put` no hubiera tenido respuesta: «PutQuery timed out with no responses neither success or errors» (`PutQuery::check`). Así, durante hasta 5 minutos, quien publica no llega a un router que ha vuelto.

Se vio en la primera ejecución de prueba del arnés, sobre un árbol sin commit y sin el ajuste siguiente. Su JSON se sobrescribió. Esto es lo que se anotó al leer sus ficheros `raw/` y los registros de los nodos, que git ignora. `r1` volvió a arrancar al final de S2, y en los 80 s de espera siguientes no recibió nada de nadie: el arnés no vio asentarse la topología. Las dos republicaciones de `p1` que siguieron fallaron con ese mensaje. Como `p1` solo llega a `r1` ([Routers en dos redes](#routers-en-dos-redes)), al empezar S3 ningún router guardaba nada suyo, y S3 no podía medir nada.

Por eso, un `put` que no llega a ningún nodo se repite una vez, después de una búsqueda del mismo objetivo (`get_mutable` o `get_signed_peers` hasta el final). Esa búsqueda guarda en la caché los nodos que contestan ahora, con *tokens* nuevos. La prueba `a_restarted_router_gets_the_records_again` lo comprueba: vuelve a arrancar un router en los mismos puertos y pide un anuncio más al proveedor. Se quitó la repetición de los items para comprobar que la prueba falla sin ella, y falló: el router que había vuelto no recibió los items en 20 s.

Solo cubre a quien no llega a ningún nodo. Un proveedor que llega a otros routers no ve el fallo, y el router que ha vuelto no recibe sus registros hasta que caduca su caché, en 5 minutos como mucho.

### Solo red privada

- La lista de arranque de la DHT es la de `--bootstrap`, entera (`DhtBuilder::bootstrap`), que sustituye a `DEFAULT_BOOTSTRAP_NODES`. Con la lista vacía, el nodo no conoce a nadie (S0 y la prueba `a_node_without_bootstrap_finds_nothing`). Las entradas son IP, así que no hay resolución DNS.
- `--bootstrap` es el `bootstrap_addr` de un router: todas sus direcciones con el puerto de la DHT, `ip:puerto` separados por comas, como en la línea base. El nodo las da todas a la biblioteca. La que está en una red que el nodo no tiene falla al enviar.
- No hay pkarr ni relays de ningún tipo. Iroh va como en la línea base.
- BEP 42 (identificadores seguros, derivados de la IP pública) no estorba: la biblioteca da por seguras las IP privadas (`Id::is_valid_for_ip`).

### Modo servidor y modo cliente

- **Routers:** modo servidor desde el arranque (`server_mode`).
- **Proveedores y clientes:** van en el modo cliente de `n0-mainline`, que es adaptativo y no se puede fijar. A los 15 minutos (`REFRESH_TABLE_INTERVAL`), un cliente que no está tras un cortafuegos pasa a modo servidor (`actor.rs`, «Adaptive mode … switching to server mode»). La biblioteca da a un nodo por fuera de un cortafuegos cuando le llega su propio ping a la dirección pública que le dicen los demás (`does_verify_our_new_public_address_with_self_ping`). En la 0.7.2 ese ping no se manda nunca: `Core::update_address_votes_from_iterative_query` guarda la dirección nueva antes de compararla con la que tenía, y la comparación nunca da distinta (`core.rs`, líneas 388 a 404). Así que un cliente sigue siéndolo. La prueba `providers_and_clients_stay_in_client_mode` lo comprueba con lo que dice la propia biblioteca (`Info::server_mode` e `Info::firewalled`). Una versión que corrigiera eso los pasaría a servidores a los 15 minutos, y habría que impedirlo, por ejemplo descartando en el gancho las peticiones que les llegan. La ejecución del arnés no lo puede ver: ningún nodo vive en ella 15 minutos seguidos. El que más, `r3`, 14 min 46 s, según `harness.log` y su registro, que git ignora.
- Las peticiones de un nodo en modo cliente van marcadas como de solo lectura (BEP 43), y los routers no lo meten en su tabla de rutas.

### Validación en el router

Un router comprueba cada `put` antes de que la biblioteca lo guarde. Como `RequestFilter` no deja leer los argumentos de una petición, lo hace en el `DatagramHook`, y lee él mismo el mensaje KRPC, con `serde_bencode`, que es lo que usa la biblioteca ([`krpc.rs`](../../../spikes/hito-3/mainline/src/krpc.rs) y [`router.rs`](../../../spikes/hito-3/mainline/src/router.rs)):

- **Item mutable** (`put` con clave): su *salt* tiene que ser una de las dos del índice (`unknown_key`) y su clave, la de un agente (`bad_key`). Su valor tiene que verificar como el registro de esa clase de `pals-core`, con su firma, su forma canónica y sus reglas (`bad_contact` o `bad_announce`). Además, el registro tiene que ser del agente cuya clave firma el item (`wrong_key`), tener el mismo `seq` que el item (`seq_mismatch`) y estar en plazo (`expired`). Lo de `rogue` en S5, un contacto de firma alterada, es `bad_contact`.
- ***Signed peer*** (`announce_signed_peer`): su clave tiene que ser la de un agente. Si el router aceptó un anuncio de ese agente, el anuncio más nuevo tiene que listar una capacidad con ese *infohash* (`capability_not_announced`). Sin anuncio aceptado, no sabe qué capacidad es ni si el agente la ofrece, y lo acepta.
- **Item inmutable** (`put` sin clave) y **BEP 5** (`announce_peer`): se rechazan (`unknown_key`). El índice no usa ninguno.

Lo rechazado nunca llega a la biblioteca. El gancho se queda el datagrama, contesta a quien lo envió con un error KRPC 203 («agentnet: \<motivo>»), lo suma a `records_rejected` y lo registra con su motivo. Lo aceptado sigue hasta la biblioteca, que aplica sus propias comprobaciones y aún puede rechazarlo.

Con `rogue` (S5), los routers rechazan su contacto y guardan su anuncio y su *signed peer*, que son válidos por sí mismos. Quien busca encuentra su clave, no encuentra contacto y lo descarta (`bad_contact`), como en los dos candidatos de Kademlia.

### Lo que un router de Mainline puede y no puede rechazar

| Comprobación | Quién la hace |
|-|-|
| La firma BEP 44 sobre la *salt*, el `seq` y el valor, y que el item esté en `SHA-1(clave ‖ salt)` | La biblioteca (error 206) |
| Un `seq` menor que el guardado, o un CAS que no coincide | La biblioteca (errores 302 y 301) |
| Un valor de más de 1000 bytes, una *salt* de más de 64 | La biblioteca (errores 205 y 207) |
| El *token* de BEP 5 | La biblioteca (error 203) |
| La firma de un *signed peer* sobre el *infohash* y su marca de tiempo, y que esa marca esté a 45 s o menos | La biblioteca (error 203) |
| Que el valor sea un registro de PALS válido, de la clave que firma, con su `seq` y en plazo | El gancho del candidato |
| Que un *signed peer* sea de una capacidad que lista el anuncio guardado | El gancho del candidato, si tiene el anuncio |

Lo que un router no puede hacer, con esta biblioteca:

- quitar o caducar nada de lo que guarda: no hay API para ello;
- saber si un *infohash* es el de una capacidad cuando no tiene el anuncio del agente. Cualquier clave puede poner *signed peers* bajo cualquier *infohash*;
- limitar a un emisor por su ritmo o su volumen: `RequestFilter` solo decide sí o no a cada petición, y la biblioteca no limita nada («it doesn't implement any rate-limiting or blocking», `core/server.rs`);
- leer los argumentos de una petición en `RequestFilter`.

**El *token* de BEP 5** no autentica a nadie. Es un resumen de la IP de quien pide (sin el puerto) y de un secreto del router, que cambia cada 5 minutos (`TOKEN_ROTATE_INTERVAL`) y vale con el secreto actual o el anterior. Prueba que quien hace el `put` recibió hace poco una respuesta en esa IP, así que impide poner con una IP de origen falsificada, y nada más.

### Lo que guarda un router: `stored_by_agent`

`n0-mainline` no deja leer lo que guarda su servidor. Por eso, cada segundo, el router se pregunta a sí mismo, por la dirección de bucle, por cada objetivo que conoce: un `get` de cada item y un `get_signed_peers` de cada *infohash* que nombró un `put` aceptado. Son las mismas consultas que manda quien busca. El gancho se queda las respuestas antes de que las vea la biblioteca, y dicen lo que la biblioteca serviría en ese momento. Es lo que cuentan las estadísticas del router. El router no quita nada por su cuenta: una entrada sale de las cuentas solo cuando la biblioteca deja de servirla. Las preguntas van por `lo`, que el arnés no cuenta en los bytes de la interfaz.

| Lo que guarda la biblioteca | Campo de `StoredRecords` | De qué agente |
|-|-|-|
| Item con *salt* `agentnet:contact:v1` | `contact_records` | El de la clave del item |
| Item con *salt* `agentnet:announce:v1` | `announce_records` | El de la clave del item |
| *Signed peer* | `provider_records` | El de su clave |

`records_stored` son los items que guarda y `provider_records_stored`, los *signed peers*. `other_records` queda a 0, porque el router solo pregunta por objetivos de items que aceptó. Todo lo que cuenta `records_stored` está atribuido.

Una respuesta de `get_signed_peers` lleva como mucho 10 *signed peers*, elegidos al azar si hay más (`SignedPeersStore::get_random_peers`). Con más de 10 proveedores de una capacidad en un router, las cuentas serían de una muestra. En la topología hay como mucho 3: dos proveedores y `rogue`.

**Las demás estadísticas.** `connections` es siempre 0: Mainline va en datagramas UDP sueltos, sin conexiones, y nadie marca el endpoint de Iroh, así que no hay nada más cercano que contar. `routing_peers` es el tamaño de la tabla de rutas principal de la biblioteca (`Info::routing_table_size`), sin la de los nodos que contestan *signed peers*, que se solapa con ella. Solo los routers dan `bootstrap_addr`: un nodo en modo cliente no contesta, así que no sirve de arranque.

### Caducidad y republicación

**`n0-mainline` 0.7.2 no caduca nada por tiempo.** Lo guarda todo en cachés LRU acotadas por número de entradas: 1000 items mutables, 1000 inmutables, 2000 *infohashes* y 500 pares por *infohash* (`MAX_VALUES`, `MAX_INFO_HASHES` y `MAX_PEERS` en `core/server.rs`, y `core/server/peers.rs` y `signed_peers.rs`). Una entrada solo sale cuando la desplaza otra, con la caché llena. `ServerSettings` solo deja cambiar esos números y el filtro. `mainline 8.0.1` hace lo mismo. Así que no hay nada que ajustar a `TTL_S`:

- el nodo republica cada `REPUBLISH_S`, 40 s;
- S3 se mide en el plazo normal del arnés, y la biblioteca no lo puede cumplir: los routers no olvidan nunca al proveedor parado, y las búsquedas lo siguen devolviendo como válido, porque los registros de `pals-core` valen 24 h;
- no hay filtro en el cliente ni barrido propio. La marca de tiempo de un *signed peer* llega en las respuestas, y quien busca podría descartar los viejos, pero eso sería emular la caducidad en el cliente.

### Límites por nodo

| Límite | Valor | De dónde |
|-|-|-|
| Valor de un item | 1000 bytes | BEP 44, en la biblioteca. Es menos que los 4 KiB de un registro de `pals-core` |
| *Salt* | 64 bytes | La biblioteca |
| Items mutables guardados | 1000 | `ServerSettings`, por defecto |
| *Infohashes* guardados | 2000 | Ídem |
| Pares por *infohash* | 500, y como mucho 10 en cada respuesta | Ídem |
| Plazo de una petición | El de la biblioteca, ajustado al tiempo de ida y vuelta, de 500 ms como mínimo | `MIN_REQUEST_TIMEOUT` |
| Tabla de rutas | Se refresca cada 15 min, hace ping a sus nodos cada 5 min y saca a uno a los 15 min sin verlo | La biblioteca, sin cambiar |
| Espera del arranque | 20 s | El candidato |
| Tiempo de una búsqueda entera | 30 s | El candidato |
| Preguntas del router a sí mismo | Cada segundo | El candidato |

**Lo que cabe en 1000 bytes.** Son tamaños medidos con los registros de la prueba, no cifras del JSON. Un contacto con una IPv4 ocupa 436 bytes, y con ocho IPv6, las que admite `pals-core`, 776, así que siempre cabe. El anuncio de `p1`, con dos capacidades, ocupa 544 bytes. Cada capacidad del vocabulario común suma unos 50, así que caben unas 10. Doce de 47 caracteres ya no caben (1071 bytes), y la prueba `an_item_holds_at_most_bep_44s_1000_bytes` lo comprueba. Un agente con más capacidades tendría que repartirlas en varios items, que el índice no define.

## Qué hubo que definir para tener un índice de capacidades

Mainline no trae nada de esto. Es el coste de definir el índice:

- **La clave de una capacidad:** el SHA-1 de su cadena. El prefijo `agentnet:capability:v1:` la separa de cualquier otro *infohash*.
- **La entrada del índice.** BEP 5 `announce_peer` guarda la IP y el puerto UDP de quien anuncia, que son los de su socket de la DHT, no los de Iroh, y nada lo ata a un agente. Quien busca tendría que pasar de esa dirección a un agente con otra búsqueda que habría que definir. Además, un proveedor que cambia de red dejaría una dirección vieja en el índice. Los *signed peers* guardan la clave del agente, así que no hay nada que traducir. A cambio, son una propuesta de BEP del autor de `mainline`, no un BEP aceptado. Van tras una feature `unstable_` y solo los contestan los nodos de esta biblioteca (`VERSIONS_SUPPORTING_SIGNED_PEERS`). En una red privada son todos.
- **Dos *salts*** para el contacto y el anuncio, y el `seq` de BEP 44 igual al del registro, para que el item más nuevo sea el registro más nuevo.
- **La validación en el router.** Hubo que leer KRPC en el gancho de datagramas, porque la biblioteca no expone otro sitio ([Validación en el router](#validación-en-el-router)).
- **Ver lo que guarda un router,** preguntándose a sí mismo ([Lo que guarda un router](#lo-que-guarda-un-router-stored_by_agent)).
- **Un cliente de la DHT nuevo al cambiar de red** ([Cómo está hecho](#cómo-está-hecho)), y **una búsqueda antes de repetir un `put` fallido** ([Un router que vuelve a arrancar](#un-router-que-vuelve-a-arrancar)).
- **El tamaño:** un anuncio con más de unas 10 capacidades no cabe en un item.

## Controles de abuso de Mainline

- ***Tokens* de BEP 5:** impiden poner con una IP de origen falsificada, nada más. Cualquiera que reciba en su IP los obtiene ([*token*](#lo-que-un-router-de-mainline-puede-y-no-puede-rechazar)).
- **Firmas:** un item BEP 44 o un *signed peer* solo lo puede escribir quien tiene la clave. Pero las claves no cuestan nada: un atacante puede crear tantas como quiera, y cada una escribe sus propios items y *signed peers* (Sybil).
- **Límites de almacenamiento:** son por número de entradas, en LRU, sin cuota por emisor. Una inundación de entradas válidas desplaza a las más antiguas, también a las honradas. El gancho de esta prueba solo deja pasar registros de PALS bien firmados, pero cualquier clave nueva los puede firmar.
- **Marca de tiempo de un *signed peer*:** 45 s de margen al ponerlo, para que no se pueda reenviar uno viejo. Hay uno por clave y *infohash*.
- **Identificadores seguros (BEP 42):** atan el *node id* a la IP pública para dificultar que alguien se coloque cerca de una clave. En una red privada no hacen nada, porque las IP privadas cuentan siempre como seguras.
- **`RequestFilter`:** puede decir no a una petición según su origen, pero no puede leer sus argumentos ni contar volumen. La biblioteca no limita ritmos.

La prueba no mide ningún abuso aparte del contacto de firma alterada de S5.

## API de `n0-mainline` usadas

Todas con `n0-mainline =0.7.2` y la feature `unstable_signed_peers`:

- `Dht::builder()` y `DhtBuilder::{bootstrap, port, server_mode, build}`;
- `Dht::{info, bootstrapped, set_datagram_hook, send_datagram, put_mutable, get_mutable, announce_signed_peer, get_signed_peers}`, y de `Info`, `local_addr`, `routing_table_size`, `server_mode` y `firewalled`;
- `MutableItem::{new, target_from_key, key, value, seq, salt, target}`, `Id`, `SigningKey` (el `ed25519-dalek 3.0.0` de `pals-core`) y `DatagramHook::new`;
- `SignedAnnounce::key`: el tipo no se exporta, pero sus métodos se pueden llamar sobre lo que devuelve `get_signed_peers`.

Del protocolo KRPC, el candidato lee `put` (`k`, `salt`, `v`, `seq`), `announce_signed_peer` (`info_hash`, `k`) y las respuestas a `get` (`k`, `v`) y `get_signed_peers` (`peers`, de 104 bytes cada uno), y escribe `get`, `get_signed_peers` y errores. La biblioteca solo acepta identificadores de transacción de 2 o 4 bytes. Las preguntas del router a sí mismo usaban al principio 6, y la biblioteca las descartaba sin contestar ni avisar. Lo encontró la prueba de integración.

## Código

Las líneas de `src/`, sin pruebas, con `wc -l` (todas) y sin las vacías ni los comentarios (`grep -v '^\s*//' | grep -cv '^\s*$'`):

| Fichero | Líneas | Sin comentarios ni vacías | Qué es |
|-|-|-|-|
| `index.rs` | 191 | 119 | El índice: *salts*, *infohash*, items y lo que acepta un router, sin red |
| `krpc.rs` | 206 | 146 | Los mensajes KRPC que lee el gancho del router y los que escribe |
| `router.rs` | 259 | 189 | El gancho del router: sus comprobaciones, sus errores y lo que guarda la biblioteca |
| `node.rs` | 514 | 409 | El `Backend`: Iroh y la DHT, el anuncio y su republicación, la búsqueda y las estadísticas |
| `lib.rs` | 20 | 5 | Los módulos |
| `main.rs` | 7 | 4 | `run_node` sobre el `Backend` |
| **Total** | **1197** | **872** | |

`index.rs`, `krpc.rs` y `router.rs` son lo que costó definir el índice y validarlo en los routers: 656 líneas, 454 sin comentarios ni vacías. `lib.rs` existe para que las pruebas lleguen al nodo y al índice.

Las pruebas son de dos clases:

- sin red: tres en `tests/index.rs` (el *infohash* de una capacidad, contra un SHA-1 calculado aparte; lo que acepta y rechaza un router, motivo por motivo; el límite de 1000 bytes) y tres en `krpc.rs` (leer una consulta, un `put` mutable frente a uno inmutable y un error);
- cinco con nodos reales sobre la dirección de bucle, en `tests/index.rs`, con tres routers del candidato, el primero sin arranque y los otros dos desde él. No se usa el `Testnet` de la biblioteca, porque sus routers no llevan el gancho del candidato. `announced_capability_is_found` busca lo que anunció un proveedor. `found_entries_lead_to_a_verified_contact` añade un proveedor con el contacto alterado: comprueba que los routers lo rechazan y lo cuentan, lo que guarda cada router de cada agente y que el cliente recibe al honrado con su contacto verificado y al otro como `bad_contact`. `a_node_without_bootstrap_finds_nothing` comprueba que un cliente sin lista de arranque no conoce a nadie ni encuentra nada, mientras otro, a su lado, conoce a los routers. Las dos últimas son las de [Un router que vuelve a arrancar](#un-router-que-vuelve-a-arrancar) y [Modo servidor y modo cliente](#modo-servidor-y-modo-cliente).

`CARGO_TARGET_DIR=target/msrv cargo +1.91 check --locked -p spike-mainline --all-targets` pasa: el candidato compila con la MSRV.

**Dependencias.** 306 crates, frente a 300 de la línea base, 370 de `kad-libp2p` y 337 de `kad-iroh`. Es una observación, no una cifra del JSON: el recuento de `compat.sh` hecho a mano sobre `cargo tree --locked -e normal --prefix none`. Sobre la línea base solo entran `n0-mainline`, `crc`, `crc-catalog`, `dyn-clone`, `lru 0.19` y `serde_bencode`. Lo demás que usa `n0-mainline` (`noq-udp`, `ed25519-dalek`, `sha1_smol`, `futures-core`…) ya estaba en el árbol de la línea base. La tarea 6 lo vuelve a medir, con el tiempo de compilación y el tamaño del binario.

## Resultados

Las cifras de esta sección son las de [`results/mainline.json`](results/mainline.json): la ejecución del arnés del 2026-11-16 sobre el commit `37ada0e`, con los parámetros comunes de `params.env` (`TTL_S` = 120 s, `REPUBLISH_S` = 40 s, `IDLE_S` = 300 s, 10 búsquedas por cliente y capacidad y `SEARCH_LIMIT` = 8). Las de la línea base y los candidatos de Kademlia son las de [`results/baseline.json`](results/baseline.json), [`results/kad-libp2p.json`](results/kad-libp2p.json) y [`results/kad-iroh.json`](results/kad-iroh.json). Según `harness.log`, que git ignora, la ejecución duró 15 min 16 s, en un anfitrión de 32 CPU con una carga media al empezar de 11,2.

| Escenario | Estado | Cifras |
|-|-|-|
| S0 | `pass` | 4 búsquedas, ningún candidato. `c0` acaba con 0 pares en la tabla de rutas y 0 conexiones |
| S1 | `pass` | 160 búsquedas, todas con los dos proveedores verificados. Primer candidato válido a los 2 ms (p50 y p95), siempre fechado por el backend. Búsqueda entera en 3 ms (p50) y 1102 ms (p95). Peticiones por búsqueda: `null`, la biblioteca no las cuenta. 13 922 bytes netos por búsqueda (p50), 15 383 (p95) |
| S2 | `fail` | Con `r1` parado, ninguna búsqueda de los clientes está completa (0 %). Su primer candidato válido llega a 1 ms (p50 y p95), siempre fechado por el backend, y la búsqueda termina a los 925 ms (p50) y 1459 ms (p95). `c5` tiene su primer candidato válido 2,2 s después de arrancar su contenedor, pero en sus 84 búsquedas nunca encuentra a todos los proveedores |
| S3 | `fail` | Los routers guardaban 4 registros de `p1` antes de pararlo. En los 34 sondeos, ni los routers dejan de guardarlos ni las búsquedas dejan de devolverlo: `routers_forget_s` y `searches_forget_s` son `null`. El límite es 160 s |
| S4 | `pass` | Cobertura inmediata 1,0, completa desde el principio (0 s). Con `r2` y `r4` parados, el primer candidato válido llega a los 2 ms (p50) y 3 ms (p95), siempre fechado por el backend, y la búsqueda termina a los 908 ms (p50) y 1200 ms (p95) |
| S5 | `pass` | Tres routers rechazan el contacto de `rogue`, 6 rechazos en total. Ninguno lo guarda y `records_stored` no crece sin explicación. Los clientes lo reciben las 4 veces como `bad_contact`, nunca como válido |
| S6 | `measured` | Ver la tabla siguiente |
| S7 | `pass` | Los cuatro clientes reciben el contacto nuevo de `p3`, con su IP en la red A, a los 7,2 s (p50) y 7,4 s (máximo) de la desconexión. El límite es 80 s |

En reposo (S6), por papel, con la cifra de la línea base, la de `kad-libp2p` y la de `kad-iroh` entre paréntesis, en ese orden. Todas son p50, salvo las conexiones, que van como p50 / máximo:

| Papel | RSS (KiB) | Conexiones | Tabla de rutas | Bytes por minuto | Enviados por minuto |
|-|-|-|-|-|-|
| Router | 16 020 (14 396; 23 240; 21 128) | 0 / 0 (0 / 0; 0 / 1; 1 / 4) | 2 (1; 3; 3) | 11 364 (444; 249 735; 355 418) | 2774 (0; 126 338; 174 690) |
| Proveedor | 15 640 (14 612; 22 764; 20 372) | 0 / 0 (0 / 0; 0 / 4; 0 / 4) | 2 (1; 4; 4) | 5992 (222; 180 386; 218 700) | 4031 (0; 88 376; 112 145) |
| Cliente | 15 584 (14 824; 22 748; 20 400) | 0 / 0 (0 / 0; 0 / 0; 0 / 0) | 3 (2; 4; 4) | 359 (222; 18 262; 27 142) | 171 (0; 8997; 14 162) |

Lo que explica las cifras, sin que el JSON lo desglose. Lo que viene de los ficheros `raw/` y de los registros de la ejecución, que git ignora, lo dice cada punto:

- **S1: la red A espera.** Según `raw/S1.jsonl`, las búsquedas de `c3` y `c4`, en la red B, terminan en 3 ms (p50 y máximo), y las de `c1` y `c2`, en la A, a los 915 y 917 ms (p50). `r1` da a los de la A las direcciones de la B de los otros tres routers ([Routers en dos redes](#routers-en-dos-redes)). La consulta les manda peticiones que no llegan, y espera a que venzan. Los candidatos llegan en milisegundos igualmente.
- **S2: la mitad de los proveedores se pierde.** Según `raw/settle.jsonl`, tras el arranque, `r1` guardaba los registros de los cuatro proveedores y los otros tres routers solo los de `p3` y `p4`. Con `r1` parado, cada una de las 16 búsquedas de los clientes y de las 84 de `c5` encontró solo a `p3` o a `p4`, según `raw/S2.jsonl`.
- **S3: nada caduca.** Según `raw/S3.jsonl`, solo `r1` guardaba registros de `p1`, sus 4: el contacto, el anuncio y un *signed peer* por cada una de sus dos capacidades. En el último sondeo, 203,5 s después de pararlo, los seguía guardando, y las 136 búsquedas de la observación devolvieron a `p1` como válido. Es lo que se espera del código de la biblioteca ([Caducidad](#caducidad-y-republicación)). Las `notes` de S3 en el JSON son las del arnés, que no tiene sitio para el motivo que da el backend; ese motivo, con su fuente, está aquí.
- **S5: dos rechazos por router.** Cada uno de los tres routers rechazó el contacto de `rogue` dos veces: el `put` y su repetición tras una búsqueda ([Un router que vuelve a arrancar](#un-router-que-vuelve-a-arrancar)). `r3` no recibió nada de `rogue`, según `raw/S5.jsonl`. Además, `r2` y `r4`, que el arnés vuelve a arrancar al final de S4, no guardaban nada al empezar S5: `p3` y `p4` seguían llegando a `r1` y a `r3`, así que no repetían sus `put` y les duraban los *tokens* viejos de su caché.
- **S6: poco tráfico.** Mainline va en datagramas UDP sueltos, sin apretones de manos ni latidos que mantengan conexiones. Lo que queda es la republicación: cada 40 s, cada proveedor pone sus dos items y sus dos *signed peers*, y cada `put` pasa antes por una búsqueda cuando su caché caduca, a los 5 minutos. Las preguntas de los routers a sí mismos van por `lo`, que no cuenta. La tabla de rutas de un router es pequeña (2, p50): un router solo aprende nodos de sus propias búsquedas, y `r1`, además, de quien le manda `find_node`.
- **S7.** Según el registro de `p3`, arrancó su cliente de la DHT nuevo y 3,1 s después había anunciado su contacto nuevo: el arranque del cliente, su búsqueda inicial y los `put`. Los clientes lo resolvieron 7,2 s después de la desconexión (p50).

## Lo que no se pudo medir

- **Escala.** Con 4 routers, menos que `K` = 20, todos están entre los más cercanos a cualquier clave y guardan todo. La prueba no ejercita el encaminamiento por distancia.
- **Abuso.** No se prueban identidades falsas en masa (Sybil), inundaciones que llenen las LRU ni *signed peers* bajo *infohashes* ajenos. El único registro inválido es el contacto de firma alterada de S5.
- **NAT y relays.** Las redes son privadas y sin NAT. Mainline no tiene relays, y en la red pública un nodo tras NAT se queda en modo cliente.
- **El contacto en uso.** Nadie marca el endpoint de Iroh que nombra el contacto: S7 comprueba que el cliente recibe la dirección nueva, no que conecte con ella.
- **Peticiones por búsqueda.** La biblioteca no las cuenta.
- **Lo que la biblioteca rechaza tras el gancho** (un *token* caducado, un `seq` viejo). El router no lo cuenta en `records_rejected`, y no se ve aparte.
- **El hueco del arranque.** El gancho se pone con un mensaje al actor de la DHT justo después de crearla, porque el constructor no lo admite. Un `put` que llegara antes de ese mensaje lo guardaría la biblioteca sin comprobar. No se midió.

## Metadatos que ve un nodo intermedio

- **Nada va cifrado ni autenticado.** KRPC es UDP en claro: un router, y cualquiera en el camino, ve cada mensaje entero.
- **Quién busca.** El router ve la IP y el puerto del socket de la DHT de quien busca, su *node id* (aleatorio en cada arranque), su versión de la biblioteca (`RS`, 0.6) y que es de solo lectura. Buscar no firma nada, así que no ve la clave del agente. Pero la IP lo identifica en una red pequeña.
- **Qué busca.** La clave de `get_signed_peers` es el SHA-1 de la cadena de la capacidad, que con un vocabulario público se recupera con un diccionario. Después ve los `get` de `SHA-1(clave ‖ salt)` de cada proveedor. Un router que conoce las claves de los agentes y las dos *salts* sabe a qué proveedores mira quien busca. Los nodos por los que pasa una consulta iterativa ven también esas claves.
- **Quién ofrece.** Cada 40 s, cada proveedor le envía su clave, su contacto, con sus IP, su anuncio, con sus capacidades, y un *signed peer* por capacidad, con una marca de tiempo. El router sabe quién ofrece qué, dónde está y si sigue vivo.

Lo que guarda un router lo puede pedir cualquiera que conozca la clave del agente o el *infohash*.
