# Extensión A2A `inventory/v1` y perfil `agentnet.inventory/1`

Extensión opcional de A2A con la que un agente PALS describe, además de lo que ofrece, con qué lo hace: skills instaladas, plugins, conexiones MCP, runtime, modelos, fuentes de información, hardware y red (diseño §9.1–§9.3, §9.8 y §9.9). La Agent Card sigue siendo la ficha principal: sus skills son los servicios del inventario, y el inventario no duplica la ficha estándar. Es una extensión propia de PALS: no forma parte del estándar A2A ni es una extensión oficial.

## Identificación

| Elemento | Valor |
|-|-|
| URI | `https://github.com/rosepetal-ai/rosepetal-pals/extensions/inventory/v1` |
| Perfil | `agentnet.inventory/1` |
| Declaración | `capabilities.extensions` de la card pública, sin `required`, con `params = {"profile": "agentnet.inventory/1", "discoveryAlpn": "agentnet-discovery/0"}` (solo cadenas) |
| Cuándo se declara | Solo cuando el daemon sirve el ALPN de `discoveryAlpn`: la card declara únicamente lo que el agente sirve |
| Transporte | El inventario no viaja por A2A: lo sirve, en vistas firmadas y filtradas por el permiso de quien pregunta, el protocolo de descubrimiento que nombra `discoveryAlpn`, [`agentnet-discovery/0`](../protocols/agentnet-discovery-0.md) |
| Esquema | [`inventory-v1/inventory.schema.json`](inventory-v1/inventory.schema.json) (JSON Schema 2020-12), con el vector `crates/pals-core/tests/vectors/inventory-v1.json` |
| Vistas | `agentnet.inventory-view/1`, dominio de firma `agentnet.inventory-view/1\n`: [`inventory-v1/inventory-view.schema.json`](inventory-v1/inventory-view.schema.json), con el vector `crates/pals-core/tests/vectors/view-v1.json` |
| Consultas de servicio | `agentnet.service-query/1`: [`inventory-v1/service-query.schema.json`](inventory-v1/service-query.schema.json), con el vector `crates/pals-core/tests/vectors/service-query-v1.json` |
| Ofertas | `agentnet.service-offer/1`, dominio de firma `agentnet.offer/1\n`: [`inventory-v1/service-offer.schema.json`](inventory-v1/service-offer.schema.json), con el vector `crates/pals-core/tests/vectors/service-offer-v1.json` |

La URI y el perfil no cambian de significado: un cambio incompatible será `inventory/v2` y `agentnet.inventory/2`.

## Clientes que no entienden la extensión

Un cliente que no entiende el inventario puede seguir usando las capacidades A2A que el agente declara en su card: la extensión no cambia nada del intercambio estándar, y una petición que no la activa se atiende exactamente igual que sin ella.

Lo que un cliente no entiende no lo da por cumplido (§9.1, §9.9). Si una tarea exige un requisito de la extensión que el cliente no comprende, ese requisito no está satisfecho: el cliente pide aclaración o descarta al candidato. Los metadatos opcionales desconocidos sí puede ignorarlos.

## El fichero `inventory.toml`

El operador declara el inventario en `inventory.toml`, junto a `config.toml` salvo que `[inventory] file` diga otra ruta (una ruta relativa se resuelve contra el directorio de `config.toml`). `pals init` lo crea solo con comentarios, con un ejemplo de cada bloque. Sin fichero, el inventario está vacío.

El daemon lo lee al arrancar, y solo entonces: en el hito 2 no hay recarga en vivo, y cambiar un modelo o un servicio exige reiniciar el daemon. Un inventario inválido impide arrancar, con un error que nombra el fichero y la entrada.

`[inventory] refresh_interval_s` (900 por defecto) es el intervalo con el que los adaptadores de inventario comprueban las conexiones MCP configuradas.

## Bloques

Todos los bloques son opcionales (§9.2), y en cada entrada todos los campos salvo `id`, `description` y `provenance`. **Un campo ausente es desconocido**: PALS nunca lo escribe, y nunca vale cero, `false` ni vacío. Una cadena vacía no es un valor y se rechaza: lo que no se conoce se omite.

`runtime`, `hardware` y `network` son una sola entrada (`[runtime]`); los demás bloques, listas (`[[models]]`).

| Bloque | Campos propios |
|-|-|
| `services` | `id`, `name`, `description`, `capabilities`, `tags`, `examples`, `input_modes`, `output_modes`, `languages`, `processing_region`, `external_cloud_used`, `estimated_start_delay_ms`, `estimated_duration_ms`, `components` (`{id, role, group}`, con `role` `required`, `optional` o `alternative`), `limitations`, `visibility` |
| `skills` | `package_id`, `name`, `offered_for` |
| `plugins` | `runtime`, `components` |
| `mcp` | `server`, `server_version`, `tools` (`[{name, description, annotations}]`, declaradas), `resources`, `offered_tools` (lo único que una vista puede nombrar), `adapter` (cómo la comprueba un adaptador: configuración del operador, nunca sale en una vista) |
| `runtime` | `kind`, `modes`, `restrictions` |
| `models` | `provider`, `model_id`, `revision`, `mutable_alias`, `role` (`primary`, `auxiliary` u `offered`), `execution` (`{kind = "local"}` o `{kind = "external", provider = "…"}`), `inference_runtime`, `quantization`, `precision`, `modalities`, `context_tokens`, `output_tokens`, `dynamic_selection` |
| `information` | `domains`, `languages`, `coverage`, `updated_at`, `live`, `formats`, `schema_ref`, `approximate_volume`, `operations` (`answer_with_sources`, `search`, `query_records`, `aggregate`, `process_in_place`, `deliver_subset`), `limits` |
| `hardware` | `cpu {arch, model, cores}`, `ram_mib`, `accelerators [{kind, model, count, memory_mib_each}]`, `storage {kind, capacity_gib}`, `offered {cpu_cores, ram_mib, vram_mib_each, concurrency, accelerators}` y `basis` |
| `network` | `transports`, `region` (aproximada y autorizada), `direct_connectivity_observed` |

Toda entrada de un bloque distinto de `services` lleva además `id`, `description`, `provenance` (`operator` en una declaración del fichero), y, si se conocen, `version`, `visibility`, `expires_at` y `evidence` (`{reference, hash}`, al menos uno).

- **Hardware.** Las cifras son nominales: las declara el operador y nunca son medidas (§9.6). Por eso el bloque lleva siempre `"basis": "declared"`; las medidas van en informes de benchmark, aparte. `offered` es lo que puede usar una tarea, y los requisitos de recursos se comparan con eso, nunca con lo instalado.
- **Modelos.** Un modelo que se ejecuta en un proveedor externo no usa el hardware de este inventario.
- **MCP.** `adapter` es `{transport = "stdio", command, args, env_passthrough}` (los nombres de las variables de entorno que recibe el proceso, nunca sus valores) o `{transport = "http", url}`. En TOML, `transport` va junto a sus claves en `[mcp.adapter]`. Los nombres de herramienta son de 1 a 128 caracteres `[A-Za-z0-9_.-]`, el juego que recomienda MCP; las anotaciones son declaraciones, no una política de seguridad.

Las fechas (`expires_at`, `updated_at`) son cadenas en UTC con segundos enteros, `"2026-11-14T00:00:00Z"`, como en los grants; una fecha nativa de TOML se rechaza.

## Identificadores, capacidades y extensiones

- **Ids de entrada:** `<espacio de nombres>:<nombre>`, de 1 a 64 caracteres `[a-z0-9._-]` en el espacio de nombres y de 1 a 128 `[A-Za-z0-9._/-]` en el nombre. El espacio de nombres `agentnet` está reservado. Un id nombra una sola entrada en todo el inventario. Ejemplos: `example:repo-reader`, `project:repo-controller`.
- **Ids de servicio:** ids de skill seguros, `[a-z0-9][a-z0-9-]{0,63}`, porque cada servicio es un skill de la card y el gateway del hito 1 comprueba el `id` de cada objeto como un id de tarea. `ask` está reservado: es el skill propio de la card.
- **Servicios y card.** Un servicio necesita `name`, `description` y al menos una capacidad o etiqueta: cada uno se convierte en un skill de la card, y un campo vacío no sería punto fijo de las formas canónicas con que se firma. Sus `components` son entradas del mismo inventario. Que todos los servicios quepan en la card (32 KiB, 64 skills) se comprueba al construirla, antes de servir nada.
- **Capacidades.** Una capacidad sin `:` es del vocabulario común (`vision.ocr`, de 1 a 128 caracteres `[a-z0-9._-]`) y viaja en la card como la etiqueta de skill `agentnet:capability:v1:<capacidad>`. Una con `:` es del espacio de nombres de su proveedor (`example:ocr-metal`), con la forma de un id de entrada, y viaja tal cual. El campo `capabilities` de A2A, que describe funciones del protocolo, nunca lleva categorías de búsqueda (§9). Las etiquetas propias de un servicio (`tags`) viajan en el skill junto a las de capacidad, así que ninguna puede empezar por `agentnet:`, en cualquier combinación de mayúsculas: se haría pasar por una capacidad.
- **Extensiones.** Una clave que este esquema no conoce es un error, salvo bajo `extensions` de una entrada, donde las claves llevan espacio de nombres (`"example:tier"`), nunca `agentnet`, y el valor es libre.

**Topes:** cadenas y claves de hasta 4 KiB, como mucho 1000 entradas por bloque, fichero de hasta 1 MiB.

## Cinco dimensiones

Instalado, operativo, ofrecido, autorizado y disponible son dimensiones distintas, no una única bandera de disponibilidad (§9.3):

| Dimensión | Qué la da en PALS |
|-|-|
| Instalado o configurado | Estar en el inventario |
| Operativo | Una observación de un adaptador de inventario, con su fecha, su caducidad y su resultado. El fichero no puede declarar `operational`: una declaración manual nunca equivale a una comprobación, y se rechaza el inventario que lo intenta |
| Ofrecido | La visibilidad de la entrada en el ámbito de quien pregunta |
| Autorizado para esta petición | El grant del solicitante; se vuelve a comprobar al ejecutar, y ninguna respuesta de inventario es un grant |
| Disponible ahora | La oferta de servicio, con su validez; nunca el inventario |

Una observación lleva `source` (`{"kind": "adapter", "method": …}` o `{"kind": "declared"}`), `status` (`ok`, `failed` o `unknown`), `observed_at`, `expires_at`, las herramientas ofrecidas que el servidor listó (con la descripción recortada a 1 KiB) y, en `detail`, solo uno de estos códigos fijos: `timeout`, `unreachable`, `protocol error`, `too many tools` o `missing tools: <herramientas ofrecidas que faltan>`. Nunca el error en crudo, que puede nombrar una URL o una orden: ese va al log del daemon.

## Visibilidad y ámbitos

El inventario completo nunca se publica. Cada entrada y cada servicio tiene una visibilidad, `private` por defecto:

| Visibilidad (TOML) | Se muestra en |
|-|-|
| `"private"` | Ningún ámbito: instalado pero no ofrecido |
| `"public"` | Todos los ámbitos |
| `{ project = ["demo"] }` | Los ámbitos de proyecto y de tarea de esos proyectos |
| `{ task = ["demo"] }` | Solo los ámbitos de tarea de esos proyectos |

El ámbito de quien pregunta sale del grant que presenta, una vez verificado, y de nada más que mande. Los grants `agentnet.grant/1` no cambian (§9): un grant cuyos recursos cubren `skill:ask` da

| Grant | Ámbito | Clave |
|-|-|-|
| Ninguno | Público | `public` |
| `task.send` sobre `skill:ask` | Tarea de su proyecto | `task:<proyecto>` |
| `task.read` o `task.cancel` sobre `skill:ask` | Proyecto de su proyecto | `project:<proyecto>` |
| Cualquier otro (`blob.fetch`, otro recurso) | Público | `public` |

Cada ámbito ve también lo que ven los anteriores: el de tarea ve lo público, lo de su proyecto y lo de sus tareas. Las vistas se firman por ámbito, con su secuencia y su caducidad, y una vista pública no lleva recuentos, hashes ni referencias que dejen inferir lo oculto (§9.8).

## Vistas

Quien pregunta nunca recibe el inventario, sino la vista de su ámbito: páginas firmadas por el agente, con el esquema `agentnet.inventory-view/1` ([`inventory-v1/inventory-view.schema.json`](inventory-v1/inventory-view.schema.json)). Cada página lleva su ámbito, su destinatario, su secuencia y su caducidad (§9.8).

**Qué muestra.** Solo las entradas y los servicios visibles en el ámbito: lo privado (instalado pero no ofrecido) no aparece en ninguna vista. Cada entrada lleva su sección, su `id`, `description`, `provenance` y, si las tiene, `version` y `expires_at`, sus dimensiones y un `body`:

- `body` son los campos propios del bloque, con `evidence` y `extensions` si la entrada los tiene. Nunca lleva la visibilidad, que nombraría los demás proyectos que la ven. En un servicio son sus campos, sin `id` ni `description`.
- `dimensions` lleva `installed` y `offered` siempre a `true`, porque lo que aparece está instalado y ofrecido en ese ámbito; `authorized`, `true` solo en un ámbito de tarea, y que se vuelve a comprobar al ejecutar; y `operational`, la observación del adaptador sin su lista de herramientas, o nada si no hay observación. La disponibilidad del momento nunca está en la vista: la da la oferta.
- Un servicio conserva solo los vínculos a componentes que el ámbito muestra. Una alternativa que se queda sola de su grupo aparece como `optional` y sin grupo, así que se lee igual haya o no alternativas ocultas: la vista no revela que exista un componente oculto.
- Una conexión MCP publica solo las herramientas de `offered_tools`; si tiene adaptador, solo las que este además observó. Cada una se llama `<id de la conexión>/<herramienta>` y lleva la descripción y las anotaciones observadas o, si no las hay, las declaradas. Ni `adapter`, ni `offered_tools`, ni las herramientas declaradas que no se ofrecen salen nunca.

**Destinatario y firma.** Una página de proyecto o de tarea lleva en `audience` a quien pregunta, y solo vale para él; una pública no lleva `audience`. La firma es Ed25519 del agente sobre `agentnet.inventory-view/1\n` seguido del JCS de la página, y la página firmada ocupa como mucho 64 KiB. Caduca 300 s después de emitirse, y quien la recibe admite 60 s de desfase de reloj, solo para rechazarla.

**Revisión.** `seq` es la revisión de la vista del ámbito, sobre todas sus secciones. Solo sube cuando cambia lo que ese ámbito muestra, nunca vuelve atrás y sobrevive a los reinicios. Lo oculto no cuenta en nada que lleve una página: una vista pública no tiene totales, recuentos, hashes ni referencias de lo que deja fuera.

**Páginas.** Una página termina en el primer límite que alcanza: 64 KiB, 100 entradas (o las que se pidan, si son menos) o 1 MiB acumulado en la cadena de cursores. `truncated_by` dice cuál fue. Con `page_bytes` o `page_entries`, `next_cursor` lleva a la página siguiente; con `query_total`, la cadena termina ahí y no hay cursor. Una entrada que no cabe sola en una página no se corta: la petición falla.

**Cursores.** Quien recibe un cursor lo devuelve tal cual: puede leerlo, pero no alterarlo sin invalidarlo, porque lleva un MAC con una clave del agente. Solo vale para el mismo solicitante, ámbito, secciones y revisión. Si el contenido cambia, cambia la revisión y el cursor deja de valer. Cada arranque usa una clave nueva, así que los cursores mueren al reiniciar. Que un cursor no valga no dice por qué.

**Caché.** El agente guarda como mucho 256 páginas, por ámbito, destinatario, secciones, cursor, tamaño de página y revisión: una página privada nunca se sirve a otro solicitante.

`pals inventory show [--scope public|project:<p>|task:<p>] [--sections a,b]` imprime en JSON lo que vería un par de ese ámbito, renderizado en local desde el fichero: las entradas de su vista de una pieza, sin firma, sin `seq` y sin paginar.

## Consultas de servicio y ofertas

Antes de encargar nada, quien busca un servicio pregunta a cada candidato si puede atender su caso con una consulta de servicio (§7.1), y el candidato contesta con una oferta firmada (§7.2). Las dos viajan por [`agentnet-discovery/0`](../protocols/agentnet-discovery-0.md#servicequery). Contestar nunca ejecuta nada: el candidato compara la consulta con los metadatos de su inventario y con los contadores de su worker, sin arrancar un LLM, una herramienta ni un adaptador (§7.3).

**Consulta.** Lleva `query_id` (de 1 a 64 caracteres `[A-Za-z0-9_-]`), `capability` (con la forma de las capacidades del inventario), los tipos MIME de entrada y de salida (16 por lista como mucho), `requirements`, un `summary` de 1 KiB como mucho, `reply_timeout_ms` (más de 3000 cuenta como 3000), `requested_inventory_sections` (las secciones de la vista que piensa leer; la oferta no depende de ellas), `project` y `execution_authorized`, que tiene que ser `false`: una consulta nunca autoriza ejecución. Viaja como su JCS, de 8 KiB como mucho, con una sola grafía, y su BLAKE3 (`query_hash`) la vincula con la oferta. `requirements` lleva `language`, `external_cloud_allowed`, `required_region` e `inventory`, con `required` y `preferred`: ids de conexiones MCP, fuentes de información, skills instaladas, plugins y modelos (32 por lista como mucho), `execution_external_cloud_allowed`, `min_offered_ram_mib`, `min_offered_vram_mib` y `accelerators`. Lo que no está en `inventory.preferred` es obligatorio. Las claves desconocidas se admiten (32 por grupo como mucho) y nunca se dan por cumplidas. Quien pregunta solo envía ids de su inventario a candidatos con un grant del proyecto (§7.1, §9.9).

**Ámbito.** El que da el grant presentado ([Visibilidad y ámbitos](#visibilidad-y-ámbitos)). `project` solo elige entre los proyectos que ese grant cubre: si coincide con el del grant, el ámbito es el del grant; si no, o sin grant, la consulta se contesta como pública.

**Matching.** Sobre los servicios que el ámbito muestra con la capacidad pedida. Si no hay ninguno, la oferta es `not_offered`, sin servicio ni nada más: la misma exista o no un servicio que el ámbito no muestra. Si hay varios, el primero compatible, si no el primero al que solo le falta información, si no el primero, en el orden del inventario. Cada requisito da un resultado (`met`, `not_met` o `unknown`) y en qué se apoya (`declared` u `observed`):

| Requisito | Se cumple si |
|-|-|
| `input_media_type:<tipo>`, `output_media_type:<tipo>` | el servicio acepta, o produce, el tipo (`*/*` y `tipo/*` lo cubren); desconocido si no declara ningún tipo |
| `language`, `required_region` | el servicio lo declara; desconocido si no declara nada |
| `external_cloud_allowed: false` | el servicio declara `external_cloud_used = false`; con `true` no se cumple; sin declaración, desconocido |
| `mcp_integration:<id>` | la conexión es visible en el ámbito y una observación en vigor la da por operativa; con una en vigor que dice `failed`, no se cumple; visible sin observación en vigor, o no visible, exista o no, desconocido |
| `information_source:<id>`, `skill_package:<id>`, `plugin:<id>`, `model:<id>` | la entrada es visible (`declared`, u `observed` con una observación en vigor que dice `ok`); con una que dice `failed`, no se cumple; no visible, desconocido |
| `execution_external_cloud_allowed: false` | ningún modelo entre los componentes del servicio visibles en el ámbito se ejecuta en un proveedor externo, y el servicio declara que no usa nube externa |
| `min_offered_ram_mib`, `min_offered_vram_mib`, `accelerator:<nombre>` | contra lo que ofrece el hardware (`offered`), nunca contra lo instalado (§8, §9.6); desconocido si el ámbito no lo muestra o no ofrece eso |
| otra clave | nunca: desconocida, y si es obligatoria va en `unknown_requirements` (`<clave>`, o `inventory.<clave>` bajo `inventory.required`); bajo `preferred` se ignora |

Una observación está en vigor hasta su `expires_at` y 60 s más, según el reloj del propio agente. Lo desconocido o no demostrable nunca se da por cumplido (§7.2).

**Resultado.** La oferta es compatible si se cumple todo lo obligatorio: `available` si el worker tiene menos tareas en marcha de las que admite a la vez, `busy` si no. Si solo le falta información, `needs_information`. Si algo obligatorio no se cumple, `not_offered`, con el servicio y los resultados que dicen por qué. Los tiempos son los que declara el servicio, y si no declara ninguno no aparecen: nunca valen cero por desconocidos. `conditions` repite lo que declara el servicio sobre nube y región, y `authorization_required` dice si el grant presentado no deja enviar tareas.

**Oferta.** La firma el candidato para quien pregunta (`requester`) con el dominio `agentnet.offer/1\n`, de 16 KiB como mucho (si no cabe, se recortan por el final las limitaciones, después los resultados y después los requisitos desconocidos, y la última limitación es `truncated`). Nombra el servicio (`skill_id`), el ámbito, el hash de la card de ese ámbito y la revisión de su vista, que cambia con todo cambio de lo que muestra el ámbito, también con las fechas de una observación. Dura `[discovery] offer_valid_for_ms` (15000 por defecto, como mucho 300000) redondeado al segundo por arriba, con fechas de segundos enteros como los grants. `benchmark_refs` queda vacío hasta que el agente sirva informes de benchmark. Una oferta no reserva capacidad ni autoriza ejecución: la tarea que la nombra se vuelve a comprobar, grant incluido, al llegar (§7.2, §9.3).

**Aceptación.** Quien recibe una oferta la acepta solo si cabe en 16 KiB, está firmada por el par al que preguntó, es para él, es de su consulta y no ha caducado, con 60 s de desfase solo para rechazar. La guarda con la clave (proveedor, `offer_id`), porque dos proveedores pueden usar el mismo `offer_id`. Cada lado purga cada hora las suyas: el candidato, las que caducaron hace más de un día; quien pregunta, las caducadas.
