# Simulator validation

AVIAN's stakeholder visualizer and its validation harness serve different jobs. The visualizer explains the intended workflow. The validation harness produces deterministic evidence about logical topology, message delivery, partitions, recovery, and the ARC-facing CHUD API contract.

## What is actually simulated

- One ground-control node plus 5, 25, 50, 100, 150, or 200 aircraft nodes.
- A bounded peer graph with no self-links or duplicate links and at most eight direct peers per node.
- Per-hop delivery events whose synthetic delay, jitter, and loss values affect the outcome.
- A ten-percent distributed node outage followed by restoration and connectivity verification.
- CHUD's ARC-facing discovery, snapshot, apply, operation, and confirm API shapes on loopback.
- Radios that share a factory IP but retain distinct identities through canonical MAC addresses.
- Apply failures, timeouts, missing operation IDs, readback mismatches, expired operations, reboot indications, stale devices, and failed authentication.

This does **not** simulate RF propagation, interference, antenna performance, real hardware behavior, airworthiness, or flight-control safety. It performs no radio writes and is not evidence of field validation.

## Run the validation gate

On Windows:

```powershell
.\scripts\Invoke-SimulatorValidation.ps1
```

Reports are written outside the repository to `Downloads\AVIAN-validation`. To reproduce a run, retain the seed and the `event_digest_sha256` values in the report.

Individual contracts:

```powershell
node --test simulators/mesh-operations/chud-emulator/*.test.mjs
cargo test -p mesh-sim --locked
cargo run --quiet -p mesh-sim -- --validate --seed 20260825
```

Add `--summary` to omit the event arrays from console output while retaining
the scenario metrics and deterministic event digests. External evidence
reports intentionally retain the complete event ledger.

## CHUD contract emulator

Start the loopback-only emulator with:

```powershell
node simulators/mesh-operations/chud-emulator/server.mjs
```

It binds only to `127.0.0.1:3212` and implements the contract consumed by ARC:

- `GET /api/radio/devices`
- `GET /api/radio/snapshot?mac=...`
- `GET /api/radio/operations?mac=...`
- `POST /api/radio/apply`
- `POST /api/radio/confirm`

Test-only control routes are under `/__sim`. Every response is marked `simulated: true` and `hardware_write: false`. The emulator never proxies unknown routes. Set `AVIAN_CHUD_EMULATOR_TOKEN` to exercise bearer authentication.

## Evidence interpretation

Passing means the seeded logical scenarios meet their coded invariants and the emulator matches the API contract under test. It does not mean 200 radios have been operated, a 200-aircraft RF network has been fielded, or RF performance has been predicted. The RF-planning simulator's 150-node product limit remains a separate claim and is not raised by this harness.

The next evidence tier is available with `just peat-validation`. It starts bounded 3-, 5-, and 10-node clusters using real PEAT Automerge/Iroh nodes over loopback, then records authenticated connections and payload convergence. The nodes currently share one OS process, so independent-process lifecycle and crash isolation remain a separate future gate. This tier remains distinct from both the deterministic model and hardware validation.
