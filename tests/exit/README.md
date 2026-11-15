# Prueba de salida del hito 1 con Docker

Este directorio tiene dos arneses: `run.sh`, del hito 1, en esta sección, y `hito-2.sh`, del hito 2, en la [siguiente](#prueba-de-salida-del-hito-2-con-docker).

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
- **En el anfitrión:** el daemon de B, con su socket UDP de Iroh y su gateway en loopback. Ese gateway, con su MCP, queda abierto a cualquier usuario local de la máquina mientras dura la prueba (la limitación «No local authentication» del README). Con `relay = true`, B publica sus direcciones en el descubrimiento de n0. El arnés nunca ejecuta `iptables`, `nft` ni `tc` en el anfitrión: los cortes y los límites se aplican solo dentro de A.
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

# Prueba de salida del hito 2 con Docker

[`hito-2.sh`](hito-2.sh) ejecuta los casos 1 a 8 del runbook [`docs/runbooks/hito-2-consulta-de-servicios.md`](../../docs/runbooks/hito-2-consulta-de-servicios.md) en una sola máquina Linux, con un contenedor Docker por agente. Cada caso comprueba el resultado esperado del runbook e imprime `RESULTADO <caso>: superado` o `FALLIDO`. El caso 9, el TCK y el cliente de Python, se ejecuta aparte, como dice el runbook.

## Topología

Todos los contenedores salen de la imagen [`hito-2/Dockerfile`](hito-2/Dockerfile) (Debian trixie fijada por su digest, con Node, `@modelcontextprotocol/server-everything` fijado por [`hito-2/mcp/package-lock.json`](hito-2/mcp/package-lock.json), y un venv con el SDK oficial de Python fijado por [`../interop/requirements.txt`](../interop/requirements.txt) y [`hito-2/requirements-f.txt`](hito-2/requirements-f.txt)) y están en una red bridge propia, `pals-exit-h2-net`.

| Contenedor | Agente | Qué |
|-|-|-|
| `pals-exit-h2-a` | A, quien pide | Worker `echo`; la CA de prueba de F en su almacén del sistema. Arranca con `--cap-add NET_ADMIN` solo para las cuatro reglas de `iptables` que cuentan bytes en su `eth0` y dicen el camino de red; no corta nada |
| `pals-exit-h2-b` … `-e`, `-g` | B, C, D, E y G, los proveedores | Los inventarios de la tarea 10, con el worker de pruebas del runbook y su conexión MCP comprobada por el adaptador con server-everything por stdio |
| `pals-exit-h2-g` | H, el contacto caído del caso 5 | Un segundo daemon en el contenedor de G, con su propio `PALS_HOME` |
| `pals-exit-h2-f` | F, el agente A2A de serie | [`hito-2/agente_f.py`](hito-2/agente_f.py), por `https` en el 8443 con un certificado de una CA de prueba hecha en el contenedor, y su copia con la card alterada en el 8444 |
| `pals-exit-h2-m` | M1 y M2 del caso 8 | El `pals` del hito 1 y el del hito 2 sobre los mismos directorios de datos, en loopback |

Los daemons usan `net.relay = true`, los relays públicos de n0: con `relay = false` un daemon solo escucha en `127.0.0.1` y no lo alcanza otro contenedor. Entre contenedores el camino es UDP directo por el bridge (la línea `camino caso 1:` del registro lo mide). Al rearrancar B en el caso 5, A lo encuentra por su id sin volver a guardar su ticket.

## Requisitos

- Linux con Docker, con permiso para crear imágenes, redes y contenedores. Además, `bash`, `git`, `curl`, `python3`, `sha256sum`, `tar` y `awk`.
- Go 1.26 o posterior y Node 20 o posterior en el anfitrión, para los verificadores de Go y JS del caso 3, y `cargo`, para sus tests de los pares de prueba (`PALS_EXIT_TESTS=omitir` los salta).
- Salida a internet: los relays de n0 por TCP 443, y Debian, PyPI y el registro de npm para construir la imagen (y el proxy de módulos de Go y npm para los verificadores, si no están ya descargados).
- `pals` compilado en el anfitrión, con una glibc no más nueva que la de Debian trixie: `cargo build --release --locked -p pals`. Se copia a cada contenedor.
- Para el caso 8, el `pals` del hito 1 (commit `71092a0`) y el programa `events_verify`, como dice la sección 1 del runbook:

  ```bash
  git archive 71092a0 | tar -x -C <dir>/hito1-src
  (cd <dir>/hito1-src && CARGO_TARGET_DIR=<dir>/hito1-target cargo build --release --locked -p pals)
  cargo build --release --locked -p pals-core --example events_verify
  ```

- Espacio: la imagen ocupa unos 700 MB; los registros, unos pocos MB.
- Tiempo: unos 2 min con la imagen ya construida; la primera vez, unos 3 más para construirla.

## Uso

```bash
cargo build --release --locked -p pals
cargo build --release --locked -p pals-core --example events_verify
export PALS_EXIT_DIR=$(mktemp -d)                 # nuevo o vacío: los registros
export PALS_EXIT_BIN_HITO1=<el pals del hito 1>
tests/exit/hito-2.sh                              # todo: preparar, casos 1 a 8 y desmontar
```

Las fases también se pueden ejecutar sueltas, en orden y con el mismo `PALS_EXIT_DIR`: `preparar`, `caso1` … `caso8` y `desmontar`. Por ejemplo, `tests/exit/hito-2.sh preparar caso1 caso2`, y después `tests/exit/hito-2.sh desmontar`. Los casos dependen de los anteriores: el 2 usa la búsqueda del 1, el 5 rearranca B, y el 7 busca entre F y el F alterado que guarda el 3.

| Variable | Valor por defecto | Qué es |
|-|-|-|
| `PALS_EXIT_DIR` | (obligatoria) | Los registros (`registro/`) y el contexto de construcción de la imagen. `preparar` solo acepta uno nuevo o vacío |
| `PALS_EXIT_BIN` | `target/release/pals` | El binario de `pals` |
| `PALS_EXIT_BIN_HITO1` | (ninguno) | El `pals` del hito 1, para el caso 8; sin él, el caso 8 falla |
| `PALS_EXIT_EVENTS_VERIFY` | `target/release/examples/events_verify` | El verificador de la cadena de eventos del caso 8 |
| `PALS_EXIT_TESTS` | `cargo` | `omitir` salta, en el caso 3, los tests de los pares de prueba con la card alterada |
| `PALS_EXIT_KEEP_IMAGE` | `1` | `0` borra también la imagen al desmontar |

El registro completo queda en `$PALS_EXIT_DIR/registro/run.log` y los resultados en `registro/resultados.txt`; las respuestas de las búsquedas, en `registro/busquedas/`, y las páginas de las vistas del caso 6, en `registro/vistas/`. Al desmontar se copian ahí, por contenedor, los registros de los daemons, las ejecuciones de cada worker, lo que recibió y respondió cada servidor MCP y los de F. Contienen ids de agente, hashes e ids de grants y de ofertas, pero no los tokens de los grants ni los tickets: el arnés los pasa de un contenedor a otro sin imprimirlos.

**Si una ejecución se interrumpe.** Con Ctrl-C o `SIGTERM`, el arnés desmonta antes de salir; los pasos largos corren en segundo plano mientras espera, así que lo hace en el acto. `todo` desmonta también cuando una fase falla. Después de cualquier otro final, ejecuta `tests/exit/hito-2.sh desmontar` con el mismo `PALS_EXIT_DIR`. Si ese directorio ya no existe y quedan contenedores o la red, comprueba antes su etiqueta (`docker inspect -f '{{ index .Config.Labels "pals-exit.dir" }}' pals-exit-h2-a`, `docker network inspect -f '{{ index .Labels "pals-exit.dir" }}' pals-exit-h2-net`) y bórralos a mano: `docker rm -f pals-exit-h2-{a,b,c,d,e,g,f,m}; docker network rm pals-exit-h2-net`.

## Qué crea y qué toca

- **Docker:** la imagen `pals-exit-h2:latest` (con la etiqueta `pals-exit.image`), y la red `pals-exit-h2-net` y los contenedores `pals-exit-h2-{a,b,c,d,e,g,f,m}`, todos con la etiqueta `pals-exit.dir=<PALS_EXIT_DIR>`. Al crear y borrar la red, Docker añade y quita sus propias reglas de cortafuegos y de NAT en el anfitrión. `preparar` se niega si existe ya algún recurso `pals-exit-h2-*`. `desmontar` borra solo los contenedores y la red cuya etiqueta es este `PALS_EXIT_DIR`, y la imagen solo con `PALS_EXIT_KEEP_IMAGE=0`. Nunca toca la imagen `pals-exit-a:latest` del hito 1 ni ningún otro recurso.
- **En el anfitrión:** ningún daemon. El arnés no ejecuta `iptables`, `nft`, `tc` ni `sysctl` en el anfitrión; las únicas reglas de `iptables` son las de cuenta de bytes dentro de A. Ejecuta allí los verificadores de Go y JS del caso 3 sobre la card que guarda en el registro, y, con `cargo`, los dos tests de integración de los pares de prueba.
- **Ficheros:** los de la prueba, solo bajo `PALS_EXIT_DIR`. `preparar` lo marca con un fichero `.pals-exit`, y las demás fases se niegan a trabajar en uno sin esa marca. `desmontar` borra el contexto de la imagen y la copia del certificado de la CA de prueba (nunca sale su clave del contenedor de F); conserva `registro/` y la marca. Fuera de él, el caso 3 escribe en el anfitrión lo que escriben sus herramientas, todo ignorado por git: `node_modules/` en `tests/interop/card_signatures/js` (con `npm ci`, si no existe), los tests en `target/` (`cargo test`) y las cachés de módulos y de compilación de Go (`go run`).
- **Lo que entra en los contenedores:** el binario, los guiones extraídos del runbook, el certificado de la CA de prueba (solo en A) y los ficheros que git sigue, como están en el árbol de trabajo; nunca un fichero sin seguimiento.
- **Procesos:** todos los daemons y servidores viven en los contenedores. El `para` del runbook envía `SIGTERM` a un daemon solo si su PID sigue siendo el `pals daemon` de ese `PALS_HOME`, y el caso 8 hace lo mismo con los suyos.

## Cómo sigue el runbook

- Los ocho guiones que el runbook escribe en `~/pals-h2` (`funciones.sh`, `proveedor.sh`, `mcp-everything.sh`, `req.json`, `metricas.py`, `caso4.sh`, `vista.py` y `caso8.sh`) se extraen del propio runbook y se copian a cada contenedor: un cambio en ellos se prueba con el arnés.
- Las demás órdenes del runbook están copiadas en `hito-2.sh`. Cada orden en un contenedor es una terminal nueva, como root, que carga `funciones.sh` y elige su agente con `agente <x>`. Un cambio en esos bloques del runbook hay que llevarlo a mano a `hito-2.sh`.

Diferencias con el runbook:

- El binario se compila en el anfitrión y se copia a cada contenedor, en lugar de `cargo install` en cada máquina. Cada contenedor recibe en `~/rosepetal-pals` los ficheros que git sigue, como están en el árbol de trabajo: uno nuevo tiene que estar al menos añadido (`git add`) para llegar. El servidor MCP y el venv vienen en la imagen, en `/opt/mcp` y `/opt/venv`, enlazados desde `~/pals-h2/mcp` y `~/pals-h2/venv`, donde los instala el runbook.
- El nombre de la máquina de F (`$F_HOST`) es el del contenedor, `pals-exit-h2-f`, que el DNS de la red de Docker resuelve. La CA de prueba pasa de F a A por el anfitrión; su clave no sale de F.
- **Caso 3.** Los verificadores de Go y JS corren en el anfitrión, sobre la card que el gateway de A sirvió. El arnés comprueba además que esa card, la pública de B, solo tiene la skill `ask` y no menciona nada de lo que es de proyecto, y, al preparar, que el JWK Set de F responde en los dos puertos: `card signature unverifiable` en el 8444 solo puede ser la firma.
- **Caso 4.** La ráfaga tiene que dar al menos un `200` y un `429`, y al menos 45 de las 50 seguidas el ranking completo (B y C, y D, E y G descartados por sus ofertas): así las consultas llegaron a los proveedores. `tools/list` sin cambios, o como mucho uno por cada vencimiento del temporizador de 60 s de cada proveedor durante el caso, contado desde su primera comprobación, la primera línea de su `*-mcp-arranques.log`. Ningún `tools/call` en ningún servidor MCP, en el caso 4 y, al desmontar, en toda la prueba (`RESULTADO sin tools/call en toda la prueba`): la comprobación falla cerrada, y una copia de lo que recibió un servidor que falta o no se puede leer, o una cuenta que no es un número, la hacen fallar.
- **Caso 5.** Si la oferta de B sigue en vigor cuando A pregunta (si H tardara menos que la oferta en descartarse), el arnés repite 5b con otra pregunta, hasta tres veces, y exige que en algún intento se renueve: que la oferta con la que se preguntó no sea la del ranking. Cada intento comprueba que su pregunta se ejecuta una sola vez.

## Lo que no cubre

- Todos los agentes comparten el kernel, el reloj y la máquina; entre ellos solo hay el bridge de Docker. No hay máquinas, redes ni NAT distintos: el camino es UDP directo, y los relays solo se usan para registrarse. Los relojes nunca difieren, así que el desfase de 60 s no se ejercita aquí (lo hacen los tests).
- F es un agente corriente sobre el SDK de Python, con un certificado de una CA de prueba; no un agente de terceros en internet.
- **Las órdenes del runbook fuera de los ocho guiones:** son copias en `hito-2.sh`.
