# AVIAN

AVIAN provides authenticated connectivity, relay forwarding, and traffic policy
for ground stations, drones, and gateways. CHUD configures supported radios; ARC
owns fleet and aircraft procedures. The first release supports IP connections.

## Build and run

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
mkdir -p .local
target/debug/avianctl init --state-dir .local/ground --network field-network
target/debug/aviand --config .local/ground/avian.toml
```

Requires Rust 1.88 or newer. Android builds require Rust 1.95 and the dependencies
in [Android setup](platforms/android/README.md). Initialization creates protected
state and a loopback API token. The daemon starts in observation-only mode.

```sh
target/debug/avianctl --config .local/ground/avian.toml status
target/debug/avianctl --config .local/ground/avian.toml --help
```

Keep state directories private and out of version control. Unix storage rejects
unsafe ownership, writable directories, links, and identity replacement. Windows
secret storage uses CurrentUser DPAPI and owner ACLs; native Windows qualification
has not passed. Android wraps secrets through Android Keystore.

## Runtime and policy

An explicit `[runtime]` configuration enables owned engine start/stop. Disposable
Linux namespace mode requires an existing namespace different from the manager's.
Native Linux mode requires the scoped network helper and an unprivileged manager;
see [Linux service setup](packaging/linux/README.md). macOS and Windows native
runtime adapters remain unavailable. The Android runtime is a library component.

The supervisor checks pinned engine sources, ordered patches, binary hashes,
shared policy code, signed membership, and transport identity before startup.
Native administration uses a private authenticated RPC channel. Runtime startup
files are removed after readiness; persistent identities remain protected.

Each traffic class selects an eligible topology-wide path and physical connection.
Control and telemetry prioritize reliability and latency. Video uses a fresh,
authenticated delivered-rate lower bound; unknown-capacity unmetered fallback is
reported as degraded. Probe evidence does not measure spare bandwidth. Stale
measurements expire. Healthy improvements wait five seconds; failed or revoked
paths are removed immediately. Video and bulk on metered connections require
explicit permission. Unknown metering restricts both classes.

Owned bootstrap endpoints and local discovery provide connection hints, never
membership authority. No public discovery fallback is enabled in the managed
profile. Local interface changes reconcile bound TCP/UDP connectors and listeners.
Discovery advertises only confirmed listeners. Connection, page, and probe budgets
bound local resource use without imposing a mesh membership ceiling.

## Local API

All administration endpoints require the generated bearer token. Browser-origin
requests are rejected. The loopback API is intended for the local ARC bridge.

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/status`, `/v1/qualification`, `/v1/runtime` | Runtime identity and qualification scope |
| `POST /v1/network-control` | Typed `{"action":"start"}` or `{"action":"stop"}` |
| `GET /v1/nodes` | Verified membership; enrollment alone does not establish reachability |
| `GET /v1/connections` | Observed local interfaces and metering |
| `GET /v1/paths`, `/v1/links` | Paged live class routes and connection evidence |
| `POST /v1/link-probes` | Bounded, authenticated measurement request |
| `POST /v1/route-preview` | Evaluate supplied observations without applying routes |
| `GET /v1/events` | Change notifications with resynchronization for slow readers |
| `GET`, `POST /v1/services` | Read owner-signed services or publish local endpoints |
| `GET /v1/radios` | CHUD inventory when configured |

Path and link pages accept `after` and `limit`; paths also accept `traffic_class`.
CLI commands include `runtime`, `connections`, `paths`, `links`, `probe`, and
`services`; use `--help` for their arguments. Service announcements expire, retain
owner signatures through relays, and accept only the owner's enrolled overlay
addresses. An advertised endpoint does not establish application reachability or
aircraft-control permission.

Configure `[services]` with a private `state_file` and optional owner
`identity_key_file`. `[service_exchange]` binds a read-only announcement endpoint
to the local overlay address and accepts explicit enrolled peers. Its TCP port
must be listed in `runtime.control_tcp_ports` when exchange crosses metered links.

CHUD credentials use a protected file. HTTPS verifies certificates; a private CA
file is supported. Plain HTTP is limited to loopback. Radio configuration is off
by default. With `chud.allow_configuration = true`, `/v1/radios/apply` requires
explicit disruption acknowledgement and always defers persistence;
`/v1/radios/confirm` requires a separate persistence acknowledgement. Ambiguous
writes are not retried automatically.

## Signed revocation exchange

```sh
target/debug/avian-revocationd \
  --trust-root .local/ground/trust-root.json \
  --membership .local/ground/membership.json \
  --engine-trust .local/ground/engine-trust.json \
  --listen 10.201.0.1:9180 \
  --peer http://10.201.0.2:9180
```

Bind to the assigned overlay address and add `9180` to `runtime.control_tcp_ports`.
The service verifies and durably saves administrator-signed updates before
publishing engine leases. Unreachable peers retain the last verified revision;
rollback is rejected. Read-only exchange exposes no administration or private keys.

## Qualification and releases

[Qualification commands](qualification/README.md) run inside disposable Linux
namespaces. [Android checks](platforms/android/README.md) distinguish actual native
engine tests from compiler and platform API probes. No test command deploys to ARC
or Tank. Native macOS, native Windows, physical Android Wi-Fi/cellular operation,
and cross-platform hardware integration remain unqualified.

The shared workspace version uses ARC's prefixed semantic tags. The first release
is `avian-v0.1.0`. Only pushes or manual runs targeting GitLab `main` run CI.
Publication requires all four platforms' exact installed artifacts and passing
native and hardware evidence; missing evidence prevents tag creation. See
[release commands](tools/RELEASE.md). Local builds report the source commit and
dirty marker; qualified release builds use `AVIAN_RELEASE=avian-v0.1.0`.

AVIAN uses the [Rownd license](LICENSE.md) and preserves
[third-party notices](THIRD_PARTY_NOTICES.md).
