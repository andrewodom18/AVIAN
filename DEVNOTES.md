## Project Spin Up

### Local Rust

RustUp can be installed with the following command:

```zsh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

```zsh
rustup toolchain install 1.91.1
rustup override set 1.91.1
rustc --version
cargo --version
```

### Docker alternative

```zsh
avian_cargo() {
  docker run --rm -v "$PWD:/work" -w /work rust:1.91-bookworm cargo "$@"
}
```


## Build and verify everything

```zsh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo build --release --workspace
```

## Run the deterministic mesh simulation

```zsh
cargo run -p mesh-sim
```

This exercises formation partitioning, a crashed node, recovery, PEAT state convergence, and the scoped Betaflight emergency action.