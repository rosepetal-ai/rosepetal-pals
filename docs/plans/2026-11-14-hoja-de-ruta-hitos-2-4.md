# PALS: hoja de ruta de los hitos 2 a 4

**Fecha:** 2026-11-14 · **Spec:** [`docs/plans/initial.md`](initial.md) (revisión 3) · **Planes:** [hito 1](2026-11-14-hito-1-flujo-basico-a2a.md) (implementado), [hito 2](2026-11-14-hito-2-consulta-de-servicios.md) (implementado) y [hito 3](2026-11-16-hito-3-descubrimiento-kademlia.md) (implementado)

Este documento no es un plan ejecutable. El spec cubre cuatro subsistemas que se construyen en orden y dice que «no se asignan tiempos de desarrollo o promesas de rendimiento sin construir y medir cada hito» (§14). Por eso cada hito recibe su propio plan paso a paso en `docs/plans/` cuando el anterior está construido, porque sus interfaces dependen de código que todavía no existe y, en el hito 3, de una comparación cuyo resultado no se conocía. Los hitos 1, 2 y 3 ya tienen el suyo; el del hito 2 se escribió, a petición del responsable, antes de cerrar el hito 1, y el del hito 3 tiene dos fases: la comparación y, después, el daemon con el backend elegido.

Aquí queda fijado lo que ya se puede decidir: qué implica para cada hito el requisito del 2026-11-14 («completamente compatible con A2A en su intercambio final»), qué hereda, sus tareas, su criterio de salida y las decisiones pendientes.

## Estado

| Hito | Estado | Plan | PR | Evidencia de salida |
|-|-|-|-|-|
| 1. Flujo básico | Cerrado y unido con `dev`. Prueba de salida: 8 de 8 casos con un contenedor Docker en el papel de la segunda máquina, que cierra el hito; `AUTH-TLS-001` aceptado ([registro de decisión](../decisions/2026-11-16-cierre-hitos-1-y-2.md)) | [Hito 1](2026-11-14-hito-1-flujo-basico-a2a.md) | [#1](https://github.com/rosepetal-ai/rosepetal-pals/pull/1), unida con `dev` | [Conformidad](../conformance/hito-1/README.md), [prueba de salida](../conformance/hito-1/exit-test.md), arnés [`tests/exit/`](../../tests/exit/README.md) |
| 2. Consulta de servicios e inventario | Cerrado y unido con `dev`: las 13 tareas del plan. Prueba de salida: 8 de 8 casos con un contenedor Docker por agente, en una sola máquina, que cierra el hito, y el caso 9, la conformidad repetida, sin regresión ([registro de decisión](../decisions/2026-11-16-cierre-hitos-1-y-2.md)) | [Hito 2](2026-11-14-hito-2-consulta-de-servicios.md) | [#2](https://github.com/rosepetal-ai/rosepetal-pals/pull/2), unida con `dev` | [Conformidad](../conformance/hito-2/README.md), [prueba de salida](../conformance/hito-2/exit-test.md), [firmas de la card](../conformance/hito-2/card-signatures.md), arnés [`tests/exit/hito-2.sh`](../../tests/exit/README.md#prueba-de-salida-del-hito-2-con-docker) |
| 3. Descubrimiento con Kademlia | Cerrado y unido con `dev` ([registro de decisión](../decisions/2026-11-17-cierre-hito-3.md): `AUTH-TLS-001` se extiende al hito 3). La fase 1 midió tres backends frente a una línea base, evaluó un cuarto sobre el papel y eligió `libp2p-kad` sobre las conexiones Iroh del daemon, ratificado con condiciones (H3-D1, [registro de decisión](../decisions/2026-11-16-hito-3-backend-dht.md)); H3-D2 a H3-D5, [decididas](../decisions/2026-11-16-hito-3-decisiones.md). La fase 2 lo lleva al daemon: la DHT `agentnet-kad/0`, apagada por defecto, como fuente de la búsqueda de servicios. Prueba de salida: los 9 casos superados con un contenedor Docker por agente, dos operadores y dos redes, en una sola máquina; conformidad repetida sin que cambie ningún requisito | [Hito 3, fases 1 y 2](2026-11-16-hito-3-descubrimiento-kademlia.md) | [#4](https://github.com/rosepetal-ai/rosepetal-pals/pull/4), unida con `dev` | [Conformidad](../conformance/hito-3/README.md), [prueba de salida](../conformance/hito-3/exit-test.md), [runbook](../runbooks/hito-3-descubrimiento-kademlia.md), arnés [`tests/exit/hito-3.sh`](../../tests/exit/README.md#prueba-de-salida-del-hito-3-con-docker) |
| 4. Colaboración privada con P2panda | Solo si aparece un caso real; sin plan | — | — | — |

## Regla común a los tres hitos

El intercambio final no cambia: sea cual sea el modo de encontrar o evaluar a un proveedor, la tarea se envía con los bindings estándar de A2A v1.0 del hito 1 y se repite el TCK antes de cerrar cada hito. Lo que estos hitos añaden ocurre antes de ese intercambio (descubrir, consultar, sincronizar catálogos) y puede usar protocolos propios, como prevé el spec en §3 y §10.

## Cómo se cierra un hito

Como se cerró el hito 1, en la última tarea de cada plan:

1. **Conformidad sin regresión.** Se repiten el TCK, en su commit fijado, y el cliente del SDK oficial de Python, con los informes en `docs/conformance/hito-N/` (la variable `PALS_CONFORMANCE_DIR` de `run_tck.sh` elige ese directorio desde el hito 2). Ningún MUST fallido en `jsonrpc` ni en `http_json`, y cada diferencia con el hito anterior se explica requisito a requisito.
2. **Prueba de salida con varios nodos.** Un runbook en `docs/runbooks/` con los casos del criterio de salida y los escenarios del §14 del hito, ejecutado con el arnés Docker de `tests/exit/`, ampliado a los nodos y redes que haga falta. Se registra en `docs/conformance/hito-N/exit-test.md` con las métricas del §14 y lo que no cubre. Un caso que falla se corrige y se repite. Repetirla en máquinas físicas en redes distintas es un extra opcional, como en el hito 1.
3. **Revisión de toda la rama**, además de la de cada tarea, con sus correcciones, hasta que no quede nada crítico ni importante.
4. **Versiones fijadas y MSRV.** Dependencias nuevas con `=` en `Cargo.toml` y el `Cargo.lock` versionado, comprobadas al escribir el plan; nunca `latest` (§14). Todo compila con la MSRV, Rust 1.91, que la CI comprueba aparte; si una dependencia exige otra, se anota como desviación, como iroh en el hito 1. Subir una versión fijada repite la conformidad.
5. **Documentación y PR.** Se actualizan el README, `CLAUDE.md` y esta hoja de ruta; las desviaciones del spec quedan en la tabla del plan. La PR va en borrador contra `dev`, o apilada sobre la del hito anterior mientras esta no se una; unirla es decisión del responsable.

## Hito 2: consulta de servicios e inventario

**Plan paso a paso:** [`2026-11-14-hito-2-consulta-de-servicios.md`](2026-11-14-hito-2-consulta-de-servicios.md). Manda sobre este resumen.

**Objetivo:** antes de delegar, un agente elige entre varios candidatos: valida sus Agent Cards firmadas, lee sus inventarios según sus permisos y sus informes de benchmark existentes, y les pregunta con `ServiceQuery` si pueden atender su caso, sin que ninguna consulta ejecute trabajo.

**Entregable (spec §14):** Agent Cards, inventarios filtrados, informes existentes, `ServiceQuery` y `ServiceOffer`.

**Criterio de salida (spec §14):** elegir entre al menos tres candidatos; comprobar requisitos sobre integraciones y recursos; validar cards; no ejecutar trabajo al consultar.

**Hereda del hito 1:** `canonical_card` y `gateway_view` (se amplían), `TunnelServer` y el registro de protocolos del nodo (se añade un ALPN), `Store` (migración a esquema 2), `jcs` y los dominios de firma, `GuardedHandler` y la capa de autenticación.

**Consecuencias del requisito A2A:** la card del hito 1 gana la firma JWS de A2A §8.4; la restringida llega por la operación estándar `GetExtendedAgentCard` (§9); el inventario es una extensión A2A declarada en la card (§9.1); la oferta elegida va en `message.metadata`, sin tocar los tipos estándar.

**Decisiones tomadas por el responsable (2026-11-14):**

| Id | Decisión | Opciones que había |
|-|-|-|
| H2-D1 | La card de un par se obtiene solo por las rutas A2A estándar ya tunelizadas en el hito 1 (`/.well-known/agent-card.json` y `GetExtendedAgentCard`). `agentnet-discovery/0` no tiene `GetAgentCard`. Es una desviación del spec §10, anotada en la tabla del plan | (a) Operación propia `GetAgentCard`, como dice el spec §10. (b) Solo las rutas estándar |
| H2-D2 | Firma JWS de la card con `EdDSA` y la clave Ed25519 del agente, si la verifican las herramientas independientes; si no, una clave `ES256` aparte. La tarea 1 registra que el vector congelado verifica con los SDK oficiales de Python, Go y JS, y mantiene `EdDSA`; la prueba de salida lo repite con la card en vivo de un par ([firmas de la card](../conformance/hito-2/card-signatures.md)) | (a) `EdDSA` con la clave del agente. (b) `ES256` aparte |
| H2-D3 | Los contactos que son agentes A2A corrientes, por URL `https://` y sin daemon PALS, entran en este hito en sentido saliente. Exponer PALS por HTTPS sigue fuera (ver «Candidatos sin hito asignado») | Incluirlo en este hito o dejarlo sin fecha |

**Tareas del plan**, en serie, todas implementadas:

1. Firma JWS `EdDSA` de la card, formas canónicas, hash y verificación con los SDK de Python, Go y JS.
2. Registros firmados de contacto y de anuncio; esquema 2 del store, con migración desde el 1.
3. Esquemas `agentnet.inventory/1`: bloques opcionales del §9.2, cinco dimensiones del §9.3, ámbitos de visibilidad.
4. Card firmada en el nodo, card extendida según el grant y cards de pares verificadas en el gateway.
5. Vistas del inventario por permisos, firmadas, con cursores y caché por contexto de autorización.
6. Protocolo `agentnet-discovery/0` y `GetInventory`, con una lógica de respuesta que no llega al worker.
7. `ServiceQuery` y `ServiceOffer`: matching sobre metadatos locales, sin LLM ni herramientas.
8. Informes de benchmark existentes o importados, con procedencia; «sin medición» en lugar de cero.
9. Contactos que son agentes A2A corrientes, en saliente (H2-D3).
10. Selección en dos etapas (§8), límites del §7.3 y oferta referenciada en el mensaje de ejecución.
11. Adaptador de inventario: configuración manual y una integración MCP real con `tools/list`.
12. Herramientas MCP `pals_discover` y `pals_inventory`, y `offer_id` en `pals_ask`.
13. Prueba de salida, conformidad en `docs/conformance/hito-2/` y documentación.

**Qué cambió el plan respecto al esquema anterior de esta hoja de ruta:**

- **Orden:** los esquemas del inventario (3) van antes que la card extendida (4), que enumera sus servicios.
- **Tareas propias** para los contactos A2A corrientes (9) y las herramientas MCP (12): 13 en lugar de 11.
- **Validación de cards** con los verificadores de los SDK de Python, Go y JS, no con `a2a-inspector`, que no verifica firmas.
- **Desviaciones del spec**, en la tabla del plan: sin `GetAgentCard` (H2-D1), lector de frames nuevo, JWS sin biblioteca JOSE, cards «normalizadas» para las tres formas canónicas en uso, vistas derivadas de los grants v1 y registros sin DHT hasta el hito 3.
- **Valores que el spec no fija**, como la validez de una oferta, las cuotas o la caducidad de vistas y registros, los fija el plan. Los límites del §7.3 y del §10 pasan tal cual.

## Hito 3: descubrimiento con Kademlia

**Plan paso a paso:** [`2026-11-16-hito-3-descubrimiento-kademlia.md`](2026-11-16-hito-3-descubrimiento-kademlia.md), en dos fases. Manda sobre este resumen, y su tabla «Cambios respecto al spec y a la hoja de ruta» dice en qué se aparta de él: por ejemplo, con `kad-iroh` el `PeerId` es la clave del agente, y la tarea 3, el vínculo de identidades, no aplica.

**Objetivo:** encontrar proveedores de una capacidad sin contacto previo ni directorio central (§1, §5), y seguir con la consulta y la selección del hito 2.

**Entregable (spec §14):** comparación acotada del backend, registro de capacidades y resolución de contactos.

**Criterio de salida (spec §14):** justificar la implementación elegida; encontrar un proveedor sin contacto previo; funcionar sin el bootstrap original y caducar anuncios.

**Lo que dejó preparado el hito 2**, según su plan, y hoy está en el daemon:

- `agentnet.contact/1` (endpoint Iroh, relay y direcciones) y `agentnet.announce/1` (hash de la card pública, hasta 32 capacidades públicas, ALPN de descubrimiento): firmados con dominio propio, con secuencia, caducidad de 24 h, renovación y detección de regresiones. En el hito 2 viajan en las respuestas de descubrimiento; el hito 3 los publica.
- La card firmada con `kid` igual al id del agente y su `agent_card_hash`: la verificación del paso 5 del §5.3.
- Las etiquetas `agentnet:capability:v1:<id>` en los skills de la card.
- `ServiceQuery`, la selección en dos etapas y los límites del §7.3. Su búsqueda parte de los contactos; la DHT añade otra fuente (pasos 3 y 4 del §5.3).

**Lo que faltaba**, antes de la comparación: el contacto del hito 2 lo firma solo la clave del agente. Si ganaba `libp2p-kad`, el vínculo con el PeerId del §6.2, firmado por las dos claves, pedía un esquema nuevo o un registro aparte, porque los `agentnet.*/1` están congelados. H3-D1 lo hizo innecesario: con `kad-iroh`, `libp2p-kad` va sobre las conexiones Iroh del daemon y el `PeerId` es la clave del agente, así que no hay nada que vincular, y el contacto del hito 2 se publica tal cual.

### Comparación del backend (tarea 1)

| Candidato (§6.1) | Lo que se evalúa | Coste o límite que señala el spec |
|-|-|-|
| Iroh e iroh-blobs más `libp2p-kad` (candidato inicial; 0.49.0, con `libp2p` 0.57.1, a fecha de hoy) | Un Kademlia existente junto a los blobs del hito 1 | Dos pilas de conectividad y dos identidades que vincular (§6.2) |
| Toda la red en libp2p | QUIC, Kademlia, relays y NAT en una pila | iroh-blobs no pasa a funcionar sobre libp2p automáticamente: replantear ficheros y transporte |
| Iroh y su ecosistema Mainline DHT | La resolución de direcciones con pkarr | No da por sí solo el índice de capacidades; su descubrimiento de contenido es experimental y reconoce límites de privacidad |

**Método.** Se contrasta el coste de añadir `libp2p-kad` con el de la alternativa más viable, revisando la compatibilidad (versiones fijadas, MSRV 1.91, iroh 1.4.1) y construyendo solo las pruebas que resuelvan dudas concretas (§6.1). Mismo montaje para todos, con las métricas del §6.3 (memoria, conexiones abiertas, tráfico en reposo, tiempo hasta un candidato útil, recuperación tras cambios de red, trabajo de mantenimiento) y las del §14 que dependen del backend (búsquedas con candidato, mensajes y bytes por búsqueda, anuncios caducados, coste de adaptadores, filtraciones de metadatos). Lo que no se mida se dice.

**Qué decide:**

- **Descarta a un candidato:** no poder cumplir el criterio de salida; exigir reimplementar Kademlia (§6.3) o depender de que una DHT pública existente admita nuestros valores (§6.2); no poder validar registros propios ni el vínculo entre identidad de red y agente (§6.2, §13).
- **Pesan en contra:** tener que definir el índice de capacidades y sus mecanismos de anuncio, identificación y abuso, como con Mainline; cambiar el transporte de tareas o de ficheros, que además exige una decisión explícita (§6.1).
- **No basta para ganar:** reducir dependencias (§6.1). Entre los que pasan, deciden las medidas.

**Registro de decisión** en `docs/decisions/`: candidatos y versiones, pruebas y la duda de cada una, montaje, métricas y lo no medido, resultado justificado frente al criterio de salida y qué lo haría cambiar. Está en [`2026-11-16-hito-3-backend-dht.md`](../decisions/2026-11-16-hito-3-backend-dht.md), con la recomendación ratificada con condiciones (H3-D1).

**Gate:** si la comparación no favorece a `libp2p-kad`, el cambio de backend es una decisión explícita del responsable (spec §6.1); no se sustituye solo por reducir dependencias.

### Tareas previstas

Es el esquema de antes de la comparación. El plan lo convirtió en sus tareas 1 a 15, en dos fases. La tarea 1 de esta tabla, la comparación, es su fase 1 (tareas 1 a 6). Su tabla «Cómo se reparten las tareas de la hoja de ruta», en [«Cambios respecto al spec y a la hoja de ruta»](2026-11-16-hito-3-descubrimiento-kademlia.md#cambios-respecto-al-spec-y-a-la-hoja-de-ruta), dice adónde fueron en la fase 2 las tareas 2 a 9: la 3 no aplica.

| Tarea | Entregable | Parte del criterio |
|-|-|-|
| 1. Comparación acotada | Registro de decisión; el resto del plan se escribe después | Justificar la implementación |
| 2. Vocabulario `agentnet:capability:v1:*` publicado (§5.1), su clave en el backend y alias locales (§5.4) | Documento del vocabulario y vectores de clave | Encontrar un proveedor |
| 3. Vínculo de identidades (§6.2), si el backend lo pide: registro firmado por las dos claves, bajo una clave derivada del PeerId | Esquema y vectores congelados; un contacto con firma inválida se descarta | Encontrar un proveedor sin contacto previo |
| 4. Red propia `/agentnet/kad/1.0.0` con validadores; modos router y cliente (§6.3) | Validadores con pruebas de rechazo; configuración del modo | Funcionar sin el bootstrap original |
| 5. Anuncio, renovación y caducidad; límites por nodo (§5.5) | Pruebas con el reloj inyectable del hito 2 | Caducar anuncios |
| 6. Entrada: varios puntos de arranque, contactos guardados e invitaciones (§6.3) | Configuración e invitaciones | Funcionar sin el bootstrap original |
| 7. Búsqueda en los seis pasos del §5.3, como fuente de la selección del hito 2, con cobertura parcial declarada | La DHT como fuente de candidatos | Encontrar un proveedor sin contacto previo |
| 8. Escenarios y métricas del §14 con varios routers y clientes en al menos dos redes | Runbook y `exit-test.md` | Todo el criterio |
| 9. Cierre | «Cómo se cierra un hito» | — |

La tarea 8 cubre los escenarios del §14 de este hito: card sin vínculo válido, corte del bootstrap inicial, caída de routers, búsquedas maliciosamente amplias. Valida integración y fallos básicos, no resistencia a ataques ni escalabilidad mundial (§14).

### Riesgos

- **Dos pilas de red** con `libp2p-kad`: más conexiones, configuración y mantenimiento (§2.1, §6.1). `kad-iroh` lo evitó (H3-D1): una sola pila, la de Iroh, con un adaptador de libp2p sobre ella, cuyas pruebas son la puerta de cualquier cambio de versión de `libp2p` o de `iroh`.
- **Dependencias:** libp2p tiene que compilar con la MSRV 1.91 y convivir con las versiones fijadas. Comprobado: `libp2p =0.57.1` está fijada en el `Cargo.toml` raíz junto a las demás, y la CI compila el workspace con la MSRV (`cargo +1.91 check --workspace --all-targets --locked`).
- **Sybil:** las firmas no evitan identidades falsas en masa (§5.5, §13); la red abierta necesitará políticas de admisión.
- **Resultados sesgados o incompletos:** claves populares saturadas; una búsqueda vacía no demuestra que no haya proveedor (§5.3, §5.5).
- **Metadatos públicos:** lo anunciado es público, y hashear un nombre predecible no lo oculta (§11.1). Solo se anuncia lo público.
- **Alcanzabilidad:** los routers deben ser estables y alcanzables, y un nodo nuevo necesita un contacto alcanzable (§6.3); la prueba del hito 1 no cubrió NAT de operador ni redes físicas distintas.

### Decisiones para el responsable

El responsable las delegó el 2026-11-16 ([registro de decisión](../decisions/2026-11-16-cierre-hitos-1-y-2.md#decisiones-del-hito-3)): se toman con los datos de cada tarea y se registran en `docs/decisions/`. H3-D1 está en el [registro del backend](../decisions/2026-11-16-hito-3-backend-dht.md#decisión), y H3-D2 a H3-D5, en el [de las decisiones del hito 3](../decisions/2026-11-16-hito-3-decisiones.md).

| Id | Decisión | Recomendación |
|-|-|-|
| H3-D1 | Backend, tras la comparación | La provisional del spec, Iroh e iroh-blobs más `libp2p-kad`, salvo un coste medido que la descarte; nunca solo por reducir dependencias (§6.1) |
| H3-D2 | Quién opera los puntos de arranque y los routers de la prueba | Al menos dos operadores, y una prueba sin el punto de arranque inicial (§6.3) |
| H3-D3 | Modo por defecto: router o cliente | Cliente; router por configuración en nodos estables y alcanzables (§6.3) |
| H3-D4 | Política por defecto de anuncio y búsqueda pública | El spec pide que exista (§5.3) y que nada privado llegue a la DHT (§5.2, §9.8), sin fijar el valor. Recomendación: anunciar solo lo que el inventario marca como público, que en el plan del hito 2 es privado por defecto |
| H3-D5 | Dónde se publica el vocabulario común y cómo se amplía | Sin recomendación: el §5.1 solo fija una definición publicada y espacios de nombres propios sin aprobación de cada alta |

## Hito 4: colaboración privada con P2panda

**Objetivo:** varios nodos autorizados de un proyecto mantienen una copia sincronizada de su catálogo privado, y de ciertos eventos, sin estar conectados a la vez y sin publicarlo (§11.1).

**Condición de entrada (spec §11 y §14):** que exista un caso real en el que el catálogo o el registro local resulten insuficientes. Sin ese caso, este hito no se planifica.

**Qué cuenta como caso real:** un proyecto concreto cuyos nodos autorizados necesitan su catálogo privado mientras quien lo mantiene está desconectado, donde no bastan los contactos, el catálogo local ni la consulta directa del hito 2 (que pide al proveedor conectado), y cuyas capacidades no pueden anunciarse en la DHT. El responsable deja escrito qué datos se comparten y entre qué nodos: eso, y nada más, se sincroniza (§11.5). Solo ocultar capacidades privadas no es un caso: basta con no anunciarlas (§9.8).

**Entregable (spec §14):** prueba acotada de P2panda, si hay una necesidad de replicación.

**Criterio de salida (spec §14):** sincronizar un catálogo privado sin publicarlo; controlar altas, bajas y permisos bajo desconexiones.

**Evaluación:**

| Paso | Entregable |
|-|-|
| 1. Prueba acotada de `p2panda-core` y `p2panda-sync` (0.8.1 a fecha de hoy; sus API pueden cambiar) | Registro de decisión: versiones, API usadas, compatibilidad con las versiones fijadas y la MSRV (§14) |
| 2. `p2panda-store` con el SQLite existente, solo con lo que deba compartirse; una opción es envolver el evento del hito 1, sin cambios, como payload (§11.5) | Migración y prueba de que las firmas originales siguen verificando |
| 3. `p2panda-auth` solo si varios administradores gestionan el grupo; `p2panda-discovery` con temas aleatorios, nunca derivados de nombres adivinables (§11.3) | Entrega y rotación de temas; trato de los expulsados |
| 4. Regla del §11.4: membresía válida cuando corresponda, grant de ejecución válido y política local compatible | Pruebas de que la membresía no concede ejecución |
| 5. Prueba de salida | Runbook y `exit-test.md` |

**Escenario de salida:** tres nodos autorizados de un proyecto; uno se desconecta y, mientras está fuera, otro añade y retira entradas del catálogo. Si sigue autorizado, al volver converge al catálogo actual. Si se le dio de baja o se revocó su grant mientras estaba fuera, al volver no obtiene permisos ni entradas nuevas por sincronizar estado antiguo (§14). En ningún caso aparece nada del catálogo en la DHT ni en las vistas públicas.

**Riesgos:** API que pueden cambiar (§11.1); la revocación converge, no es instantánea, así que mandan la caducidad y la frescura antes de efectos sensibles (§11.4, §13); sincronizar un evento no demuestra que sea cierto (§11.5).

## Candidatos sin hito asignado

Quedan anotados para no perderlos; ninguno está comprometido.

- Exponer el agente directamente por HTTPS a clientes A2A sin daemon PALS. Requiere diseñar una prueba de posesión de la clave del sujeto del grant.
- Notificaciones push de A2A y binding gRPC.
- Adaptador que ponga un agente A2A ya existente detrás de un daemon PALS.
- Lo que el spec deja fuera en §15: pagos, reputación global, subdelegación de permisos, blockchain.
- **Deuda técnica conocida:** los menores que las revisiones del hito 1 dejaron para después, en la descripción de la [PR #1](https://github.com/rosepetal-ai/rosepetal-pals/pull/1) («Menores revisados y aplazados»). Ninguno bloquea el hito 1. Del hito 3, los que dejaron sus revisiones y no se anotan en ningún otro sitio; ninguno lo bloquea:
  - **Routers caídos.** No hay espera progresiva para un router caído: `libp2p-kad` 0.49 lo sigue preguntando en cada búsqueda mientras su hueco no haga falta (ver la limitación del README).
  - **Ficheros grandes.** Dividir `crates/pals/src/discovery/search.rs` y `crates/pals/src/dht/swarm.rs`, que han crecido mucho.
  - **`timeout_at`.** La búsqueda en la DHT de la tarea 13 calcula su plazo a mano y lo pasa a la tarea del swarm, que la corta en sus temporizadores, en lugar de usar `tokio::time::timeout_at`.
  - **Duración de una consulta.** La §8 de `agentnet-kad-0.md` y la cabecera de `publish.rs` dicen que una consulta dura como mucho `query_timeout` (20 s), pero `put_record` y `start_providing` tienen dos fases y pueden tardar cerca del doble. Sigue acotada.
  - **Logs.** `key_text` (`dht/swarm.rs`) cuenta los caracteres ya escapados, así que una clave corta con escapes puede salir cortada con `…`.
  - **Prueba de salida.** El caso 4 lee `estados.jsonl` sin filtro de tiempo, así que repetirlo en el mismo `PALS_EXIT_DIR` mezcla ejecuciones. El caso 5 imprime las vueltas con error pero no falla por ellas.
  - **Prueba sensible a la carga.** `dht_without_routers_within_the_default_limits_still_ranks_the_contacts` puede fallar con el anfitrión muy cargado: falló una vez con una carga de unos 30/83/103.
