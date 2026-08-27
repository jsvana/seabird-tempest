# Build stage. protoc is used instead of the protobuf-sys package because it
# greatly cuts down on build times (same approach as seabird-ham).
FROM rust:1-bookworm AS builder
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*

# Cache dependencies separately from source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/seabird-tempest target/release/deps/seabird_tempest*

COPY src ./src
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/seabird-tempest /usr/local/bin/seabird-tempest
ENV RUST_LOG=seabird_tempest=info

CMD ["seabird-tempest"]
