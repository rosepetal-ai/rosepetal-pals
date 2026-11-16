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
| Informes de benchmark | `agentnet.benchmark-report/1`, dominio de firma `agentnet.benchmark/1\n`: [`inventory-v1/benchmark-report.schema.json`](inventory-v1/benchmark-report.schema.json), con el vector `crates/pals-core/tests/vectors/benchmark-v1.json` |

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
- **MCP.** `adapter` es `{transport = "stdio", command, args, env_passthrough}` (los nombres de las variables de entorno que recibe el proceso, nunca sus valores, cada uno `[A-Za-z_][A-Za-z0-9_]*`) o `{transport = "http", url}`, con una `url` `https://`, o `http://` solo a `127.0.0.1`, `[::1]` o `localhost`, sin credenciales. En TOML, `transport` va junto a sus claves en `[mcp.adapter]`. Como mucho 16 conexiones llevan adaptador; el daemon rechaza al arrancar el inventario que no cumple estas reglas. Los nombres de herramienta son de 1 a 128 caracteres `[A-Za-z0-9_.-]`, el juego que recomienda MCP; las anotaciones son declaraciones, no una política de seguridad.

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

Una observación lleva `source` (`{"kind": "adapter", "method": "mcp tools/list"}`, el único método de este hito, o `{"kind": "declared"}`), `status` (`ok`, `failed` o `unknown`), `observed_at`, `expires_at`, las herramientas ofrecidas que el servidor listó (con la descripción y el `title` de sus anotaciones recortados a 1 KiB) y, en `detail`, solo uno de estos códigos fijos: `timeout`, `unreachable`, `protocol error`, `too many tools` o `missing tools: <herramientas ofrecidas que faltan>`. Nunca el error en crudo, que puede nombrar una URL o una orden: ese va al log del daemon. Una observación solo nombra herramientas que su entrada ofrece, y solo vale para una entrada del inventario.

El adaptador de PALS observa cada conexión con adaptador al arrancar el daemon (en segundo plano, sin retrasar nada), cada `refresh_interval_s` y con `pals inventory refresh`; nunca para contestar una consulta. Por conexión, dentro de 10 s: `initialize` y `tools/list` página a página, nunca `tools/call`; se para pasadas 256 herramientas o 16 páginas (`too many tools`). Un servidor `stdio` arranca en su propio grupo de procesos, con un entorno que solo lleva `PATH`, `HOME`, `LANG` y las variables de `env_passthrough`, y al acabar la observación se mata el grupo entero. Cada observación vale dos veces `refresh_interval_s`. Si con ella la entrada no cabe en una página de 64 KiB de algún ámbito que la muestra (de los ámbitos que se calculan para toda entrada, sea cual sea su visibilidad, los que la muestran: el público, los de los proyectos que nombran la configuración y la visibilidad de la entrada, y el de proyecto y el de tarea de cualquier proyecto de grant, medidos con el nombre más largo, de 64 bytes, que JSON escriba con dos caracteres cada uno; estos dos últimos solo muestran una entrada pública), se recortan todas las descripciones y títulos observados a un mismo tope, el mayor que cabe; si no cabe ni sin ellos, la observación es `failed` con `too many tools` y sin herramientas. Las observaciones guardadas que valen al arrancar se recortan otra vez del mismo modo. Un cambio de observación cambia la revisión de las vistas que la muestran, y las ofertas hechas antes pasan a `offer_stale`.

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
- La sección `benchmarks` solo sale si se pide por su nombre: sin secciones, una página lleva las nueve del inventario. Lleva una referencia a cada [informe de benchmark](#informes-de-benchmark) que el ámbito ve, y por cada servicio, modelo o hardware que el ámbito muestra y del que no hay informes, `{"subject": …, "status": "no_measurement"}`, nunca un cero (§9.7).

**Destinatario y firma.** Una página de proyecto o de tarea lleva en `audience` a quien pregunta, y solo vale para él; una pública no lleva `audience`. La firma es Ed25519 del agente sobre `agentnet.inventory-view/1\n` seguido del JCS de la página, y la página firmada ocupa como mucho 64 KiB. Caduca 300 s después de emitirse, y quien la recibe admite 60 s de desfase de reloj, solo para rechazarla.

**Revisión.** `seq` es la revisión de la vista del ámbito, sobre todas sus secciones, `benchmarks` incluida. Solo sube cuando cambia lo que ese ámbito muestra, nunca vuelve atrás y sobrevive a los reinicios. Lo oculto no cuenta en nada que lleve una página: una vista pública no tiene totales, recuentos, hashes ni referencias de lo que deja fuera.

**Páginas.** Una página termina en el primer límite que alcanza: 64 KiB, 100 entradas (o las que se pidan, si son menos) o 1 MiB acumulado en la cadena de cursores. `truncated_by` dice cuál fue. Con `page_bytes` o `page_entries`, `next_cursor` lleva a la página siguiente; con `query_total`, la cadena termina ahí y no hay cursor. Una entrada que no cabe sola en una página no se corta: la petición falla.

**Cursores.** Quien recibe un cursor lo devuelve tal cual: puede leerlo, pero no alterarlo sin invalidarlo, porque lleva un MAC con una clave del agente. Solo vale para el mismo solicitante, ámbito, secciones y revisión. Si el contenido cambia, cambia la revisión y el cursor deja de valer. Cada arranque usa una clave nueva, así que los cursores mueren al reiniciar. Que un cursor no valga no dice por qué.

**Caché.** El agente guarda como mucho 256 páginas, por ámbito, destinatario, secciones, cursor, tamaño de página y revisión: una página privada nunca se sirve a otro solicitante.

`pals inventory show [--scope public|project:<p>|task:<p>] [--sections a,b]` imprime en JSON lo que vería un par de ese ámbito, renderizado en local desde el fichero: las entradas de su vista de una pieza, sin firma, sin `seq` y sin paginar.

## Consultas de servicio y ofertas

Antes de encargar nada, quien busca un servicio pregunta a cada candidato si puede atender su caso con una consulta de servicio (§7.1), y el candidato contesta con una oferta firmada (§7.2). Las dos viajan por [`agentnet-discovery/0`](../protocols/agentnet-discovery-0.md#servicequery). Contestar nunca ejecuta nada: el candidato compara la consulta con los metadatos de su inventario y con los contadores de su worker, sin arrancar un LLM, una herramienta ni un adaptador (§7.3).

**Consulta.** Lleva `query_id` (de 1 a 64 caracteres `[A-Za-z0-9_-]`), `capability` (con la forma de las capacidades del inventario), los tipos MIME de entrada y de salida (16 por lista como mucho), `requirements`, un `summary` de 1 KiB como mucho, `reply_timeout_ms` (más de 3000 cuenta como 3000), `requested_inventory_sections` (las secciones de la vista que piensa leer; la oferta no depende de ellas), `project` (un nombre de proyecto: ASCII imprimible sin espacios) y `execution_authorized`, que tiene que ser `false`: una consulta nunca autoriza ejecución. Viaja como su JCS, de 8 KiB como mucho, con una sola grafía, y su BLAKE3 (`query_hash`) la vincula con la oferta. Lo que no se pide se omite: un `inventory` que no pide nada (`{}`) se rechaza, porque sería otra grafía de no pedir nada del inventario. `requirements` lleva `language`, `external_cloud_allowed`, `required_region` e `inventory`, con `required` y `preferred`: ids de conexiones MCP, fuentes de información, skills instaladas, plugins y modelos (32 por lista como mucho), `execution_external_cloud_allowed`, `min_offered_ram_mib`, `min_offered_vram_mib` y `accelerators`. Lo que no está en `inventory.preferred` es obligatorio. Las claves desconocidas se admiten (32 por grupo como mucho, cada una de 1 a 255 caracteres ASCII imprimibles, porque la oferta repite su nombre) y nunca se dan por cumplidas. Quien pregunta solo envía ids de su inventario a candidatos con un grant del proyecto (§7.1, §9.9), y a ninguno si la búsqueda no nombra proyecto: un requisito de ids sin proyecto cuenta como desconocido para todos.

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

**Oferta.** La firma el candidato para quien pregunta (`requester`) con el dominio `agentnet.offer/1\n`, de 16 KiB como mucho (si no cabe, se recortan por el final las limitaciones, después los resultados y después los requisitos desconocidos, y la última limitación es `truncated`). Nombra el servicio (`skill_id`), el ámbito, el hash de la card de ese ámbito y la revisión de su vista, que cambia con todo cambio de lo que muestra el ámbito, también con las fechas de una observación. Dura `[discovery] offer_valid_for_ms` (15000 por defecto, como mucho 300000) redondeado al segundo por arriba, con fechas de segundos enteros como los grants. `benchmark_refs` lleva las referencias de los informes de benchmark de ese servicio que el ámbito ve, los medidos más recientemente primero, 16 como mucho; sin informes queda vacío. Una oferta no reserva capacidad ni autoriza ejecución: la tarea que la nombra se vuelve a comprobar, grant incluido, al llegar (§7.2, §9.3).

**Aceptación.** Quien recibe una oferta la acepta solo si cabe en 16 KiB, está firmada por el par al que preguntó, es para él, es de su consulta y no ha caducado, con 60 s de desfase solo para rechazar. La guarda con la clave (proveedor, `offer_id`), porque dos proveedores pueden usar el mismo `offer_id`. Cada lado purga cada hora las suyas: el candidato, las que caducaron hace más de un día; quien pregunta, las caducadas.

## Informes de benchmark

Un informe de benchmark dice qué se midió, cómo, cuándo y quién lo midió (§9.7), con el esquema `agentnet.benchmark-report/1` ([`inventory-v1/benchmark-report.schema.json`](inventory-v1/benchmark-report.schema.json)). Es independiente del inventario: el agente guarda los suyos y los que importa, y nunca mide nada para contestar. Una búsqueda solo lee informes que ya existen.

**Contenido.** `benchmark_id` (de 1 a 128 caracteres `[A-Za-z0-9._-]`); `subject`, con una sola clave: un servicio por su id (`{"service": "ocr-metal-v1"}`), un modelo o un perfil de hardware por su id de entrada del inventario (`{"model": …}`, `{"hardware": …}`) o una pareja de nodos distintos (`{"node_pair": {"a": …, "b": …}}`); `measurement_kind`: `declared_spec` (especificación declarada), `own_measurement` (medición propia de quien mide) o `peer_observation` (observación de otro peer); `procedure`, con el perfil, su versión, el runner, la configuración (un objeto, anidado 16 niveles como mucho) y un hash de las entradas; `environment` (revisiones del modelo y del runtime, hardware, carga, calentamiento, estado de caché, ruta `direct` o `relay` y el otro nodo), donde lo desconocido se omite; `measured_at`, `duration_ms`, `samples` (al menos 1 salvo en una especificación declarada) y `expires_at`; de 1 a 64 resultados, cada uno con métrica, unidad, estadístico, valor finito y, si se conocen, errores e incertidumbre; `provenance`, con quién midió (`measured_by`) y cómo (`method`), y `evidence`, un hash y una referencia a un blob con los resultados brutos. Lo ausente se omite: nunca es `null`, y nunca un cero en lugar de un dato desconocido.

**Perfiles.** Los seis que propone el diseño se aceptan por su nombre: `compute.inference`, `compute.llm`, `storage.io`, `network.rtt`, `network.transfer` y `service.task`. Uno propio va en el espacio de nombres de su autor (`example:bench`, nunca `agentnet:`).

**Firma.** La de quien midió: Ed25519 de la clave de `provenance.measured_by` sobre `agentnet.benchmark/1\n` seguido del JCS de `report`, en el sobre `{"report": …, "signature": …}`, de 64 KiB como mucho. Un informe firmado por otro se rechaza, también al importarlo. La firma dice quién emitió el informe y que no ha cambiado; no prueba que el hardware exista ni que la ejecución ocurriera como se afirma. Los resultados brutos voluminosos solo van como `evidence`: nadie los descarga al consultar.

**Clase de medición.** `measurement_kind` se conserva tal cual al importar, guardar y servir: una especificación declarada nunca se presenta como medición, ni en la vista ni en las ofertas.

**Visibilidad.** No forma parte del informe, porque quien importa no puede cambiar lo que firmó otro: el agente la guarda aparte, y por defecto es privada. Un ámbito ve un informe si su visibilidad lo muestra en él y si el ámbito muestra lo que mide (el servicio, el modelo o el hardware), o si mide una pareja de nodos: un informe público sobre un servicio privado no lo nombra en ninguna vista.

**Servirlo.** `GetBenchmarkReport` de [`agentnet-discovery/0`](../protocols/agentnet-discovery-0.md#getbenchmarkreport) devuelve un informe que el agente ya tiene, tal como lo firmó quien midió, si el ámbito lo ve. Si no existe o no lo ve, la respuesta es la misma, `NOT_FOUND`. La sección `benchmarks` de la vista lleva sus referencias, y las ofertas, las del servicio que ofrecen. Las tres aplican una misma regla, así que nunca discrepan: `GetBenchmarkReport` sirve exactamente los informes que la vista del ámbito lista.

**Una fila que no se puede leer** (un informe que ya no verifica o es de otro id, o una visibilidad que no se entiende, como los que puede dejar un disco dañado u otro programa) solo se oculta a sí misma: el agente lee primero la visibilidad, nunca verifica lo que el ámbito no ve, y se salta esa fila con un aviso en el registro que nombra su id. Las vistas y las ofertas siguen sirviéndose con los demás informes, y pedirla da el mismo `NOT_FOUND` que un informe que no existe.

**Comparabilidad.** Quien busca usa un informe solo si está en vigor según su propio reloj, sin desfase; es del perfil y la versión que pide; tiene la misma configuración, comparada por su JCS; en `compute.llm` y `compute.inference`, es del mismo modelo, con su tokenizador, que nombra `environment.model_revision` (quien busca dice cuál quiere en `model_revision`; sin él, ningún informe de esos perfiles vale, y en los demás perfiles se compara solo si lo dice); y, si lo pide, es de la misma ruta y de la misma pareja de nodos, en cualquier orden. Un informe que no dice su ruta, su pareja o su modelo no vale cuando se pide. Nunca se comparan perfiles, versiones, configuraciones ni modelos distintos, y los informes no se convierten en una puntuación universal.

`pals benchmark import <fichero> [--visibility public|project:<p>|task:<p>|private]` guarda un informe firmado, leído de un fichero JSON con cualquier grafía (lo firmado es su JCS): solo si lo firma quien lo midió. `--visibility` (por defecto `private`) fija la visibilidad guardada, nunca el informe. Importar otra vez el mismo informe solo cambia su visibilidad; otro informe con un `benchmark_id` ya guardado se rechaza. `pals benchmark ls` lista los guardados, uno por línea: id, clase, perfil y versión, sujeto, quien midió, `measured_at`, `expires_at` y visibilidad, separados por tabuladores; una fila que no se puede leer aparece como su id, `unreadable` y el motivo.

## Selección y mensaje de ejecución

Quien busca un proveedor (`pals discover`, `POST /discovery/search` del gateway) filtra en dos etapas (§8) entre sus contactos, o los que nombra, dentro de los límites de `[discovery]` (§7.3): 8 candidatos por búsqueda, 3 consultas a la vez para todo el daemon, 3 s por candidato y 15 s en total, con como mucho 4 búsquedas a la vez; más allá, contesta con lo que tiene y dice que la cobertura es parcial (`candidate limit`, `time budget`), también cuando un candidato agota su propio plazo sin acabar (`timeout`). Nada reenvía una consulta.

- **Etapa 1, la card.** La de un par PALS se verifica con su `AgentId` como `kid`, y también la extendida si quien busca tiene un grant del proyecto; el anuncio que llega con la oferta tiene que nombrar el hash de la card pública verificada, y la oferta el de una de las cards leídas. La de un agente A2A corriente solo sigue si su firma está verificada (`card not verified` si no; se le puede preguntar igual por su nombre), y nunca se le pide oferta: es un candidato solo de card, que se descarta si se exige inventario, región, nube, idioma o un requisito desconocido. La card tiene que tener un skill con la etiqueta de la capacidad cuyos modos, o los de la card si el skill no tiene, cubran los tipos MIME pedidos.
- **Consulta.** Se espera la respuesta el tiempo que le queda al candidato (3 s por defecto), y su `reply_timeout_ms` lo dice, 3000 como mucho. Solo a un candidato del que quien busca tiene un grant del proyecto (cualquier grant que sirva tareas si la búsqueda no nombra proyecto) le presenta ese grant, y le dice el proyecto y los ids de inventario de los requisitos, obligatorios y preferidos; a los demás no les presenta ningún grant ni les dice el proyecto ni los ids, y un id obligatorio cuenta como desconocido.
- **Etapa 2, la oferta.** Se descarta la que no es compatible y la que no dice que se cumple cada requisito obligatorio de la búsqueda: lo que no dice es desconocido, y lo desconocido nunca se da por cumplido. El resto se ordena: disponibles con la oferta en vigor, después solo card, después ocupados; dentro de cada grupo, más preferencias cumplidas, un informe de benchmark utilizable del perfil pedido (una medición antes que una especificación declarada), menor tiempo estimado (los desconocidos al final), más tareas completadas con ese par, y el id. Una restricción incumplida nunca se compensa con un tiempo mejor.

**Mensaje de ejecución.** La tarea va al elegido con los bindings estándar de A2A del hito 1, y su mensaje nombra la oferta en `message.metadata`, bajo la URI de esta extensión: `{"offerId": …, "agentCardHash": …, "queryId": …}`, con `A2A-Extensions` nombrando la extensión de idempotencia y esta. Va en la petición que guarda el outbox, así que cada reintento la repite, y cada intento presenta el grant que quien busca tiene para el ámbito en que se hizo la oferta, no el que elegiría el gateway. Quien busca no pregunta a un par PALS del que no tiene un grant que sirva tareas, y decide con su propio reloj, sin desfase, si una oferta ha caducado: entonces la renueva una vez antes de preguntar. Si un rechazo llega cuando el par ya tiene una tarea para la pregunta, no pregunta a otro: la tarea podría ejecutarse en los dos.

**Revalidación en el proveedor.** Una petición que activa esta extensión y nombra una oferta solo se ejecuta si la oferta sigue valiendo ahora: emitida por el proveedor a ese solicitante tal como se nombra (id, card y consulta) y guardada por él, que solo guarda las de un ámbito de tarea, no caducada según su propio reloj, hecha para el ámbito del grant con que llega la tarea, y con la card y la revisión actuales de ese ámbito. Si no, `InvalidParams` con el mensaje `offer_unknown`, `offer_expired` u `offer_stale`, sin crear tarea; quien busca renueva una vez la oferta caducada o desfasada, o pasa al siguiente. La autenticación del grant va antes: una oferta nunca sustituye al grant, y la capacidad se sigue comprobando al ejecutar. Con idempotencia, la comprobación va cuando la petición resulta nueva, y un rechazo libera su `messageId`; una petición repetida se une a su tarea como en el hito 1, aunque su oferta haya caducado después. Una petición que no activa la extensión se atiende como en el hito 1, diga lo que diga su `metadata`, y la respuesta solo nombra en `A2A-Extensions` las extensiones activadas.
