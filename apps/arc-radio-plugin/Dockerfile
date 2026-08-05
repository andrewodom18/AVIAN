FROM rust:1-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY apps/arc-radio-plugin/ apps/arc-radio-plugin/
RUN cargo build --release --locked -p arc-radio-plugin

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/arc-radio-plugin /usr/local/bin/arc-radio-plugin
ENV RUST_LOG=info
ENTRYPOINT ["arc-radio-plugin", "--serve"]
