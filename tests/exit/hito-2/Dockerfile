# The agents of the hito 2 exit test (docs/runbooks/hito-2-consulta-de-servicios.md):
# one container per agent, A to G, all from this image. A Debian userland
# with what the runbook needs on any agent; the `pals` binary is copied in at
# run time, built on the host (`cargo build --release --locked -p pals`),
# and so are the runbook's scripts and F's server.
#
# - Node and @modelcontextprotocol/server-everything, pinned with all its
#   dependencies by mcp/package-lock.json, at /opt/mcp: the real MCP server
#   the providers' inventory adapter observes over stdio.
# - A Python venv at /opt/venv with the official A2A SDK as pinned in
#   tests/interop/requirements.txt, and the HTTP server F needs on top of
#   it, pinned in requirements-f.txt: F's server, and the SDK's card
#   verifier (tests/interop/card_signatures.py) in A.
# - openssl, for F's test CA; iptables, for A's byte counters.
#
# Build it with tests/exit/hito-2.sh, which assembles the context (this
# file, mcp/, requirements-f.txt and tests/interop/requirements.txt).

# debian:trixie-slim as of 2026-11-04, pinned by the digest of its multi-arch
# index, as tests/exit/Dockerfile.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# `hito-2.sh desmontar` removes only an image with this label.
LABEL pals-exit.image="agents of the hito 2 exit test"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash ca-certificates coreutils curl iproute2 iptables procps \
        python3 python3-venv util-linux openssl nodejs npm \
    && rm -rf /var/lib/apt/lists/*

COPY requirements.txt requirements-f.txt /opt/f/
RUN python3 -m venv /opt/venv \
    && /opt/venv/bin/pip install --no-cache-dir --no-deps -r /opt/f/requirements.txt -r /opt/f/requirements-f.txt \
    && /opt/venv/bin/pip check

COPY mcp/package.json mcp/package-lock.json /opt/mcp/
RUN cd /opt/mcp && npm ci --ignore-scripts --no-audit --no-fund

CMD ["sleep", "infinity"]
