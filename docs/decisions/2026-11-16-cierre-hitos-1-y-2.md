# Registro de decisión: cierre de los hitos 1 y 2

**Fecha:** 2026-11-16 · **Quién decide:** el responsable del proyecto delegó estas decisiones el 2026-11-16, al pedir que se unieran las PR de los dos hitos. Las tomó el agente que los implementó, con esa delegación. · **Afecta a:** [PR #1](https://github.com/rosepetal-ai/rosepetal-pals/pull/1) (hito 1) y [PR #2](https://github.com/rosepetal-ai/rosepetal-pals/pull/2) (hito 2)

Son las decisiones que los dos hitos dejaron pendientes del responsable en su conformidad, sus pruebas de salida y la [hoja de ruta](../plans/2026-11-14-hoja-de-ruta-hitos-2-4.md). Cada una dice qué se decide, por qué, qué cuesta si es errónea y qué la haría cambiar.

## D1. `AUTH-TLS-001` se acepta

**Decisión.** Se acepta el requisito `MANUAL` de nivel MUST `AUTH-TLS-001` del TCK («tráfico cifrado en producción») tal como está en los informes de conformidad del [hito 1](../conformance/hito-1/README.md) y del [hito 2](../conformance/hito-2/README.md).

**Por qué.** Todo el tráfico que sale de la máquina va cifrado:
- entre pares, el binding A2A, `agentnet-discovery/0` y los blobs viajan por QUIC de Iroh, con TLS 1.3;
- los contactos A2A corrientes del hito 2 se alcanzan por `https://`; `http://` solo se admite hacia `127.0.0.1`, `[::1]` y `localhost`.

El gateway local habla HTTP sin TLS, pero solo escucha en loopback. TLS en loopback no protege de quien ya está en la máquina, que es el riesgo real del gateway. Ese riesgo es la falta de autenticación local, y ya figura en las limitaciones conocidas del README.

**Coste si es errónea.** Un cliente A2A que exija `https://` también en loopback no podría usar el gateway. Se arreglaría con TLS opcional en el gateway, sin tocar el protocolo entre pares.

**Qué la haría cambiar.** Que el gateway escuche fuera de loopback, o que un despliegue lo exponga con un proxy.

## D2. Las pruebas de salida con Docker cierran los hitos 1 y 2

**Decisión.** Las ejecuciones con Docker registradas en [`hito-1/exit-test.md`](../conformance/hito-1/exit-test.md) y [`hito-2/exit-test.md`](../conformance/hito-2/exit-test.md) bastan para cerrar los dos hitos. Repetirlas en máquinas físicas pasa a ser un extra opcional, como la hoja de ruta ya dice para los hitos 2 a 4. Los runbooks se mantienen para quien la haga.

**Por qué.** Los dos hitos pasaron todos sus casos con red real entre espacios de red distintos. El hito 1 los pasó con cortes de red reales con `iptables`, el camino por relay forzado y `claude -p` real. El hito 2 los pasó con un contenedor por agente, un servidor MCP real y un agente A2A de terceros. Lo que no cubren está dicho en cada registro: NAT de operador, redes físicas distintas, Wi-Fi o red móvil reales. Es un riesgo de alcanzabilidad, y el hito 3 lo vuelve a tocar, porque sus routers tienen que ser alcanzables (§6.3). Su prueba de salida ya está prevista en al menos dos redes (tarea 8 de la hoja de ruta).

**Coste si es errónea.** Un fallo de NAT o de relay en redes reales aparecería en el primer despliegue entre máquinas, no en una prueba.

**Qué la haría cambiar.** Un despliegue real entre máquinas que falle por alcanzabilidad. Entonces el runbook del hito 1 se ejecuta en dos máquinas antes de seguir.

## D3. Una búsqueda sin proyecto no envía ids de inventario

**Decisión.** Se mantiene lo implementado en el hito 2: quien busca solo envía los ids de su inventario a los candidatos de los que tiene un grant del proyecto. Si la búsqueda no nombra proyecto, no los envía a nadie, y un requisito de ids sin proyecto cuenta como desconocido para todos ([`inventory-v1.md`](../extensions/inventory-v1.md)).

**Por qué.** Es lo que piden el §7.1 y el §9.9 del spec: los ids del inventario son metadatos privados, y el grant del proyecto es lo que autoriza compartirlos.

**Coste si es errónea.** Casar ids privados obliga a nombrar un proyecto (`--project` en la CLI, `project` en `pals_discover`).

**Qué la haría cambiar.** Que el inventario marque ids como públicos. El hito 3 lo plantea al decidir qué se anuncia (H3-D4).

## D4. Plazo de 3 s por candidato con las conexiones sin abrir

**Decisión.** No se cambia. El plazo por candidato sigue incluyendo abrir la conexión, con el valor por defecto del §7.3 (3 s, configurable con `per_candidate_timeout_ms` hasta 10 s).

**Por qué.** Pasó una sola vez, en un ensayo con la carga del anfitrión en 32 ([observación 1 de la prueba de salida del hito 2](../conformance/hito-2/exit-test.md#observaciones)). No se reprodujo: cinco búsquedas con todas las conexiones por abrir tardaron de 375 a 478 ms. Además, la cobertura de la búsqueda ya dice `timeout` cuando un candidato agota su plazo. El tiempo hasta un candidato útil es una métrica del §6.3, y el hito 3 la mide con relay y en varias redes.

**Coste si es errónea.** En redes lentas, una primera búsqueda puede quedarse sin candidatos y haber que repetirla.

**Qué la haría cambiar.** Que las medidas del hito 3 muestren que la primera conexión suele pasar de 3 s. Entonces se descuenta del plazo del candidato.

## D5. Cómo se unen las PR

**Decisión.** La PR #1 se une con `dev` con un commit de merge, sin squash. La PR #2 cambia su base a `dev` y se une también con un commit de merge.

**Por qué.** El commit de merge conserva los commits de cada tarea y de cada revisión, que los registros de conformidad y las descripciones de las PR citan por su hash. Además, la PR #2, apilada sobre la #1, no tiene que reescribir nada: con squash o rebase, la #2 volvería a traer los commits del hito 1 con otros hashes.

**Coste si es errónea.** La historia de `dev` es larga. Se puede leer con `git log --first-parent dev`.

## Decisiones del hito 3

Las decisiones H3-D1 a H3-D5 de la [hoja de ruta](../plans/2026-11-14-hoja-de-ruta-hitos-2-4.md#decisiones-para-el-responsable) también quedan delegadas. Se toman y se registran en `docs/decisions/` cuando haya datos para ello: H3-D1, el backend, tras la comparación acotada de la tarea 1.
