# Fieldable implementation status

This ledger distinguishes implemented/automated behavior from hardware proof.
It is intentionally conservative: the milestone is not complete until every
required field row contains dated, sanitized evidence.

Last automated verification: 2026-08-19 on macOS and Debian Linux
(`dev` at `6894b54` or later).

## Implemented and automated

| Area | Current evidence |
| --- | --- |
| Baseline | macOS `just verify` passes formatting, warning-free workspace Clippy, workspace tests, doc tests, and locked debug build; Debian Linux locked workspace tests and release build also pass |
| Configuration | Strict versioned/size-bounded TOML, consistent relative path resolution, scalar CLI precedence, peer-list replacement, identifier/count/timing bounds, distinct sockets, unknown-field rejection, production examples |
| Deployment | Locked release build path, prebuilt path, service account/group, preserved live config, capability-free hardened independent systemd units |
| Mesh/status | Stable derived identity across restart, startup node advertisement, ordered multi-address fallback, transition status, owner-only/time- and size-bounded control protocol and record inspection |
| Payload | Schema-v2 image manifest/detection contracts, source assignment by local agent, metadata-only stardogOS image publication, malformed-event and socket-mode tests |
| Commands | Ed25519 key generation, short record/config lifetime, target/issuer/action/fresh-system-lock checks, atomic nonce/replay state, dry-run path, restart replay suppression, ACK source correlation, five-second total ACK/retry budget, execute-on-SITL-only configuration gate |
| Radio | Read-only StreamCaster client, sanitized API failures, bounded bidirectional probes, Unix observation socket plus legacy UDP ingress, fail-closed relay geometry/calibration |
| stardogOS regression | `odom-dev` at `f841d0e`: 35 root Python tests and 258 isolated KLV tests pass; absolute imagery paths are excluded from AVIAN/camera failure logs and telemetry CSV output |
| Ground dashboard | Companion `avian-ground-ui` `dev` at `66f57d7`: loopback-only read-only field projection, sanitized bounded logs, explicit degraded states, clean ARM64 Rust/Node builds, transactional installer, systemd exposure score 2.7 (`OK`) |

## Required field evidence

| Acceptance run | Status | Evidence required |
| --- | --- | --- |
| Native Pi `just verify` and locked release build | Not run | Pi model/OS/Rust version, commit, UTC time, result; Debian Linux container validation is already recorded separately |
| Linux installer and both systemd units on Pi | Not run | unit status, ownership/modes, restart identity |
| Ground-dashboard install on field device | Not run | UI unit status, exact loopback URL, live/degraded screenshots, no impact on mesh service |
| Pi → Mac real Cube telemetry | Not run | sanitized telemetry/status samples and versions |
| Pi → Mac image-manifest convergence | Not run | manifest fields/hash comparison; no bytes/absolute path |
| Pi → Pi service startup, identity, telemetry, partition/reconcile | Not run | before/after endpoint IDs and transition timestamps |
| Real Cube signed RTL dry-run with no command packet | Not run | ACK plus independent MAVLink capture |
| ArduPilot SITL RTL and correlated ACK | Not run | SITL version/mode transition/ACK/restart replay result |
| SL5200 ↔ 4200-series read-only observations | Not run | models, firmware, API freshness, neighbors, RF/probe metrics |
| Silvus loss → ZeroTier-over-Starshield reconnect | Not run | disconnect/fallback/recovery times and selected underlay |
| Silvus restoration without regressions | Not run | AVIAN, payload, GPS Guard, RFD900, and video results |

## Evidence record template

Append one sanitized record per run:

```text
Run:
UTC start/end:
Operator/site:
AVIAN commit/branch:
stardogOS commit/branch:
Host OS and Rust/Python versions:
Cube/SITL version:
Radio models/firmware:
Topology and underlays (no secrets):
Steps:
Expected:
Observed:
Artifacts or log references:
Result: PASS | FAIL | BLOCKED
Follow-up:
```

## Explicitly outside this milestone

JPEG transfer, mission execution beyond signed RTL, dynamic in-flight
membership, make-before-break handoff, hardware RTL, AVIAN transport over
RFD900, and Starshield terminal/GPS mutation remain future work.
