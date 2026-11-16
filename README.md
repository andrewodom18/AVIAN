# AVIAN

AVIAN provides network membership and routing policy for ground stations, drones,
and gateways. CHUD manages supported radios; ARC owns fleet and flight procedures.

## Build and run

Requires Rust 1.88 or newer. Private-key file storage currently supports Unix.

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
mkdir -p .local
target/debug/avianctl init --state-dir .local/ground --network field-network
target/debug/aviand --config .local/ground/avian.toml
```

From another terminal:

```sh
target/debug/avianctl --config .local/ground/avian.toml status
target/debug/avianctl --help
```

The daemon is observation-only and does not change host networking. Its loopback
API requires the generated bearer token. Keep the state directory private and
out of version control. Membership changes reload automatically; rejected reloads
retain the last verified state and appear in `/v1/status`.

## Local API

- `/v1/status`, `/v1/qualification`: runtime status and network-control availability.
- `/v1/nodes`: paginated verified membership; unmeasured reachability is unknown.
- `/v1/route-preview`: evaluates supplied observations without applying routes.
- `/v1/events`: change notifications; slow readers receive a resync indication.
- `/v1/radios`: read-only CHUD inventory when configured.

`/v1/services` and `/v1/paths` report unavailable runtime integration. Enrollment
does not grant aircraft-control permission.

CHUD credentials come from an owner-only file. HTTPS certificate verification is
enabled; a configured certificate file supports private certificate authorities.
Plain HTTP is accepted only for a loopback CHUD instance.

## Engine tests

See [qualification commands](qualification/README.md) for isolated EasyTier tests.
The experimental engine is separate from the daemon and is not deployed to hardware.
Local budgets bound work and response sizes without imposing a membership ceiling.
