# Simulator validation

AVIAN's stakeholder visualizer and its validation harness serve different jobs. The visualizer explains the intended workflow. The validation harness produces deterministic evidence about logical topology, message delivery, partitions, recovery, and the ARC-facing CHUD API contract.

## What is actually simulated

- One ground-control node plus 5, 25, 50, 100, 150, or 200 aircraft nodes.
- A bounded peer graph with no self-links or duplicate links and at most eight direct peers per node.
- One deterministic event timeline shared by all messages, node/link faults and directed transmission queues. Rates and packet sizes determine serialization delay; queues use bounded tail-drop, retries are bounded per hop, and TTL applies from creation through delivery.
- Synthetic latency/jitter, loss, duplicate delivery and reordering affect state propagation. Deduplication prevents repeated model-command execution; older generations cannot overwrite newer state.
- A ten-percent contiguous aircraft outage at 5,000 simulated milliseconds, restoration at 10,000, and explicit latest-state repair. Recovery measures completion of generation-two propagation after restoration, not graph connectivity.
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

Individual contracts (Node 20+ and the repository Rust toolchain):

```powershell
just sim-fast
cargo test -p mesh-sim --locked
cargo run --quiet -p mesh-sim -- --validate --seed 20260825
```

Add `--summary` to omit the event arrays from console output while retaining
the scenario metrics and deterministic event digests. External evidence
reports intentionally retain the complete event ledger. Summary reports set
`events_included: false`; the full-evidence validator rejects summaries by default.

### Report v2 and fault matrix

The six baseline sizes are retained in `scenarios`. `fault_scenarios` adds queue
congestion, total loss, duplication/reordering and TTL expiry at each size. An
adverse scenario passes only when its expected adverse outcome is observed:
100% loss requires zero deliveries and bounded retry exhaustion, for example.
Baseline delivery must account for every message, with at most the explicitly
isolated recipients' updates lost. Healthy generation one and post-restoration
generation two must converge within their fixed 5,000 ms fixture budgets.

Report v2 includes the exact links, message inputs, fault schedule, queue policy,
seed, event sequence and digest. Delivery latency percentiles are separate from
generation convergence and recovery. An unfinished recovery is `null`, never a
fabricated duration. `latest_generation_converged` reports replicated state,
not RF connectivity. `queue_drops` counts rejected transmission attempts;
`messages_dropped` counts unique messages with terminal failure. V1 reports stay
historical: their old convergence percentile fields represented delivery
latency and are not silently converted to new measurements.

The strict event/report schemas are in `simulators/mesh-operations/schemas/`.
The consumer validates nested types, unknown fields, message accounting,
chronological event order and complete event digests. The default model uses
three initial messages per aircraft plus an update during the outage and repair
after restoration. Link-cut behavior has a separate focused fixture. No model
command counter sends an aircraft command.

### Isolated checks when PEAT is unavailable

`just sim-model-isolated` tests the exact model source in a temporary external
Cargo package with the same direct dependency version constraints. It resolves
an independent offline lockfile and saves it with source hashes, logs and strict
reports for seeds 20260825, 20260929 and 43. Set `AVIAN_VALIDATION_OUTPUT` to an
external directory; the default is `~/Downloads/AVIAN-validation`.

This is partial model evidence, **not** a locked workspace build or real PEAT
validation. The repository's Cargo.lock and PEAT pin remain unchanged.
`just verify` still requires the locked workspace and PEAT gates. The full
cross-repository workflow and separate-process PEAT lifecycle remain pending;
neither can be inferred from the fast/isolated checks.

## CHUD contract emulator

For the attended Windows walkthrough and checkpoint recorder, see
[ARC emulator PowerShell walkthrough](arc-emulator-walkthrough.md). Its startup
check currently records BLOCKED because ARC mock Fleet does not initialize the
radio-control client; this is an outstanding integration prerequisite.

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

For restart tests, set `AVIAN_CHUD_EMULATOR_STATE_FILE` to a file inside an
explicit disposable directory created by the caller. Operations, device state,
configuration, faults, confirmation deadlines and the ledger are atomically
saved with a checksum. The token is not persisted. Corrupt state fails startup;
storage failure rolls back the in-memory mutation. This proves process-crash
behavior, not power-loss durability on arbitrary filesystems.

`POST /__sim/control` accepts one action per request:

| Payload | Effect |
| --- | --- |
| `{"fault":"readback_mismatch","mac":"00:1e:3f:20:9a:10"}` | Per-device fault; omit MAC for the legacy global fault. Use null to clear. |
| `{"device":{"mac":"00:1e:3f:20:9a:10","state":"rebooting"}}` | Persist management lifecycle state; `connected` restores visibility. Also supports unplugged/stale/authentication_failed, driver availability and capability metadata. |
| `{"advance_ms":31000}` | Advance the simulated clock and expire unconfirmed operations. |
| `{"response_delay_ms":1000}` | Delay mutation responses by a bounded duration after side effects. |
| `{"barrier":"side_effect_recorded"}` | Pause the next mutation at an exact boundary. Other phases: request_accepted and before_response. |
| `{"release":"barrier-1"}` | Release the pending barrier ID returned by GET control. |

The `accepted_response_lost` fault applies/persists the operation and closes the
response connection. Query operations to reconcile it; a second apply on the
same unconfirmed device returns 409. Confirmation is idempotent. Deadline expiry
restores the prior configuration and records an explicit rollback with its
lifecycle cause. Readiness/reads can observe expiry; this legitimate rollback
is distinct from initiating an operator apply.

Barriers have deadlines and do not survive restart. Tests kill actual emulator
children at each barrier and inspect the persisted operation after restart.
These tests do not prove ARC's recovery logic. The server enforces explicit
loopback binding, bounded request bytes, connections, pending mutations,
operation storage and ledger storage. Unknown routes never proxy elsewhere.

## Evidence interpretation

Passing means the seeded logical scenarios meet their coded invariants and the emulator matches the API contract under test. It does not mean 200 radios have been operated, a 200-aircraft RF network has been fielded, or RF performance has been predicted. The RF-planning simulator's 150-node product limit remains a separate claim and is not raised by this harness.

The next evidence tier is available with `just peat-validation`. It starts bounded 3-, 5-, and 10-node clusters using real PEAT Automerge/Iroh nodes over loopback, then records authenticated connections and payload convergence. The nodes currently share one OS process, so independent-process lifecycle and crash isolation remain a separate future gate. This tier remains distinct from both the deterministic model and hardware validation.

As of September 29, 2026, the restored build cannot resolve the pinned
`peat-mesh =0.9.0-rc.60`. [AVIAN #47](https://github.com/andrewodom18/AVIAN/issues/47)
tracks adopting DU's refactored release once published. That future migration
does not waive #43's remaining ARC/ArcUI and independent-process acceptance.
