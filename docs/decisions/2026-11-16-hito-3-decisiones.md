# Registro de decisión: H3-D2 a H3-D5, las decisiones del hito 3 sobre operación, modo, política y vocabulario

**Fecha:** 2026-11-16 · **Quién decide:** el responsable del proyecto delegó estas decisiones el 2026-11-16 ([registro](2026-11-16-cierre-hitos-1-y-2.md#decisiones-del-hito-3)). Las toma el agente que dirige el hito 3, con esa delegación. · **Afecta a:** la fase 2 del [plan del hito 3](../plans/2026-11-16-hito-3-descubrimiento-kademlia.md).

Son las decisiones H3-D2 a H3-D5 de la [hoja de ruta](../plans/2026-11-14-hoja-de-ruta-hitos-2-4.md#decisiones-para-el-responsable). H3-D1, el backend, está en su propio [registro](2026-11-16-hito-3-backend-dht.md#decisión): `libp2p-kad` sobre las conexiones Iroh del daemon (`kad-iroh`). Cada decisión dice qué se decide, por qué, qué cuesta si es errónea y qué la haría cambiar.

## H3-D2. Quién opera los puntos de arranque y los routers de la prueba

**Decisión.** En la prueba de salida del hito 3 (tarea 8 de la hoja de ruta), el agente que implementa el hito opera todos los puntos de arranque y todos los routers. Lo hace en Docker y emula dos operadores, como la topología de la [fase 1](2026-11-16-hito-3-backend-dht.md#montaje):
- cada operador tiene sus propias identidades, su propia configuración y sus propios puntos de arranque;
- la prueba usa al menos dos redes;
- incluye una prueba sin el punto de arranque inicial (§6.3).

El binario no lleva puntos de arranque por defecto. Se entra en una red por configuración, por contactos guardados o por invitaciones (§6.3).

**Por qué.**
- Es el criterio de la decisión D2 del [cierre de los hitos 1 y 2](2026-11-16-cierre-hitos-1-y-2.md#d2-las-pruebas-de-salida-con-docker-cierran-los-hitos-1-y-2): una prueba con Docker cierra un hito, y repetirla en máquinas y redes físicas es un extra opcional.
- No hay todavía una red PALS ni operadores externos.
- Lo que el §6.3 pide demostrar es que nadie depende de un operador único ni del arranque inicial. Eso se prueba con identidades y configuraciones separadas aunque las opere la misma persona.

**Coste si es errónea.** La prueba no ve los fallos que solo aparecen entre administraciones distintas: relojes, versiones, políticas de red. Tampoco ve los de NAT de operador. Ese riesgo de alcanzabilidad ya está registrado.

**Qué la haría cambiar.** Un primer despliegue con un segundo operador real. En ese momento, la prueba se repite con él antes de anunciar la DHT como utilizable entre organizaciones.

## H3-D3. Modo por defecto: router o cliente

**Decisión.**
- **Por defecto, cliente.** Un nodo es router solo por configuración, en nodos estables y alcanzables directamente, sin relay.
- **El modo nunca es automático.** El daemon fija siempre el modo de `libp2p-kad`. Con el modo automático, `libp2p-kad` pasa a servidor a un nodo que tenga direcciones externas confirmadas ([registro del backend](2026-11-16-hito-3-backend-dht.md#lo-que-hereda-la-fase-2)).

**Por qué.**
- Es la recomendación de la hoja de ruta y del §6.3: los agentes de portátiles o de equipos intermitentes no tienen que ser routers.
- Las medidas de `kad-iroh` la apoyan (S6, p50): un router mueve 355 418 bytes por minuto en reposo, frente a 27 142 de un cliente.
- Un cliente solo marca hacia los routers. Por eso no necesita relay ni atravesar NAT para usar la DHT.

**Coste si es errónea.** Con pocos routers configurados, la DHT tiene poca capacidad y poca diversidad de rutas. Se compensa con más routers por configuración.

**Qué la haría cambiar.** Que en la prueba de salida los clientes no encuentren proveedores con los routers de un solo operador caídos. Entonces se revisa cuántos routers hacen falta, no el modo por defecto.

## H3-D4. Política por defecto de anuncio y de búsqueda pública

**Decisión.**

**Anuncio.** El daemon no anuncia nada en la DHT por defecto. Si el operador activa el anuncio, solo se anuncian las capacidades de los servicios del inventario con `visibility = "public"`. Son las mismas que ya llevan la card pública y el `agentnet.announce/1` del hito 2. Nada privado ni de proyecto llega nunca a la DHT (§5.2, §9.8).

**Búsqueda.**
- Si el operador configura la DHT, es una fuente más de la búsqueda de servicios (pasos 3 y 4 del §5.3). Va detrás de los contactos y la caché, y está activa por defecto.
- Una búsqueda puede excluirla.
- Lo que se envía a la DHT es solo la clave de la capacidad, que es pública. Las consultas que siguen a los candidatos que da la DHT siguen las reglas del hito 2. En particular, se aplica la decisión D3 del [cierre de los hitos 1 y 2](2026-11-16-cierre-hitos-1-y-2.md#d3-una-búsqueda-sin-proyecto-no-envía-ids-de-inventario): los ids del inventario solo van a los candidatos de los que hay un grant del proyecto.

**Por qué.**
- **Anuncio.** Es la recomendación de la hoja de ruta. Además, el inventario ya es privado por defecto ([`inventory-v1.md`](../extensions/inventory-v1.md#visibilidad-y-ámbitos)), así que anunciar lo público no añade una segunda lista que mantener.
- **Búsqueda.** Configurar la DHT ya es la decisión del operador de usarla. Pedir otra activación por búsqueda haría que un nodo con la DHT configurada no encontrara a nadie sin contacto previo, que es el objetivo del hito.

**Coste si es errónea.**
- Con `kad-iroh`, cada búsqueda en la DHT dice a los routers qué agente busca qué capacidad, porque la conexión va autenticada con la clave del agente ([registro del backend](2026-11-16-hito-3-backend-dht.md#qué-la-haría-cambiar)).
- Activar la búsqueda por defecto acepta esa filtración para todo nodo que configure la DHT. El README y la documentación de la configuración lo dirán.

**Qué la haría cambiar.** Un caso de uso en el que el operador necesite buscar sin identificarse ante los routers. Entonces la búsqueda pasa a hacerse con una identidad aparte para buscar: una clave Iroh efímera, sin publicar, en un segundo endpoint. El registro del backend ya prevé ese coste.

## H3-D5. Dónde se publica el vocabulario común y cómo se amplía

**Decisión.**

**Dónde.** El vocabulario común se publica en el repositorio de PALS como `docs/extensions/capabilities-v1.md`, junto a [`inventory-v1.md`](../extensions/inventory-v1.md). Lo escribe la fase 2.

Cada capacidad común lleva:
- su id, sin `:`, con la gramática del hito 2: de 1 a 128 caracteres `[a-z0-9._-]`;
- una definición;
- ejemplos;
- sus vectores de clave en la DHT.

**Cómo se amplía.**
- El vocabulario común se amplía con un cambio en ese documento, revisado como cualquier otro: que no se solape con una capacidad existente y que tenga definición y ejemplos.
- Las capacidades propias de un proveedor van en su espacio de nombres, `<espacio>:<id>`, con la gramática del hito 2, y no necesitan aprobación de nadie (§5.1).
- Los alias y las traducciones los resuelve el buscador del solicitante (§5.4), no el vocabulario.

**Por qué.**
- El §5.1 pide una definición publicada y espacios de nombres propios sin aprobación de cada alta.
- La gramática del hito 2 ya distingue los dos casos por el `:`.
- El repositorio es público y versionado, y los formatos de PALS ya se publican así (`inventory-v1.md`, `idempotency-v1.md`).

**Coste si es errónea.** Ampliar el vocabulario común depende de quien mantiene el repositorio. Mientras tanto, un proveedor puede anunciar en su espacio de nombres. Lo que se pierde es que lo encuentre quien solo busca la capacidad común.

**Qué la haría cambiar.** Que varias implementaciones independientes de la red necesiten ampliar el vocabulario sin pasar por este repositorio. Entonces haría falta un registro aparte, con su propio proceso.
