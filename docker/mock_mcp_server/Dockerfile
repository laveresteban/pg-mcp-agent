# Multi-stage Dockerfile for building a release binary (kept for CI/production builds)

FROM rust:1.97 AS builder

WORKDIR /usr/src/pg-mcp-agent
COPY . .

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN cargo build --release --bin mock_mcp_server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && update-ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/pg-mcp-agent/target/release/mock_mcp_server /usr/local/bin/mock_mcp_server
WORKDIR /app
ENTRYPOINT ["/usr/local/bin/mock_mcp_server"]
