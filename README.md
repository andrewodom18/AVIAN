# AVIAN

**Autonomous Vehicle Interoperability and Networking**

An experimental, leaderless UAV coordination framework built around
[PEAT](https://github.com/defenseunicorns/peat). Every aircraft runs the same
Linux companion service. Ground and cloud services join as ordinary peers and
are not required for continued operation.

## v0.1 scope

- ArduPilot and PX4: telemetry, navigation tasks, and emergency control.
- Betaflight: mesh telemetry and emergency control only.
- Leaderless, bounded-degree topology planning for 5-200 aircraft with no
  all-to-all peer requirement.
- Versioned membership manifests from which every aircraft independently
  selects its bounded PEAT neighbors.
- Automatic multi-link selection with redundant emergency delivery.
- Source-rate-limited telemetry and radio observations, plus rotating compact
  swarm summaries so the normal operator feed is not every drone's detailed
  stream.
- Signed, expiring, replay-resistant emergency commands.
- Deterministic pre-mission relay reservation plus live-observation relay
  chaining, range discovery, and exact manual relay overrides.
- A system planning ceiling of 30,000 ft MSL (9,144 m), with MSL, AGL, and
  above-launch altitude kept separate.
- Deterministic four-node simulation covering partitions, a crashed node,
  recovery, state convergence, and a Betaflight emergency action.

The implementation now includes both the deterministic simulator and a real
PEAT Automerge/Iroh peer with formation authentication, stable identity,
persistent state, and static peer bootstrap. `mesh-agent` can ingest live
ArduPilot/PX4 MAVLink telemetry over UDP or TCP; direct serial is an optional
build feature. Silvus StreamCaster is modeled as an IP MANET underlay, and each
PEAT peer can have multiple underlay addresses for reconnection across Silvus,
Wi-Fi, cellular, or other paths. Emergency flight-controller output and live
radio-specific metrics are not implemented yet.

When supplied a shared relay-runtime configuration, `mesh-agent` also combines
mesh telemetry and relay-link observations into leaderless in-flight chain
decisions. See the [ARC relay-planning guide](docs/arc-ui-relay-planning.md)
and [runtime configuration sample](examples/relay-runtime-config.sample.json).

## Workspace

| Package | Responsibility |
| --- | --- |
| `mesh-core` | Shared messages, identity, command security, altitude rules, link scoring, and leaderless relay decisions |
| `mesh-peat` | PEAT Automerge/Iroh node, AVIAN record store, delivery policy, and PACE configuration |
| `vehicle-adapters` | Hardware-neutral ArduPilot, PX4, and Betaflight adapter contract |
| `mesh-sim` | Deterministic failure and recovery simulation |
| `mesh-agent` | Onboard companion-service entry point |
| `mission-planner` | ARC UI JSON engine for pre-mission corridors and in-flight relay decisions |
| `arc-radio-plugin` | Arc-authoritative StreamCaster validation, traffic assessment, PEAT encoding, and dry-run API sequencing |

## ARC and StreamCaster integration

ARC remains the authority for desired radio configuration. The AVIAN
`arc-radio-plugin` validates that configuration, reads the effective state of
the locally attached radio, publishes live observations to ARC, and
synchronizes accepted configuration and mesh state through PEAT. It does not
write ARC's canonical configuration or communicate with a flight controller.

The current integration provides:

- StreamCaster 4200, 4400, and SL5200-family profiles, including capability-
  checked 5, 10, and 20 MHz operation;
- live radio identity, frequency, bandwidth, network, power, firmware, and
  direct-neighbor signal observations when the radio reports them;
- real node enrollment and connected PEAT-peer inventory without generated
  nodes, estimated spacing, or fabricated links;
- a network capacity qualification target of at least 150 nodes, which is not
  treated as a minimum active node count; and
- guarded validate, activate, confirm, and rollback operations. Hardware apply
  is disabled by default and confirmation is required before volatile settings
  are persisted.

Zero connected radios is a valid local state. ARC should show an empty live
mesh instead of treating the absence of hardware as a service failure.

Validate the integration without radio hardware:

```sh
cargo test --workspace
cargo run -p arc-radio-plugin -- \
  --input examples/arc-radio-plugin-request.sample.json
```

On Linux, or in an ARC development container with the ARC Zenoh socket mounted,
the sidecar can use its in-process StreamCaster simulator:

```sh
cargo run -p arc-radio-plugin -- \
  --serve \
  --simulate-radio \
  --source avian/local-sim \
  --zenoh-endpoint unixsock-stream//run/arc/zenoh.sock
```

The complete local ARC application also requires ARC `comms`, Link Manager,
`dev-bridge`, and the ARC UI. Docker Engine or Docker Desktop with Linux
containers is the recommended Windows development environment. See the
[radio-plugin guide](docs/arc-radio-plugin.md) and
[deployment guide](docs/arc-radio-deployment.md) before connecting or changing
physical radios.

## Run

With Rust 1.91.1 installed:

```sh
cargo test --workspace
cargo run -p mesh-sim
cargo run -p mesh-agent -- --help
```

With Docker:

```sh
docker run --rm -v "$PWD:/work" -w /work rust:1.91-bookworm cargo test --workspace
docker run --rm -v "$PWD:/work" -w /work rust:1.91-bookworm cargo run -p mesh-sim
```

See [the architecture](docs/architecture.md) and
[message contract](docs/message-contract.md) for the current design boundary.
The [scalability contract](docs/scalability.md) describes the 5-200 aircraft
overlay and what remains to validate on real radios.
The [local PEAT demonstration](docs/peat-local-demo.md) starts two real peers.
The [MAVLink guide](docs/mavlink.md) connects ArduPilot or PX4 telemetry.
The [Silvus integration guide](docs/silvus.md) defines the current radio
boundary and multi-underlay peer format.
The [Arc radio-plugin guide](docs/arc-radio-plugin.md) defines the local
Arc-to-AVIAN ownership boundary and the 4200/4400/5200 configuration contract.
The [radio mesh bootstrap guide](docs/arc-radio-bootstrap.md) generates
deterministic PEAT identities and ARC-ready bounded peer maps before surrogate
deployment.
The [radio integration evidence checklist](docs/radio-integration-evidence.md)
lists the radio, antenna, airframe, and traffic facts required before live
hardware apply or mission-ready range claims.
The [membership guide](docs/membership.md) shows how a formation is provisioned
without selecting a leader.
The [ARC UI relay-planning guide](docs/arc-ui-relay-planning.md) defines
automatic relay reservation, manual overrides, and individual/group tasks.
The [traffic-management guide](docs/traffic-management.md) defines routine,
priority, radio-observation, and operator-summary traffic bounds.
