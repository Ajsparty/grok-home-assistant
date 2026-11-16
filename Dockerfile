FROM rust:1.82-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

# The image has no microphone. Satellites send audio to this hub.
RUN cargo build --release --no-default-features

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /app/logs /app/data \
    && chown -R nobody:nogroup /app

WORKDIR /app
COPY --from=builder /app/target/release/grok-home-assistant /usr/local/bin/

USER nobody

CMD ["grok-home-assistant"]
