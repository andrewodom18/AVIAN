set dotenv-load := false
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Keep these recipes as the shared contract between local development and CI.
fmt-check:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo test --workspace --locked

test-docs:
    cargo test --workspace --doc --locked

build:
    cargo build --workspace --locked

build-release:
    cargo build --workspace --release --locked

docker-build:
    docker build --file apps/arc-radio-plugin/Dockerfile --tag avian-arc-radio-plugin:ci .

# `cargo test --workspace` already runs the workspace documentation tests.
verify: fmt-check clippy test build
