# `ghcr.io/protobox/nirdosha-presence-gateway` — the small, standalone
# WebSocket relay `docs/WORKFLOW.md`'s "notify presence bridge" section and
# `docs/ROADMAP.md`'s Track A5 describe. Deliberately its own image, not baked
# into the main `nirdosha` runtime image (repo-root `Dockerfile`) — this
# is a separate deployable (its own Deployment/sidecar in a real cluster),
# and has none of that image's build requirements (no Z3/C++20 vendoring,
# so no need for that Dockerfile's `trixie`-over-`bookworm` base-image
# pin — plain `rust:1-slim`/`debian:bookworm-slim` build fine here).

# ---- build stage --------------------------------------------------------
FROM rust:1-slim AS build

# `crates/compiler/` has to be present in the build context too -- even
# though it's only this crate's `[dev-dependencies]` (`Cargo.toml`'s own
# doc comment: real integration tests spin up a real `nirdosha serve`
# in-process), Cargo still reads a path dependency's manifest to compute
# the unified `Cargo.lock` graph even for a plain non-test `build`. It is
# **not** actually compiled for this image -- `cargo build --release`
# below only builds this crate's own `[[bin]]`, never a dev-dependency,
# so none of `compiler`'s own dependencies (z3, postgres, rusqlite, ...)
# get pulled into this build, confirmed by this image's own build time.
WORKDIR /src
COPY crates/compiler/Cargo.toml crates/compiler/Cargo.lock ./compiler/
COPY crates/compiler/build.rs ./compiler/build.rs
COPY crates/compiler/src ./compiler/src

WORKDIR /src/presence-gateway
COPY crates/presence-gateway/Cargo.toml crates/presence-gateway/Cargo.lock ./
COPY crates/presence-gateway/src ./src
RUN cargo build --release

# ---- runtime stage -------------------------------------------------------
FROM debian:bookworm-slim AS runtime

LABEL org.opencontainers.image.source="https://github.com/kannamma-labs/nirdosha" \
      org.opencontainers.image.description="nirdosha-presence-gateway: terminates real browser WebSocket connections for workflow's notify() live-push path" \
      org.opencontainers.image.licenses="Apache-2.0"

# `ca-certificates` -- `reqwest`'s `rustls-tls` feature still needs a real
# root store on disk if `--nirdosha-base-url` is ever an `https://` one
# (a cross-namespace/cross-cluster deployment, not just the common
# same-Pod-network `http://` case); costs nothing when it's unused.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10002 --create-home --shell /usr/sbin/nologin presence-gateway

COPY --from=build /src/presence-gateway/target/release/nirdosha-presence-gateway /usr/local/bin/nirdosha-presence-gateway

# No writable state of its own (unlike the main `nirdosha` image's
# `/data` — this process holds nothing on disk, only in memory: the
# per-subject connection registry), so the whole root filesystem can run
# read-only (`securityContext.readOnlyRootFilesystem: true`) with no
# further change needed here, and no `WORKDIR`/writable directory to
# create.
USER presence-gateway

EXPOSE 8090
# `/healthz`/`/readyz` (`gateway.rs::route_connection`) are what a
# Kubernetes Deployment's own probes should point at.
ENTRYPOINT ["nirdosha-presence-gateway"]
CMD ["--help"]
