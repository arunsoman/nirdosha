# `ghcr.io/protobox/nirdosha-runtime` — the image protobox's
# `plugins/languages/nirdosha.py` already names as `docker_image`
# (docs/KUBERNETES.md P0's first item: "no published nirdosha binary/base
# image exists yet"). Bakes in the `nirdosha` binary itself plus
# python3/pytest/requests, since a nirdosha-lane protobox project's
# `test_command` is `pytest -q qa` (black-box HTTP tests against `nirdosha
# serve` — see that plugin's own module docstring for why `cargo test`
# is the wrong target: this language has no test/assert construct or
# `test` CLI subcommand of its own).
#
# Two stages: build compiles the binary with Z3 statically vendored (the
# `dist` feature, same as `.github/workflows/release.yml`'s Linux leg —
# reusing that exact build recipe rather than inventing a second one),
# runtime is a plain `python:slim` base with nothing else compiled in.

# ---- build stage --------------------------------------------------------
# `trixie` (Debian 13, GCC 14), not `bookworm` (Debian 12, GCC 12): z3-src
# 416.0.2's C++ sources use `#include <format>` (C++20), which libstdc++
# only ships starting with GCC 13 -- confirmed by a real build failure
# against `rust:1-slim-bookworm` ("fatal error: format: No such file or
# directory"), not assumed.
FROM rust:1-slim-trixie AS build

# cmake + a C++ toolchain + python3 are what `z3-src`'s vendored build
# (the `dist` feature) needs to compile Z3 from source, matching
# `release.yml`'s own Linux leg (`cargo build --release --features dist`,
# no system libz3 dependency).
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake python3 pkg-config ca-certificates git libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src/compiler
COPY crates/compiler/Cargo.toml crates/compiler/Cargo.lock ./
COPY crates/compiler/build.rs ./build.rs
COPY crates/compiler/src ./src
RUN cargo build --release --features dist

# ---- runtime stage -------------------------------------------------------
# `trixie`, matching the build stage exactly -- NOT bookworm: a binary
# built against trixie's glibc/libstdc++ (needed for z3-src's <format>
# usage, see the build stage comment) fails to even start on bookworm's
# older runtime ("version `GLIBC_2.39' not found"/"version
# `GLIBCXX_3.4.31' not found") -- confirmed by a real `docker run`
# against a first version of this image that mismatched the two.
FROM python:3.12-slim-trixie AS runtime

LABEL org.opencontainers.image.source="https://github.com/kannamma-labs/nirdosha" \
      org.opencontainers.image.description="nirdosha: one process serves both the API and the UI generated from a single .nir program" \
      org.opencontainers.image.licenses="Apache-2.0"

# `pytest`/`requests`: exactly what protobox's black-box QA tasks need to
# run `pytest -q qa` against a booted `nirdosha serve` inside this image
# (`nirdosha-default-pipeline-plan.md` Phase 6) — nothing else from PyPI.
RUN pip install --no-cache-dir --root-user-action=ignore pytest==8.* requests==2.* \
    && useradd --uid 10001 --create-home --shell /usr/sbin/nologin nirdosha

COPY --from=build /src/compiler/target/release/nirdosha /usr/local/bin/nirdosha

# `docs/KUBERNETES.md`'s "Security posture" row: non-root UID, and the only
# writable path is `/data` (where a project's `.nir` source,
# `.transact.db`/`.workflow.db` durability logs, and `--db`'s SQLite
# table-browser file all live) — everything else can run under a
# read-only root filesystem (`securityContext.readOnlyRootFilesystem:
# true` in the Helm chart / Kustomize manifests) with no further change
# needed here.
RUN mkdir -p /data && chown nirdosha:nirdosha /data
USER nirdosha
WORKDIR /data

EXPOSE 8080
# `/healthz`/`/readyz` (this same change, `serve.rs`) are what the
# Kubernetes manifests point their probes at — see
# `deploy/kubernetes/README.md`.
ENTRYPOINT ["nirdosha"]
CMD ["--help"]
