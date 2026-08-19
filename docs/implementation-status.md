# Fieldable implementation status

This ledger distinguishes implemented/automated behavior from hardware proof.
It is intentionally conservative: the milestone is not complete until every
required field row contains dated, sanitized evidence.

Last automated verification: 2026-08-19 on macOS and Debian Linux
(`dev` at `406e2b3` or later).

## Implemented and automated

| Area | Current evidence |
| --- | --- |
| Baseline | macOS `just verify` passes formatting, warning-free workspace Clippy, workspace tests, doc tests, and locked debug build; Debian Linux locked workspace tests and release build also pass |
| Configuration | Strict versioned/size-bounded TOML, consistent relative path resolution, scalar CLI precedence, peer-list replacement, identifier/count/timing bounds, distinct sockets, unknown-field rejection, production examples |
| Deployment | Locked release build path, prebuilt path, service account/group, preserved live config, capability-free hardened independent systemd units |
| Mesh/status | Stable derived identity across restart, startup node advertisement, ordered multi-address fallback, transition status, owner-only/time- and size-bounded control protocol and record inspection, plus atomic ground-side one-code aircraft pairing with safe corrected-code replacement |
| Payload | Schema-v2 image manifest/detection contracts, source assignment by local agent, metadata-only stardogOS image publication, malformed-event and socket-mode tests |
| Commands | Ed25519 key generation, short record/config lifetime, target/issuer/action/fresh-system-lock checks, atomic nonce/replay state, dry-run path, restart replay suppression, ACK source correlation, five-second total ACK/retry budget, execute-on-SITL-only configuration gate |
| Radio | Read-only StreamCaster client, sanitized API failures, bounded bidirectional probes (including passive Ethernet/ZeroTier operation with vendor APIs disabled), Unix observation socket plus legacy UDP ingress, fail-closed relay geometry/calibration |
| stardogOS regression | `odom-dev` at `966d1e8`: 35 root Python tests and 258 isolated KLV tests pass; absolute imagery paths are excluded from AVIAN/camera failure logs and telemetry CSV output |
| Ground dashboard | Companion `avian-ground-ui` `dev` at `8d78a1b`: loopback-only local ground-agent projection, guided non-secret aircraft-code setup, synchronized 2-second aircraft view, explicit unavailable-position/stale/failsafe warnings, retained last-known data, 10-second status and 30-second bounded event/record polling, background-tab pause, searchable/filterable/paginated events, sanitized logs, clean ARM64 macOS and Linux builds, and persistent macOS LaunchAgents |

## Pi integration snapshot

At `2026-08-19T17:53:45Z`, the connected field setup reported:

- Raspberry Pi 5 Model B Rev 1.1, Debian 13 (`aarch64`), host `melStarDog`;
- stardogOS `odom-dev` at `966d1e8`, clean and synchronized with its remote;
- `avian-mesh-agent`, `avian-link-monitor`, `avian-ground-ui`, MAVProxy,
  MediaMTX, and the Pi camera stream enabled and active;
- the loopback dashboard health endpoint healthy and its projected AVIAN status
  ready;
- a CubeOrange+ present under `/dev/serial/by-id`, with MAVLink locked to
  system ID 1;
- a persistent `odom-mac` ground peer connected to `mel-stardog` over direct
  Ethernet, with bounded probes reporting the selected underlay reachable;
- the Mac loopback Ground UI receiving the real Cube's synchronized 2 Hz
  telemetry without an aircraft HTTP or SSH dependency; and
- a controlled 12-second aircraft-agent interruption that left the page
  available, retained and marked the last sample stale, then recovered to
  `Live` without restarting the Mac services.

This is a sanitized point-in-time operational check and direct-Ethernet proof,
not proof of Silvus/ZeroTier route failover, signed command handling, or flight
acceptance. The Cube currently has no usable GPS/EKF position, power monitor,
or receiver-quality value; Ground correctly warns that position is unavailable
instead of treating the `(0,0)` sentinel as a flight location.

## Required field evidence

| Acceptance run | Status | Evidence required |
| --- | --- | --- |
| Native Pi `just verify` and locked release build | Not run | Pi model/OS/Rust version, commit, UTC time, result; Debian Linux container validation is already recorded separately |
| Linux installer and both AVIAN systemd units on Pi | Partial (2026-08-19) | Both units are enabled and active; ownership/modes and restart identity still require a witnessed acceptance run |
| Ground-dashboard install on operator Mac | Pass (2026-08-19) | Three persistent user LaunchAgents run the ground agent, passive link monitor, and loopback UI; browser/API live, stale, recovery, warning expansion, filtering, and pagination were exercised |
| Pi → Mac real Cube telemetry | Pass on direct Ethernet (2026-08-19) | Stable peer identities, symmetric peer configuration, system-ID-1 MAVLink lock, 2 Hz synchronized samples, selected `ethernet` underlay, and bounded bidirectional probe evidence |
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
