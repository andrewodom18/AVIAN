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
- Signed, expiring, replay-resistant emergency commands.
- A system ceiling of 25,000 ft MSL (7,620 m), with MSL, AGL, and
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

## Workspace

| Package | Responsibility |
| --- | --- |
| `mesh-core` | Shared messages, identity, command security, altitude rules, and link scoring |
| `mesh-peat` | PEAT Automerge/Iroh node, AVIAN record store, delivery policy, and PACE configuration |
| `vehicle-adapters` | Hardware-neutral ArduPilot, PX4, and Betaflight adapter contract |
| `mesh-sim` | Deterministic failure and recovery simulation |
| `mesh-agent` | Onboard companion-service entry point |

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
The [membership guide](docs/membership.md) shows how a formation is provisioned
without selecting a leader.
