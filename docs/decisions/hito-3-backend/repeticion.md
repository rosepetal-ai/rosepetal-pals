# Repetición de las medidas del backend del hito 3

**Fecha:** 2026-11-16 · Tarea 7 de la [fase 2 del plan del hito 3](../../plans/2026-11-16-hito-3-descubrimiento-kademlia.md). Repite las medidas de la fase 1, que son las condiciones con que se ratificó H3-D1 ([registro del backend](../2026-11-16-hito-3-backend-dht.md#decisión), «Condiciones»). Va antes de congelar nada: ni el ALPN, ni las claves de la DHT, ni cómo sale el `PeerId`.

**Veredicto: sigue `kad-iroh`.** `repeat.py` sale con 0, y no se da ninguna de las dos condiciones de cambio:

1. **S3 y S7.** Ningún candidato queda en `fail` en S3 ni en S7, en ninguna de sus cuatro ejecuciones.
2. **Reposo con intervalos de producción.** Un router de `kad-iroh` envía y recibe 90 908 B/min (p50), y uno de `kad-libp2p`, 67 950. Es 1,34 veces, y la condición pide más del doble, 135 900. **Con una salvedad:** la medida de `kad-iroh` no marcó del todo una sola vez a cada par. Con `SPIKE_DIAL_CONCURRENCY=1` desaparecen las marcaciones dobles a la vez de la fase 1. Aun así, los routers registran 24 apretones de manos que el otro extremo abandona, 7 de ellos durante el reposo ([abajo](#marcando-una-sola-vez)). Por eso esta medida no cuenta como «marcando una sola vez». Lo que sí se puede decir: un apretón abandonado solo suma tráfico, así que marcando de verdad una sola vez la cifra no sería mayor. Es una inferencia, no una medida.

## Qué se ejecutó

- **Seis ejecuciones completas del arnés** de [`spikes/hito-3/`](../../../spikes/hito-3/README.md#ejecuciones-repetidas), alternando los candidatos para repartir la carga del anfitrión: `kad-iroh` y `kad-libp2p`, con las etiquetas 2, 3 y 4. Con las dos de la fase 1 ([`results/kad-iroh.json`](results/kad-iroh.json) y [`results/kad-libp2p.json`](results/kad-libp2p.json)), son cuatro por candidato. Usan el mismo `params.env` que la fase 1: `check_results.py --repeated` sobre las ocho sale con 0.
- **Dos medidas de S6 con intervalos de producción**, con [`params-production.env`](../../../spikes/hito-3/harness/params-production.env) y `--only S6`. Son `TTL_S` = 3600, `REPUBLISH_S` = 1200 e `IDLE_S` = 2400: el reposo cubre dos vueltas de republicación, y duró 2402 y 2404 s. La de `kad-iroh` corrió con `NODE_ENV=SPIKE_DIAL_CONCURRENCY=1`, y sus 12 nodos registran `dial_concurrency=Some(1)` al arrancar. `check_results.py` sobre las dos sale con 0.
- **Ninguna ejecución tuvo un escenario en `error`**, así que ninguna se repitió ni se descartó. Ningún asentamiento agotó su plazo.

```bash
bash spikes/hito-3/harness/harness.sh --tag 2 kad-iroh      # y --tag 2 kad-libp2p; luego 3 y 4
bash spikes/hito-3/harness/harness.sh --tag production \
    --params spikes/hito-3/harness/params-production.env --only S6 kad-libp2p
NODE_ENV=SPIKE_DIAL_CONCURRENCY=1 bash spikes/hito-3/harness/harness.sh --tag production-dial-once \
    --params spikes/hito-3/harness/params-production.env --only S6 kad-iroh
R=docs/decisions/hito-3-backend/results
python3 spikes/hito-3/harness/check_results.py --repeated $R/kad-{iroh,libp2p}.json $R/runs/kad-{iroh,libp2p}-{2,3,4}.json
python3 spikes/hito-3/harness/check_results.py $R/runs/kad-libp2p-production.json $R/runs/kad-iroh-production-dial-once.json
python3 spikes/hito-3/harness/repeat.py --runs $R/kad-{iroh,libp2p}.json $R/runs/kad-{iroh,libp2p}-{2,3,4}.json \
    --production $R/runs/kad-iroh-production-dial-once.json $R/runs/kad-libp2p-production.json
```

Los JSON nuevos están en [`results/runs/`](results/runs/). Los registros de cada ejecución (`spikes/hito-3/harness/logs/<id>-<etiqueta>/`) no van en git. Las tablas de [Estado](#estado-de-s0-a-s7-por-ejecución), [Variación](#variación-entre-ejecuciones), [S6 con intervalos de producción](#s6-con-intervalos-de-producción) y el texto de [Veredicto de `repeat.py`](#veredicto-de-repeatpy) son su salida, con las filas ordenadas por candidato y las medianas redondeadas a una décima.

### Lo medido es lo de la fase 1

Las ejecuciones nuevas son del commit `dada503`, el de las herramientas de esta tarea, sin cambios sin commit. Desde los commits de la fase 1 (`c954836` para `kad-iroh` y `420a7e1` para `kad-libp2p`), el código medido cambió así:

- **Dependencias:** ninguna. El `Cargo.lock` de `spikes/hito-3/` no añade ningún paquete ni cambia ninguna versión. Solo quita los que dejó de usar `mainline`.
- **`kad-libp2p`:** solo comentarios.
- **`kad-iroh`:**
  - el despertar del transporte que añadió `e17558f` ([`transport.rs`](../../../spikes/hito-3/kad-iroh/src/transport.rs)): `listen_on` y `remove_listener` despiertan a la tarea que espera, un camino que el arnés no recorre, según ese commit;
  - comentarios;
  - y la lectura de `SPIKE_DIAL_CONCURRENCY` de esta tarea. Sin la variable, el swarm se queda con el factor de marcado por defecto de libp2p, 8, como en la fase 1.

  Las seis ejecuciones completas corrieron sin la variable. Los routers de `kad-iroh-2` registran `dial_concurrency=None` al arrancar y dejan la misma huella de marcaciones dobles que en la fase 1: 77, 77, 95 y 56 líneas `aborted by peer` por router, frente a 79, 70, 91 y 56.
- **El arnés:** las opciones de esta tarea, de las que las ejecuciones completas solo usan `--tag`. Además, el JSON de `kad-libp2p` de la fase 1 se evaluó antes de que el arnés añadiera las métricas `connected_first_valid_backend_pct` (S2) y `degraded_first_valid_backend_pct` (S4), que no cambian ningún estado.

## Anfitrión, carga y fechas

El anfitrión es el de la fase 1: Ubuntu 24.04.4 LTS, kernel 6.12.74, 32 CPU y Docker 29.4.1, compartido con contenedores de otros. Cada nodo es un contenedor con `--cpus 1 --memory 512m`. La carga son las medias de 1, 5 y 15 minutos de `/proc/loadavg`. Al empezar, la que anota el arnés en su `harness.log`; al acabar, la del anfitrión cuando terminó el proceso.

| Ejecución | Commit | Inicio (UTC) | Fin (UTC) | Carga al empezar | Carga al acabar | Salida |
|-|-|-|-|-|-|-|
| `kad-libp2p` (fase 1) | `420a7e1` | 09:11:32 | 09:25:04 (JSON) | 22.60 15.78 10.34 | — | — |
| `kad-iroh` (fase 1) | `c954836` | 10:18:51 | 10:31:42 (JSON) | 25.01 23.32 32.45 | — | — |
| `kad-iroh-2` | `dada503` | 14:37:32 | 14:50:20 | 19.90 13.36 12.77 | 22.16 21.21 17.41 | 0 |
| `kad-libp2p-2` | `dada503` | 14:50:28 | 15:04:30 | 21.38 21.07 17.41 | 12.40 14.29 16.22 | 0 |
| `kad-iroh-3` | `dada503` | 15:04:37 | 15:21:49 | 12.77 14.33 16.23 | 16.93 15.22 15.19 | 0 |
| `kad-libp2p-3` | `dada503` | 15:21:56 | 15:34:55 | 15.86 15.05 15.13 | 15.08 13.50 14.06 | 0 |
| `kad-iroh-4` | `dada503` | 15:35:03 | 15:49:52 | 14.19 13.34 14.00 | 26.11 20.75 17.47 | 0 |
| `kad-libp2p-4` | `dada503` | 15:49:59 | 16:03:04 | 27.39 21.10 17.60 | 20.22 22.27 21.54 | 0 |
| `kad-libp2p-production` | `dada503` | 16:03:15 | 16:44:19 | 18.90 21.91 21.43 | 33.13 23.89 20.27 | 0 |
| `kad-iroh-production-dial-once` | `dada503` | 16:44:31 | 17:25:43 | 35.71 24.73 20.59 | 13.07 14.02 15.26 | 0 |

Todo es del 2026-11-16. De las dos ejecuciones de la fase 1 solo quedan el inicio y la carga de su `harness.log`, y la hora a la que se escribió su JSON (`date`).

## Estado de S0 a S7 por ejecución

| Ejecución | Backend | Commit | Fecha (UTC) | S0 | S1 | S2 | S3 | S4 | S5 | S6 | S7 |
|-|-|-|-|-|-|-|-|-|-|-|-|
| `kad-iroh` | `kad-iroh` | `c954836` | 2026-11-16T10:31:42Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-iroh-2` | `kad-iroh` | `dada503` | 2026-11-16T14:50:07Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-iroh-3` | `kad-iroh` | `dada503` | 2026-11-16T15:20:32Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-iroh-4` | `kad-iroh` | `dada503` | 2026-11-16T15:49:34Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-libp2p` | `kad-libp2p` | `420a7e1` | 2026-11-16T09:25:04Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-libp2p-2` | `kad-libp2p` | `dada503` | 2026-11-16T15:03:13Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-libp2p-3` | `kad-libp2p` | `dada503` | 2026-11-16T15:34:27Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |
| `kad-libp2p-4` | `kad-libp2p` | `dada503` | 2026-11-16T16:02:46Z | `pass` | `pass` | `pass` | `pass` | `pass` | `pass` | `measured` | `pass` |

## Variación entre ejecuciones

Con una cantidad par de ejecuciones, la mediana es la media de las dos centrales.

| Cifra | Backend | Con cifra | Mínimo | Mediana | Máximo |
|-|-|-|-|-|-|
| S1 `first_valid_ms_p50` | `kad-iroh` | 4 de 4 | 2 | 2 | 2 |
| S1 `first_valid_ms_p50` | `kad-libp2p` | 4 de 4 | 2 | 2 | 2 |
| S3 `routers_forget_s` | `kad-iroh` | 4 de 4 | 98.9 | 103.0 | 115.7 |
| S3 `routers_forget_s` | `kad-libp2p` | 4 de 4 | 103.6 | 111.5 | 114.6 |
| S7 `resolved_s_max` | `kad-iroh` | 4 de 4 | 0.8 | 1.9 | 5.9 |
| S7 `resolved_s_max` | `kad-libp2p` | 4 de 4 | 1.4 | 4.0 | 13 |
| S6 `router_bytes_per_min_p50` | `kad-iroh` | 4 de 4 | 339342 | 365129.5 | 385609 |
| S6 `router_bytes_per_min_p50` | `kad-libp2p` | 4 de 4 | 233987 | 246889 | 250338 |

Lo que se ve, de los mismos JSON:

- **S3.** Los límites son 160 s (`TTL_S + REPUBLISH_S`), y los dos quedan muy por debajo. Antes de parar a `p1`, los routers guardaban 12 registros suyos en las ocho ejecuciones salvo en la de `kad-libp2p` de la fase 1, que tenía 16 (`stored_before`). El registro del backend atribuía los 12 de `kad-iroh` a un router vuelto a arrancar que tardó en recibirlos ([Lo no medido](../2026-11-16-hito-3-backend-dht.md#lo-no-medido)). Repetido, `kad-libp2p` también da 12: la diferencia no era de `kad-iroh`.
- **S7.** El límite es 80 s. Los unos 34 s de la ejecución de prueba de la fase 1 no reaparecen. El máximo de `kad-iroh` es 5,9 s, y el de `kad-libp2p`, 13,0 s (`kad-libp2p-4`, con un p50 de 12,8 s). S7 varía en los dos candidatos, no solo en `kad-iroh`.
- **S6 con los intervalos de la prueba** (`REPUBLISH_S` = 40 s, el factor de marcado por defecto): un router de `kad-iroh` envía y recibe entre 339 342 y 385 609 B/min, y uno de `kad-libp2p`, entre 233 987 y 250 338. Las medianas están a 1,48 veces.

## S6 con intervalos de producción

TTL_S = 3600, REPUBLISH_S = 1200, IDLE_S = 2400. `kad-iroh-production-dial-once`: commit `dada503`, 2026-11-16T17:25:25Z; `kad-libp2p-production`: commit `dada503`, 2026-11-16T16:44:05Z.

| S6, papel y métrica | `kad-iroh` (`kad-iroh-production-dial-once`) | `kad-libp2p` (`kad-libp2p-production`) |
|-|-|-|
| `router_rss_kib_p50` | 21028 | 22520 |
| `router_rss_kib_max` | 23532 | 22952 |
| `router_connections_p50` | 0 | 0 |
| `router_connections_max` | 4 | 6 |
| `router_routing_peers_p50` | 3 | 3 |
| `router_routing_peers_max` | 3 | 3 |
| `router_bytes_per_min_p50` | 90908 | 67950 |
| `router_bytes_per_min_max` | 102127 | 69404 |
| `router_sent_bytes_per_min_p50` | 45576 | 34274 |
| `router_sent_bytes_per_min_max` | 51083 | 35141 |
| `provider_rss_kib_p50` | 19860 | 22368 |
| `provider_rss_kib_max` | 21900 | 24744 |
| `provider_connections_p50` | 0 | 0 |
| `provider_connections_max` | 4 | 0 |
| `provider_routing_peers_p50` | 4 | 4 |
| `provider_routing_peers_max` | 4 | 4 |
| `provider_bytes_per_min_p50` | 29002 | 19215 |
| `provider_bytes_per_min_max` | 30530 | 22175 |
| `provider_sent_bytes_per_min_p50` | 14055 | 9506 |
| `provider_sent_bytes_per_min_max` | 15193 | 10317 |
| `client_rss_kib_p50` | 19864 | 22248 |
| `client_rss_kib_max` | 22012 | 23976 |
| `client_connections_p50` | 0 | 0 |
| `client_connections_max` | 0 | 0 |
| `client_routing_peers_p50` | 4 | 4 |
| `client_routing_peers_max` | 4 | 4 |
| `client_bytes_per_min_p50` | 23248 | 18319 |
| `client_bytes_per_min_max` | 24277 | 20478 |
| `client_sent_bytes_per_min_p50` | 11737 | 9023 |
| `client_sent_bytes_per_min_max` | 12153 | 9608 |

Con los intervalos de producción, el tráfico en reposo de un router baja a una cuarta parte, más o menos, en los dos candidatos: de una mediana de 365 130 a 90 908 B/min en `kad-iroh` y de 246 889 a 67 950 en `kad-libp2p`. La distancia entre ellos baja de 1,48 a 1,34 veces, pero son dos cambios a la vez (intervalos y marcado) y una sola ejecución por candidato. Tampoco es la misma ventana: con los intervalos de la prueba, el reposo dura 300 s (`IDLE_S` de `params.env`) y S6 llega tras S0 a S5 en la misma ejecución; con los de producción, dura 2400 s y S6 corre sola (`--only S6`). La memoria (p50) sigue a favor de `kad-iroh` en los tres papeles, como en la fase 1.

### Marcando una sola vez

El plan lo comprueba en los registros de los routers de `kad-iroh`: no debe aparecer ningún `aborted by peer`, la huella de las marcaciones dobles. **Aparecen 24** (4, 5, 5 y 10 en `r1` a `r4`), así que la comprobación falla y esta medida no cuenta como «marcando una sola vez». Pero no son la huella de la fase 1. Iroh registra dos mensajes distintos con `aborted by peer`:

| Ejecución | Factor de marcado | «Accepting incoming connection ended with error: aborted by peer» | «Ignoring connection: invalid handshake: aborted by peer» |
|-|-|-|-|
| `kad-iroh` (fase 1) | 8, por defecto | 295 | 1 |
| `kad-iroh-2` | 8, por defecto | 304 | 1 |
| `kad-iroh-3` | 8, por defecto | 342 | 1 |
| `kad-iroh-4` | 8, por defecto | 330 | 4 |
| `kad-iroh-production-dial-once` | 1 | **0** | **24** |

Son las líneas de los cuatro routers juntos, de toda la ejecución (los registros de los nodos, que git no guarda).

- **Las marcaciones dobles a la vez desaparecen.** La primera columna es la que el [documento de `kad-iroh`](kad-iroh.md#el-transporte) describe en la fase 1. libp2p marca las dos direcciones de un router a la vez, y abandona la segunda conexión a mitad del apretón de manos. Con el factor 1 no aparece ninguna.
- **Queda otra marcación abandonada.** Las 24 de la segunda columna llegan cada una 5,0 s después de que arranque un nodo, el plazo de marcado del transporte (`DIAL_TIMEOUT`). Por ejemplo, `p1` arranca a las 16:44:51,38 y `r2` y `r4` registran una a las 16:44:56,38. Los nodos de la red A las dejan en `r2` y `r4`, y los de la red B, en `r1` y `r3`. `p3` y `p4` dejan una en `r1` a los 5 s y otra en `r3` a los 10 s, y anuncian justo después. Diecisiete son del arranque, antes del reposo. Las otras siete caen en el reposo, entre las 16:50:01 y las 16:50:07, unos 5 minutos después del arranque, cuando toca el arranque periódico de `libp2p-kad`.

  Así que el router ve, de algunos pares, una marcación que no termina y se abandona a los 5 s, y después otra que sí termina, una detrás de otra. El reparto por redes apunta a la marcación a la dirección que el router tiene en la otra red, que el par prueba primero. No se comprobó, ni por qué entonces su apretón de manos llega al router y no termina.
- **Lo que esto dice de la condición 2.** Siete apretones abandonados, en 2402 s y repartidos entre tres routers (5 en `r4`, 1 en `r2`, 1 en `r3` y ninguno en `r1`), solo suman bytes al reposo de `kad-iroh`. Marcando de verdad una sola vez, `router_bytes_per_min_p50` no sería mayor que 90 908 B/min, que ya no llega al doble de 67 950. Es una inferencia, no una medida: la medida exacta que pide la condición no se tiene. La respaldan los routers uno a uno: `r1`, sin ningún apretón abandonado en el reposo, mueve 89 716 B/min (1,32 veces), y ningún router pasa de 1,50 veces (`r3`, 102 127). Las tablas de rutas siguieron llenas en todas las muestras de S6, así que ningún apretón quitó tráfico al sacar un par de una tabla.

## Veredicto de `repeat.py`

`repeat.py` sale con 0.

1. **S3 y S7.** Ejecuciones en `fail`: `kad-iroh`, en S3, 0 de 4; en S7, 0 de 4; `kad-libp2p`, en S3, 0 de 4; en S7, 0 de 4.
   **No se da:** `kad-iroh` no queda en `fail` en S3 ni en S7 en ninguna de sus ejecuciones.
2. **Reposo con intervalos de producción.** `router_bytes_per_min_p50`: `kad-iroh`, 90908 B/min; `kad-libp2p`, 67950 B/min. Es 1,34 veces el de `kad-libp2p`, y el doble son 135900 B/min.
   **No se da:** el de `kad-iroh` no pasa del doble del de `kad-libp2p`.

**Veredicto: sigue `kad-iroh`.** No se da ninguna condición de cambio.

La condición 1 se lee al pie de la letra: `kad-iroh` en `fail` en S3 o en S7 en alguna ejecución, y `kad-libp2p` en ninguno de los dos en ninguna. Leída escenario por escenario daría lo mismo aquí, porque no hay ningún `fail`. `repeat.py` avisa cuando las dos lecturas no coinciden. La condición 2 se da por no cumplida con la medida de arriba como cota superior: los apretones abandonados solo suman bytes, y la cota ya no llega al doble.

## Lo que no se midió

- **El reposo de `kad-iroh` con intervalos de producción marcando de verdad una sola vez.** La medida tuvo 24 apretones abandonados en los routers, 7 en el reposo ([arriba](#marcando-una-sola-vez)). No se investigó la causa ni se cambió `kad-iroh` para evitarlos: la condición se juzga sobre lo medido, como cota superior.
- **El reposo de `kad-iroh` con intervalos de producción y el factor por defecto.** Sin esa medida no se separa cuánto aportan las marcaciones dobles, ni con los intervalos de la prueba ni con los de producción.
- **Variación con intervalos de producción.** Hay una sola medida de S6 por candidato.
- **S0 a S5 y S7 con intervalos de producción.** Solo se midió el reposo. La caducidad con `TTL_S` = 3600 s y la recuperación con `REPUBLISH_S` = 1200 s no se comprueban: S3 tardaría más de una hora por ejecución.
- **De dónde sale el tráfico en reposo.** Como en la fase 1, no se separan los bytes de Kademlia, de `identify` y de Iroh.
- **La carga del anfitrión.** Es un anfitrión compartido, con una carga entre 12 y 36 durante las ejecuciones ([tabla](#anfitrión-carga-y-fechas)), que no se controló. Las ejecuciones se alternaron para repartirla entre los candidatos. Tampoco hay máquinas físicas, NAT ni relays, como en la fase 1.
- **El final de las ejecuciones de la fase 1.** Su carga al acabar y su código de salida no se anotaron.
