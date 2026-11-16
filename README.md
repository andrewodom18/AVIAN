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

## Signed revocation exchange

`avian-revocationd` exchanges administrator-signed updates over an explicitly
bound private-network address. It verifies and saves updates before publishing
the engine trust cache. Use a separate enrolled state directory on each peer.

```sh
target/debug/avian-revocationd \
  --trust-root .local/ground/trust-root.json \
  --membership .local/ground/membership.json \
  --engine-trust .local/ground/engine-trust.json \
  --listen 10.201.0.1:9180 \
  --peer http://10.201.0.2:9180
```

Bind the service to the overlay address. Its read-only endpoint serves public
signed records; it does not expose administration or private keys. Configure
`control_tcp_ports: [9180]` in the experimental engine's startup traffic policy
when signed updates must cross metered links. Ordinary HTTP remains bulk traffic.
The service renews engine leases from verified durable membership independently
of peer availability. Unreachable peers leave the last accepted revision intact.

## Engine tests

See [qualification commands](qualification/README.md) for isolated EasyTier tests.
The experimental engine is separate from the daemon and is not deployed to hardware.
Local budgets bound work and response sizes without imposing a membership ceiling.

## Releases and licensing

AVIAN uses the [Rownd license](LICENSE.md) and preserves
[third-party notices](THIRD_PARTY_NOTICES.md).

The workspace version in `Cargo.toml` is shared by all AVIAN packages. Release
tags follow ARC's prefixed semantic-version format: `avian-v0.1.0`, with optional
prerelease identifiers such as `avian-v0.1.0-rc.1`. The first release is `avian-v0.1.0`.

A push to GitLab `main` runs verification, builds with `AVIAN_RELEASE`, and creates
an immutable tag and GitLab release for that commit. Manual pipelines are accepted
only on `main`; other branches, tag pushes, and scheduled pipelines do not run.
Retries reuse an existing release only when its commit matches. Change the workspace
version on `dev` before promoting a subsequent release; tags are never moved.

```sh
AVIAN_RELEASE=avian-v0.1.0 cargo build --workspace --release --locked
target/release/avianctl --version
```

Local builds without `AVIAN_RELEASE` report a development version with the source
commit and dirty-state marker. The local API exposes the same identity in `/v1/status`.
