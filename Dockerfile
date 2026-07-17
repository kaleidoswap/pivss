# Builds both binaries (pivss-server, pivss-client) in one image so the
# client can be run via `docker compose run --rm client ...` with zero local
# Rust toolchain. Heavy dependency tree (breez-sdk-liquid pulls in lwk,
# boltz-client, nostr-sdk, tonic/prost codegen) — the builder stage needs
# protoc + a C toolchain + OpenSSL headers.

FROM rust:1-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release -p pivss-server -p pivss-client

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 pivss
COPY --from=builder /build/target/release/pivss-server /usr/local/bin/pivss-server
COPY --from=builder /build/target/release/pivss-client /usr/local/bin/pivss-client
USER pivss
WORKDIR /data
EXPOSE 8339
ENTRYPOINT ["pivss-server"]
CMD ["--config", "/config/config.toml"]
