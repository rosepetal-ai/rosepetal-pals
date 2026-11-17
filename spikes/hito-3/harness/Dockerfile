# The nodes of the hito 3 backend comparison (spikes/hito-3/harness/harness.sh):
# one container per node, all from this image, whatever the candidate. The
# candidate's binary is built on the host
# (`cargo build --release --locked -p spike-<id>` in spikes/hito-3/) and
# copied into each container before it starts; Debian trixie's glibc is at
# least as new as the build host's. curl drives the control API on
# 127.0.0.1:7600; iproute2 is there to look at a node's interfaces and
# routes by hand.
#
# harness.sh builds it from this file alone, with no context, and never
# removes it.

# debian:trixie-slim as of 2026-11-04, pinned by the digest of its multi-arch
# index, as tests/exit/Dockerfile.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

LABEL pals-spike.image="nodes of the hito 3 backend comparison"

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl iproute2 \
    && rm -rf /var/lib/apt/lists/*
