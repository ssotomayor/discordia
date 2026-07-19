# Discordia gateway server (dioxusfun-server) — standalone deployment image.
#
# Build:  docker build -t discordia-server .
# Run:    docker run -p 9000:9000 -v discordia-data:/data discordia-server
# (Voice needs LiveKit alongside — see docker-compose.yml for the full stack.)

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
# Skip the bundled-LiveKit download in the build script: in containers the SFU
# runs as its own service (see compose), not as a subprocess.
ENV LIVEKIT_BUNDLE_SKIP=1
RUN cargo build --release -p dioxusfun-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/dioxusfun-server /usr/local/bin/discordia-server

ENV DIOXUSFUN_ADDR=0.0.0.0:9000 \
    DIOXUSFUN_DATA_DIR=/data \
    DIOXUSFUN_LIVEKIT_AUTOSPAWN=0
VOLUME /data
EXPOSE 9000
ENTRYPOINT ["discordia-server"]
