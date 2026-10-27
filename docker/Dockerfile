# syntax=docker/dockerfile:1
#
# The meshfox container image: `meshfox serve --http` with a mounted working
# directory, plus everything `docker exec` needs to drive it (`meshfox tui`,
# `run`, `view`).
#
#   docker build -f docker/Dockerfile --build-arg MESHFOX_VERSION=0.27.0 \
#       --target slim -t meshfox:slim docker/
#   docker build -f docker/Dockerfile --build-arg MESHFOX_VERSION=0.27.0 \
#       --target full -t meshfox:full docker/
#
# The binary is the one from the GitHub release `v${MESHFOX_VERSION}`
# (`meshfox-<target triple>.tar.gz`), so the image runs exactly what users
# download. To use a locally built Linux binary instead, put it at
# `docker/bin/<arch>/meshfox` (`arm64`, `amd64`) and pass
# `--build-arg BINARY_SOURCE=local`; the build context is `docker/`.
#
# The base is Debian 13 (trixie, glibc 2.41): release binaries are linked
# dynamically against the glibc of the CI runner (Ubuntu 24.04, 2.39), so the
# base must not be older than that. Debian 12 would not run them.

ARG DEBIAN=trixie-slim

# ---- where the binary comes from ---------------------------------------------
ARG BINARY_SOURCE=release

FROM debian:${DEBIAN} AS binary-release
ARG TARGETARCH
ARG MESHFOX_VERSION
RUN test -n "${MESHFOX_VERSION}" || { echo "set --build-arg MESHFOX_VERSION=<release, e.g. 0.27.0>" >&2; exit 1; } \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && case "${TARGETARCH}" in \
        amd64) triple=x86_64-unknown-linux-gnu ;; \
        arm64) triple=aarch64-unknown-linux-gnu ;; \
        *) echo "unsupported architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL "https://github.com/orofarne/meshfox/releases/download/v${MESHFOX_VERSION}/meshfox-${triple}.tar.gz" \
    | tar -xz -C /tmp "${triple}/meshfox" \
 && install -m 0755 "/tmp/${triple}/meshfox" /meshfox

FROM scratch AS binary-local
ARG TARGETARCH
COPY --chmod=0755 bin/${TARGETARCH}/meshfox /meshfox

FROM binary-${BINARY_SOURCE} AS binary

# ---- slim: meshfox, a shell, git, DuckDB, certificates -----------------------
FROM debian:${DEBIAN} AS slim

# `source` is what GHCR uses to link the package to the repository on the
# first push; the other two show on the package page.
LABEL org.opencontainers.image.source="https://github.com/orofarne/meshfox" \
      org.opencontainers.image.description="meshfox: canvases of runnable Markdown, served over HTTP" \
      org.opencontainers.image.licenses="MIT"

ARG TARGETARCH

# The DuckDB CLI drives `display="table"`, so even the slim image has it. It is
# not packaged by Debian or Ubuntu, hence the release binary, pinned.
ARG DUCKDB_VERSION=v2.0.0

# bash and git are what most blocks reach for; curl is for the healthcheck;
# tini reaps the processes blocks leave behind, so `docker run --init` is not
# something users have to remember.
RUN apt-get update \
 && apt-get install -y --no-install-recommends bash ca-certificates curl git tini \
 && rm -rf /var/lib/apt/lists/* \
 && curl -fsSL "https://github.com/duckdb/duckdb/releases/download/${DUCKDB_VERSION}/duckdb-cli-linux-${TARGETARCH}.tar.gz" \
    | tar -xz -C /usr/local/bin duckdb \
 && duckdb --version

COPY --from=binary --chmod=0755 /meshfox /usr/local/bin/meshfox

# `install-method` makes `meshfox check-updates` say "pull a newer image"
# instead of replacing the binary inside a container.
#
# Home, the coordinator's socket directory and /work are world-writable (with
# the sticky bit where they are shared): the image is meant to run as whatever
# uid owns the mounted directory (`--user "$(id -u):$(id -g)"`), which has no
# passwd entry and no home of its own.
RUN useradd --uid 1000 --user-group --home-dir /home/meshfox --create-home --shell /bin/bash meshfox \
 && chmod 1777 /home/meshfox \
 && install -d -m 1777 /run/meshfox /work \
 && install -d /usr/local/share/meshfox \
 && echo docker > /usr/local/share/meshfox/install-method

ENV HOME=/home/meshfox \
    LANG=C.UTF-8 \
    # `docker exec … meshfox tui|run|view` talks to the `serve` running in
    # this container (the coordinator) instead of starting a private core.
    MESHFOX_SERVER_SOCKET=/run/meshfox/meshfox.sock

WORKDIR /work
USER meshfox
EXPOSE 4590

# A token is required on a non-loopback address: set MESHFOX_TOKEN, or read
# the one `serve` generates from `docker logs`.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s \
    CMD curl -fsS http://127.0.0.1:4590/_/ping || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "meshfox"]
CMD ["serve", "--http", "0.0.0.0:4590", "--root", "/work"]

# ---- full: slim plus the interpreters and tools blocks commonly use ----------
FROM slim AS full

USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        chromium fonts-liberation fonts-noto-color-emoji \
        nodejs npm \
        python3 python3-pip python3-venv \
 && rm -rf /var/lib/apt/lists/*
USER meshfox
# Chromium cannot create its sandbox in an unprivileged container; the
# container is the boundary (`meshfox pdf` honours this, see pdf/browser.rs).
ENV MESHFOX_CHROME_NO_SANDBOX=1
