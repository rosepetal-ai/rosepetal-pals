# Candidato `kad-iroh`: `libp2p-kad` sobre conexiones Iroh

Tarea 4 de la [fase 1 del hito 3](../../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). Es el segundo de los tres candidatos que la tarea 6 compara con la línea base. El código está en [`spikes/hito-3/kad-iroh/`](../../../spikes/hito-3/kad-iroh/) y cumple el contrato de nodo de [`spikes/hito-3/README.md`](../../../spikes/hito-3/README.md#el-contrato-de-nodo). Los resultados de su ejecución del arnés están en [`results/kad-iroh.json`](results/kad-iroh.json).

No es código del producto. El ALPN `agentnet-kad/0` es provisional.

## La duda que resuelve

`kad-libp2p` respondió que `libp2p-kad` convive con Iroh en el mismo nodo, cada uno con su pila, su puerto UDP y su clave, unidas por un vínculo firmado. La duda de este candidato es si `libp2p-kad` puede ir sobre las propias conexiones Iroh del daemon, con una sola identidad (el `PeerId` es el `AgentId`), un solo puerto UDP y ninguna segunda pila de red, y qué cuesta.

El resumen y las cifras están en [Resultados](#resultados), que salen de la ejecución del arnés sobre el commit del código.

## Paso 1: un transporte ya publicado

Antes de escribir el adaptador se buscó en crates.io, el 2026-11-16, un `Transport` de libp2p sobre Iroh compatible con `iroh =1.4.1` y `libp2p =0.57.1`. Se buscó con `cargo search` por `libp2p iroh`, `iroh-libp2p`, `libp2p-iroh`, `iroh transport libp2p`, `iroh libp2p transport`, `iroh-transport`, `libp2p-transport-iroh`, `iroh-p2p`, `iroh kademlia`, `iroh kad` e `iroh dht`, y las dependencias de cada resultado se miraron en su `Cargo.toml` o en la API de crates.io:

- **`libp2p-iroh` 0.1.3**, de 2026-03-16, el único que es lo que se buscaba («iroh quic as libp2p transport»). Depende de `iroh ^0.97`, sin features por defecto, y de `libp2p ^0.56`. Ninguna de las dos es compatible: `0.97` no admite `1.x`, y en versiones `0.x` `0.56` no admite `0.57`. Además arranca su endpoint con `presets::N0`, es decir, con los relays de n0 y la búsqueda de direcciones por DNS de n0, que las redes privadas de la prueba prohíben (§6.2). Usa su propio ALPN, `/iroh/libp2p-transport/0.1.0`, y un byte de saludo en cada stream. No se usa. Su diseño, un muxer como el de `libp2p-quic` sobre una conexión Iroh, es el mismo que el de aquí, pero no se copió nada de él.
- `natty` 0.0.2: QUIC propio hacia la clave pública de un par, sin `iroh` ni `libp2p` entre sus dependencias.
- `eidetica` 0.2.0: depende de `iroh ^0.91` y no de `libp2p`.
- `iroh-p2p` 0.2.0: es la parte libp2p del antiguo `iroh` de IPFS, con `libp2p ^0.50`, no un transporte sobre Iroh 1.x.
- `iroh-dht-experiment` 0.1.1: una DHT propia sobre `iroh ^0.92`, no `libp2p-kad`.

No hay ninguno utilizable, así que se escribió el adaptador.

## Cómo está hecho

Cada nodo tiene una sola pila de red: un endpoint de Iroh, arrancado como en el daemon con `relay = false` y como en la línea base. Tiene la clave del agente, está en UDP en el puerto de `--listen` (4433) y no tiene relays, mapeo de puertos ni búsqueda de direcciones. Un `Router` de Iroh, como el que monta el daemon en `node.rs`, sirve en él el ALPN `agentnet-kad/0`. libp2p no tiene transporte propio: su swarm, con `libp2p-kad` e `identify`, solo llega a otros nodos por conexiones de ese endpoint.

### El transporte

[`transport.rs`](../../../spikes/hito-3/kad-iroh/src/transport.rs) y [`muxer.rs`](../../../spikes/hito-3/kad-iroh/src/muxer.rs) son el adaptador:

- **`IrohTransport`** implementa `libp2p::core::Transport` con `Output = (PeerId, StreamMuxerBox)` sobre un `iroh::Endpoint`. No lleva negociación de seguridad ni de muxer de libp2p: la conexión ya llega autenticada por el TLS de Iroh y multiplexada por QUIC.
- **Direcciones.** Un par se nombra como en el QUIC de libp2p: `/ip4/<ip>/udp/<puerto>/quic-v1/p2p/<peer_id>`, o `/ip6/…`. Para marcar, el transporte saca del `PeerId` la clave Ed25519, que es el `EndpointId` del par, y conecta a un `EndpointAddr` con ese id y esa dirección directa. Una dirección sin `/p2p/` no nombra ningún endpoint y no se marca.
- **Marcado.** `Endpoint::connect` con el ALPN `agentnet-kad/0` y un plazo de 5 s, el `handshake_timeout` de `libp2p-quic`, para que los dos candidatos de Kademlia esperen lo mismo a un par que no contesta. El `PeerId` que recibe libp2p sale del `EndpointId` que autenticó Iroh (`Connection::remote_id`), no de la dirección. Una dirección con el `PeerId` de otro falla en el apretón de manos de Iroh (prueba `dial_with_a_peer_id_not_matching_the_endpoint_fails`).
- **Escucha.** El endpoint ya está ligado a su puerto. El transporte crea un `IrohListener`, el manejador de protocolo que el `Router` sirve en `agentnet-kad/0`, y este le pasa cada conexión ya autenticada. `listen_on` solo nombra el socket del endpoint para que libp2p lo conozca. Sin escucha, una conexión entrante se cierra.
- **`IrohMuxer`** implementa `StreamMuxer`: cada substream es un stream bidireccional QUIC de la conexión, `open_bi` al abrirlo y `accept_bi` al aceptarlo, como hace `libp2p-quic` con las suyas. Cerrar un substream termina su mitad de envío (`finish`). Cuando la otra parte o Iroh cierran la conexión, el `accept_bi` pendiente falla y libp2p la da por cerrada.
- **Una conexión por ALPN, varias rutas por par.** Iroh guarda por cada `EndpointId` las direcciones que conoce y las rutas que ha probado, y una conexión nueva suma su dirección a ese conjunto (`RemoteStateActor::handle_msg_resolve_remote` en `iroh` 1.4.1). Así, una marcación de libp2p hacia una dirección concreta puede acabar yendo por otra que Iroh ya conocía de ese par. Es una observación del código de Iroh, no algo que la prueba mida aparte.
- **Dos marcaciones por router.** libp2p marca a la vez todas las direcciones de un par (`dial_concurrency_factor`, 8 por defecto, sin cambiar), y los routers tienen dos, una en cada red. Cada una es una conexión Iroh aparte, y como Iroh puede llevar las dos por la ruta que funciona, a menudo las dos llegan al router. libp2p se queda con la primera y abandona la otra a mitad del apretón de manos. Los routers lo registran como `WARN … Accepting incoming connection ended with error: aborted by peer` cada pocos segundos mientras hay actividad (registros de los nodos, que git ignora). Es tráfico de apretones de manos que `kad-libp2p` no tiene: allí, la marcación a la dirección de la otra red no llega a ninguna parte.

**Identidad.** El `PeerId` es el de la clave pública Ed25519 del agente: `identity::PublicKey::from(ed25519::PublicKey::try_from_bytes(agent_id))`. Esa clave es también el `EndpointId` de Iroh. Son los mismos 32 bytes con dos escrituras distintas: el `PeerId` en base58 (`12D3KooW…`) y el `AgentId` en hexadecimal. No hace falta vínculo de identidades ni segunda clave (prueba `peer_id_is_the_agent_key`).

**Lo que se publica**, todo bajo el `PeerId` del nodo, que es su agente:

| Clave de Kademlia | Valor |
|-|-|
| `/agentnet/contact/<peer_id>` | El `Signed<ContactRecord>` de `pals-core`, sin cambios |
| `/agentnet/announce/<peer_id>` | El `Signed<AnnounceRecord>` de `pals-core`, sin cambios |
| Multihash SHA2-256 (código `0x12`) de la cadena completa de cada capacidad (§5.1) | Un provider record con el `PeerId` |

**Búsqueda y anuncio.** Son los de `kad-libp2p`, con el contacto donde allí iba el vínculo. La búsqueda pide los proveedores (`get_providers`) y, según llega cada `PeerId`, hasta `limit`, su contacto y su anuncio (`get_record`). Se queda con el contacto válido de mayor `seq` y con el anuncio válido del mismo agente, y `spike_common::verify` los vuelve a comprobar. `found_ms` es cuándo tuvo por primera vez un contacto y un anuncio válidos del mismo agente, y `requests` es la suma de `QueryStats::num_requests` de sus consultas menos los intentos de conexión que deniega `NoDial`. Con cuatro routers y dos proveedores son 4 peticiones de `get_providers` y 16 de `get_record`. El anuncio comprueba antes que la tienda tiene sitio, pone el contacto y el anuncio con `Quorum::All`, reintenta cada segundo durante 25 s el que no llega a ningún router y después anuncia cada capacidad. `kad-libp2p.md` lo explica con detalle.

### Solo red privada

- No hay nodos de arranque por defecto. El nodo arranca solo desde `--bootstrap`, que es el `bootstrap_addr` de un router: todas sus direcciones como `/ip4/<ip>/udp/4433/quic-v1/p2p/<peer_id>`, separadas por comas, como en `kad-libp2p` pero con el puerto de `--listen`.
- El protocolo de Kademlia es `/agentnet/kad/1.0.0`, nunca `/ipfs/kad/1.0.0`.
- libp2p no lleva transportes: solo las features `kad`, `tokio`, `macros`, `identify` y `ed25519`. Ni QUIC ni TCP de libp2p, ni mDNS, relay, AutoNAT, UPnP o DCUtR. Iroh no tiene relays ni búsqueda de direcciones (`presets::Minimal`, `RelayMode::Disabled`).
- Un nodo con la lista de arranque vacía no conoce a nadie (S0 y la prueba `a_node_with_no_bootstrap_finds_no_one`).

**Se usa `identify`,** con la misma configuración que en `kad-libp2p`, por dos cosas que `kad::Mode` no da. La primera: el modo propio de un nodo dice si acepta peticiones de Kademlia, pero no le dice si el par es servidor. `libp2p-kad` 0.49 lo sabe por la lista de protocolos que publica `identify` (`ConnectionEvent::RemoteProtocolsChange` en `handler.rs`). Sin ella, toma por servidor a cualquier par que le abre un stream de Kademlia, también a los clientes. La segunda: un router solo mete en su tabla de rutas, con dirección, a los pares a los que él marcó. En una conexión entrante, `libp2p-kad` no usa la dirección del otro (`ConnectedPoint::Listener` en `on_connection_handler_event` de `behaviour.rs`), y `r1`, que no arranca desde nadie, solo recibe conexiones entrantes. Sin `identify`, su tabla se quedaría vacía. Sobre Iroh, la dirección de origen de una conexión entrante sí es la de escucha del par, porque cada nodo usa un solo socket para todo. Eso podría sustituir las direcciones de `identify`, pero no su lista de protocolos. Todo esto sale de leer el código de `libp2p-kad`: no se probó una versión sin `identify`.

### Los ajustes de `kad-libp2p`

`kad-libp2p.md` explica cinco comportamientos de `libp2p-kad` y libp2p que obligan a ajustar algo. Aquí se mantienen todos, con el mismo código:

- **la republicación de valores depende del intervalo de replicación:** es configuración de `libp2p-kad` y no depende del transporte;
- **un provider record sin direcciones externas no sale en las respuestas:** cada nodo sigue declarando como externas sus direcciones locales, en forma de multiaddr de QUIC;
- **los proveedores encontrados entran en la consulta iterativa:** `NoDial` sigue denegando los intentos de conexión a los proveedores que encuentra una búsqueda. Sin él, uno en otra red retendría la consulta hasta el plazo de marcado del transporte, también de 5 s;
- **la caché de `identify` mete la dirección de bucle en las conexiones:** `identify` sigue anunciando solo las direcciones externas y sin caché;
- **`libp2p-kad` olvida direcciones tras un fallo pasajero:** el nodo sigue devolviéndole las de cada servidor tras cada intento fallido.

Los tres primeros no dependen del transporte. No se probó si los dos últimos siguen haciendo falta sobre Iroh, que reúne por su cuenta las direcciones de cada par.

Hubo que añadir un ajuste propio, en el anuncio. El de `kad-libp2p` contesta a los 25 s con un error si algún registro no ha llegado a ningún router, pero solo cuando no hay ningún `put` en curso. Un `put` que sigue en curso al llegar el plazo retiene la respuesta hasta que acaba. Sobre Iroh pasó en S7, en una ejecución de prueba del arnés hecha sobre un árbol sin commit, cuyo JSON se sobrescribió. Según su `harness.log` y el registro de `p3`, anotados al leerlos, `p3` contestó a los 32,5 s, cuando la API de control ya había dejado de esperar a los 30 s, y S7 quedó en `error`. Ahora el anuncio contesta en su plazo, con lo que esté en curso: si todos los registros llegaron, bien; si no, anuncia igualmente las capacidades y falla, y los `put` en curso siguen y pueden llegar después (`Task::end_announce`). La prueba `an_announce_answers_within_the_control_api_time` lo comprueba con un router que acepta la conexión y no contesta nunca: antes del ajuste, el anuncio contestaba a los 34 s (salida de `cargo test`).

### Validación en el router

Con `StoreInserts::FilterBoth`, cada `PutRecord` y cada `AddProvider` que recibe un router pasa por [`validate.rs`](../../../spikes/hito-3/kad-iroh/src/validate.rs) antes de llegar a la tienda. Lo que rechaza suma a `records_rejected` y se registra con su motivo. Como el `PeerId` es el agente, cada registro se comprueba solo contra su clave, sin depender de otro registro guardado:

- **Contacto y anuncio:** tienen que verificar como los de `pals-core`, con su firma, y estar en plazo. Además, quien los firma tiene que ser el agente que nombra el `PeerId` de su clave. Un contacto de `A` bajo `/agentnet/contact/<B>` se rechaza (`wrong_key`), y también uno cuya firma no verifica (`bad_contact`), que es lo que publica `rogue` en S5.
- **Provider record:** solo nombra un `PeerId`. `libp2p-kad` ya descarta el que no es del par que lo envía, y el TLS de Iroh ya autenticó que ese par tiene la clave del agente. El router comprueba la forma de la clave (`bad_provider_key`), que el `PeerId` es una clave Ed25519 (`not_an_agent`, que sobre este transporte no puede pasar) y, si guarda el anuncio de ese agente, que lista una capacidad con ese hash (`capability_not_announced`).
- **Antiguo:** un contacto o un anuncio con un `seq` menor que el que el router guarda bajo la misma clave no se guarda y no cuenta como rechazado.
- Cualquier otra clave de valor se rechaza (`unknown_key`).

Con `rogue` (S5), los routers rechazan su contacto, y guardan su anuncio y su provider record, que son válidos por sí mismos. Quien busca encuentra su `PeerId`, no encuentra contacto y lo descarta (`bad_contact`). Es lo mismo que en `kad-libp2p`. Lo que cambia es que desaparece la comprobación cruzada entre registros (`agent_mismatch`), que allí dependía de qué vínculo tuviera ya el router.

### `stored_by_agent` en los routers

| Registro guardado | Campo de `StoredRecords` | De qué agente |
|-|-|-|
| Contacto (`/agentnet/contact/…`) | `contact_records` | El que es el `PeerId` de su clave |
| Anuncio (`/agentnet/announce/…`) | `announce_records` | El que es el `PeerId` de su clave |
| Provider record | `provider_records` | El que es su `PeerId` |

`other_records` queda siempre a 0. La atribución es directa: el router solo guarda un contacto o un anuncio firmado por el agente de su clave, y el `PeerId` de un provider record es su agente. Todo lo que cuenta `records_stored` está atribuido. A diferencia de `kad-libp2p`, el provider record de `rogue` también queda atribuido a su agente, en `provider_records`.

### Caducidad, republicación y límites

Son los de `kad-libp2p`, copiados: `set_record_ttl` y `set_provider_record_ttl` a `TTL_S`, `set_publication_interval`, `set_provider_publication_interval` y `set_replication_interval` a `REPUBLISH_S`, y un barrido cada segundo de lo que la propia biblioteca marca como caducado. Los límites de `MemoryStoreConfig` (1024 valores de 8 KiB como mucho, 20 proveedores por clave y 1024 claves de proveedores) y los plazos (consulta de 20 s, búsqueda de 30 s, reintentos de anuncio durante 25 s) son los mismos. Dos cambian o se añaden:

| Límite | Valor | Por qué |
|-|-|-|
| Tamaño de un valor | 4 KiB | Contacto y anuncio, `MAX_RECORD_BYTES` de `pals-core`. Ya no hay vínculo de 6 KiB. El límite de la tienda sigue en 8 KiB |
| Marcado de una conexión | 5 s | El `handshake_timeout` de `libp2p-quic`, para que los dos candidatos esperen lo mismo |
| Conexiones entrantes en cola | 32 | Entre el `Router` de Iroh y el swarm |
| Conexión ociosa | 10 s | El de `libp2p-swarm`, sin cambiar. Al cerrarla, libp2p cierra la conexión Iroh |

## Reutilizar la clave del agente como identidad libp2p

**Qué se firma con ella.** En este candidato, libp2p no recibe nunca la clave secreta del agente, y se ha comprobado en el código:

- el swarm se crea con `Swarm::new`, que solo pide el `PeerId`, y no con `SwarmBuilder::with_existing_identity`, que pide el par de claves. El código del candidato no construye ningún `libp2p::identity::Keypair`;
- `identify` se configura con `identify::Config::new(versión, clave_pública)`. Con solo la clave pública no envía un registro de par firmado: en `libp2p-identify` 0.48, `Handler::build_info` deja `signed_peer_record` vacío para `KeyType::PublicKey`. Ese registro firmado solo aparece con `Config::new_with_signed_peer_record`, que pide el par de claves;
- `libp2p-kad` no firma sus registros: un `Record` lleva el `PeerId` de quien lo publica, pero ninguna firma;
- no hay negociación de seguridad de libp2p (Noise o TLS de libp2p), que firmaría con la clave de identidad, porque el transporte entrega la conexión ya autenticada.

Lo único que se firma con la clave del agente al conectar es el apretón de manos TLS 1.3 de Iroh: el `CertificateVerify` lo firma `IrohSecretKey`, el `rustls::sign::Signer` de Iroh. El daemon ya lo hace en cada conexión Iroh. Las otras firmas con esa clave son las de los registros de `pals-core` (contacto y anuncio), como en cualquier otro candidato.

**Qué autentica a los pares:** el TLS de Iroh, con claves públicas en bruto (RFC 7250). El transporte toma el `PeerId` del `EndpointId` autenticado. `libp2p-kad` comprueba que el proveedor de un `AddProvider` es el `PeerId` de la conexión, así que un provider record queda autenticado por el TLS de Iroh. `identify` comprueba que la clave pública que recibe es la del `PeerId` de la conexión, y lo es.

**Lo que supone:**

- No hace falta vínculo de identidades ni segunda clave. Desaparece un formato provisional (`agentnet.spike-kadlink/0`) con su verificación, y una clave que generar, guardar y rotar.
- La posición del agente en la DHT, su distancia XOR, la fija su clave. No se puede cambiar la identidad de la DHT sin cambiar la del agente, y cambiar la clave del agente lo mueve en la DHT, además de invalidar sus contactos y sus grants.
- Quien busca se identifica ante los routers con su clave de agente, porque cada búsqueda va por una conexión Iroh autenticada. En `kad-libp2p`, el `PeerId` de un cliente que no publica vínculo no lleva a su agente. Aquí sí lleva ([Metadatos que ve un router](#metadatos-que-ve-un-router)).
- Si la clave del agente se compromete, también se compromete su identidad en la DHT. En PALS ya pasa lo mismo con todo lo demás.
- Fabricar identidades para colocarse cerca de una clave cuesta generar una clave por intento, igual que con cualquier `PeerId`. No se prueba (Sybil, [Lo que no se pudo medir](#lo-que-no-se-pudo-medir)).

## El ALPN nuevo que el daemon tendría que servir

El daemon tendría que servir `agentnet-kad/0` en su `Router` de Iroh, junto a `agentnet-a2a-http/1`, el de `iroh-blobs` y `agentnet-discovery/0`. En la prueba es provisional, y congelarlo sería una decisión de la fase 2.

- **Dentro de cada conexión** va libp2p tal cual. Cada stream QUIC negocia su protocolo con multistream-select: `/agentnet/kad/1.0.0` para Kademlia, con los mensajes protobuf de `libp2p-kad` (16 KiB como mucho), e `/ipfs/id/1.0.0` para `identify`, cuyo nombre fija la biblioteca.
- **Una conexión más por par.** El ALPN se negocia por conexión, así que dos daemons que hablan A2A y Kademlia tienen dos conexiones QUIC. Comparten el socket UDP y el estado de rutas que Iroh guarda de cada par.
- **Todos los nodos lo sirven en la prueba,** también clientes y proveedores, que están en modo cliente de Kademlia y rechazan sus peticiones. Lo sirven porque `identify` corre en las dos direcciones de cada conexión.
- **Cuotas.** El daemon pone cuotas a `agentnet-discovery/0` (4 streams y 60 peticiones por minuto y par, 64 streams en total). Para `agentnet-kad/0` habría que decidir las suyas. La prueba solo tiene los límites de la biblioteca: 32 streams entrantes de Kademlia por conexión y los streams concurrentes que admite el QUIC de Iroh. No configura límites de conexiones del swarm ni mide abusos.

## API de `libp2p` e Iroh usadas

Con `libp2p =0.57.1` (`libp2p-kad 0.49.0`, `libp2p-identify 0.48.0`, `libp2p-swarm 0.48.0`, `libp2p-core 0.44.0`, `libp2p-identity 0.3.1`) e `iroh =1.4.1`:

- **`libp2p-kad` e `identify`:** los mismos que en `kad-libp2p` ([su lista](kad-libp2p.md#api-de-libp2p-usadas)), salvo `identify::Config::new` con la clave pública del agente.
- **`libp2p-core`:** `Transport` (`listen_on`, `remove_listener`, `dial` con `DialOpts`, `poll` con `TransportEvent::NewAddress`, `Incoming` y `ListenerClosed`, `boxed`), `StreamMuxer` (`poll_inbound`, `poll_outbound`, `poll_close`, `poll`) y `StreamMuxerBox`.
- **`libp2p-swarm`:** `Swarm::new` con `Config::with_tokio_executor()` (la misma configuración por defecto que pone `SwarmBuilder`), `#[derive(NetworkBehaviour)]` y lo que ya usaba `kad-libp2p`.
- **`libp2p-identity`:** `ed25519::PublicKey::try_from_bytes`, `PublicKey::from`, `PublicKey::to_peer_id`, `PublicKey::try_decode_protobuf` y `try_into_ed25519`, y el multihash del `PeerId` (`code`, `digest`) para sacar la clave.
- **Iroh:** `Endpoint::builder(presets::Minimal)` como en la línea base, `Endpoint::connect`, `bound_sockets` e `id`; `EndpointAddr::from_parts` con `TransportAddr::Ip`; `protocol::Router` y `ProtocolHandler`; `Connection` (`remote_id`, `open_bi`, `accept_bi`, `close`, `closed`, `paths`), `SendStream` (`poll_write`, `finish`) y `RecvStream` (`poll_read`).

El adaptador no necesitó nada que estas versiones no tengan. `noq`, el QUIC de Iroh, implementa `AsyncRead` y `AsyncWrite` de `futures` solo con una feature que `iroh` no activa, así que `IrohSubstream` llama a `poll_read` y `poll_write` de sus streams directamente.

## Código

El código común con `kad-libp2p` se copió, con una nota en la cabecera de cada módulo. No se movió a `spike-common`, para no tocar `kad-libp2p`, que ya está medido, ni meter libp2p en el binario de la línea base y de `mainline`:

- **`behaviour.rs`** es una copia de `kad-libp2p/src/behaviour.rs`. Cambia `spawn`, que liga el endpoint de Iroh, monta el `Router` y crea el swarm con `Swarm::new` sobre `IrohTransport`. También cambian el contacto en lugar del vínculo (anuncio, búsqueda y `Found::into_raw`) y la atribución de `stored_by_agent`, que ya no necesita `stored_agent` ni `bindings`. El plazo del anuncio contesta aunque haya un `put` en curso (`Task::on_tick` y `Task::end_announce`, [Los ajustes de `kad-libp2p`](#los-ajustes-de-kad-libp2p)). `quic_addr` pasa a `transport.rs`. La configuración de Kademlia, `NoDial`, la búsqueda, el barrido y los contadores son los mismos.
- **`validate.rs`** es una copia de `kad-libp2p/src/validate.rs`. El vínculo se sustituye por el contacto, cada registro se comprueba contra el agente de su clave (`contact_for`, `announce_for`, `peer_of`, `agent_of`) y `Verdict::Store` ya no lleva el agente.
- **`lib.rs`** toma `now_unix` de `kad-libp2p/src/lib.rs` y añade `bind_endpoint`, que es el arranque de Iroh de la línea base.
- `transport.rs`, `muxer.rs` y `main.rs` son nuevos. `link.rs` no se usa.

Las líneas de `src/`, sin pruebas, contadas con `wc -l` (todas) y sin las vacías ni los comentarios (`grep -v '^\s*//' | grep -cv '^\s*$'`):

| Fichero | Líneas | Sin comentarios ni vacías | Qué es |
|-|-|-|-|
| `transport.rs` | 354 | 261 | `IrohTransport`, `IrohListener`, el `PeerId` del agente y las direcciones |
| `muxer.rs` | 155 | 120 | `IrohMuxer` e `IrohSubstream` |
| **Adaptador** | **509** | **381** | |
| `behaviour.rs` | 1426 | 1132 | Copiado de `kad-libp2p`: el swarm, `NoDial`, la tarea que lo mueve, la validación de lo que llega, la búsqueda, el anuncio, la caducidad y los contadores |
| `validate.rs` | 234 | 162 | Copiado de `kad-libp2p` y adaptado: las claves de la DHT y lo que guarda un router |
| `main.rs` | 65 | 45 | El `Backend` |
| `lib.rs` | 52 | 27 | Los módulos, `bind_endpoint` y `now_unix` |
| **Total** | **2286** | **1747** | |

El adaptador está muy por debajo del límite de la prueba (unas 1000 líneas sin pruebas, paso 7 del plan). En `kad-libp2p`, `behaviour.rs`, `link.rs` y `validate.rs` sumaban 1854 líneas (1460 sin comentarios ni vacías). Aquí, `behaviour.rs` y `validate.rs` suman 1660 (1294). Sale el vínculo y entran el transporte y el muxer.

Las pruebas son de tres clases:

- del transporte (`tests/transport.rs`), con dos endpoints reales en loopback: un substream en cada sentido con bytes de ida y vuelta, que el `PeerId` que da el transporte es la clave del agente del otro lado, y que marcar la dirección de uno con el `PeerId` de otro falla en el apretón de manos de Iroh y no por plazo;
- de los validadores (`tests/kad.rs`), sin red: contacto y anuncio solo bajo su propio agente, el contacto de `rogue`, el `seq` y los provider records;
- tres de integración (`tests/kad.rs`) con nodos reales sobre Iroh en loopback. La primera, como en `kad-libp2p`: dos routers, un proveedor honrado, uno con el contacto alterado y un cliente. Comprueba lo que guardan los routers, lo que encuentra el cliente, que la búsqueda cuenta 10 peticiones y que solo tiene conexiones con los dos routers. La segunda comprueba que un nodo sin lista de arranque no encuentra a nadie. La tercera, que un anuncio que no llega a ningún router contesta antes de los 30 s de la API de control. Tarda unos 36 s: 10 de arranque, que espera a su router mudo, y 25 del plazo del anuncio.

`CARGO_TARGET_DIR=target/msrv cargo +1.91 check --locked -p spike-kad-iroh --all-targets` pasa: el candidato compila con la MSRV.

**Dependencias.** 337 crates, frente a 370 de `kad-libp2p` y 300 de la línea base. Es una observación, no una cifra del JSON: el recuento de `compat.sh` hecho a mano sobre `cargo tree --locked -e normal --prefix none`. Desaparecen la pila QUIC de libp2p y lo que trae: `libp2p-quic`, `libp2p-tls`, `quinn`, `quinn-proto`, `quinn-udp`, `rcgen`, `x509-parser`, `aws-lc-rs`, `aws-lc-sys`, `if-watch`, `rtnetlink` y otros. No entra ningún crate que no estuviera ya en `kad-libp2p` o en la línea base. La tarea 6 lo vuelve a medir, con el tiempo de compilación y el tamaño del binario.

## Resultados

Las cifras están en [`results/kad-iroh.json`](results/kad-iroh.json), que se añade con la ejecución del arnés sobre el commit del código.

## Lo que no se pudo medir

- **Escala.** Con 4 routers, menos que `K` = 20, todos están entre los más cercanos a cualquier clave y guardan todo. La prueba no ejercita el encaminamiento por distancia, la distribución de claves ni las rutas disjuntas.
- **Abuso.** No se prueban identidades falsas en masa (Sybil), inundaciones de provider records, una clave popular saturada ni un par que abre muchas conexiones o streams en `agentnet-kad/0`. El único registro inválido es el contacto de firma alterada de S5.
- **NAT y relays.** Las redes son privadas y sin NAT, y los relays de Iroh están apagados. Iroh sabría atravesar una NAT con sus relays y su perforación de agujeros, y este candidato los heredaría sin código de libp2p. La prueba no lo mide.
- **El contacto en uso.** Nadie conecta al endpoint que nombra el contacto con otro ALPN: S7 comprueba que el cliente recibe la dirección nueva, no que conecte con ella. Aquí, al menos, el endpoint del contacto es el mismo al que conectan los demás nodos para Kademlia.
- **Sin `identify` y sin los dos últimos ajustes.** No se probó si hacen falta sobre Iroh. Lo que dice este documento sobre `identify` sale de leer el código de `libp2p-kad`.
- **La convivencia con el daemon.** La prueba sirve solo `agentnet-kad/0`. No mide el endpoint compartido con A2A, blobs y descubrimiento, ni las cuotas que harían falta.

## Metadatos que ve un router

- **Quién busca.** Cada búsqueda llega por una conexión Iroh autenticada. El router ve la clave del agente que busca, que es a la vez su `PeerId` y su `AgentId`, y su IP y puerto. `identify` le da además sus direcciones externas, sus protocolos y la versión de `rust-libp2p`. En `kad-libp2p`, el `PeerId` de un cliente no llevaba a su agente. Aquí el router sabe qué agente busca, y puede juntar sus búsquedas con el resto de lo que hace ese agente, por ejemplo sus contactos y sus grants.
- **Qué busca.** La clave de `GET_PROVIDERS` es el SHA-256 de la cadena de la capacidad, que con un vocabulario público se recupera con un diccionario. Después ve los `GET_VALUE` de `/agentnet/contact/<P>` y `/agentnet/announce/<P>`: qué proveedores mira quien busca.
- **Quién ofrece.** Cada 40 s, cada proveedor le envía su contacto, con su agente y sus IP, su anuncio, con sus capacidades, y un provider record por capacidad. El router sabe quién ofrece qué, dónde está y si sigue vivo.

Ningún registro va cifrado ni restringido: lo que guarda un router lo puede pedir cualquiera que conozca la clave.
