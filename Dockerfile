
# Dependencies get a layer of their own, keyed on the manifests alone, so a
# source change rebuilds only the workspace crates on top of it.
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ENV LIVEKIT_BUNDLE_SKIP=1
# The recipe leaves out the `[patch]` crates, which the workspace excludes.
COPY vendor vendor
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release -p discordia-server --recipe-path recipe.json
COPY . .
RUN cargo build --release -p discordia-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/discordia-server /usr/local/bin/discordia-server

ENV DISCORDIA_ADDR=0.0.0.0:9000 \
    DISCORDIA_QUIC_PORT=9001 \
    DISCORDIA_DATA_DIR=/data \
    DISCORDIA_LIVEKIT_AUTOSPAWN=0
VOLUME /data
EXPOSE 9000 9001/udp
ENTRYPOINT ["discordia-server"]
