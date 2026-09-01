# One statically linked binary plus the built UI, in a distroless image (spec §10.1).
#
# Two things this file is careful about, because the previous version was not:
#
#   * **The dependency layer is a dependency build, not the wreckage of one.** The older version
#     built a stub `main.rs` to warm the cache and ended the line with `; rm -rf src`, so the
#     `cargo build` in it could fail and the layer would still succeed. It did fail, every time:
#     `build.rs` was not in the context, so cargo could not compile the build script. It got far
#     enough first that most dependencies were cached anyway — which is worse than failing,
#     because what the layer contains then depends on how far a crashing build happened to get,
#     and nothing says so. `cargo-chef` computes a recipe from Cargo.lock and cooks the whole
#     dependency graph deliberately, and `&&` means a failure is a failure.
#     It is `cargo-chef` rather than a BuildKit cache mount because a cache mount lives in one
#     builder instance and is *not* carried by the GitHub Actions cache exporter; a layer is.
#   * **It never reaches the network for content.** `build.rs` refreshes the bundled vocabularies
#     from upstream when they are missing or a day stale. `TAR_VOCAB_OFFLINE=1` pins the build to
#     the bundles committed at this revision: an image built from a tag is then reproducible, and
#     it cannot silently depend on a third-party host being up. A missing bundle is a loud
#     failure rather than a fetch.
#
# `.dockerignore` keeps `target/` and `frontend/node_modules/` out of the context; without it
# every build uploads gigabytes and nothing ever hits cache.

# ----------------------------------------------------------------------------- the UI
FROM node:22-alpine AS frontend
WORKDIR /ui
# Lockfile first: `npm ci` is then cached until a dependency actually changes.
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ----------------------------------------------------------------------------- rust toolchain
# Pinned to bookworm to match the distroless runtime below — both are Debian 12 — because the
# floating tag has already moved on. `rust:1-slim` is Debian 13 (trixie) today, and a binary
# built there does not start on `distroless/cc-debian12`:
#
#     /tar: libstdc++.so.6: version `GLIBCXX_3.4.31' not found (required by /tar)
#     /tar: libc.so.6: version `GLIBC_2.38' not found (required by /tar)
#
# An image that builds and cannot run. Pinning the build base to the runtime's Debian release is
# the fix; if the runtime moves to `cc-debian13`, this line moves with it.
FROM rust:1-slim-bookworm AS chef
ENV CARGO_TERM_COLOR=never
# clang/libclang: oxigraph's RocksDB backend binds its C++ through bindgen.
RUN apt-get update && apt-get install -y --no-install-recommends \
      clang libclang-dev pkg-config && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version ^0.1
WORKDIR /src

# ----------------------------------------------------------------------------- the recipe
# Nothing is compiled here. The output is a description of the dependency graph, which changes
# only when Cargo.toml or Cargo.lock does.
FROM chef AS planner
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# ----------------------------------------------------------------------------- the binary
FROM chef AS build
# See the header: the bundled vocabularies come from this checkout, not from the internet.
ENV TAR_VOCAB_OFFLINE=1
COPY --from=planner /src/recipe.json recipe.json
# ~250 crates, including RocksDB's C++. Cached until a dependency changes.
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock build.rs ./
# shapes/ and migrations/ are compiled *into* the binary — `include_str!` and `sqlx::migrate!` —
# so they are needed here and in no later stage.
COPY shapes ./shapes
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked --bin tar

# ----------------------------------------------------------------------------- the image
FROM gcr.io/distroless/cc-debian12
LABEL org.opencontainers.image.source="https://github.com/MaastrichtU-IDS/tool-artifact-registry" \
      org.opencontainers.image.description="Registry of software, deployments, runs and data artifacts" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /src/target/release/tar /tar
COPY --from=frontend /ui/dist /ui
ENV TAR_DATA_DIR=/data \
    TAR_LISTEN=0.0.0.0:8080 \
    TAR_STATIC_DIR=/ui
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s CMD ["/tar", "healthcheck"]
ENTRYPOINT ["/tar"]
CMD ["serve"]
