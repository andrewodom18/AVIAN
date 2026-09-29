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

sim-deps:
    npm ci --prefix simulators/mesh-operations --ignore-scripts --no-audit --no-fund

# Node-only emulator/schema gates; no PEAT acceptance claim.
sim-fast: sim-deps
    npm run --prefix simulators/mesh-operations test:fast

sim-contract: sim-fast
    node --test --test-concurrency=1 "simulators/mesh-operations/validation-contract.test.mjs" "simulators/mesh-operations/visualizer/visualizer.test.mjs"

# Partial, external-package unit evidence while PEAT resolution is blocked.
sim-model-isolated: sim-deps
    node scripts/Test-SimulatorModel.mjs

sim-validation:
    cargo run --locked --quiet -p mesh-sim -- --validate --summary --seed 20260825

peat-validation:
    cargo run --locked --quiet -p mesh-sim -- --validate-peat

docker-smoke: docker-build
    docker run --rm --entrypoint /bin/sh avian-arc-radio-plugin:ci -c 'id -un | grep -x avian'
    docker run --rm avian-arc-radio-plugin:ci --help

audit:
    cargo audit

feature-check:
    cargo test --workspace --all-features --locked
    cargo build --workspace --all-features --release --locked

# The current workspace baseline is 77.48% line coverage. Keep a small margin
# for platform-specific instrumentation while preventing material regression.
coverage:
    cargo llvm-cov --workspace --locked --summary-only --fail-under-lines 75

# Vendor identifiers cross process and repository boundaries in topics and
# records. Keep this critical, fully-tested mutation scope bounded for CI.
mutate-radio:
    cargo mutants --package mesh-core --file crates/mesh-core/src/vendor_radio.rs --re 'RadioVendorId::as_str|validate_token' --baseline skip

powershell-quality:
    pwsh -NoLogo -NoProfile -File scripts/ci/Test-PowerShell.ps1

# `cargo test --workspace` already runs the workspace documentation tests.
verify: fmt-check clippy test build sim-contract sim-validation peat-validation
