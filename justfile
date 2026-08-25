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

sim-contract:
    node --test --test-concurrency=1 "simulators/mesh-operations/chud-emulator/emulator.test.mjs" "simulators/mesh-operations/validation-contract.test.mjs" "simulators/mesh-operations/visualizer/visualizer.test.mjs"

sim-validation:
    cargo run --quiet -p mesh-sim -- --validate --summary --seed 20260825

peat-validation:
    cargo run --quiet -p mesh-sim -- --validate-peat

# `cargo test --workspace` already runs the workspace documentation tests.
verify: fmt-check clippy test build sim-contract sim-validation peat-validation
