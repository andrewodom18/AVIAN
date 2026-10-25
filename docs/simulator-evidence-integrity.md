# Simulator recovery and evidence integrity

The emulator validates persisted transaction history as well as its checksum. Every operation needs one apply event; a terminal confirm or expiry rollback must follow that apply, agree with the saved operation state, and produce the saved device configuration. Sequences are contiguous, identities agree, and duplicate or missing events cannot be silently repaired. Valid version-1 state remains supported. State envelopes are limited to 32 MiB; a failed save preserves the previous in-memory state. Atomic file replacement and process-crash tests do not establish power-loss durability.

The v2 report consumer checks matrix coverage, aggregate verdict consistency, graph and message identities, and full-event accounting. It checks terminal outcomes, delivery ratio/latencies, command counts, fault coverage and reported recovery-time relationships. A recomputed digest does not bypass these checks. This validates evidence consistency; it does not independently reconstruct every internal transport queue or prove that a report came from a particular executable. Keep executable/source manifests with reports.

Truthful failed reports remain readable. Full event evidence is required by default. An explicit `requireEvents: false` permits a summary with no events, but cannot establish full qualification. Consumers must still check `passed` after validation. The v2 wire format is unchanged; previously accepted contradictory documents are now rejected.

## Selected transport mutations

Create an external evidence directory and set `AVIAN_VALIDATION_OUTPUT` to its absolute path, then run:

```sh
just sim-mutations-isolated
```

The runner requires Rust 1.91.1 and cached dependencies satisfying the workspace constraints. It uses exact source copies in a temporary package with an independent offline lockfile, and never edits the workspace lockfile or source. It requires an unmodified baseline before testing 12 selected faults: queue/retry bounds, command/generation updates, duplicate rejection, TTL boundary, bandwidth serialization, link/node outages, stale-generation rejection, convergence timing and delivered-message accounting.

Each patch must match its expected source exactly. Each mutant must compile before its named behavior test runs. Only a test assertion failure counts as caught; survivors, compile failures, timeouts and infrastructure failures remain separate and prevent a passing overall result. The evidence directory retains source/runner hashes, lockfile, exact replacements, logs and dispositions. This is selected mutation evidence, not the full `cargo-mutants` gate, locked workspace, production PEAT, ARC or hardware qualification.

The Node recovery/report tests run through the existing `test:fast` CI job. The isolated mutation recipe remains a local gate until its dependency cache and runtime are qualified on a clean CI runner; a warm-cache Mac result does not establish that readiness.
