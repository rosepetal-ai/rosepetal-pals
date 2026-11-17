# Protocolo `agentnet-kad/0`

Especificación de la DHT con la que un agente PALS encuentra a los proveedores de una capacidad sin contacto previo ni directorio central (diseño §5 y §6): una red Kademlia privada, con `libp2p-kad`, cuyas conexiones son las del endpoint de Iroh que el daemon ya usa para A2A, los blobs y `agentnet-discovery/0`. Es un protocolo propio de PALS, no una parte de A2A: con lo que encuentra, el agente sigue el camino del hito 2 (card verificada, consulta de servicio y selección), y la tarea elegida se envía con los bindings estándar de A2A v1.0.

Los valores de este documento son los del código, `crates/pals/src/dht/`, y el vector `crates/pals-core/tests/vectors/kad-v0.json` congela la identidad, las claves y las formas de dirección. El backend es `kad-iroh`, ratificado en el [registro del backend](../decisions/2026-11-16-hito-3-backend-dht.md#decisión) (H3-D1).

Describe el protocolo tal como lo deja el hito 3: la identidad, las claves, las formas de dirección, la conexión con sus límites, lo que guarda un router, el swarm del daemon, con sus modos y su entrada en la red, lo que un nodo publica, cuándo caduca y cómo se busca en la DHT, y cómo usa la DHT la búsqueda de servicios (sección 9). Lo que el hito deja fuera, como una forma de dirección con relay o las políticas de admisión, está en «Fuera de alcance» del [plan](../plans/2026-11-16-hito-3-descubrimiento-kademlia.md#fuera-de-alcance).

## 1. Identificación

| Elemento | Valor |
|-|-|
| ALPN de Iroh | `agentnet-kad/0` |
| Protocolo de libp2p | `/agentnet/kad/1.0.0` |
| Implementación | `libp2p-kad` 0.49.0, por `libp2p` 0.57.1, sin ningún transporte propio de libp2p |
| Vector | `crates/pals-core/tests/vectors/kad-v0.json` |

Son dos nombres porque eligen cosas distintas: el ALPN elige la conexión de Iroh, y dentro de ella libp2p negocia `/agentnet/kad/1.0.0` en cada stream. La red es privada: nunca habla `/ipfs/kad/1.0.0` ni entra en una DHT pública.

El ALPN, el protocolo, la identidad, las claves y las formas de dirección no cambian de significado. Un cambio incompatible será `agentnet-kad/1`, con ALPN propio.

## 2. Identidad

**Una sola identidad.** El `PeerId` de un agente es su clave pública Ed25519: la de su `AgentId` y la de su endpoint de Iroh. No hay una segunda clave para libp2p ni un registro que vincule dos identidades, como preveía el §6.2 del diseño. El TLS de Iroh autentica esa clave en cada conexión, y libp2p no firma nada ni recibe la clave secreta del agente.

**Cómo sale.** La clave pública va en el mensaje protobuf `PublicKey` de libp2p, con el tipo Ed25519 (`1`) y los 32 bytes de la clave: `08 01 12 20 <clave>`. Esos 36 bytes van bajo el multihash identidad (código `0x00`, longitud `0x24`): `00 24 08 01 12 20 <clave>`, 38 bytes. En texto, el `PeerId` es el base58 de esos bytes (el alfabeto de Bitcoin), que empieza por `12D3KooW`.

**Cómo se lee.** Un `PeerId` es de un agente solo si es exactamente esa codificación de una clave Ed25519 válida. No es de ningún agente uno de otro tipo de clave, uno con un hash de la clave en lugar del multihash identidad, ni uno con la misma clave codificada de otra forma, por ejemplo con los dos campos del protobuf en otro orden, que libp2p sí decodifica. Así cada agente tiene un solo `PeerId`.

Con la clave del vector, 32 bytes `0x01` como clave secreta, la misma de los vectores del hito 2:

| | Valor |
|-|-|
| `AgentId` | `8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c` |
| Bytes del `PeerId` | `0024080112208a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c` |
| `PeerId` | `12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5` |

## 3. Claves

### Valores

Un agente publica dos valores, los registros firmados del hito 2 tal cual, sin esquema nuevo:

| Valor | Clave | Contenido |
|-|-|-|
| Contacto | `/agentnet/contact/<peer_id>` | su `agentnet.contact/1` firmado (`pals_core::records`) |
| Anuncio | `/agentnet/announce/<peer_id>` | su `agentnet.announce/1` firmado |

La clave son los bytes UTF-8 de esa cadena, con el `PeerId` en base58. Una clave nombra un valor solo si es exactamente la de un agente: el prefijo, el base58 de su `PeerId` en la forma de la sección 2 y nada más, sin `/` al final ni espacios. Cualquier otra no es un contacto ni un anuncio.

Con la clave del vector:

| Valor | Clave |
|-|-|
| Contacto | `/agentnet/contact/12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5` |
| Anuncio | `/agentnet/announce/12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5` |

### Provider records

Un agente se anuncia como proveedor de cada capacidad pública bajo una clave que sale de la cadena completa de la capacidad (§5.1). La clave es el multihash SHA2-256 de los bytes UTF-8 de esa cadena: `12 20` y los 32 bytes del SHA-256.

- Para una capacidad común, la cadena completa es su etiqueta, `agentnet:capability:v1:<id>`. Para una con espacio de nombres, `<espacio>:<id>`, es la capacidad tal cual. Es lo que escribe `pals_core::inventory::capability_tag`.
- La etiqueta completa no es una capacidad: `agentnet:capability:v1:vision.ocr` escrito como capacidad se rechaza. Así cada capacidad tiene una sola clave.
- A la DHT solo llega esa clave. Ni el nombre del servicio ni los ids del inventario (H3-D4).

El vocabulario común, la tabla de sus claves y cómo se amplía están en [`capabilities-v1.md`](../extensions/capabilities-v1.md). Por ejemplo, `vision.ocr` tiene la clave `122092485626d851a455cd10cd7ae3800eb76ff67220722638435d3ae050e92c8463`.

## 4. Direcciones

Hay dos formas de dirección, y nada va después de `/p2p/<peer_id>`:

| Forma | Multiaddr | Qué dice |
|-|-|-|
| Router | `/ip4/<ip>/udp/<puerto>/quic-v1/p2p/<peer_id>`, o con `/ip6/<ip>` | que hay un endpoint de Iroh del agente en esa dirección UDP |
| Cliente | `/p2p/<peer_id>` | ninguna ubicación |

**Router.** Un router declara direcciones de la primera forma, en las que se le puede marcar directamente. Aquí `quic-v1` no es el QUIC de libp2p: con esa dirección se marca un endpoint de Iroh, con el ALPN `agentnet-kad/0`, y la conexión prueba que es el del `PeerId` de la dirección. No hay forma con relay: un router tiene que ser alcanzable directamente (H3-D3).

**Cliente.** Un cliente declara solo `/p2p/<peer_id>`, así que sus provider records no llevan ninguna dirección de su red. A un proveedor no se le marca por Kademlia: se le llega por su contacto, por los ALPN de los hitos 1 y 2. Las direcciones no pueden quedar vacías: con `libp2p-kad` 0.49.0, un router no devuelve en `GET_PROVIDERS` un provider record sin direcciones de un par que no está en su tabla de rutas, y un cliente nunca está en ella. Con `/p2p/<peer_id>`, el router guarda el provider record y lo devuelve tal cual.

En la red, una dirección viaja con la codificación binaria de multiaddr. Con la clave del vector:

| Forma | Texto | Bytes |
|-|-|-|
| Router | `/ip4/192.0.2.10/udp/4433/quic-v1/p2p/12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5` | `04c000020a` (`ip4`), `91021151` (`udp`, 4433), `cd03` (`quic-v1`), `a50326` (`p2p`, 38 bytes) y los bytes del `PeerId` |
| Cliente | `/p2p/12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5` | `a50326` y los bytes del `PeerId` |

## 5. Conexión

El código es `crates/pals/src/dht/transport.rs` y `crates/pals/src/dht/muxer.rs`, y sus pruebas, `crates/pals/tests/dht_transport.rs`, son la puerta de cualquier cambio de versión de `libp2p` o de `iroh` (H3-D1, condición 3).

### Qué va en una conexión

Una conexión de `agentnet-kad/0` es una conexión de Iroh con ese ALPN, en el mismo endpoint y el mismo puerto UDP que los demás ALPN del daemon. libp2p no tiene transporte propio: no abre sockets, no habla TCP ni su propio QUIC, y al abrir la conexión no negocia ni seguridad ni multiplexado. El TLS de Iroh ya autentica la clave del otro lado (claves públicas en crudo, RFC 7250), y QUIC ya multiplexa.

- **Quién es el otro.** El `PeerId` que recibe libp2p sale siempre de la clave que Iroh autenticó, nunca de la dirección marcada. Si en esa dirección hay otro endpoint, la conexión falla en el handshake de Iroh.
- **Los streams.** Cada stream bidireccional de QUIC es un substream de libp2p. Quien lo abre escribe primero: multistream-select 1.0 de libp2p (`/multistream/1.0.0`) negocia en él el protocolo, `/agentnet/kad/1.0.0` en esta red, y después van los mensajes de Kademlia. El otro lado solo se entera de un stream cuando quien lo abre envía algo en él.
- **El cierre.** Un substream se cierra terminando su mitad de envío (`FIN`). Cuando libp2p cierra la conexión, por ejemplo por ociosa, Iroh la cierra con el código `0`.

| Código | Qué cierra |
|-|-|
| `0` | una conexión que libp2p cerró |
| `1` (`REFUSED`) | una conexión o un stream que el nodo rechaza: los de las secciones siguientes |

### Marcar

- Solo se marca la forma router de la sección 4, con el `PeerId` de un agente y nada después. No se marcan la forma cliente, `/p2p/<peer_id>`, ni ninguna otra: TCP, DNS o `quic` sin `-v1`.
- **Un marcado lleva todas las direcciones conocidas del par.** El nodo marca a la vez la dirección pedida y las demás directas que conoce de ese par, en una sola conexión de Iroh, y Iroh elige el camino. Un par con dos direcciones es una conexión, no dos. El nodo guarda las 8 últimas direcciones de cada par, de 1024 pares como mucho.
- Un marcado tiene 5 s, el plazo del handshake de `libp2p-quic`. libp2p prueba las direcciones de un par una tras otra, un marcado por dirección, y cada uno lleva además las que el nodo recuerda de ese par. Cuando uno falla, los de las demás direcciones del mismo intento que ese llevaba fallan en el acto, sin esperar su plazo. Uno de una dirección que el fallido no llevaba sí lo intenta. Un par caído en direcciones que el nodo recuerda cuesta 5 s, no 5 s por dirección. Importa porque una consulta que espera a un router caído deja ociosas mientras tanto sus conexiones con los demás routers, y una conexión ociosa se cierra a los 10 s (sección 7). Con dos direcciones y 5 s por cada una, la espera llegaba justo a esos 10 s, y el `put` que sigue a la búsqueda de los routers más cercanos encontraba cerradas las conexiones que iba a usar.
- Con `relay = true`, Iroh puede además buscar el endpoint en el DNS de n0, como desde el hito 1. Sin relay, solo cuentan las direcciones directas.

### Conexiones entrantes

El endpoint entrega cada conexión de `agentnet-kad/0`, una vez autenticada, a una cola de 32 conexiones para libp2p. **La cola nunca espera:** si está llena, la conexión se cierra al momento con el código `1` y el motivo `busy`. Se cierra igual, con el motivo `not listening`, una conexión que llega cuando libp2p no escucha.

Ya en libp2p, el swarm admite como mucho 2 conexiones establecidas con cada par, entrantes y salientes juntas, y 256 conexiones entrantes en total (`connection_limits` de libp2p). Una conexión de más se cierra al establecerla, con el código `0`, sin que Kademlia la vea.

### Streams entrantes

Antes de entregar a libp2p un stream que abrió el otro lado, el nodo comprueba estas cuotas:

| Cuota | Valor |
|-|-|
| Streams de un par abiertos a la vez | 8, sumando todas sus conexiones |
| Streams que abre un par en un minuto | 600, en el minuto del reloj del nodo, sumando todas sus conexiones |
| Streams de todos los pares abiertos a la vez | 256 |

Un stream que pasa de una cuota se resetea al aceptarlo, sin leerlo: el nodo cierra sus dos mitades con el código `1` (`RESET_STREAM` y `STOP_SENDING`), y libp2p no llega a verlo. Un stream cuenta mientras libp2p lo tiene abierto, y uno rechazado no cuenta en el minuto.

**Por qué 8 y 600.** Las cuotas defienden al nodo y nunca rechazan el tráfico propio de un nodo que se porta bien:

- Una vuelta de publicación de 32 capacidades (sección 8) abre unos 4 + 4 × 32 = 132 streams con cada router, en una red de 20 routers o menos. Son el contacto y el anuncio, con la búsqueda de sus routers, y, por cada capacidad, la búsqueda de sus routers, el anuncio, la búsqueda de los routers del provider record y el provider record.
- Una búsqueda de 8 proveedores abre 1 + 2 × 8 = 17 streams con cada router: la de los proveedores, y la del contacto y la del anuncio de cada uno.

600 por minuto deja sitio para una vuelta completa y unas 25 búsquedas. Lo que un nodo abre a la vez lo limita él mismo, por debajo de 8 (más abajo, «Streams propios»).

**La replicación entre routers.** Un router replica lo que guarda cada `[dht] republish_s`, con dos streams por valor: la búsqueda de sus routers y el `put`. Por eso, si un router replica a otro más de unos 300 valores en un minuto, el otro rechaza una parte. La vuelta siguiente de los que publicaron esos valores la recupera.

**Streams ociosos.** Un stream que abrió el otro lado y pasa 10 s (el plazo de conexión ociosa de la sección 7) sin que se lea ni se escriba nada en él se resetea igual, con el código `1`, y libp2p lo suelta: deja su sitio en las cuotas. `libp2p-kad` 0.49 espera la petición siguiente en un stream entrante sin plazo propio, así que sin este un par podría quedarse con sus 8 streams para siempre, y 32 identidades con los 256 del nodo.

**Crédito de streams.** En cada conexión de `agentnet-kad/0`, el nodo da al otro lado crédito de QUIC para 12 streams bidireccionales a la vez: los 8 de su cuota y 4 más, para que uno de más llegue y se rechace en lugar de quedarse esperando. Es lo que hacen `agentnet-discovery/0` y el túnel de A2A. QUIC no retira el crédito que dio el handshake (100 streams): el nuevo límite vale según se cierran los streams.

El nodo cuenta desde el arranque las conexiones y los streams que rechaza, y aparte los streams que resetea por ociosos.

### Streams propios

Un nodo tiene abiertos como mucho 4 streams propios de `agentnet-kad/0` con un mismo par, sumando todas sus conexiones con él (`OUTBOUND_STREAMS_PER_PEER`). Son la mitad de los 8 a la vez que acepta un par con las cuotas por defecto. El margen cubre lo que tarda el otro lado en ver que un stream se cerró, y una segunda conexión.

Pasan por este límite las vueltas de publicación, las búsquedas y la replicación de un router. Un stream de más espera a que se cierre uno de los 4: el límite nunca hace fallar un stream. La espera cuenta, eso sí, en el plazo de 10 s que `libp2p-kad` da a cada petición. Así, un nodo que se porta bien nunca recibe un rechazo por su propio tráfico.

### El puerto: `[net] listen`

Un router necesita un puerto fijo para que los demás lo marquen. `[net] listen = "<ip>:<puerto>"` liga el endpoint de Iroh a esa dirección y solo a ella, con relay o sin él. Es el puerto de todo el endpoint: también el de A2A, los blobs y `agentnet-discovery/0`.

Sin `listen`, todo queda como en el hito 2: con relay, los puertos libres del preset N0 en todas las interfaces; sin relay, un puerto libre de 127.0.0.1. Un valor que no es una dirección de socket, con IP y puerto, se rechaza al leer la configuración.

## 6. Lo que guarda un router

El código es `crates/pals/src/dht/validate.rs`, y sus pruebas, `crates/pals/tests/dht_validate.rs`. Un router comprueba cada `PutRecord` y cada `AddProvider` que recibe antes de que llegue a su tienda (`StoreInserts::FilterBoth` de `libp2p-kad`). Quien busca lee los contactos y los anuncios que recibe con las comprobaciones de los valores, el tamaño de la regla 1 y la regla 2, al `Clock` del nodo, y de varias copias de un registro se queda con la válida de mayor `seq` (sección 8). La búsqueda de servicios compara después el contacto y el anuncio de cada proveedor que encontró con los más nuevos que conoce de ese agente, como los de una respuesta de `agentnet-discovery/0` (`Store::peer_record_accept`): uno que vuelve atrás descarta al proveedor, con el motivo `record regression`. Cada registro tiene en un router uno de tres resultados:

| Resultado | Qué hace el router |
|-|-|
| Guardar | lo guarda |
| Viejo | se queda con el que tiene, y no lo cuenta como rechazado: nada en él es inválido |
| Rechazar | no lo guarda, y lo cuenta y lo registra con su motivo |

Con `libp2p-kad` 0.49, quien publica no se entera de un rechazo: el router le contesta como si lo hubiera guardado. Cambiarlo pide un fork de `libp2p-kad`, y queda fuera del hito 3. Por eso el router cuenta cada rechazo con su motivo.

### Valores

Las comprobaciones van en este orden, y gana el primer fallo:

1. La clave es la de un contacto o un anuncio, en la forma exacta de la sección 3 (`unknown_key`). El valor tiene como mucho 4 KiB (`MAX_RECORD_BYTES`), y se mide antes de leerlo (`too_large`).
2. El valor es un registro firmado de su tipo, `agentnet.contact/1` o `agentnet.announce/1`, tal como lo lee `pals_core::signed` (`bad_contact`, `bad_announce`): JSON canónico, sin campos desconocidos, con una firma válida y con las reglas del registro, entre ellas 32 capacidades como mucho en un anuncio. Lo firma el agente cuyo `PeerId` nombra la clave (`wrong_key`). Está dentro de su ventana de validez, con 60 s de tolerancia en cada extremo (`CLOCK_SKEW_S`), como en el resto de PALS: un registro emitido 59 s en el futuro se guarda, y uno de 61 s se rechaza (`not_yet_valid`); uno que caducó hace 59 s se guarda, y uno de hace 61 s se rechaza (`expired`).
3. Si el router guarda bajo esa clave un registro de ese agente que sigue siendo válido, con un `seq` mayor, o con el mismo `seq` y otros bytes, el nuevo es viejo. Los mismos bytes otra vez se guardan: una republicación nunca se rechaza.

Por la regla 3, un proveedor que vuelve a arrancar con otro directorio de datos, y con él vuelve al `seq` 1, no sustituye en los routers su registro de `seq` mayor hasta que ese caduca.

### Provider records

Un provider record solo dice un `PeerId` bajo una clave, y la clave es un hash. El router lo comprueba contra el anuncio que guarda de ese agente, en este orden:

1. La clave es un multihash SHA2-256 de 34 bytes: `12 20` y los 32 bytes del hash (`bad_provider_key`).
2. El `PeerId` es el de un agente, en la forma de la sección 2 (`not_an_agent`). `libp2p-kad` ya descarta uno que no es el `PeerId` del par que lo envía, y el TLS de Iroh autenticó la clave de ese par.
3. El router guarda un anuncio válido de ese agente, bajo la clave de su anuncio (`provider_without_announce`).
4. Ese anuncio lista una capacidad cuya clave de la sección 3 es la del provider record (`capability_not_announced`). El anuncio lista las capacidades como las escribe el inventario, `vision.ocr` o `example:metal-plates`, y su clave sale de la cadena completa, la de `capability_tag`.

Así, un router solo admite de un agente provider records de las capacidades de su anuncio firmado. Por eso quien publica envía su anuncio antes que sus provider records. La comprobación se hace al recibir cada provider record, y cuando el router guarda un anuncio nuevo de un agente, borra los provider records de ese agente de las capacidades que el anuncio nuevo ya no lista. Así un agente tiene en un router, en todo momento, como mucho 32 provider records (`MAX_ANNOUNCED_CAPABILITIES`).

Con el anuncio del vector `announce-v1`, que lista `vision.ocr` y `example:metal-plates`, un router guarda los provider records de su agente bajo `122092485626d851a455cd10cd7ae3800eb76ff67220722638435d3ae050e92c8463` y `12202a561a2441f0cc4eba6d49bc3d437b467d69cb6c640d213337a81b4826b173b6`. Rechaza con `capability_not_announced` el de `code.rust.review` y el del SHA-256 de `vision.ocr` sin el prefijo `agentnet:capability:v1:`.

### Motivos de rechazo

Los nombres no cambian: son los que llevan los registros del daemon y `pals dht status`.

| Motivo | Qué se rechaza |
|-|-|
| `unknown_key` | un valor bajo una clave que no es la de un contacto ni la de un anuncio |
| `too_large` | un valor de más de 4 KiB |
| `wrong_key` | un registro firmado por un agente distinto del de su clave |
| `bad_contact` | bajo la clave de un contacto, algo que no es un contacto firmado válido |
| `bad_announce` | bajo la clave de un anuncio, algo que no es un anuncio firmado válido |
| `expired` | un registro pasada su ventana de validez |
| `not_yet_valid` | un registro antes de su ventana de validez |
| `bad_provider_key` | un provider record cuya clave no es un multihash SHA2-256 |
| `not_an_agent` | un provider record cuyo `PeerId` no es el de un agente |
| `provider_without_announce` | un provider record de un agente cuyo anuncio el router no guarda |
| `capability_not_announced` | un provider record de una capacidad que el anuncio de su agente no lista |
| `store_full` | un registro válido que no cabe en la tienda ni después de borrar de ella lo caducado (sección 8). No lo da ninguna comprobación: lo cuenta el swarm |

## 7. El swarm del daemon

El código es `crates/pals/src/dht/swarm.rs` y `crates/pals/src/dht/mod.rs`, y sus pruebas, `crates/pals/tests/dht.rs`: nodos reales sobre Iroh en loopback.

### Activación y modos

La DHT está apagada por defecto, y el binario no lleva puntos de arranque. Sin `[dht] enabled = true`, el nodo no sirve `agentnet-kad/0` ni abre ninguna conexión con ese ALPN. Encendida, el nodo sirve el ALPN en su endpoint de siempre y mueve un swarm de libp2p sobre él, en una tarea propia.

| Modo | Configuración | Qué hace |
|-|-|-|
| Cliente | por defecto, `mode = "client"` | Solo marca hacia los routers. No acepta peticiones de Kademlia y no entra en ninguna tabla de rutas |
| Router | `mode = "router"` | Guarda y contesta para los demás (sección 6). Lo marcan directamente: necesita un puerto fijo, `[net] listen` (sección 5) |

El modo de `libp2p-kad` se fija siempre: servidor en un router y cliente en un cliente (H3-D3). Nunca es automático: en el modo automático, `libp2p-kad` pasa a servidor a un nodo en cuanto declara una dirección externa, y todo nodo declara una (más abajo).

Con `relay = true`, un marcado a un router puede buscar además su endpoint en el DNS de n0, como cualquier marcado del daemon desde el hito 1. No es una DHT pública. Pero Kademlia solo conoce a un router por una dirección IP directa, la de su ticket o la que declara él mismo, así que un router tiene que tenerlas.

### Kademlia

| Parámetro | Valor |
|-|-|
| Protocolo | `/agentnet/kad/1.0.0` (sección 1) |
| Vida de un valor o un provider record en un router | `[dht] record_ttl_s`: 3600 s por defecto, de 60 a 86 400 |
| Replicación de lo que guarda un router | cada `[dht] republish_s`: 1200 s por defecto, al menos 20 y como mucho la mitad de `record_ttl_s` |
| Publicación propia por los temporizadores de `libp2p-kad` | ninguna: la hacen las vueltas de publicación del nodo (sección 8) |
| Lo que llega | todo `PutRecord` y `AddProvider` pasa por las comprobaciones de la sección 6 (`StoreInserts::FilterBoth`) |
| Caché de lo encontrado | ninguna |
| Una consulta | 20 s como mucho |
| Conexión ociosa | se cierra a los 10 s sin streams. Nada mantiene una conexión abierta |
| Arranque periódico | cada 5 min, el de `libp2p-kad` |
| Marcados a la vez a un par | 1: un router con dos direcciones es una conexión, que el transporte marca en todas (sección 5) |
| Tienda de un router | 1024 valores de 8 KiB como mucho, 20 proveedores por clave y 1024 claves de proveedores |
| Conexiones | las de la sección 5: 2 establecidas por par y 256 entrantes |

Un cliente no gasta tráfico en reposo: sus conexiones se cierran a los 10 s, y cada búsqueda después de un reposo paga un apretón de manos de Iroh con los routers.

Lo que guarda un router caduca con el mecanismo de `libp2p-kad`, sin barridos propios (sección 8). El estado del nodo cuenta solo lo que no ha caducado.

### Entrada en la red

1. Cada ticket de `[dht] bootstrap` da sus direcciones IP directas, que el nodo da a Kademlia en la forma router de la sección 4, y el nodo arranca (`bootstrap` de Kademlia).
2. Si al terminar ese arranque no contestó ningún router, el nodo marca sus contactos guardados que tienen ticket, los 8 primeros por nombre. La mayoría son agentes que no son routers, así que ninguno entra en la tabla de rutas ni en las direcciones que el nodo recuerda hasta que muestra que sirve `/agentnet/kad/1.0.0`, como un router (más abajo, `identify`). El nodo arranca a través del primero que lo muestra.
3. Un nodo sin `bootstrap` arranca solo: un router espera a que lo marquen, y un cliente no conoce a nadie.

`libp2p-kad` deja en la tabla de rutas a un par que no pudo marcar, con su última dirección, así que la tabla no se queda vacía aunque no conteste nadie. Por eso el paso 2 mira si contestó algún router, no si la tabla está vacía.

Tras un marcado fallido a un router, `libp2p-kad` le quita todas las direcciones menos la última, aunque el fallo fuera pasajero. El nodo le devuelve todas las que recuerda de ese router, así que uno que se paró un momento vuelve a marcarse en todas. El nodo solo recuerda las direcciones de los tickets de `bootstrap` y de los pares de la tabla de rutas: a un par que nunca fue router no le devuelve nada.

Un ticket de `bootstrap` tiene que leerse, nombrar otro endpoint que el propio y llevar al menos una dirección IP directa. Si no, el nodo no arranca.

### Lo que declara un nodo

`libp2p-kad` 0.49 no devuelve un provider record sin direcciones de un par que no está en la tabla de rutas (sección 4), así que todo nodo declara alguna:

- **Un router** declara sus `[dht] external_addrs` o, si no tiene, las direcciones directas de su endpoint, en la forma router. Nunca declara una IP sin especificar: con `[net] listen = "0.0.0.0:<puerto>"`, esa dirección no es de nadie, y un router así necesita `external_addrs`. Su ticket (`pals id --ticket`), que es la invitación a la red, y su contacto llevan también las `external_addrs`.
- **Un cliente** declara solo `/p2p/<peer_id>`, que no es ninguna ubicación.

`external_addrs` solo vale en un router, y ninguna puede tener una IP sin especificar ni el puerto 0.

Los provider records propios llevan esas direcciones y ninguna más. `libp2p-kad` guarda el suyo sin direcciones y, al contestar por él, pone las direcciones en las que el nodo escucha: con `[net] listen = "0.0.0.0:<puerto>"`, `0.0.0.0`. Por eso el nodo guarda su provider record con las direcciones que declara, y lo actualiza cuando cambian. Un router que también anuncia contesta por él con sus `external_addrs` o con sus direcciones directas, nunca con una IP sin especificar.

### Cómo conoce un router a los que lo marcan: `identify`

`libp2p-kad` 0.49 mete a un par en la tabla de rutas solo con la dirección a la que lo marcó el propio nodo. De un par que lo marcó a él, no sabe ninguna dirección, y lo da por inalcanzable. Por eso el swarm lleva también `identify` de libp2p (`/ipfs/id/1.0.0`), y es la única razón:

- **La prueba lo pidió.** Sin `identify`, un router sin `bootstrap` nunca tuvo a otro en su tabla. En `a_router_with_no_bootstrap_learns_the_others`, R1 aceptó las conexiones de R2 y R3 y siguió con 0 pares en la tabla, y fallaron igual todas las pruebas en que los routers se conocen a través de R1, `clients_never_enter_a_routing_table` entre ellas. Es la causa que daba el registro del backend: las direcciones de una conexión entrante.
- **Qué se usa.** Con `identify`, cada lado de una conexión dice al otro los protocolos que sirve y las direcciones que declara. Si el otro sirve `/agentnet/kad/1.0.0`, el nodo da a Kademlia sus direcciones en la forma router que lo nombran a él y que se pueden marcar desde aquí: ni una IP sin especificar ni de multidifusión, y loopback solo si el propio nodo está en loopback. Solo sirve ese protocolo un router, en modo servidor, así que un cliente nunca entra en una tabla de rutas.
- **Qué no se usa.** `identify` envía solo las direcciones declaradas (`with_hide_listen_addrs`), nunca los sockets del endpoint, y no guarda lo que dicen los pares (`with_cache_size(0)`), que añadiría a cada marcado. Solo tiene la clave pública del agente, así que no firma ningún registro de pares. Envía además la versión de protocolo `agentnet/0`, la de libp2p como nombre del agente (`rust-libp2p/<versión>`) y la dirección por la que vio al otro, que el nodo no usa.

### Lo que ven los routers de cada búsqueda

La conexión va autenticada con la clave del agente, así que cada búsqueda en la DHT dice a los routers a los que pregunta qué agente busca qué capacidad (H3-D4). La plantilla de `pals init` lo dice junto a `[dht]`.

### El estado: `pals dht status` y `GET /dht`

`pals dht status` pide el estado al daemon por su gateway. Con `--json`, lo escribe tal como lo da `GET /dht` del gateway, que contesta `404` si la DHT está apagada.

| Campo | Qué es |
|-|-|
| `mode` | el modo de `libp2p-kad`: `router` (servidor) o `client` |
| `routing_peers` | los routers en la tabla de rutas, también un router de `bootstrap` que no contestó: `libp2p-kad` lo deja con su última dirección |
| `connections` | las conexiones de `agentnet-kad/0` abiertas ahora |
| `connections_accepted` | las conexiones entrantes establecidas desde el arranque |
| `refused` | lo que el nodo rechazó desde el arranque: `connections`, `streams` (por las cuotas) e `idle_streams` (por ociosos), sección 5 |
| `records_stored` | los valores de otros que guarda y no han caducado |
| `provider_records_stored` | los provider records de otros que guarda y no han caducado |
| `stored_by_agent` | de esos valores y provider records, cuántos tiene cada agente |
| `rejected` | los registros rechazados desde el arranque, por cada motivo de la sección 6, también los que van a cero |
| `publication` | con `[dht] announce`, lo que el nodo publica (sección 8); sin él, `null` |

## 8. Publicación, caducidad y búsqueda

El código es `crates/pals/src/dht/publish.rs`, y sus pruebas, `crates/pals/tests/dht_publish.rs`: nodos reales sobre Iroh en loopback.

### Qué se publica

Un nodo publica solo con `[dht] announce = true`, y entonces solo las capacidades de los servicios del inventario con `visibility = "public"` (H3-D4). Son las mismas que lista su `agentnet.announce/1` y su card pública. Publica tres cosas:

- su contacto firmado, bajo `/agentnet/contact/<peer_id>`;
- su anuncio firmado, bajo `/agentnet/announce/<peer_id>`;
- un provider record de cada capacidad pública, bajo la clave de la sección 3, con las direcciones que declara (sección 7): las de un router, o `/p2p/<peer_id>` en un cliente.

Sin `announce`, no publica nada, tampoco el contacto. Un servicio privado o de proyecto nunca llega a la DHT. Al parar, el nodo no envía nada: las copias de los routers caducan solas.

### Las vueltas de publicación

Un nodo publica en vueltas:

| Cuándo | Para qué |
|-|-|
| Al entrar en la red: cuando termina el arranque de la sección 7, o al empezar, sin `bootstrap` | la primera publicación |
| Cada `[dht] republish_s`, 1200 s por defecto | que las copias de los routers no caduquen: se pueden perder dos vueltas antes de que caduque nada |
| Cada vez que el nodo vuelve a emitir uno de sus registros: un cambio de direcciones, o la renovación de las 12 h | que el contacto nuevo llegue a los routers en cuanto se emite, no en la vuelta siguiente |
| 30 s después de una vuelta que no llegó a ningún router | entrar cuanto antes, por ejemplo tras arrancar con los routers caídos |

Una vuelta nueva cierra las consultas que quedaban de la anterior. Cada vuelta:

1. Deja de anunciar las capacidades que el anuncio ya no lista. Después guarda en la tienda del nodo el provider record propio de cada una de las demás, en el orden del inventario. Una capacidad que no cabe falla sola, por ejemplo en un router que también anuncia y tiene llenas sus 1024 claves de proveedores.
2. Pone el contacto y el anuncio en los routers más cercanos a sus claves, con quórum de todos ellos. Un registro llegó cuando al menos un router contestó.
3. Cuando ha terminado el `put` del anuncio, lleva cada capacidad a los routers, como mucho 4 a la vez y en el orden del inventario: cuando termina una, empieza la siguiente. Así una vuelta de muchas capacidades no hace cola tras sí misma por los 4 streams propios que el nodo abre con cada router (sección 5), y esa espera no gasta el plazo de sus peticiones. Para cada capacidad:
   - busca los routers más cercanos a su clave;
   - les pone el anuncio, con quórum de todos ellos: el `put` espera a cada uno;
   - si llegó al menos a uno, les envía después el provider record.

   Así el anuncio viaja con cada provider record: cada router que guarda uno tiene ya el anuncio que pide la sección 6, aunque con muchos routers los más cercanos a la clave del anuncio y a la de la capacidad no sean los mismos.

Una capacidad falla sola, y las demás siguen sin que se deshaga nada, cuando:
- no cabe en la tienda del nodo (paso 1);
- ningún router de los más cercanos a su clave contestó al anuncio;
- su provider record no llegó a salir: su consulta terminó antes.

Una capacidad que falla va a `failed_capabilities`, en el orden del inventario, y la vuelta siguiente la intenta otra vez.

Una vuelta en la que ningún router contestó termina con `last_error`, y la siguiente llega a los 30 s. Con `libp2p-kad` 0.49, un router contesta también a lo que rechaza (sección 6): si lo guardó o no, lo dicen el estado del router y sus rechazos.

### Caducidad

Un router guarda un valor o un provider record `[dht] record_ttl_s` desde que lo recibe: 3600 s por defecto. Esa vida es la de `libp2p-kad`, que la mide con su propio reloj, no con el `Clock` del nodo. La replicación entre routers no la alarga: una copia replicada lleva la caducidad del original.

Un registro firmado deja además de valer a las 24 h de emitirse (`RECORD_TTL_S`). Por eso, quien publica da a cada copia de su contacto y de su anuncio la vida que le quede, la menor de dos:
- `record_ttl_s`;
- lo que le falta a su firma para caducar, por el `Clock` del nodo.

Un `put` de `libp2p-kad` lleva esa vida, y el router se queda con la menor entre ella y su propio `record_ttl_s`. Así un router no sirve un contacto ni un anuncio después de que caduque su firma, salvo por el desajuste entre los relojes de los dos nodos. Un provider record no lleva firma: vive el `record_ttl_s` del router.

No hay barridos propios. `libp2p-kad` borra un valor caducado cuando se lee o en la vuelta de replicación, y un provider record caducado cuando se leen los proveedores de su clave. Hasta entonces ocupa su sitio en la tienda, así que un router con la tienda llena borra lo caducado antes de rechazar un registro con `store_full`, y lo intenta otra vez.

Un proveedor que se para sin avisar sale de la DHT en `record_ttl_s` desde la última vuelta que llegó a los routers.

### La búsqueda en la DHT

Una búsqueda pide los proveedores de una capacidad, hasta un límite y con un plazo:

1. Pide los proveedores de la clave de la capacidad.
2. Según llega cada proveedor, hasta el límite, pide su contacto y su anuncio. Al llegar al límite, deja de pedir proveedores.
3. Comprueba cada copia como un router (sección 6), al `Clock` del nodo, y se queda con la válida de mayor `seq`.
4. Un proveedor es candidato cuando tiene un contacto y un anuncio válidos y el anuncio lista la capacidad.

El nodo nunca se encuentra a sí mismo. Un proveedor que no es candidato se descarta con su motivo:

| Motivo | Cuándo |
|-|-|
| `capability_not_announced` | su anuncio válido no lista la capacidad |
| el de la sección 6 | el último rechazo de una copia de su contacto o de su anuncio, por ejemplo `bad_contact` |
| `no valid contact`, `no valid announce` | no llegó ninguna copia de su contacto, o de su anuncio |

Una búsqueda termina de una de tres formas:

| Resultado | Cuándo |
|-|-|
| Lo que encontró | sus consultas terminaron |
| Lo que tiene, marcado como cortado (`cut`) | llegó su plazo antes: cierra sus consultas |
| El error `no DHT router answered` | sus consultas terminaron sin que contestara ningún router |

Las peticiones de una búsqueda (`requests`) son las que sus consultas mandaron a la red. `libp2p-kad` añade a la consulta cada proveedor que encuentra, como un nodo más al que preguntar, pero a un proveedor que no es router no se le marca por Kademlia (sección 4): el nodo rechaza ese marcado antes de empezarlo, y no cuenta como petición. Cada rechazo se cuenta en la consulta que lo pidió, así que dos búsquedas a la vez que encuentran el mismo proveedor no se restan las peticiones de la otra.

El estado de cada vuelta y de cada búsqueda va por consulta: el nodo hace a la vez cualquier número de búsquedas y vueltas. Lo que llega de la DHT son datos que no son de fiar: quien usa lo encontrado vuelve a comprobarlo.

### El estado de la publicación

Con `[dht] announce`, `GET /dht` y `pals dht status --json` llevan `publication`, y `pals dht status` sin `--json` lo escribe en unas pocas líneas:

| Campo | Qué es |
|-|-|
| `capabilities` | las capacidades públicas de la última vuelta; antes de que termine la primera, las del anuncio en vigor |
| `last_round_unix` | cuándo terminó la última vuelta, por el reloj del nodo |
| `last_error` | por qué se quedó corta la última vuelta. Si hay más de un motivo, el más grave: la tienda del propio nodo no tenía sitio para sus registros; no contestó ningún router; o cuántas capacidades no se anunciaron, por ejemplo `2 capabilities not provided` |
| `failed_capabilities` | las capacidades que la última vuelta no pudo anunciar, en el orden del inventario: no cabían en la tienda del nodo, ningún router de su clave contestó al anuncio, o su provider record no llegó a salir |

## 9. La DHT como fuente de la búsqueda de servicios

El código es `crates/pals/src/discovery/search.rs` (`Searcher`), y sus pruebas, `crates/pals/tests/dht_search.rs`. La búsqueda es la del hito 2 (`pals discover`, `pals_discover` del MCP y `POST /discovery/search` del gateway): la card verificada, la consulta de servicio por [`agentnet-discovery/0`](agentnet-discovery-0.md) y la selección en dos etapas. La DHT le añade candidatos, los pasos 3 y 4 del §5.3 del diseño, y no cambia nada más: con la DHT apagada, una búsqueda es la del hito 2.

### Los candidatos

1. **Alias.** La capacidad pasa primero por `[discovery.aliases]`, un solo paso: un alias busca la capacidad que nombra, aunque esa sea el nombre de otro alias. Hay 256 alias como mucho, y cada uno nombra una capacidad válida; si no, la configuración se rechaza al leerla.
2. **Los contactos,** primero, como en el hito 2.
3. **La DHT,** detrás, solo si el nodo la tiene encendida y la búsqueda no nombra contactos (`--peer`, `peers`) ni dice `--no-dht` (`exclude_dht`). A la DHT solo va la clave de la capacidad (sección 3). La búsqueda en la DHT (sección 8) pide hasta `max_candidates` + 1 proveedores, con el plazo `min(per_candidate_timeout, lo que quede del presupuesto)`: con la DHT encendida, una búsqueda espera a la DHT, como mucho `per_candidate_timeout` (3 s por defecto), antes de evaluar sus contactos. Los proveedores se añaden en el orden en que llegaron, por su id de agente. Uno que ya es contacto sigue siendo el contacto, una sola vez, y la DHT nunca da al propio agente.
4. **El límite.** Solo se contactan los `max_candidates` primeros (8 por defecto), contactos y proveedores de la DHT juntos.

Las peticiones de la búsqueda en la DHT se cuentan aparte, en `dht_requests` de las métricas.

### Cobertura parcial

La búsqueda nunca falla por la DHT. Cuando la DHT no da todo lo que podría, los contactos se buscan igual, y la cobertura es parcial. Además de `time budget` y `timeout`, del hito 2, la marcan estas etiquetas. `candidate limit` ya existía en el hito 2, cuando quedaban fuera contactos; el hito 3 amplía lo que la da. Las otras dos son nuevas:

| Etiqueta | Cuándo |
|-|-|
| `candidate limit` | quedaron fuera contactos, como en el hito 2, o proveedores de la DHT por `max_candidates`; o la búsqueda en la DHT trajo tantos proveedores como pidió, así que pudo dejar alguno |
| `dht timeout` | la búsqueda en la DHT llegó a su plazo y se cortó (`cut`, sección 8) |
| `dht unavailable` | no contestó ningún router (`no DHT router answered`) |

Una búsqueda vacía o con cobertura parcial no prueba que no haya proveedor (§5.3).

### La verificación de un candidato de la DHT

Lo que llega de la DHT no se usa sin comprobarlo, y a un candidato no se le envía nada antes de esto:

- **Los descartes de la DHT.** Un proveedor que la búsqueda en la DHT descartó (sección 8) se descarta en la etapa `Card` con el motivo `dht: <por qué>`, por ejemplo `dht: no valid contact`. Si ese agente es un contacto, cuenta solo como contacto.
- **Sus registros.** Su contacto y su anuncio, que la búsqueda en la DHT ya verificó, pasan por `Store::peer_record_accept`, como los de una respuesta de `agentnet-discovery/0`. Uno que vuelve atrás respecto al más nuevo que este agente conoce de ese agente lo descarta en la etapa `Card`, con el motivo `record regression`.
- **Lo que se le pide.** Se le marca en las direcciones directas y el relay de su contacto, y se evalúa como cualquier par PALS. Se le pide su card pública, firmada con `kid` igual a su id de agente, y también la extendida (`GetExtendedAgentCard`) si este agente tiene un grant suyo que sirve tareas en el proyecto de la búsqueda, o en cualquiera si no nombra ninguno. Después, una oferta por `agentnet-discovery/0`. Si la búsqueda quiere informes de benchmark y la oferta es compatible, se le piden con `GetBenchmarkReport`: los que la oferta cita del perfil pedido y siguen en vigor, 4 como mucho (`MAX_REPORTS_PER_CANDIDATE`).
- **Sin grant, nada privado.** Va con el grant que este agente tenga de él, si tiene alguno. Si no tiene ninguno del proyecto de la búsqueda, no recibe ni el proyecto ni los ids del inventario, y entra en el ranking con `grant_held: false`. Todo candidato de la DHT lleva `from_dht: true`, y `pals discover` escribe `found in the DHT`.

### Preguntar

Como en el hito 2: a un par del que este agente no tiene un grant que sirva tareas no se le pregunta nunca (`no grant held`). Encontrarlo en la DHT no autoriza nada. A uno encontrado en la DHT se le pregunta por su id de agente, y el gateway lo marca, aunque no sea un contacto guardado, en el contacto firmado más nuevo que este agente conoce de él y que sigue en vigor por el reloj del nodo, con 60 s de tolerancia. Ese contacto le llegó de una respuesta de descubrimiento o de la DHT, verificado. Si no tiene ninguno, lo marca solo por su id. La tarea va, como siempre, con los bindings estándar de A2A sobre `http-over-iroh/v1`.

### Lo que nunca hace

Buscar en la DHT no ejecuta trabajo: ni en los routers, que contestan desde su tienda, ni en los proveedores encontrados, a los que solo se pide lo de más arriba: su card, la extendida con un grant, una oferta y, si la búsqueda los quiere, sus informes de benchmark. Lo prueba `querying_the_dht_runs_no_work`: veinte búsquedas, las más amplias con proyecto, preferencias e informes de benchmark, no ejecutan ninguna tarea en los proveedores ni en los routers, y ninguno acepta una tarea.

Pruebas de esta sección, en `crates/pals/tests/dht_search.rs`: `a_provider_with_no_prior_contact_is_found`, `contacts_come_first_and_each_agent_once`, `exclude_dht_and_named_peers_never_touch_the_dht`, `dht_without_routers_leaves_the_contacts_search_intact`, `a_tampered_card_is_discarded_even_with_a_valid_dht_announce`, `a_dht_record_that_goes_back_is_discarded`, `inventory_ids_never_go_to_a_dht_candidate_without_a_grant`, `an_alias_searches_its_capability`, `aliases_to_invalid_capabilities_are_refused`, `a_dht_candidate_is_asked_once_a_grant_is_held`, `querying_the_dht_runs_no_work` y `discover_cli_and_mcp_take_exclude_dht`.
