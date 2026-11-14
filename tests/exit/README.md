# Prueba de salida del hito 1 con Docker

`run.sh` ejecuta los ocho casos del runbook [`docs/runbooks/hito-1-dos-maquinas.md`](../../docs/runbooks/hito-1-dos-maquinas.md) en una sola máquina Linux, con un contenedor Docker en el papel de la segunda máquina. Cada caso comprueba el resultado esperado del runbook e imprime `RESULTADO <caso>: superado` o `FALLIDO`. No sustituye a la prueba en dos máquinas físicas (ver «Lo que no cubre»).

## Topología

| | Dónde | Qué |
|-|-|-|
| A, quien pregunta | Contenedor `pals-exit-a` (imagen de este [`Dockerfile`](Dockerfile), Debian trixie fijada por su digest) en su propia red bridge, `pals-exit-net`, detrás del NAT de Docker | Worker `echo`, gateway en `127.0.0.1:7420` del contenedor. Arranca con `--cap-add NET_ADMIN`: los cortes de red de los casos 3 y 4 son descartes reales de paquetes con `iptables` en el espacio de red propio de A |
| B, quien responde | El anfitrión, fuera de cualquier contenedor | Su propio `PALS_HOME` en `$PALS_EXIT_DIR/b`, gateway en `127.0.0.1:17420`. Caso 1 con el `claude` del anfitrión; casos 2 a 8 con el worker de pruebas del runbook |
| C, tercera identidad del caso 7 | Un tercer daemon dentro de A | Gateway en `127.0.0.1:7421` del contenedor |

Los daemons usan `net.relay = true`, con los relays públicos de n0. El camino directo es UDP de A al anfitrión por el bridge. Para el camino por relay (la repetición del caso 3 en «redes distintas»), A descarta su UDP saliente salvo DNS y loopback, y rearranca su daemon, como indica el runbook.

## Requisitos

- Linux con Docker, con permiso para crear imágenes, redes y contenedores. Además, `bash`, `git`, `curl`, `python3`, `sha256sum`, `tar`, `awk`, `procps` (`ps`, `pgrep`) y `util-linux` (`setsid`).
- El puerto 17420 de loopback libre en el anfitrión (o otro, con `PALS_EXIT_B_GATEWAY`).
- Que el cortafuegos del anfitrión acepte en su entrada el UDP que llega desde la red bridge de Docker. Si lo descarta, no hay camino directo entre A y B: todo irá por el relay, y los casos 3a, que comprueban el camino directo, fallarán.
- Salida a internet: los relays de n0 por TCP 443, Debian para construir la imagen y PyPI para el caso 8.
- `pals` compilado en el anfitrión, con una glibc no más nueva que la de Debian trixie: `cargo build --release --locked -p pals`. El binario se copia en A.
- Para el caso 1, `claude` en el `PATH` y con sesión iniciada. Hace dos llamadas a `claude -p`: la comprobación del paso 2 del runbook y la pregunta del caso 1. Usa la suscripción o la clave de quien ejecuta el arnés. Las credenciales no se copian a ningún sitio: `claude` corre en el anfitrión con el `HOME` del usuario.
- Espacio: unos 3,5 GiB en el almacenamiento de Docker (los ficheros de 256 MiB de A y su almacén de blobs) y 1,5 GiB bajo `PALS_EXIT_DIR` (el almacén de blobs de B). Si `PALS_EXIT_DIR` está en `/tmp` y `/tmp` es un `tmpfs`, ese 1,5 GiB ocupa memoria.
- Tiempo: unos 45 min, de los que unos 40 son las dos transferencias de 256 MiB por relay del caso 3b (los relays públicos de n0 limitan el tráfico a unos 250 KB/s).

## Uso

```bash
cargo build --release --locked -p pals
export PALS_EXIT_DIR=$(mktemp -d)        # nuevo o vacío: B y los registros
tests/exit/run.sh                        # todo: preparar, casos 1 a 8, limpieza y desmontar
```

Las fases también se pueden ejecutar sueltas, siempre en orden y con el mismo `PALS_EXIT_DIR`: `preparar`, `caso1`, `cambio-worker`, `caso2` … `caso8`, `limpieza` y `desmontar`. Por ejemplo, `tests/exit/run.sh preparar caso2 caso3`, y después `tests/exit/run.sh desmontar`.

| Variable | Valor por defecto | Qué es |
|-|-|-|
| `PALS_EXIT_DIR` | (obligatoria) | Directorio de B (`b/`, su `PALS_HOME`; `b-salida/`, su `~/pals-salida`), de la copia del proyecto (`proyecto/`) y de los registros (`registro/`). `preparar` solo acepta uno nuevo o vacío |
| `PALS_EXIT_BIN` | `target/release/pals` | El binario de `pals` |
| `PALS_EXIT_CASO1` | `claude` | `omitir` salta el caso 1 y arranca B con el worker de pruebas: no llama a `claude` |
| `PALS_EXIT_PROYECTO` | una copia de este repositorio en `HEAD` | El proyecto sobre el que pregunta el caso 1. `desmontar` nunca lo borra |
| `PALS_EXIT_B_GATEWAY` | `127.0.0.1:17420` | El gateway de B, en loopback |
| `PALS_EXIT_LIMITE` | `50mbit` | El límite del emisor en el caso 3, si la transferencia acaba antes del corte |
| `PALS_EXIT_KEEP_IMAGE` | `1` | `0` borra también la imagen al desmontar |

El registro completo queda en `$PALS_EXIT_DIR/registro/run.log` y los resultados en `registro/resultados.txt`. Al desmontar se copian ahí los registros de los tres daemons. El registro contiene ids de agente y hashes, pero no los tokens de los grants ni los tickets: el arnés los pasa de una máquina a otra sin imprimirlos.

**Si una ejecución se interrumpe.** Con Ctrl-C o `SIGTERM` (al proceso o a su grupo), el arnés desmonta antes de salir. Lo hace en el acto también durante los pasos largos (las transferencias del caso 3, de hasta 20 min, y los casos 1, 2, 4 y 8), que corren en segundo plano mientras el arnés espera; un paso corto, de unos segundos, termina antes. `todo` desmonta también cuando una fase falla. Después de cualquier otro final (un fallo en fases sueltas, `kill -9`, un reinicio), ejecuta `tests/exit/run.sh desmontar` con el mismo `PALS_EXIT_DIR`. Si ese directorio ya no existe y quedan el contenedor o la red, comprueba antes su etiqueta (`docker inspect -f '{{ index .Config.Labels "pals-exit.dir" }}' pals-exit-a`, `docker network inspect -f '{{ index .Labels "pals-exit.dir" }}' pals-exit-net`) y bórralos a mano: `docker rm -f pals-exit-a; docker network rm pals-exit-net`.

`desmontar` no borra los ficheros de B mientras su daemon siga vivo: si no termina 60 s después de `SIGTERM`, le envía `SIGKILL`, y si aun así sigue, avisa y los conserva.

## Qué crea y qué toca

- **Docker:** la imagen `pals-exit-a:latest` (con la etiqueta `pals-exit.image`), y la red `pals-exit-net` y el contenedor `pals-exit-a`, los dos con la etiqueta `pals-exit.dir=<PALS_EXIT_DIR>`. Al crear y borrar la red, Docker añade y quita sus propias reglas de cortafuegos y de NAT en el anfitrión. `preparar` se niega si ya existen la red o el contenedor (puede haber otra ejecución en marcha; el mensaje dice cuál). `desmontar` borra la red y el contenedor solo si su etiqueta es este `PALS_EXIT_DIR`, y la imagen solo con `PALS_EXIT_KEEP_IMAGE=0`.
- **En el anfitrión:** el daemon de B, con su socket UDP de Iroh y su gateway en loopback. Ese gateway, con su MCP, queda abierto a cualquier usuario local de la máquina mientras dura la prueba (la limitación «sin autenticación local» del README). Con `relay = true`, B publica sus direcciones en el descubrimiento de n0. El arnés nunca ejecuta `iptables`, `nft` ni `tc` en el anfitrión: los cortes y los límites se aplican solo dentro de A.
- **Ficheros:** solo bajo `PALS_EXIT_DIR`. `preparar` marca el directorio con un fichero `.pals-exit`, y las demás fases se niegan a trabajar en uno sin esa marca. `desmontar` borra `b/` y `b-salida/` y, si `preparar` la clonó ahí, la copia del proyecto; conserva `registro/` y la marca.
- **Procesos:** antes de enviar una señal al daemon de B, el arnés comprueba que el PID de su fichero es `<PALS_EXIT_BIN> daemon` con `PALS_HOME` de esta ejecución; un fichero de PID viejo no lleva a matar otro proceso. Las órdenes del worker de pruebas se buscan exactamente como `sh <PALS_EXIT_DIR>/b-salida/pals-prueba.sh`, y la del caso 5, por el `PALS_TASK_ID` de su tarea.

## Cómo sigue el runbook

- Los seis guiones que el runbook escribe en `~/pals-salida` (`funciones.sh`, `pals-prueba.sh`, `corte-red.sh`, `caso3.sh`, `caso4-eventos.py` y `caso8.py`) se extraen del propio runbook: un cambio en esos guiones se prueba con el arnés. El resto de las órdenes del runbook no: el arnés lleva su propia copia (ver «Lo que no cubre»).
- En A, las órdenes de cada caso son las del runbook, copiadas en `run.sh` (`~/.pals`, `~/pals-salida`, `arranca`, `para`), como root y sin `sudo`. Cada orden en A es una terminal nueva que carga `funciones.sh`.
- B no ejecuta los bloques del runbook: el arnés hace lo mismo con sus propias funciones (`pb`, `arranca_b`, `para_b`, `write_b_config`) y con las rutas de B, `PALS_HOME=$PALS_EXIT_DIR/b` en lugar de `~/.pals` y `$PALS_EXIT_DIR/b-salida` en lugar de `~/pals-salida`. Su daemon arranca con un entorno limpio (`PATH`, `HOME` y `LANG`), como desde una terminal nueva.

Diferencias con el runbook:

- El binario se compila en el anfitrión y se copia a A, en lugar de usar `cargo install` en cada máquina. A recibe una copia del árbol de trabajo del repositorio, que necesita el caso 8.
- **Caso 3.** Si una transferencia acaba antes del corte, el arnés limita el emisor y repite, como indica el runbook. En `subida` usa el `tbf` del runbook sobre la interfaz de A. En `bajada` el emisor es B, el anfitrión, cuya red no se toca, así que A limita lo que recibe con un policía de entrada de `tc`. Además, el arnés comprueba el camino medido: directo en 3a y relay en 3b.
- **Camino.** Para informar del camino de cada caso, el arnés añade al final de las cadenas de A cuatro reglas de `iptables` que solo cuentan bytes: UDP salvo DNS, y TCP 443, en cada sentido, por `eth0`. Las líneas `camino …` comparan esos bytes. Las líneas `relay:` de `caso3.sh` dan, además, la medida del runbook. Las reglas de cuenta van detrás de las de los cortes y del relay forzado, así que no ven lo que esas descartan.
- **Caso 5.** El arnés busca la orden que sobrevive al `kill -9` por el `PALS_TASK_ID` de su tarea, y la vuelve a buscar justo antes de terminar su grupo.
- **Caso 7.** Además del fichero del caso 2, C pide también el que B generó en el caso 2.

## Lo que no cubre

- A y B comparten el kernel, el reloj, la máquina y la IP pública, y entre ellos solo hay el NAT de Docker. El camino por relay se fuerza descartando UDP; no hay redes distintas ni NAT de operadores, ni Wi-Fi o red móvil reales. Esta prueba ejercita PALS con cortes de red reales y con los relays públicos, pero no sustituye a la prueba en dos máquinas físicas.
- **El camino con `sudo`:** A trabaja como root, así que `$SUDO` va vacío y no se ejecuta ningún `sudo`.
- **Las órdenes del runbook en B:** el paso 2 en B, el bloque del cambio de worker, la parte del caso 5 en B y la limpieza en B no se ejecutan tal como están escritas, porque el arnés las reimplementa.
- **Las órdenes de A fuera de los seis guiones:** son copias en `run.sh`. Un cambio en esos bloques del runbook hay que llevarlo a mano a `run.sh` para probarlo.
