FROM rust:1.91.1-bookworm@sha256:c1e5f19e773b7878c3f7a805dd00a495e747acbdc76fb2337a4ebf0418896b33 AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY apps/ apps/
RUN cargo build --release --locked -p arc-radio-plugin

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && groupadd --system avian \
    && useradd --system --gid avian --home-dir /nonexistent --shell /usr/sbin/nologin avian \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder --chown=avian:avian /build/target/release/arc-radio-plugin /usr/local/bin/arc-radio-plugin
ENV RUST_LOG=info
USER avian
ENTRYPOINT ["arc-radio-plugin"]
CMD ["--serve"]
