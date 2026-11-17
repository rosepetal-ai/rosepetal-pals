# Registro de decisión: huecos para la DHT entre los candidatos de una búsqueda

**Fecha:** 2026-11-17

**Quién decide:** el responsable del proyecto, el 2026-11-17. De las opciones que dejó abiertas el [cierre del hito 3](2026-11-17-cierre-hito-3.md#queda-abierto), eligió reservar huecos a la DHT. Descartó las otras dos: dejarlo como estaba, o relegar detrás de la DHT a los contactos cuyo anuncio conocido no ofrece la capacidad.

**Afecta a:** la búsqueda de servicios con la DHT encendida (`crates/pals/src/discovery/search.rs`, [protocolo, sección 9](../protocols/agentnet-kad-0.md#9-la-dht-como-fuente-de-la-búsqueda-de-servicios)) y la configuración `[discovery]`.

Como el cierre, dice:
- qué se decide;
- por qué;
- qué cuesta si es errónea;
- qué la haría cambiar.

## El problema

Una búsqueda tomaba sus candidatos en dos pasos:
1. todos los contactos guardados, ofrecieran la capacidad o no;
2. detrás, los proveedores que encontraba la DHT, hasta `[discovery] max_candidates` en total (8 por defecto, 32 como mucho).

Cuando los contactos llenaban `max_candidates` solos, la búsqueda no consultaba la DHT, y la cobertura decía `candidate limit`. Así, un agente con 8 contactos guardados no encontraba nunca un proveedor por la DHT.

## Decisión

Una búsqueda que consulta la DHT guarda `[discovery] dht_places` de sus `max_candidates` huecos para los proveedores que encuentra allí.

- **La clave.** `dht_places` es un entero de 0 a `max_candidates`, 2 por defecto. Una configuración con `dht_places` mayor que `max_candidates` se rechaza al leerla.
- **Cuándo.** Solo cuando la búsqueda consulta la DHT: el nodo la tiene encendida, y la búsqueda no nombra contactos (`peers`) ni dice `exclude_dht`. Si no, los contactos ocupan todos los huecos, como en el hito 2.
- **El reparto.** Sean `max = max_candidates`, `R = min(dht_places, max)` y `C` los contactos en su orden.
  - Los primeros `F = min(|C|, max − R)` contactos se evalúan siempre.
  - `D` son los proveedores que da la DHT y ocupan un hueco suyo: los primeros en llegar, como mucho `max − F`, sin el propio agente ni esos `F` contactos. Uno que no es contacto entra si sus registros pasan la comprobación (`record regression`). Un contacto posterior entra sin comprobar nada y se evalúa como el contacto que es, una sola vez: con su ticket y su grant, y `from_dht: false`.
  - Los demás contactos, en su orden y con los `F` primeros delante, ocupan `min(|C| − los de D, max − min(R, |D|))` huecos.

  La DHT tiene hasta `R` huecos siempre que tiene proveedores. Los que no llena vuelven a los contactos, y con pocos contactos ocupa más de `R`, como antes. Si la búsqueda en la DHT falla, se corta sin proveedores o no encuentra ninguno, los contactos ocupan todos los huecos.
- **La búsqueda en la DHT** se hace siempre que `R > 0`, aunque los contactos llenen `max_candidates`. Pide un proveedor más de los huecos que la DHT puede ocupar, `max − F + 1`, y deja fuera al propio agente y a los `F` primeros contactos. Si llega a ese límite, la cobertura dice `candidate limit`, como antes.
- **El orden** no cambia. Entre los candidatos, los contactos van primero y los proveedores de la DHT después, y el ranking tras evaluarlos es el mismo.

Con `dht_places = 0`, unos contactos que llenan todos los huecos dejan fuera a la DHT, como antes de esta decisión: la búsqueda no la consulta, con `dht_requests` 0 y `candidate limit`. El resto no vuelve del todo a como era: con menos contactos, la búsqueda en la DHT pide un proveedor más de los huecos que dejan, sin contar a los contactos, y un proveedor descartado por `record regression` devuelve su hueco al siguiente.

## Por qué reservar y no relegar

Relegar a los contactos cuyo anuncio no ofrece la capacidad exige saber qué ofrece cada contacto, y un anuncio solo lista las capacidades públicas (H3-D4). Un contacto que ofrece la capacidad en privado o para un proyecto quedaría detrás por error, y es justo el que este agente guardó para pedírsela. Reservar huecos no necesita saber nada de lo que ofrece un contacto: no depende de que este agente tenga su anuncio, ni de que esté al día.

Dejarlo como estaba obligaba a subir `max_candidates` con cada contacto nuevo para seguir viendo la DHT, y cada hueco más es un candidato más que consultar en cada búsqueda.

## Relación con el spec

El §5.3 del [diseño](../plans/initial.md) ordena la búsqueda:
- buscar primero en contactos, caché y catálogos privados (paso 2);
- consultar Kademlia (paso 3);
- obtener un conjunto limitado de proveedores (paso 4).

El [plan del hito 3](../plans/2026-11-16-hito-3-descubrimiento-kademlia.md) lo leyó como «los contactos ocupan los huecos primero». Esta decisión mantiene el orden de la búsqueda: los contactos se buscan primero y la DHT después, y en la lista de candidatos los contactos siguen delante. Solo cambia cómo se reparte el conjunto limitado del paso 4.

## Cómo se aplica

- **Dónde.**
  - `Searcher::run`, en `crates/pals/src/discovery/search.rs`, con `DiscoveryLimits::dht_places`.
  - `DiscoveryCfg::dht_places`, en `crates/pals/src/config.rs`.
  - `Dht::providers_except`: la búsqueda en la DHT que deja fuera a los agentes que ya conoce.
  - Las pruebas, en `crates/pals/tests/dht_search.rs`. La prueba de salida del hito 3 lo comprueba en el caso 2 ([registro](../conformance/hito-3/exit-test.md)).
- **`|D|` se cuenta después de comprobar los registros.** Los de cada proveedor que no es contacto pasan por `Store::peer_record_accept` en el orden en que llegaron, hasta llenar los huecos de la DHT. Uno que vuelve atrás se descarta y su hueco pasa al siguiente.
- **La búsqueda en la DHT deja fuera a los `F` primeros contactos.** Si los contara, unos pocos contactos que también anuncian la capacidad llenarían su límite, de `R + 1` cuando hay muchos contactos, y la DHT no traería a nadie nuevo: los contactos volverían a desplazar a la DHT, ahora dentro de la búsqueda en la DHT. Dejándolos fuera, el límite cuenta solo proveedores que pueden ocupar un hueco, y no se piden sus registros.
- **Un contacto posterior puede volver de la búsqueda en la DHT.** La revisión del cambio vio que, si la búsqueda en la DHT dejara fuera a todos los contactos, uno posterior que anuncia la capacidad no se evaluaría por ningún camino: el reparto lo dejaría fuera como contacto, y la DHT no lo daría como proveedor. Ahora ocupa un hueco de la DHT como cualquier proveedor, en el orden en que llegó, y se evalúa como el contacto que es. No pasa por `Store::peer_record_accept`, como ningún contacto: se marca en su ticket, no en lo que trae la DHT.
- **Sin la clave, ninguna configuración deja de valer.** Si `dht_places` no está y `max_candidates` es menor que 2, vale `max_candidates`. Así una configuración del hito 2 con `max_candidates = 1` sigue valiendo.

## Coste si es errónea

- **Menos contactos evaluados.** Con muchos contactos, una búsqueda evalúa `dht_places` contactos menos mientras la DHT da proveedores: con los valores por defecto, 6 de 8. Si los que se quedan fuera, los últimos en su orden salvo los que la DHT da, eran los que ofrecían la capacidad, la búsqueda los pierde, y la cobertura lo dice: `candidate limit`.
- **Más búsquedas en la DHT.** Cada búsqueda que puede consultar la DHT la consulta, aunque tenga muchos contactos. Los routers ven qué capacidad busca este agente (H3-D4), y la búsqueda espera a la DHT, como mucho `per_candidate_timeout`.

Con `dht_places = 0` vuelve, sin cambiar código, lo que más pesa: los contactos ocupan primero todos los huecos y, si los llenan, la DHT queda fuera. No vuelve todo: la búsqueda en la DHT sigue pidiendo un proveedor más de los huecos que dejan los contactos, y un proveedor descartado por `record regression` sigue devolviendo su hueco.

## Qué la haría cambiar

Cualquiera de estas cosas:
- que un agente pueda saber qué capacidades ofrece cada contacto, también las privadas y las de proyecto, por ejemplo guardando sus ofertas: entonces podría relegar solo a los que no la ofrecen;
- que, en el uso real, los contactos que la reserva deja fuera resulten ser los que ofrecían la capacidad;
- que la DHT aporte poco frente a lo que cuesta consultarla en cada búsqueda: entonces convendría `dht_places = 0` por defecto.
