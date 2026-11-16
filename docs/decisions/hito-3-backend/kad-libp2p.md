# Candidato `kad-libp2p`: Iroh más `libp2p-kad` en su propia pila

Tarea 3 de la [fase 1 del hito 3](../../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). Es el primero de los tres candidatos que la tarea 6 compara con la línea base. El código está en [`spikes/hito-3/kad-libp2p/`](../../../spikes/hito-3/kad-libp2p/) y cumple el contrato de nodo de [`spikes/hito-3/README.md`](../../../spikes/hito-3/README.md#el-contrato-de-nodo). Los resultados de su ejecución del arnés están en [`results/kad-libp2p.json`](results/kad-libp2p.json).

No es código del producto. El registro `agentnet.spike-kadlink/0` es provisional.

## La duda que resuelve

El spec (§6.1) deja como candidato inicial un módulo de `rust-libp2p` para Kademlia dentro del daemon, junto a Iroh, «pendiente de una prueba acotada de integración y coste». La duda es si las dos pilas conviven en un mismo nodo, con dos identidades vinculadas como pide el §6.2, y qué cuesta: dependencias, memoria, conexiones, tráfico en reposo y tiempos de búsqueda, frente a la línea base, que solo tiene Iroh.

**Sí: las dos pilas conviven y la DHT cumple los escenarios comunes, pero hubo que ajustar la integración y cuesta.**

- **Integración.** Iroh 1.4.1 y libp2p 0.57.1 compilan juntos, también con la MSRV 1.91, cada pila en su puerto UDP. El vínculo firmado por las dos claves deja resolver el endpoint Iroh de un proveedor sin conectar antes con él por libp2p (§6.2). En la ejecución del arnés, S0 a S5 y S7 quedan en `pass` y S6 en `measured`, sin ningún `error`.
- **Ajustes.** Hubo que sortear cinco comportamientos de `libp2p-kad` y de libp2p para que funcionara en una red privada con dos subredes ([API de `libp2p` usadas](#api-de-libp2p-usadas)). Sin dos de ellos, la red se degradaba después del primer router parado: S4 acababa en `fail` con una cobertura de 0,812 (ver [Direcciones de los routers](#direcciones-de-los-routers)).
- **Coste, frente a la línea base:**
  - dependencias: 370 crates frente a 300, por el recuento de `compat.sh` sobre `cargo tree`; la tarea 6 lo vuelve a medir, con el tiempo de compilación y el tamaño del binario;
  - memoria en reposo: unos 22 a 23 MiB por nodo frente a 14 a 15 MiB;
  - tráfico en reposo, con la republicación de la prueba cada 40 s: un router mueve unos 250 KB por minuto (127 KB enviados), un proveedor 181 KB y un cliente 19 KB, frente a menos de 0,5 KB;
  - por búsqueda: 22 peticiones y unos 31,6 KB, porque cada `get_record` pide el valor a los cuatro routers.

El detalle está en [Resultados](#resultados).

## Cómo está hecho

Cada nodo lleva dos pilas, como las llevaría el daemon:

- **Iroh**, arrancado como en el daemon con `relay = false` y como en la línea base: un endpoint con la clave del agente, en UDP en el puerto de `--listen` (4433), sin relays, sin mapeo de puertos y sin búsqueda de direcciones. El contacto lo nombra; en la prueba nadie lo marca.
- **libp2p**, con QUIC en UDP en el puerto siguiente (4434), porque Iroh y QUIC de libp2p no pueden compartir un puerto UDP. Lleva `libp2p-kad` y `identify`, y nada más.

**Dos claves.** El `PeerId` es el de una clave Ed25519 de libp2p, distinta de la del agente. En la prueba se deriva de la semilla del agente, con separación de dominio: la semilla de la clave libp2p es el SHA-256 de `agentnet.spike-kadlink/0\nlibp2p-key\n` seguido de los 32 bytes de la semilla del agente. Así, un nodo que el arnés vuelve a arrancar conserva su `PeerId`. Es un atajo de la prueba: el producto generaría la clave y la guardaría.

**Lo que se publica**, todo bajo el `PeerId` del nodo:

| Clave de Kademlia | Valor |
|-|-|
| `/agentnet/link/<peer_id>` | El vínculo de identidades `agentnet.spike-kadlink/0`, que lleva dentro el contacto |
| `/agentnet/announce/<peer_id>` | El `Signed<AnnounceRecord>` de `pals-core`, sin cambios |
| Multihash SHA2-256 (código `0x12`) de la cadena completa de cada capacidad (§5.1) | Un provider record con el `PeerId` |

**El vínculo** (§6.2) es el documento `{"schema", "peer_id", "agent_id", "contact", "issued_at", "expires_at"}`, donde `contact` es el `Signed<ContactRecord>` del agente como objeto JSON. Viaja como el JCS de `{"link": <documento>, "libp2p_key", "libp2p_signature", "agent_signature"}`:

- `libp2p_key` es la clave pública libp2p en su codificación protobuf;
- `libp2p_signature` es la firma de esa clave sobre `agentnet.spike-kadlink/0\nlibp2p\n` seguido del JCS del documento;
- `agent_signature` es la de la clave del agente sobre `agentnet.spike-kadlink/0\nagent\n` seguido del JCS del documento.

Las tres van en base64url sin relleno. La verificación ([`link.rs`](../../../spikes/hito-3/kad-libp2p/src/link.rs)) comprueba, por este orden, el tamaño (6 KiB como mucho), que el texto es canónico, los campos, el esquema y la forma de la vida (como mucho un día), que la clave libp2p es la que nombra el `PeerId`, las dos firmas, el contacto tal como lo comprueba `pals-core`, que el contacto es del mismo agente, y que el vínculo y el contacto están en plazo. El primer fallo da el motivo.

**Búsqueda.** El nodo pide los proveedores de la capacidad (`get_providers`) y, según llega cada `PeerId`, hasta `limit`, su vínculo y su anuncio (`get_record`). Verifica cada copia del vínculo, se queda con la válida de mayor `seq` del contacto y pasa a `run_node` el contacto que lleva y el anuncio del mismo agente, que `spike_common::verify` vuelve a comprobar. El `found_ms` de cada candidato es cuándo la búsqueda tuvo por primera vez un vínculo y un anuncio válidos del mismo agente, `requests` son las peticiones de la búsqueda que salen a la red. Es la suma de `QueryStats::num_requests` de todas sus consultas menos los intentos de conexión a proveedores que el propio nodo deniega (ver el párrafo siguiente). `libp2p-kad` cuenta una petición cuando la consulta elige un par, antes de marcarlo, y no distingue la que nunca sale. Con cuatro routers y dos proveedores son 4 peticiones de `get_providers` y 16 de `get_record` (4 routers por el vínculo y el anuncio de cada uno de los dos proveedores). Una búsqueda dura 30 s como mucho, y entonces devuelve lo que tiene.

**Quien busca no marca a los proveedores.** `libp2p-kad` añade cada proveedor que le devuelve un router a la consulta iterativa de `get_providers`, como si fuera un nodo más de la DHT al que preguntar. Un proveedor que no es router no puede contestar, porque es cliente de la DHT. Si está en otra red, retiene la consulta hasta que el apretón de manos QUIC de libp2p se rinde, a los 5 s: para QUIC, un datagrama sin ruta es solo un paquete perdido. Se vio en la primera ejecución de prueba del arnés, sobre un árbol sin commit, cuyo JSON no se conservó. Según lo que se anotó de sus primeras búsquedas de S1, cada una tenía los dos candidatos en 2 a 13 ms y terminaba a los 5 s justos. Por eso, mientras dura una búsqueda, el nodo no marca a los proveedores que encuentra y que no están en su tabla de rutas. Lo hace con un comportamiento propio, `NoDial`: deniega el intento antes de que empiece, así que falla al momento y la consulta sigue sin ellos, y cuenta cada denegación. A diferencia de `allow_block_list` de libp2p, no cierra ninguna conexión ni impide que el par conecte. Al proveedor se llega por el contacto de su vínculo, nunca por libp2p. La prueba de integración lo comprueba: tras la búsqueda, el cliente solo tiene conexiones con los dos routers.

**Anuncio.** Antes de cambiar nada, el nodo comprueba que su tienda tiene sitio para el vínculo, el anuncio y los provider records de sus capacidades: `put_record` y `start_providing` solo fallan cuando no lo hay, y así un anuncio que no cabe falla sin haber tocado nada. Después deja de anunciar las capacidades que el anuncio nuevo ya no lista, pone el vínculo y el anuncio con `Quorum::All` y, cuando los dos han llegado al menos a un router, anuncia cada capacidad (`start_providing`). Un `put` que no llega a ningún router se repite cada segundo durante 25 s, dentro de los 30 s que espera la API de control. Hace falta en S7: tras el cambio de red, los primeros `put` van por conexiones muertas que libp2p todavía no ha cerrado. Si a los 25 s algún registro no ha llegado a ningún router, el nodo anuncia igualmente las capacidades y contesta con un error: guarda los registros nuevos y la biblioteca los republica, así que sus provider records tienen que corresponder con ellos. Queda un hueco, solo en un nodo que fuera a la vez router y proveedor, cosa que la topología no tiene: entre la comprobación y `start_providing`, los provider records de otros podrían llenar su tienda. Entonces deja de anunciar las capacidades que esa llamada había empezado y falla, con el vínculo y el anuncio nuevos ya en la DHT. `unannounce` quita los registros propios (`remove_record`, `stop_providing`). Kademlia no borra nada, así que las copias de los routers caducan solas.

### Solo red privada

- No hay nodos de arranque por defecto: ni los de IPFS ni ningún otro. El nodo arranca solo desde `--bootstrap`, que es el `bootstrap_addr` de un router: todas sus direcciones como multiaddrs terminados en `/p2p/<PeerId>`, separados por comas.
- El protocolo de Kademlia es `/agentnet/kad/1.0.0`, nunca `/ipfs/kad/1.0.0`.
- No hay mDNS, relay, AutoNAT, UPnP ni DCUtR. El binario no enlaza esos crates: `libp2p` lleva solo `kad`, `quic`, `tokio`, `macros`, `identify` y `ed25519`.
- Kademlia aprende direcciones de `--bootstrap`, de `identify` y de las respuestas de otros servidores. De `identify` solo aprende las de pares que hablan `/agentnet/kad/1.0.0` como servidores, y solo las que el par declara externas. Un nodo con la lista de arranque vacía no conoce a nadie (S0 y la prueba `a_node_with_no_bootstrap_finds_no_one`).

**Direcciones externas.** Un provider record lleva las direcciones externas confirmadas de su proveedor. Un router deja fuera de sus respuestas a `GET_PROVIDERS` el que no lleva ninguna si su `PeerId` no está en su tabla de rutas, y ningún nodo en modo cliente lo está. Sin AutoNAT, nadie confirma una dirección. Por eso cada nodo declara como externas sus propias direcciones locales (`add_external_address`), que en una red privada sin NAT son alcanzables, y las renueva en cada anuncio (S7). Quien busca nunca las marca: llega al proveedor por el contacto de su vínculo. Sin esto, los proveedores eran invisibles. Lo encontró la prueba de integración sobre loopback, antes de la primera ejecución del arnés.

### Direcciones de los routers

Dos comportamientos de las bibliotecas rompían la red después del primer router parado. Los encontró la segunda ejecución de prueba del arnés, que ya llevaba los ajustes de las direcciones externas y de no marcar a los proveedores, pero no estos dos. Esa ejecución se hizo sobre un árbol sin commit, y su JSON y sus registros no se conservaron. Las cifras siguientes son las que dio `evaluate.py` sobre sus observaciones, anotadas al leerlas:

- S1 salió entero, como en la ejecución buena;
- en S3, después de volver a arrancar `r1`, 5 de las 40 búsquedas no devolvieron ningún candidato, y las de `c1` y `c2` tardaban unos 5 s cada una;
- S4 acabó en `fail`, con una cobertura inmediata de 0,812 y sin llegar a la cobertura completa en los 120 s del bucle: le dio tiempo a hacer 2 rondas. Además, a varias búsquedas les faltaba el anuncio o el vínculo de un proveedor;
- en S5, los clientes solo encontraron a `rogue` en 2 de sus 4 búsquedas.

Una tercera ejecución, también sin commit y con los registros de depuración de `libp2p-kad` y `libp2p-swarm`, dio las dos causas.

- **La dirección de bucle.** Todos los nodos escuchan en `0.0.0.0` y en el mismo puerto. Por defecto, `identify` anuncia también `127.0.0.1`, guarda en una caché lo que anuncia cada par y lo añade a cada intento de conexión a ese par. El intento a `127.0.0.1` llega al propio nodo, contesta antes que el bueno y falla con `WrongPeerId`, y con él falla el intento entero. Mientras las conexiones seguían abiertas, como en S1, no se notaba. Cuando se cerraban por inactividad y había que volver a conectar, las cuatro peticiones de una búsqueda podían fallar a la vez, en milisegundos. Ahora `identify` anuncia solo las direcciones externas del nodo (`with_hide_listen_addrs`), que no incluyen el bucle, y no guarda caché (`with_cache_size(0)`): las direcciones de Kademlia salen solo de lo que el nodo le da.
- **Las direcciones que `libp2p-kad` olvida.** Cuando falla una conexión a un par, `libp2p-kad` le quita las direcciones que fallaron, menos la última, aunque el fallo sea pasajero; su propio código lo marca como pendiente de mejorar. Con `r1` parado en S2, los nodos de la red A le quitaron su dirección de la A y se quedaron con la de la B, que no alcanzan. Cuando `r1` volvió, siguió fuera de su alcance: cada búsqueda esperaba 5 s su conexión imposible, y los proveedores de la A no le llegaban. Ahora el nodo recuerda las direcciones de cada servidor que le dieron `--bootstrap` o `identify` y se las devuelve a Kademlia tras cada intento fallido. Un servidor caído se vuelve a intentar en todas sus direcciones, y uno que ha vuelto contesta en la buena.

Lo que queda es de la biblioteca y se mide tal cual. `libp2p-kad` no saca de la tabla de rutas a un router caído mientras su cubo no esté lleno, y cada consulta lo espera hasta que se rinde el apretón de manos QUIC, a los 5 s. Mientras haya un router caído entre los más cercanos a una clave, una búsqueda no termina hasta ese plazo, aunque sus candidatos lleguen antes. El arnés lo mide en S2 y en S4: `connected_first_valid_ms` y `degraded_first_valid_ms` frente a `connected_elapsed_ms` y `degraded_elapsed_ms` ([Resultados](#resultados)).

### Validación en el router

Con `StoreInserts::FilterBoth`, cada `PutRecord` y cada `AddProvider` que recibe un router pasa por [`validate.rs`](../../../spikes/hito-3/kad-libp2p/src/validate.rs) antes de llegar a la tienda (`store_mut().put` y `add_provider`). Lo que rechaza suma a `records_rejected` y se registra con su motivo.

Kademlia lleva cada registro por separado y en cualquier orden: los `put` de un mismo publicador compiten entre sí, y los routers replican lo que guardan. Un router solo puede exigir a cada registro lo que prueba por sí mismo, y rechaza lo que contradice lo que ya guarda:

- **Vínculo:** tiene que verificar entero y estar bajo la clave de su propio `PeerId`. Uno de `P` bajo `/agentnet/link/<Q>` se rechaza (`wrong_key`), y también uno cuyo contacto no verifica (`bad_contact`), que es lo que publica `rogue` en S5.
- **Anuncio:** tiene que verificar como el de `pals-core` y estar en plazo. Si el router guarda el vínculo de ese `PeerId`, tiene que ser del agente que el vínculo nombra (`agent_mismatch`). Sin vínculo guardado, se guarda por su propia firma, y la vinculación la comprueba quien busca.
- **Provider record:** solo nombra un `PeerId`, y `libp2p-kad` ya descarta el que no es del par que lo envía (comprueba que `provider.node_id` es el `PeerId` de la conexión). Su clave es un hash, así que el router solo puede comprobar su forma (un multihash SHA2-256, `bad_provider_key`) y, si guarda el anuncio de ese `PeerId`, que el anuncio lista una capacidad con ese hash (`capability_not_announced`). Sin anuncio guardado, no puede saber qué capacidad es ni si el `PeerId` la ofrece, y lo guarda.
- **Antiguo:** un vínculo o un anuncio con un `seq` menor que el que el router guarda del mismo agente no se guarda, para que una copia replicada no sustituya a una más nueva. No cuenta como rechazado, porque no tiene nada inválido.
- Cualquier otra clave de valor se rechaza (`unknown_key`).

Así, con `rogue` (S5), los routers rechazan su vínculo, porque lleva el contacto de firma alterada. En cambio, guardan su anuncio y su provider record, que son válidos por sí mismos. Quien busca encuentra su `PeerId`, no encuentra vínculo y lo descarta (`bad_contact`).

La alternativa, guardar el anuncio y el provider record de un `PeerId` solo cuando el router ya tiene su vínculo, dejaría a `rogue` sin nada en los routers. Pero depende del orden de llegada, que Kademlia no garantiza: un router recién arrancado recibe los registros por republicación y replicación en cualquier orden, y descartaría registros honrados hasta la vuelta siguiente.

### `stored_by_agent` en los routers

| Registro guardado | Campo de `StoredRecords` | De qué agente |
|-|-|-|
| Vínculo (`/agentnet/link/…`) | `contact_records`, porque lleva el contacto | El del vínculo verificado |
| Anuncio (`/agentnet/announce/…`) | `announce_records` | El del anuncio verificado |
| Provider record | `provider_records` | El que el último vínculo guardado de ese `PeerId` le asigna |

`other_records` queda siempre a 0: el router no guarda otros valores. Todo valor que cuenta `records_stored` está atribuido, así que S5 no tiene crecimiento sin explicar.

Un provider record solo nombra un `PeerId`. El router recuerda el agente que le asignó el último vínculo que guardó mientras guarde algo de ese `PeerId`: su vínculo, su anuncio o alguno de sus provider records. Así, si el vínculo caduca un momento antes que los provider records, estos siguen atribuidos hasta que caducan también (S3). Un provider record de un `PeerId` del que el router nunca guardó un vínculo, como el de `rogue`, cuenta en `provider_records_stored` pero no en `stored_by_agent`.

### Caducidad y republicación

Son las de la biblioteca:

- `set_record_ttl` y `set_provider_record_ttl` a `TTL_S` (120 s);
- `set_publication_interval` y `set_provider_publication_interval` a `REPUBLISH_S` (40 s);
- además, `set_replication_interval` a `REPUBLISH_S`. En `libp2p-kad` 0.49 el trabajo que republica los registros propios es el de replicación: corre cada intervalo de replicación (una hora por defecto) y solo republica en las vueltas en que ya ha pasado el intervalo de publicación. Sin esto, los valores se republicarían cada hora.

La replicación entre routers no alarga la vida de un registro: el que lo recibe se queda con la caducidad menor. Los provider records no se replican.

`libp2p-kad` borra lo caducado con pereza: cuando lo lee (y entonces no lo sirve) o en las vueltas de su trabajo, que no pasa por los provider records ajenos. Por eso el nodo barre su tienda cada segundo. Quita lo que la propia biblioteca ya marcó como caducado (`Record::is_expired` y `ProviderRecord::is_expired`, con el instante que puso la biblioteca según su TTL), para que lo que cuenta un router sea lo que todavía serviría. No es un filtro del cliente: quien busca no descarta nada por antigüedad.

### Límites por nodo

| Límite | Valor | Por qué |
|-|-|-|
| `MemoryStoreConfig::max_records` | 1024 | Dos valores por agente, vínculo y anuncio: 512 agentes |
| `MemoryStoreConfig::max_value_bytes` | 8 KiB | Por encima del límite del vínculo (6 KiB) y del anuncio (4 KiB) |
| `MemoryStoreConfig::max_providers_per_key` | 20 | `K`, el factor de replicación; los siguientes se ignoran, como hace `libp2p-kad` contra inundaciones |
| `MemoryStoreConfig::max_provided_keys` | 1024 | La tienda cuenta todas las claves de las que guarda proveedores, no solo las propias |
| Tamaño de un vínculo | 6 KiB | Un contacto de 4 KiB como mucho, los campos del vínculo, la clave y dos firmas |
| Paquete de Kademlia | 16 KiB | El de la biblioteca, sin cambiar |
| Tiempo de una consulta de Kademlia | 20 s | 60 s por defecto |
| Tiempo de una búsqueda entera | 30 s | Después devuelve lo que tiene |
| Reintentos de un anuncio | 25 s | Dentro de los 30 s de la API de control |
| Conexión ociosa | 10 s | El de `libp2p-swarm`, sin cambiar: la conexión se cierra tras 10 s sin nada que la mantenga |
| Arranque periódico de Kademlia | 5 min | El de la biblioteca, sin cambiar |

## API de `libp2p` usadas

Todas con `libp2p =0.57.1`, que trae `libp2p-kad 0.49.0`, `libp2p-identify 0.48.0`, `libp2p-quic 0.14.0`, `libp2p-swarm 0.48.0` y `libp2p-identity 0.3.1`:

- **`libp2p-kad`**:
  - `kad::Config::new(StreamProtocol::new("/agentnet/kad/1.0.0"))`, con `set_record_ttl`, `set_provider_record_ttl`, `set_publication_interval`, `set_provider_publication_interval`, `set_replication_interval`, `set_record_filtering(StoreInserts::FilterBoth)`, `set_caching(Caching::Disabled)` y `set_query_timeout`;
  - `kad::Behaviour::with_config`, `set_mode(Some(Mode::Server))` en los routers y `Some(Mode::Client)` en el resto, `add_address`, `bootstrap`, `put_record` con `Quorum::All`, `remove_record`, `start_providing`, `stop_providing`, `get_providers`, `get_record`, `query_mut(..).finish()`, `kbuckets` y `store_mut`;
  - `store::MemoryStore::with_config(MemoryStoreConfig)` y `RecordStore` (`get`, `put`, `remove`, `records`, `add_provider`, `providers`, `remove_provider`), con `Record::is_expired` y `ProviderRecord::is_expired`;
  - los eventos `Event::InboundRequest` (`PutRecord` y `AddProvider`, con el registro que trae `FilterBoth`) y `Event::OutboundQueryProgressed` (`GetProviders`, `GetRecord`, `PutRecord`, `StartProviding`, `Bootstrap`), con `QueryStats::num_requests` y `ProgressStep::last`.
- **`libp2p-identify`:** `identify::Behaviour::new(identify::Config::new(..).with_hide_listen_addrs(true).with_cache_size(0))` e `identify::Event::Received`. De sus `listen_addrs`, que con esa configuración son las direcciones externas del par, se añaden a Kademlia las de los pares que hablan su protocolo.
- **`libp2p-swarm` y `libp2p-quic`:** `SwarmBuilder::with_existing_identity(..).with_tokio().with_quic()`, `#[derive(NetworkBehaviour)]`, `listen_on`, `add_external_address`, `remove_external_address`, `external_addresses` y `network_info().connection_counters()`.
- **El comportamiento propio `NoDial`:** implementa `NetworkBehaviour` con un `dummy::ConnectionHandler` y deniega en `handle_pending_outbound_connection` los intentos de conexión a los proveedores que una búsqueda encuentra.
- **`libp2p-identity`:** `Keypair::ed25519_from_bytes`, `Keypair::sign`, `PublicKey::encode_protobuf`, `PublicKey::try_decode_protobuf`, `PublicKey::verify` y `PublicKey::to_peer_id`.

Todo lo que pedía la tarea existe en esta versión: `FilterBoth` para valores y para provider records, los TTL de los dos y sus intervalos de republicación. Hay cinco comportamientos que no son evidentes y que obligan a ajustar algo, explicados arriba:

- la republicación de valores depende del intervalo de replicación ([Caducidad y republicación](#caducidad-y-republicación));
- un provider record sin direcciones externas no sale en las respuestas;
- los proveedores encontrados entran en la consulta iterativa;
- la caché de `identify` mete la dirección de bucle en las conexiones;
- `libp2p-kad` olvida direcciones tras un fallo pasajero.

El segundo y el tercero están en [Cómo está hecho](#cómo-está-hecho), y los dos últimos en [Direcciones de los routers](#direcciones-de-los-routers).

## Código

Las líneas de `src/`, sin pruebas, con `wc -l` (todas) y sin las vacías ni los comentarios:

| Fichero | Líneas | Sin comentarios ni vacías | Qué es |
|-|-|-|-|
| `main.rs` | 104 | 78 | El `Backend`: arranca Iroh y libp2p, firma el vínculo |
| `behaviour.rs` | 1396 | 1134 | El swarm, `NoDial`, la tarea que lo mueve, la validación de lo que llega, la búsqueda, el anuncio, la caducidad y los contadores |
| `link.rs` | 265 | 197 | El vínculo `agentnet.spike-kadlink/0` |
| `validate.rs` | 193 | 129 | Las claves de la DHT y lo que guarda un router |
| `lib.rs` | 34 | 9 | Los módulos y `now_unix` |
| **Total** | **1992** | **1547** | |

`lib.rs` existe para que las pruebas lleguen al vínculo y a sus validadores, y para que la tarea 4 parta de ellos. `link.rs` y `validate.rs` no dependen del transporte. De `behaviour.rs`, la tarea 4 puede tomar la configuración de Kademlia, la validación, la búsqueda, el anuncio, la caducidad y los contadores. Tiene que cambiar lo que supone QUIC sobre IP: el transporte de `spawn`, las direcciones propias que declara externas, el `bootstrap_addr`, el filtro de direcciones y las direcciones que devuelve a Kademlia, todas multiaddrs de IP y UDP. `NoDial` sirve tal cual, porque solo deniega intentos de conexión y no cierra ninguna.

Las pruebas (`tests/link.rs`) son de dos clases:

- del vínculo y de los validadores, sin red, incluidas una firma libp2p que no verifica y una hecha con otra clave libp2p con la clave pública de la víctima en el sobre;
- dos de integración con nodos reales sobre loopback. Una monta dos routers, un proveedor honrado, uno con el contacto alterado y un cliente, y comprueba lo que guardan los routers, lo que encuentra el cliente, que la búsqueda cuenta 10 peticiones (2 a `get_providers` y 8 a `get_record`, sin los dos intentos denegados) y que no ha marcado a los proveedores. La otra comprueba que un nodo sin lista de arranque no encuentra a nadie.

`cargo +1.91 check --locked -p spike-kad-libp2p --all-targets` pasa: el candidato compila con la MSRV.

## Resultados

Ejecución del arnés del 2026-11-16, sobre el commit `331d85c`, con los parámetros comunes de `params.env`: `TTL_S` = 120 s, `REPUBLISH_S` = 40 s, `IDLE_S` = 300 s, 10 búsquedas por cliente y capacidad y `SEARCH_LIMIT` = 8. Duró 12 minutos, con el anfitrión de 32 CPU a una carga media de 16 a 22. Las cifras completas están en [`results/kad-libp2p.json`](results/kad-libp2p.json).

| Escenario | Estado | Cifras |
|-|-|-|
| S0 | `pass` | 4 búsquedas, ningún candidato. `c0` acaba con 0 pares en la tabla de rutas y 0 conexiones |
| S1 | `pass` | 160 búsquedas, todas con los dos proveedores verificados. Primer candidato válido a los 2 ms (p50) y 3 ms (p95), y búsqueda entera en 7 ms (p50 y p95). 22 peticiones por búsqueda. 31 617 bytes netos por búsqueda (p50), 32 130 (p95) |
| S2 | `pass` | Con `r1` parado, todas las búsquedas de los clientes están completas, con el primer candidato a los 2 ms (p50), aunque cada una dura unos 5 s esperando a `r1`. `c5` tiene su primer candidato válido 15,2 s después de arrancar su contenedor, y encuentra a todos los proveedores |
| S3 | `pass` | Los routers dejan de guardar lo de `p1` a los 114,2 s de pararlo, y ninguna búsqueda lo devuelve desde los 110,8 s. El límite es 160 s |
| S4 | `pass` | Cobertura inmediata 1,0, porque `r1` y `r3` lo guardan todo. Cada búsqueda dura unos 5 s por los dos routers caídos, con el primer candidato a los 2 a 5 ms |
| S5 | `pass` | Los cuatro routers rechazan el vínculo de `rogue` (4 rechazos), ninguno guarda su contacto y `records_stored` no crece sin explicación. Los clientes lo reciben las 4 veces como `bad_contact`, nunca como válido |
| S6 | `measured` | Ver la tabla siguiente |
| S7 | `pass` | Los cuatro clientes reciben el contacto nuevo de `p3`, con su IP en la red A, a los 0,7 s (p50) y 0,9 s (máximo) de la desconexión. El límite es 80 s |

En S2, los 15,2 s de `c5` suman el arranque del contenedor, su primer bootstrap, que espera 5 s la conexión imposible a `r1`, y su primera búsqueda, que espera lo mismo.

En reposo (S6), por papel, con la cifra de la línea base entre paréntesis. Todas son p50, salvo las conexiones, que van como p50 / máximo:

| Papel | RSS (KiB) | Conexiones | Tabla de rutas | Bytes por minuto | Enviados por minuto |
|-|-|-|-|-|-|
| Router | 23 200 (14 396) | 0 / 5 (0 / 0) | 3 (1) | 250 894 (444) | 126 775 (0) |
| Proveedor | 22 372 (14 612) | 0 / 0 (0 / 0) | 4 (1) | 181 426 (222) | 88 697 (0) |
| Cliente | 22 828 (14 824) | 0 / 4 (0 / 0) | 4 (2) | 18 661 (222) | 9 068 (0) |

Las conexiones están casi siempre cerradas: `libp2p-swarm` cierra una conexión a los 10 s sin uso. Por eso cada republicación, cada 40 s, abre conexiones QUIC nuevas con su apretón de manos TLS, y eso, con las consultas iterativas de cada `put` y de cada `start_providing`, es casi todo el tráfico de proveedores y routers. Los clientes solo hacen el arranque periódico de Kademlia, cada 5 minutos. Con los intervalos de producción, de horas, el tráfico en reposo bajaría en proporción. Pero aquí se mide con los de la prueba, los mismos para todos los candidatos.

## Lo que no se pudo medir

- **Escala.** Con 4 routers, menos que `K` = 20, todos están entre los más cercanos a cualquier clave y guardan todo. La prueba no ejercita el encaminamiento de Kademlia por distancia, la distribución de claves entre routers ni las rutas disjuntas.
- **Abuso.** No se prueban identidades falsas en masa (Sybil), inundaciones de provider records ni una clave popular saturada. El único registro inválido es el contacto de firma alterada de S5.
- **NAT y relays.** Las redes son privadas y sin NAT. No hay relay, AutoNAT ni DCUtR, así que no se mide cómo atravesaría libp2p una NAT, ni cuánto añadiría.
- **El contacto en uso.** Nadie marca el endpoint de Iroh que nombra el contacto: S7 comprueba que el cliente recibe la dirección nueva, no que conecte con ella.
- **Las dos pilas por separado.** Los bytes son los de la interfaz del contenedor, Iroh y libp2p juntos. Como Iroh no hace nada en la prueba, casi todo es de libp2p, pero no se separa.
- **El provider record frente al anuncio,** cuando el router no guarda el anuncio: entonces guarda el provider record sin saber qué capacidad nombra.
- **La clave libp2p del producto.** Aquí se deriva de la semilla del agente. Generarla y guardarla aparte, y rotarla, no se prueba.

## Metadatos que ve un router

- **Quién busca.** Cada búsqueda llega por una conexión QUIC autenticada, así que el router ve el `PeerId` de quien busca y su IP y puerto. `identify` le da además sus direcciones externas (en la prueba, todas sus direcciones locales menos el bucle), su clave pública, sus protocolos y la versión de `rust-libp2p` (`agent_version` por defecto). El `PeerId` es estable entre búsquedas y reinicios, así que un router puede juntar las búsquedas de un mismo nodo. Quien solo busca no publica vínculo, y su `PeerId` no lleva a su agente por la DHT. Pero la IP sí lo identifica en una red pequeña.
- **Qué busca.** La clave de `GET_PROVIDERS` es el SHA-256 de la cadena de la capacidad. Con un vocabulario común y público, el router la recupera con un diccionario. Después ve los `GET_VALUE` de `/agentnet/link/<P>` y `/agentnet/announce/<P>`: qué proveedores mira quien busca. Las consultas iterativas (`FIND_NODE`) muestran además las claves a los routers por los que pasan.
- **Quién ofrece.** Cada 40 s, cada proveedor le envía su vínculo, con su agente y las IP de su contacto, su anuncio, con sus capacidades, y un provider record por capacidad, con sus direcciones de libp2p. El router sabe quién ofrece qué, dónde está y si sigue vivo.

Ningún registro va cifrado ni restringido: lo que guarda un router lo puede pedir cualquiera que conozca la clave.
