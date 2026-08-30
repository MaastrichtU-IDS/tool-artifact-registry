# One statically linked binary plus the built UI, in a distroless image (spec §10.1).

FROM node:22-alpine AS frontend
WORKDIR /ui
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
      clang libclang-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Cache the dependency build across source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --release --quiet ; rm -rf src
COPY src ./src
COPY migrations ./migrations
COPY shapes ./shapes
RUN touch src/main.rs && cargo build --release --locked

FROM gcr.io/distroless/cc-debian12
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
