# Build stage
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build --release -p agentbook-host

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/agentbook-host /usr/local/bin/agentbook-host

# Username directory persists here
VOLUME /var/lib/agentbook-host

EXPOSE 50100

ENTRYPOINT ["agentbook-host"]
CMD ["--listen", "0.0.0.0:50100"]
