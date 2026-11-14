# PALS: hoja de ruta de los hitos 2 a 4

**Fecha:** 2026-11-14 · **Spec:** [`docs/plans/initial.md`](initial.md) (revisión 3) · **Planes:** [hito 1](2026-11-14-hito-1-flujo-basico-a2a.md) (implementado) y [hito 2](2026-11-14-hito-2-consulta-de-servicios.md) (en curso)

Este documento no es un plan ejecutable. El spec cubre cuatro subsistemas que se construyen en orden y dice que «no se asignan tiempos de desarrollo o promesas de rendimiento sin construir y medir cada hito» (§14). Por eso cada hito recibe su propio plan paso a paso en `docs/plans/` cuando el anterior está construido, porque sus interfaces dependen de código que todavía no existe y, en el hito 3, de una comparación cuyo resultado no se conoce. Los hitos 1 y 2 ya tienen el suyo. El del hito 2 se escribió a petición del responsable con el hito 1 implementado y su prueba de salida todavía en curso.

Aquí queda fijado lo que ya se puede decidir: qué implica para cada hito el requisito del 2026-11-14 («completamente compatible con A2A en su intercambio final»), qué hereda del hito 1, el esquema de tareas, el criterio de salida del spec y las decisiones que hay que tomar antes de escribir cada plan.

## Regla común a los tres hitos

El intercambio final no cambia: sea cual sea el modo de encontrar o evaluar a un proveedor, la tarea se envía con los bindings estándar de A2A v1.0 del hito 1 y se repite el TCK antes de cerrar cada hito. Lo que estos hitos añaden ocurre antes de ese intercambio (descubrir, consultar, sincronizar catálogos) y puede usar protocolos propios, como prevé el spec en §3 y §10.

## Hito 2: consulta de servicios e inventario

**Plan paso a paso:** [`2026-11-14-hito-2-consulta-de-servicios.md`](2026-11-14-hito-2-consulta-de-servicios.md). El responsable pidió empezar este hito el 2026-11-14, con la prueba de salida del hito 1 todavía en marcha. El plan sigue el esquema de abajo con dos cambios:
- los esquemas del inventario van antes que la card extendida, que enumera sus servicios;
- los contactos A2A corrientes (H2-D3) son una tarea propia.

En total son 13 tareas.

**Entregable (spec §14):** Agent Cards, inventarios filtrados, informes existentes, `ServiceQuery` y `ServiceOffer`.

**Criterio de salida (spec §14):** elegir entre al menos tres candidatos; comprobar requisitos sobre integraciones y recursos; validar cards; no ejecutar trabajo al consultar.

**Hereda del hito 1:** `canonical_card` y `gateway_view` (se amplían), `TunnelServer` y el registro de protocolos del nodo (se añade un ALPN), `Store` (migración a esquema 2), `jcs` y los dominios de firma, `GuardedHandler` y la capa de autenticación.

**Consecuencias del requisito A2A:**

- La card ya se sirve en la ruta estándar desde el hito 1. El hito 2 añade la firma JWS de A2A §8.4, el hash canónico `agent_card_hash` y las extensiones.
- La card restringida se entrega con la operación estándar `GetExtendedAgentCard` y `capabilities.extendedAgentCard = true`, como pide el spec §9 («se aprovecharán los mecanismos de cards autenticadas de A2A cuando se expongan mediante una interfaz estándar»).
- El inventario se declara como extensión A2A en `capabilities.extensions`, con URI y versión propias (spec §9.1), y perfil `agentnet.inventory/1`.
- La oferta elegida se referencia en el mensaje de ejecución dentro de `message.metadata`, bajo la clave de la extensión, sin tocar los tipos estándar.

**Esquema de tareas:**

1. Firma y verificación JWS de la Agent Card; hash canónico; vector congelado; validación con herramientas A2A independientes (`a2a-inspector`, SDK de Python).
2. Registro de contacto firmado y registro de anuncio (secuencia, caducidad, hash de la card), con dominios de firma propios.
3. Card extendida por `GetExtendedAgentCard`, filtrada según el grant del solicitante.
4. Esquemas `agentnet.inventory/1`: bloques opcionales del spec §9.2, las cinco dimensiones del §9.3, JSON Schema y vectores.
5. Vistas por permisos (pública, proyecto autorizado, tarea autorizada) con firma, ámbito, secuencia y caducidad; caché separada por contexto de autorización.
6. Protocolo `agentnet-discovery/0` (spec §10): ALPN propio, framing protobuf con prefijo `u32` big-endian, negociación de versión y límites, manejador aislado del worker. Operaciones `GetInventory`, `GetBenchmarkReport`, `ServiceQuery`.
7. `ServiceQuery` y `ServiceOffer` con el significado de los ejemplos del spec §7.1, §7.2 y §9.9; matching sobre metadatos locales, sin arrancar LLM ni herramientas.
8. Informes de benchmark existentes o importados (spec §9.7): formato, procedencia y firma; «sin medición» en lugar de cero.
9. Selección en dos etapas (spec §8) y límites de la consulta.
10. Adaptador de inventario: configuración manual más una integración MCP real mediante `tools/list`.
11. Escenarios del spec §14 que tocan este hito y repetición del TCK.

**Límites que el spec fija y que el plan copiará tal cual:** 8 candidatos contactados por búsqueda, 3 consultas simultáneas, 3 segundos de espera por candidato, 15 segundos de presupuesto total, consulta de servicio hasta 8 KiB, oferta hasta 16 KiB, descriptor público hasta 32 KiB, reenvío por el receptor deshabilitado (§7.3); inventario con 64 KiB por página, hasta 100 entradas por página y 1 MiB acumulado por consulta (§10).

**Decisiones tomadas por el responsable (2026-11-14):**

| Id | Decisión | Opciones que había |
|-|-|-|
| H2-D1 | La card de un par se obtiene solo por las rutas A2A estándar ya tunelizadas en el hito 1 (`/.well-known/agent-card.json` y `GetExtendedAgentCard`). `agentnet-discovery/0` no tiene `GetAgentCard`. Es una desviación del spec §10, anotada en la tabla del plan | (a) Operación propia `GetAgentCard`, como dice el spec §10. (b) Solo las rutas estándar |
| H2-D2 | Firma JWS de la card con `EdDSA` y la clave Ed25519 del agente, si la verifican las herramientas independientes. La tarea 1 del plan lo comprueba con el SDK de Python 1.3.0 y con los demás verificadores existentes. Solo si no, una clave `ES256` aparte, vinculada a la del agente por una declaración firmada (rama de reserva de esa tarea). Al escribir el plan se verificó ya una firma `EdDSA` con los SDK oficiales de Python 1.3.0, Go v2.7.0 y JS 1.3.0; `a2a-inspector` no verifica firmas | (a) `EdDSA` con la clave del agente. (b) `ES256` aparte |
| H2-D3 | Los contactos que son agentes A2A corrientes, por URL `https://` y sin daemon PALS, entran en este hito en sentido saliente: este agente, su gateway y su outbox los llaman. Exponer PALS por HTTPS sigue fuera (ver «Candidatos sin hito asignado») | Incluirlo en este hito o dejarlo sin fecha |

## Hito 3: descubrimiento con Kademlia

**Entregable (spec §14):** comparación acotada del backend, registro de capacidades y resolución de contactos.

**Criterio de salida (spec §14):** justificar la implementación elegida; encontrar un proveedor sin contacto previo; funcionar sin el bootstrap original y caducar anuncios.

**Hereda de los hitos 1 y 2:** identidad del agente, registro de contacto firmado, Agent Card firmada, `ServiceQuery`, y el intercambio final sin cambios.

**Esquema de tareas:**

1. Comparación acotada (spec §6.1): `libp2p-kad` (0.49.0, con `libp2p` 0.57.1, a fecha de hoy) frente a la alternativa más viable del ecosistema Mainline de Iroh. Se construyen solo las pruebas que resuelvan dudas concretas y se miden memoria, conexiones abiertas, tráfico en reposo, tiempo hasta un candidato útil, recuperación tras cambios de red y trabajo de mantenimiento (spec §6.3). El resultado es un registro de decisión en `docs/decisions/`. **El resto del plan se escribe después de esta tarea.**
2. Vocabulario `agentnet:capability:v1:*` (spec §5.1), su codificación como clave del backend elegido, alias locales.
3. Vínculo de identidades: registro de contacto firmado por la clave Iroh y por el PeerId, recuperable como valor firmado bajo una clave derivada del PeerId (spec §6.2).
4. Red propia `/agentnet/kad/1.0.0` con validadores de registros; modos router y cliente (spec §6.3).
5. Anuncio, renovación y caducidad de proveedores; límites por nodo (spec §5.5).
6. Entrada a la red: varios puntos de arranque, contactos guardados e invitaciones.
7. Búsqueda y filtrado en los seis pasos del spec §5.3, con cobertura parcial declarada.
8. Escenarios del spec §14 propios de este hito, con varios routers y clientes en al menos dos redes; métricas del spec §14.

**Gate:** si la comparación de la tarea 1 no favorece a `libp2p-kad`, el cambio de backend es una decisión explícita del responsable (spec §6.1); no se sustituye solo por reducir dependencias.

## Hito 4: colaboración privada con P2panda

**Condición de entrada (spec §11 y §14):** que exista un caso real en el que el catálogo o el registro local resulten insuficientes, es decir, que varios nodos autorizados necesiten una copia sincronizada del catálogo privado de un proyecto sin estar conectados a la vez. Sin ese caso, este hito no se planifica.

**Criterio de salida (spec §14):** sincronizar un catálogo privado sin publicarlo; controlar altas, bajas y permisos bajo desconexiones.

**Esquema de la evaluación:**

1. Prueba acotada de `p2panda-core` y `p2panda-sync` (0.8.1 a fecha de hoy; sus API pueden cambiar) para replicar cambios de catálogo.
2. Integración de `p2panda-store` con el SQLite existente; se sincroniza solo lo que deba compartirse (spec §11.5).
3. `p2panda-auth` únicamente si varios administradores gestionan el grupo; `p2panda-discovery` con temas aleatorios, nunca derivados de nombres adivinables (spec §11.3).
4. Regla de permisos del spec §11.4: membresía válida cuando corresponda, grant de ejecución válido y política local compatible. La membresía no concede ejecución.
5. Escenario del spec §14: un nodo vuelve tras una revocación y no obtiene permisos por sincronizar estado antiguo.

## Candidatos sin hito asignado

Quedan anotados para no perderlos; ninguno está comprometido.

- Exponer el agente directamente por HTTPS a clientes A2A sin daemon PALS. Requiere diseñar una prueba de posesión de la clave del sujeto del grant.
- Notificaciones push de A2A y binding gRPC.
- Adaptador que ponga un agente A2A ya existente detrás de un daemon PALS.
- Lo que el spec deja fuera en §15: pagos, reputación global, subdelegación de permisos, blockchain.
