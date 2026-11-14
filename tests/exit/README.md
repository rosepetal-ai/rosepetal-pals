# Prueba de salida del hito 1 con Docker

`run.sh` ejecuta los ocho casos del runbook [`docs/runbooks/hito-1-dos-maquinas.md`](../../docs/runbooks/hito-1-dos-maquinas.md) en una sola máquina Linux, con un contenedor Docker en el papel de la segunda máquina. Cada caso comprueba el resultado esperado del runbook e imprime `RESULTADO <caso>: superado` o `FALLIDO`. No sustituye a la prueba en dos máquinas físicas (ver «Lo que no cubre»).

## Topología

| | Dónde | Qué |
|-|-|-|
| A, quien pregunta | Contenedor `pals-exit-a` (imagen de este [`Dockerfile`](Dockerfile), Debian trixie) en su propia red bridge, `pals-exit-net`, detrás del NAT de Docker | Worker `echo`, gateway en `127.0.0.1:7420` del contenedor. Arranca con `--cap-add NET_ADMIN`: los cortes de red de los casos 3 y 4 son descartes reales de paquetes con `iptables` en el espacio de red propio de A |
| B, quien responde | El anfitrión, fuera de cualquier contenedor | Su propio `PALS_HOME` en `$PALS_EXIT_DIR/b`, gateway en `127.0.0.1:17420`. Caso 1 con el `claude` del anfitrión; casos 2 a 8 con el worker de pruebas del runbook |
| C, tercera identidad del caso 7 | Un tercer daemon dentro de A | Gateway en `127.0.0.1:7421` del contenedor |

Los daemons usan `net.relay = true`, con los relays públicos de n0. El camino directo es UDP de A al anfitrión por el bridge. Para el camino por relay (la repetición del caso 3 en «redes distintas»), A descarta su UDP saliente salvo DNS y loopback, y rearranca su daemon, como indica el runbook.

## Requisitos

- Linux con Docker, con permiso para crear imágenes, redes y contenedores. Además, `git`, `curl`, `python3` y `sha256sum`.
- Salida a internet: los relays de n0 por TCP 443, Debian para construir la imagen y PyPI para el caso 8.
- `pals` compilado en el anfitrión, con una glibc no más nueva que la de Debian trixie: `cargo build --release --locked -p pals`. El binario se copia en A.
- Para el caso 1, `claude` en el `PATH` y con sesión iniciada. Hace dos llamadas a `claude -p`: la comprobación del paso 2 del runbook y la pregunta del caso 1. Usa la suscripción o la clave de quien ejecuta el arnés. Las credenciales no se copian a ningún sitio: `claude` corre en el anfitrión con el `HOME` del usuario.
- Unos 5 GiB libres: ficheros de 256 MiB en A, y copias en los almacenes de blobs de A y B.

## Uso

```bash
cargo build --release --locked -p pals
export PALS_EXIT_DIR=$(mktemp -d)        # B y los registros; vacío
tests/exit/run.sh                        # todo: preparar, casos 1 a 8, limpieza y desmontar
```

Las fases también se pueden ejecutar sueltas, siempre en orden y con el mismo `PALS_EXIT_DIR`: `preparar`, `caso1`, `cambio-worker`, `caso2` … `caso8`, `limpieza` y `desmontar`. Por ejemplo, `tests/exit/run.sh preparar caso2 caso3`.

| Variable | Valor por defecto | Qué es |
|-|-|-|
| `PALS_EXIT_DIR` | (obligatoria) | Directorio de B (`b/`, su `PALS_HOME`; `b-salida/`, su `~/pals-salida`), de la copia del proyecto (`proyecto/`) y de los registros (`registro/`) |
| `PALS_EXIT_BIN` | `target/release/pals` | El binario de `pals` |
| `PALS_EXIT_CASO1` | `claude` | `omitir` salta el caso 1 y arranca B con el worker de pruebas: no llama a `claude` |
| `PALS_EXIT_PROYECTO` | una copia de este repositorio en `HEAD` | El proyecto sobre el que pregunta el caso 1 |
| `PALS_EXIT_B_GATEWAY` | `127.0.0.1:17420` | El gateway de B, en loopback |
| `PALS_EXIT_LIMITE` | `50mbit` | El límite del emisor en el caso 3, si la transferencia acaba antes del corte |
| `PALS_EXIT_KEEP_IMAGE` | `1` | `0` borra también la imagen al desmontar |

El registro completo queda en `$PALS_EXIT_DIR/registro/run.log` y los resultados en `registro/resultados.txt`. Al desmontar se copian ahí los registros de los tres daemons. El registro contiene ids de agente y hashes, pero no los tokens de los grants ni los tickets: el arnés los pasa de una máquina a otra sin imprimirlos.

## Qué crea y qué toca

- Docker: la imagen `pals-exit-a:latest`, la red `pals-exit-net` y el contenedor `pals-exit-a`. `desmontar` borra la red y el contenedor; la imagen solo con `PALS_EXIT_KEEP_IMAGE=0`.
- En el anfitrión: el daemon de B, con su gateway en loopback y su socket UDP de Iroh, y los ficheros bajo `PALS_EXIT_DIR`. El arnés nunca ejecuta `iptables`, `nft` ni `tc` en el anfitrión: los cortes y los límites se aplican solo dentro de A.
- `desmontar` para el daemon de B, termina las órdenes del worker de pruebas que sigan vivas y borra `b/`, `b-salida/` y la copia del proyecto. Conserva `registro/`.

## Cómo sigue el runbook

- Los guiones que el runbook escribe en `~/pals-salida` (`funciones.sh`, `pals-prueba.sh`, `corte-red.sh`, `caso3.sh`, `caso4-eventos.py` y `caso8.py`) se extraen del propio runbook. El arnés ejecuta lo que dice el documento, y un cambio en el runbook se prueba con él.
- A sigue el runbook al pie de la letra (`~/.pals`, `~/pals-salida`, `arranca`, `para`), como root y sin `sudo`. Cada orden en A es una terminal nueva que carga `funciones.sh`.
- B ejecuta las mismas órdenes con sus rutas: `PALS_HOME=$PALS_EXIT_DIR/b` en lugar de `~/.pals`, y `$PALS_EXIT_DIR/b-salida` en lugar de `~/pals-salida`. Su daemon arranca con un entorno limpio (`PATH`, `HOME` y `LANG`), como desde una terminal nueva.

Diferencias con el runbook:

- El binario se compila en el anfitrión y se copia a A, en lugar de usar `cargo install` en cada máquina. A recibe una copia del árbol de trabajo del repositorio, que necesita el caso 8.
- **Caso 3.** Si una transferencia acaba antes del corte, el arnés limita el emisor y repite, como indica el runbook. En `subida` usa el `tbf` del runbook sobre la interfaz de A. En `bajada` el emisor es B, el anfitrión, cuya red no se toca, así que A limita lo que recibe con un policía de entrada de `tc`.
- **Camino.** Para informar del camino de cada caso, el arnés añade al final de las cadenas de A cuatro reglas de `iptables` que solo cuentan bytes: UDP salvo DNS, y TCP 443, en cada sentido, por `eth0`. Las líneas `camino …` comparan esos bytes. Las líneas `relay:` de `caso3.sh` dan, además, la medida del runbook.
- **Caso 5.** El runbook busca a mano la orden que sobrevive al `kill -9` con `grep '[p]als-prueba'`, que puede encontrar también otros procesos cuya línea contenga ese texto. El arnés busca exactamente `sh $PALS_EXIT_DIR/b-salida/pals-prueba.sh` y termina su grupo de procesos.
- **Caso 7.** Además del fichero del caso 2, C pide también el que B generó en el caso 2.

## Lo que no cubre

A y B comparten el kernel, la máquina y la IP pública, y entre ellos solo hay el NAT de Docker. El camino por relay se fuerza descartando UDP; no hay redes distintas ni NAT de operadores, ni Wi-Fi o red móvil reales. Esta prueba ejercita PALS con cortes de red reales y con los relays públicos, pero no sustituye a la prueba en dos máquinas físicas.
