# Protocolo `agentnet-kad/0`

Especificación de la DHT con la que un agente PALS encuentra a los proveedores de una capacidad sin contacto previo ni directorio central (diseño §5 y §6): una red Kademlia privada, con `libp2p-kad`, cuyas conexiones son las del endpoint de Iroh que el daemon ya usa para A2A, los blobs y `agentnet-discovery/0`. Es un protocolo propio de PALS, no una parte de A2A: con lo que encuentra, el agente sigue el camino del hito 2 (card verificada, consulta de servicio y selección), y la tarea elegida se envía con los bindings estándar de A2A v1.0.

Los valores de este documento son los del código, `crates/pals/src/dht/`, y el vector `crates/pals-core/tests/vectors/kad-v0.json` congela la identidad, las claves y las formas de dirección. El backend es `kad-iroh`, ratificado en el [registro del backend](../decisions/2026-11-16-hito-3-backend-dht.md#decisión) (H3-D1).

Este documento crece con el código de la fase 2 del hito 3. Hoy describe la identidad, las claves y las formas de dirección. El transporte, lo que guarda un router, el swarm y la publicación se añaden con su código.

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
