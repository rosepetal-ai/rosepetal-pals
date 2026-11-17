# The agents of the hito 3 exit test (docs/runbooks/hito-3-descubrimiento-kademlia.md):
# one container per agent, routers, providers and clients, all from this
# image, on two internal Docker networks. A Debian userland with what the
# harness needs on any agent; the `pals` binary and the `dht_rogue` example
# are copied in at run time, built on the host (`cargo build --release
# --locked -p pals` and `--examples`), and so are the agents' config files.
#
# - curl, for the gateway's GET /dht and POST /discovery/search;
# - iproute2, for the addresses of an agent that changes network (case 9);
# - python3, for the harness's metrics (metricas.py), read in the container:
#   /sys/class/net/*/statistics and /proc/<pid>/status;
# - procps, for the daemons' PIDs.
#
# Build it with tests/exit/hito-3.sh, which uses this file as the whole
# context.

# debian:trixie-slim as of 2026-11-04, pinned by the digest of its multi-arch
# index, as tests/exit/Dockerfile.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# `hito-3.sh desmontar` removes only an image with this label.
LABEL pals-exit.image="agents of the hito 3 exit test"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash ca-certificates coreutils curl iproute2 procps python3 util-linux \
    && rm -rf /var/lib/apt/lists/*

CMD ["sleep", "infinity"]
