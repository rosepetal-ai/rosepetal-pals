# Vocabulario común de capacidades `v1`

Las capacidades comunes con las que un agente PALS dice qué hace y por las que otro lo busca (diseño §5.1 y §5.4). Este documento es donde se publican y desde donde se amplían, como decide H3-D5 en su [registro de decisiones](../decisions/2026-11-16-hito-3-decisiones.md#h3-d5-dónde-se-publica-el-vocabulario-común-y-cómo-se-amplía).

El vocabulario es una ayuda para encaminar una búsqueda, no una lista cerrada de los servicios permitidos. Lo que un proveedor hace exactamente lo precisan la descripción de su servicio, su inventario y la oferta que da a cada consulta ([`inventory-v1.md`](inventory-v1.md)). Que un agente anuncie una capacidad no autoriza nada ni dice nada de su calidad: la tarea sigue necesitando el grant, y la calidad la miden los informes de benchmark.

## Identificación

| Elemento | Valor |
|-|-|
| Versión | `v1`, la de la etiqueta `agentnet:capability:v1:<id>` |
| En la card | la etiqueta del skill de cada servicio que la ofrece ([`inventory-v1.md`](inventory-v1.md#identificadores-capacidades-y-extensiones)) |
| En una consulta | el campo `capability` de `agentnet.service-query/1`, con la misma gramática |
| En la DHT | la clave de los provider records de [`agentnet-kad/0`](../protocols/agentnet-kad-0.md#3-claves) |
| Código | `pals_core::inventory::{CAPABILITY_PREFIX, capability_tag}` y `pals::dht::keys::provider_key` |
| Vector | `crates/pals-core/tests/vectors/kad-v0.json`, `provider_keys` |

Una capacidad de este documento no cambia de significado ni de clave, porque quien la anuncia y quien la busca dependen de las dos. Si hace falta otra definición, es otra capacidad. Un cambio incompatible del vocabulario entero sería un `v2`, con su propio documento y su propio prefijo.

## Gramática

Es la del hito 2, sin cambios:

- **Capacidad común:** un id de 1 a 128 caracteres `[a-z0-9._-]`, sin `:`, como `vision.ocr`. Viaja como la etiqueta `agentnet:capability:v1:<id>`.
- **Capacidad con espacio de nombres:** `<espacio>:<id>`, con la forma de un id de entrada del inventario. El espacio tiene de 1 a 64 caracteres `[a-z0-9._-]` y nunca es `agentnet`. El id tiene de 1 a 128 caracteres `[A-Za-z0-9._/-]`. Viaja tal cual, por ejemplo `example:metal-plates`.

Nada más es una capacidad. En particular, no lo es la etiqueta completa: `agentnet:capability:v1:vision.ocr` escrito como capacidad se rechaza, igual que cualquier cadena que empiece por `agentnet:`, en cualquier combinación de mayúsculas. Así cada capacidad tiene una sola escritura y una sola clave en la DHT.

## Las capacidades comunes

### `vision.ocr`

**Definición.** Sacar el texto de imágenes o de documentos escaneados.

**Ejemplos.**
- Leer la matrícula en la foto de un coche.
- Sacar el número, la fecha y el total de una factura escaneada.
- Pasar a texto un PDF escaneado, página a página.

No es describir lo que se ve en una imagen, ni encontrar defectos en ella (`vision.defect-detection`).

### `vision.defect-detection`

**Definición.** Encontrar defectos en imágenes de piezas fabricadas.

**Ejemplos.**
- Marcar las grietas, rayas o abolladuras en la foto de una chapa metálica.
- Decidir si una pieza de una línea de producción pasa la inspección visual, y por qué no.
- Señalar las soldaduras defectuosas en las imágenes de un conjunto soldado.

No es leer el texto o los códigos impresos en una pieza (`vision.ocr`).

### `code.rust.review`

**Definición.** Revisar código Rust y decir qué falla o qué se puede mejorar.

**Ejemplos.**
- Revisar un pull request de un crate y señalar errores, bloques `unsafe` sin justificar o pruebas que faltan.
- Leer un módulo y proponer cambios de diseño, de estilo o de rendimiento.

No es escribir el código ni aplicar los cambios: el resultado es la revisión.

### `knowledge.documentation`

**Definición.** Contestar preguntas con un cuerpo de documentación.

**Ejemplos.**
- Contestar cómo se configura una opción de un producto con su manual.
- Buscar en la documentación interna de un equipo la respuesta a una pregunta y decir de qué documento sale.

Qué documentación es la dice el inventario del proveedor, en sus fuentes de información.

## Cadenas completas y claves en la DHT

La clave de los provider records de una capacidad es el multihash SHA2-256 de los bytes UTF-8 de su cadena completa: los bytes `0x12` (SHA2-256) y `0x20` (32 bytes), seguidos del SHA-256 de la cadena. Para una capacidad común, la cadena completa es su etiqueta `agentnet:capability:v1:<id>`. Para una con espacio de nombres, es la capacidad tal cual. [`agentnet-kad-0.md`](../protocols/agentnet-kad-0.md#3-claves) describe el resto de claves de la DHT.

| Capacidad | Cadena completa | Clave en la DHT (hexadecimal) |
|-|-|-|
| `vision.ocr` | `agentnet:capability:v1:vision.ocr` | `122092485626d851a455cd10cd7ae3800eb76ff67220722638435d3ae050e92c8463` |
| `vision.defect-detection` | `agentnet:capability:v1:vision.defect-detection` | `1220451ff690aedc29578775b84d01a50c84babb625def0ef4c1546f409e08078f23` |
| `code.rust.review` | `agentnet:capability:v1:code.rust.review` | `122081ce3ea1d76d68090da37bd3df0a4562e5b50e6e7daa7c5c95dbed7728dfc934` |
| `knowledge.documentation` | `agentnet:capability:v1:knowledge.documentation` | `1220ab4d4ea5320beba8673732049203d56cac1fdae3720699a4fef8c47215345235` |
| `example:metal-plates` | `example:metal-plates` | `12202a561a2441f0cc4eba6d49bc3d437b467d69cb6c640d213337a81b4826b173b6` |

La última fila no es del vocabulario: es un ejemplo de capacidad con espacio de nombres, para fijar que su cadena va sin prefijo. Las cifras son las del vector `kad-v0.json`, calculadas también con un script independiente (`hashlib` de Python), y la prueba `capabilities_doc_quotes_the_vector` de `crates/pals/tests/dht_keys.rs` comprueba que esta tabla las cita.

## Cómo se amplía

Lo decide H3-D5:

- **El vocabulario común** se amplía con un cambio en este documento, revisado como cualquier otro cambio del repositorio. La capacidad nueva no se solapa con ninguna existente: si una tarea cabe en dos, se precisa la definición o la capacidad no entra. Lleva su id con la gramática de arriba, una definición, ejemplos y su fila en la tabla de claves, y el mismo cambio añade su entrada a `provider_keys` en `kad-v0.json`. Ninguna capacidad se quita ni cambia de definición.
- **Las capacidades propias** de un proveedor van en su espacio de nombres, `<espacio>:<id>`, y no necesitan la aprobación de nadie (§5.1). Se anuncian y se buscan como las comunes, cada una con su clave. Quien solo busca la capacidad común no las encuentra, pero un servicio puede listar las dos.
- **Los alias y las traducciones** los resuelve el buscador de quien busca, no el vocabulario (§5.4): que «OCR» o «leer matrículas» lleven a `vision.ocr` es cosa de su configuración, `[discovery.aliases]` en `config.toml`, que añade la tarea 13 del [plan del hito 3](../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). A la DHT solo llega la capacidad a la que lleva el alias.
