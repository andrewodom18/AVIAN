# Fieldable implementation status

This ledger distinguishes implemented/automated behavior from hardware proof.
It is intentionally conservative: the milestone is not complete until every
required field row contains dated, sanitized evidence.

Last automated verification: 2026-08-19 on macOS (`dev` at `dbc4ce2` or later).

## Implemented and automated

| Area | Current evidence |
| --- | --- |
| Baseline | macOS `just verify`: formatting, warning-free workspace Clippy, workspace tests, doc tests, and locked debug build pass |
| Configuration | Strict versioned TOML, relative path resolution, scalar CLI precedence, peer-list replacement, unknown-field rejection, production examples |
| Deployment | Locked release build path, prebuilt path, service account/group, preserved live config, hardened independent systemd units |
| Mesh/status | Stable derived identity across restart, startup node advertisement, ordered multi-address peers, transition status, bounded control protocol and record inspection |
| Payload | Schema-v2 image manifest/detection contracts, source assignment by local agent, metadata-only stardogOS image publication, malformed-event and socket-mode tests |
| Commands | Ed25519 key generation, short lifetime, target/issuer/action/system-lock checks, atomic nonce/replay state, dry-run path, restart replay suppression, bounded MAVLink ACK/retry path, execute-on-SITL-only configuration gate |
| Radio | Read-only StreamCaster client, sanitized API failures, bounded bidirectional probes, Unix observation socket plus legacy UDP ingress, fail-closed relay geometry/calibration |
| stardogOS regression | Root Python unit suite and isolated KLV pytest suite pass on macOS |

## Required field evidence

| Acceptance run | Status | Evidence required |
| --- | --- | --- |
| Linux/Pi `just verify` and locked release build | Not run | Pi model/OS/Rust version, commit, UTC time, result |
| Linux installer and both systemd units on Pi | Not run | unit status, ownership/modes, restart identity |
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
