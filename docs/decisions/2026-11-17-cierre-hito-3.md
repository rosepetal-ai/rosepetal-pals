# Registro de decisión: cierre del hito 3

**Fecha:** 2026-11-17

**Quién decide:** el responsable del proyecto, el 2026-11-17. Pidió unir la PR y extender `AUTH-TLS-001` al hito 3.

**Afecta a:** [PR #4](https://github.com/rosepetal-ai/rosepetal-pals/pull/4) (hito 3).

Son las decisiones que el hito 3 dejó al responsable en su [conformidad](../conformance/hito-3/README.md), su [prueba de salida](../conformance/hito-3/exit-test.md) y la PR. Cada una dice:
- qué se decide;
- por qué;
- qué cuesta si es errónea;
- qué la haría cambiar.

## D1. `AUTH-TLS-001` se extiende al hito 3

**Decisión.** La aceptación del requisito `MANUAL` de nivel MUST `AUTH-TLS-001` del TCK, «tráfico cifrado en producción», vale también para el hito 3. Es la misma aceptación que la [D1 del cierre de los hitos 1 y 2](2026-11-16-cierre-hitos-1-y-2.md#d1-auth-tls-001-se-acepta).

**Por qué.** El hito 3 no cambia nada de aquello en lo que se basa esa aceptación:
- `agentnet-kad/0` va por el mismo endpoint de Iroh que el resto del tráfico entre pares, con QUIC y TLS 1.3 ([protocolo](../protocols/agentnet-kad-0.md)).
- `GET /dht` está en el gateway, que solo escucha en loopback.
- Un contacto sacado de la DHT solo se marca a través de un relay `https`.
- El TCK y el cliente de Python, repetidos en el hito 3, no cambian ningún requisito ([informe](../conformance/hito-3/README.md)).

**Coste si es errónea.** El mismo que en los hitos 1 y 2: un cliente A2A que exija `https://` también en loopback no podría usar el gateway. Se arreglaría con TLS opcional en el gateway, sin tocar el protocolo entre pares.

**Qué la haría cambiar.** Cualquiera de estas dos cosas:
- que el gateway, o el estado de la DHT que sirve, escuche fuera de loopback;
- que la DHT lleve tráfico fuera del endpoint de Iroh.

## D2. Cómo se une la PR #4

**Decisión.** La PR #4 se une con `dev` con un commit de merge, sin squash, como las PR #1 y #2 ([D5 del cierre anterior](2026-11-16-cierre-hitos-1-y-2.md#d5-cómo-se-unen-las-pr)).

**Por qué.** El registro de decisión del backend, la prueba de salida, la conformidad y la descripción de la PR citan sus commits por su hash. Un commit de merge los conserva.

**Coste si es errónea.** La historia de `dev` es larga. Se lee con `git log --first-parent dev`.

## Lo que el cierre cumple

Son los cinco pasos con los que la [hoja de ruta](../plans/2026-11-14-hoja-de-ruta-hitos-2-4.md#cómo-se-cierra-un-hito) cierra un hito:

1. **Conformidad sin regresión.** El TCK y el cliente de Python no cambian ningún requisito respecto al hito 2 ([informe](../conformance/hito-3/README.md)).
2. **Prueba de salida con varios nodos.** Pasaron los nueve casos en una sola ejecución, con un contenedor Docker por agente, dos operadores y dos redes, sobre el árbol que se une ([registro](../conformance/hito-3/exit-test.md)). Repetirla en máquinas físicas es un extra opcional, como en los hitos 1 y 2.
3. **Revisión de toda la rama.** Una revisión de toda la fase 2 dio cinco puntos importantes y ninguno crítico. Se corrigieron en una sola tanda, y una revisión posterior no encontró nada nuevo por encima de menor.
4. **Versiones fijadas y MSRV.** Las dependencias nuevas están fijadas con `=`, y la CI compila con Rust 1.91.
5. **Documentación y PR.** Están al día el README, `CLAUDE.md`, la hoja de ruta y la tabla de desviaciones del plan.

## Queda abierto

**Cómo reparten los huecos de candidatos los contactos y la DHT.** El plan sigue el §5.3: los contactos guardados van primero. Por eso, con `max_candidates` contactos o más, una búsqueda no consulta la DHT. El README lo dice entre las limitaciones conocidas del hito 3, con dos salidas:
- subir `[discovery] max_candidates`, hasta 32;
- usar `--no-dht` y `--peer` para limitar una búsqueda a los contactos a propósito.

Cambiar el reparto es una decisión de diseño del responsable. Hay dos opciones:
- reservar huecos a la DHT;
- relegar detrás de ella a los contactos cuyo anuncio conocido no ofrece la capacidad.

Si se cambia, se registrará aparte.

**Decidido después:** el responsable eligió reservar huecos a la DHT ([registro](2026-11-17-huecos-dht.md)).
