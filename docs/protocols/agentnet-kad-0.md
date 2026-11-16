# Protocolo `agentnet-kad/0`

Especificación de la DHT con la que un agente PALS encuentra a los proveedores de una capacidad sin contacto previo ni directorio central (diseño §5 y §6): una red Kademlia privada, con `libp2p-kad`, cuyas conexiones son las del endpoint de Iroh que el daemon ya usa para A2A, los blobs y `agentnet-discovery/0`. Es un protocolo propio de PALS, no una parte de A2A: con lo que encuentra, el agente sigue el camino del hito 2 (card verificada, consulta de servicio y selección), y la tarea elegida se envía con los bindings estándar de A2A v1.0.

Los valores de este documento son los del código, `crates/pals/src/dht/`, y el vector `crates/pals-core/tests/vectors/kad-v0.json` congela la identidad, las claves y las formas de dirección. El backend es `kad-iroh`, ratificado en el [registro del backend](../decisions/2026-11-16-hito-3-backend-dht.md#decisión) (H3-D1).

Este documento crece con el código de la fase 2 del hito 3. Hoy describe la identidad, las claves, las formas de dirección, la conexión con sus límites y lo que guarda un router. El swarm y la publicación se añaden con su código.

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
- Un marcado tiene 5 s, el plazo del handshake de `libp2p-quic`.
- Con `relay = true`, Iroh puede además buscar el endpoint en el DNS de n0, como desde el hito 1. Sin relay, solo cuentan las direcciones directas.

### Conexiones entrantes

El endpoint entrega cada conexión de `agentnet-kad/0`, una vez autenticada, a una cola de 32 conexiones para libp2p. **La cola nunca espera:** si está llena, la conexión se cierra al momento con el código `1` y el motivo `busy`. Se cierra igual, con el motivo `not listening`, una conexión que llega cuando libp2p no escucha.

### Streams entrantes

Antes de entregar a libp2p un stream que abrió el otro lado, el nodo comprueba estas cuotas:

| Cuota | Valor |
|-|-|
| Streams de un par abiertos a la vez | 8, sumando todas sus conexiones |
| Streams que abre un par en un minuto | 120, en el minuto del reloj del nodo, sumando todas sus conexiones |
| Streams de todos los pares abiertos a la vez | 256 |

Un stream que pasa de una cuota se resetea al aceptarlo, sin leerlo: el nodo cierra sus dos mitades con el código `1` (`RESET_STREAM` y `STOP_SENDING`), y libp2p no llega a verlo. Un stream cuenta mientras libp2p lo tiene abierto, y uno rechazado no cuenta en el minuto. Los streams que abre el propio nodo no cuentan en ninguna cuota.

El nodo cuenta desde el arranque las conexiones y los streams que rechaza.

### El puerto: `[net] listen`

Un router necesita un puerto fijo para que los demás lo marquen. `[net] listen = "<ip>:<puerto>"` liga el endpoint de Iroh a esa dirección y solo a ella, con relay o sin él. Es el puerto de todo el endpoint: también el de A2A, los blobs y `agentnet-discovery/0`.

Sin `listen`, todo queda como en el hito 2: con relay, los puertos libres del preset N0 en todas las interfaces; sin relay, un puerto libre de 127.0.0.1. Un valor que no es una dirección de socket, con IP y puerto, se rechaza al leer la configuración.

## 6. Lo que guarda un router

El código es `crates/pals/src/dht/validate.rs`, y sus pruebas, `crates/pals/tests/dht_validate.rs`. Un router comprueba cada `PutRecord` y cada `AddProvider` que recibe antes de que llegue a su tienda (`StoreInserts::FilterBoth` de `libp2p-kad`). Quien busca lee con las mismas comprobaciones los contactos y los anuncios que recibe. Cada registro tiene uno de tres resultados:

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

Así, un router solo admite de un agente provider records de las capacidades de su anuncio firmado, como mucho 32 (`MAX_ANNOUNCED_CAPABILITIES`). Por eso quien publica envía su anuncio antes que sus provider records. La comprobación se hace al recibir cada provider record: si un anuncio nuevo sustituye al que el router guardaba, los provider records que ya tenía siguen hasta que caducan, aunque el anuncio nuevo no liste su capacidad.

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
| `store_full` | un registro válido que ya no cabe en la tienda. No lo da ninguna comprobación: lo cuenta el swarm |
