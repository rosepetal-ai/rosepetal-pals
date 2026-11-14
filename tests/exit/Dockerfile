# Machine A of the hito 1 exit test (docs/runbooks/hito-1-dos-maquinas.md)
# when Docker stands in for the second machine: a Debian userland with what
# the runbook needs on A. The `pals` binary is copied in at run time, built
# on the host (`cargo build --release --locked -p pals`); Debian trixie's
# glibc is at least as new as the build host's.
#
# Run it with `--cap-add NET_ADMIN`, so that the network cuts of cases 3 and
# 4 are real packet drops inside A's own network namespace, and with
# `--init`, so that the daemons the runbook starts in the background are
# reaped when they exit (the runbook's `para` waits on `kill -0`).
#
# No sudo: A's shell is root, and the runbook's commands and scripts use
# `$SUDO`, which is empty for root.

# debian:trixie-slim as of 2026-11-04, pinned by the digest of its multi-arch
# index; to update, `docker pull debian:trixie-slim` and take
# `docker image inspect -f '{{index .RepoDigests 0}}' debian:trixie-slim`.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# `run.sh desmontar` removes only an image with this label.
LABEL pals-exit.image="machine A of the hito 1 exit test"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash ca-certificates coreutils curl iproute2 iptables nftables procps \
        python3 python3-venv util-linux bsdutils \
    && rm -rf /var/lib/apt/lists/*

CMD ["sleep", "infinity"]
