# Fieldable implementation status

This ledger distinguishes implemented/automated behavior from hardware proof.
It is intentionally conservative: the milestone is not complete until every
required field row contains dated, sanitized evidence.

Last automated verification: 2026-08-19 on macOS and Debian Linux
(`dev` at `64f1e23` or later).

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
| stardogOS regression | `odom-dev` at `966d1e8`: 35 root Python tests and 258 isolated KLV tests pass; absolute imagery paths are excluded from AVIAN/camera failure logs and telemetry CSV output |
| Ground dashboard | Companion `avian-ground-ui` `dev` at `22c7311`: loopback-only read-only field projection, expandable warnings, 10-second status and 30-second bounded event/record polling, background-tab pause, searchable/filterable/paginated events, sanitized logs, explicit degraded states, clean ARM64 Rust/Node builds, transactional installer, systemd exposure score 2.7 (`OK`) |

## Pi integration snapshot

At `2026-08-19T17:20:20Z`, the connected field device reported:

- Raspberry Pi 5 Model B Rev 1.1, Debian 13 (`aarch64`), host `melStarDog`;
- stardogOS `odom-dev` at `966d1e8`, clean and synchronized with its remote;
- `avian-mesh-agent`, `avian-link-monitor`, `avian-ground-ui`, MAVProxy,
  MediaMTX, and the Pi camera stream enabled and active;
- the loopback dashboard health endpoint healthy and its projected AVIAN status
  ready;
- a CubeOrange+ present under `/dev/serial/by-id`, with MAVLink locked to
  system ID 1.

This is a sanitized point-in-time operational check, not proof of restart,
ground-peer transport, radio, route-failover, signed-command, or flight
acceptance. The installed dashboard assets predate `22c7311`; the updated event
monitoring build still requires deployment and operator visual acceptance.

## Required field evidence

| Acceptance run | Status | Evidence required |
| --- | --- | --- |
| Native Pi `just verify` and locked release build | Not run | Pi model/OS/Rust version, commit, UTC time, result; Debian Linux container validation is already recorded separately |
| Linux installer and both AVIAN systemd units on Pi | Partial (2026-08-19) | Both units are enabled and active; ownership/modes and restart identity still require a witnessed acceptance run |
| Ground-dashboard install on field device | Partial (2026-08-19) | Service and loopback health/status pass; deploy `22c7311`, capture live/degraded screenshots, and witness no mesh impact |
| Pi → Mac real Cube telemetry | Partial (2026-08-19) | CubeOrange+ and local system-ID-1 MAVLink lock pass; a configured Mac/ground peer and end-to-end sample remain required |
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
