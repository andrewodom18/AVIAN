# Fieldable implementation audit

Audit date: 2026-08-19

Audited code: AVIAN `dev` at `6894b54` or later; stardogOS `odom-dev` at
`f841d0e`; AVIAN Ground UI `dev` at `66f57d7`

Plan source: AVIAN Fieldable Implementation Plan supplied by the operator

This audit separates code completeness from field acceptance. No hardware-only
claim is marked complete without dated device evidence.

## Audit outcome

The software implementation covers the plan's defined interfaces and safety
boundaries. The audit corrected the following issues before this report:

- expired PEAT command records and stale MAVLink locks could reach command
  evaluation;
- `COMMAND_ACK` correlation did not verify the MAVLink source system;
- ACK timeout multiplied by retries was not capped as one total budget;
- relative command-state paths did not follow the TOML-directory rule;
- local control reads, writes, response size, and concurrent clients were not
  fully bounded;
- the control and link-observation sockets were writable by the payload group;
- configuration, membership, credential, and identifier inputs lacked several
  practical size/count/uniqueness bounds;
- peer loss could produce two log messages for one transition;
- the v1-reader/v2-writer compatibility rule lacked direct regression coverage;
- stardogOS failure logs and its telemetry CSV could contain absolute imagery
  paths.

All corrected paths are covered by focused tests, the complete macOS
verification suite, and a clean locked Debian Linux container build/test run.
Field acceptance remains open only where the plan requires the physical Pis,
Cube, Silvus radios, ZeroTier/Starshield route, or ArduPilot SITL environment.

## Requirement matrix

| Plan area | Implementation evidence | Automated result | Field state |
| --- | --- | --- | --- |
| Baseline | `just verify` runs format, warning-free Clippy, workspace tests, doc tests, and locked build | Pass on macOS; locked workspace test/release build pass in Debian Linux | Native Pi run required |
| Strict production config | `config.rs`, three production examples, precedence/relative-path/unknown-field/bounds tests | Pass | Provisioned-file review required |
| Installer and services | `deploy/install.sh`, independent capability-free systemd units, preserved config | Shell syntax/build covered; locked release build passes | Pi install/restart required |
| Stable PEAT mesh | derived endpoint identity, startup advertisement, bounded membership topology, tagged ordered addresses | Identity, convergence, v1/v2, and unavailable-first-address fallback pass | Pi partition/reconnect required |
| Status/control | strict versioned JSON, `avianctl`, readiness, record listing, owner-only bounded socket | Protocol, permissions, response-bound tests pass | Operator workflow required |
| stardogOS payload | dedicated MAVProxy output, group-only datagram ingress, schema-v2 manifest/detection, local source assignment | AVIAN tests plus 35 stardogOS tests pass | Real Cube/camera or fixture convergence required |
| Signed RTL | Ed25519 issuer allowlist, atomic replay state, expiry/fresh-lock checks, dry run, SITL-only execute gate | Replay/restart, lifetime, lock, wrong-system ACK, timeout/retry tests pass | Cube no-packet dry run and ArduPilot SITL mode/ACK required |
| Radio monitor | separate read-only service, private credentials, normalized API/probe observations, fail-closed relay geometry | API failure, read-only client, bounded probe, geometry tests pass | SL5200/4200 APIs required |
| Silvus fallback | PEAT receives ordered Silvus then satellite addresses; monitor records reachability transitions | unavailable-first-address fallback and state logic pass | Actual route loss/restoration required |
| Independence/privacy | no JPEG bytes, safe relative references, no Starshield mutation, independent RFD900/video/GPS paths | contract and regression suites pass | Cross-service field observation required |

## Verification run

On 2026-08-19, the audited macOS checkout passed:

```text
just verify
cargo test -p mesh-agent -p mesh-peat -p vehicle-adapters --locked
cargo build --workspace --release --locked
docker run ... rust:1.91-bookworm cargo test --workspace --locked
docker run ... rust:1.91-bookworm cargo build --workspace --release --locked
python3 -m unittest discover -s tests -v          # stardogOS: 35 passed
python3 -m pytest -q                              # KLV: 258 passed
```

The native Pi, real Cube, real radio, route-failover, and SITL rows remain
explicitly `Not run` in the
[implementation-status ledger](implementation-status.md). They cannot be
substituted with simulation or inferred from unit tests.

## Ground UI decision

A lightweight ground UI is warranted. The control protocol already exposes the
right read-only operational data, while requiring operators to mentally merge
JSON and journal output is error-prone during field work. The UI must remain a
separate ground process so a browser or rendering fault cannot affect the mesh
agent.

The approved boundary is:

- read-only status, records, warnings, and sanitized service logs;
- mandatory loopback binding with no non-loopback mode;
- no emergency-command, radio-mutation, Starshield, camera, or flight-control
  endpoint;
- fixed service/log queries only, with bounded output and polling;
- minimum-field projection so unused record payloads and peer addresses never
  cross into the browser API;
- visible stale/offline states instead of fabricated values;
- an independent repository, service, and failure domain.

The companion implementation is in
[andrewodom18/avian-ground-ui](https://github.com/andrewodom18/avian-ground-ui).
Its Rust bridge, React export, exact Host/Origin checks, outbound field
projection, credential/image redaction, systemd sandbox, and transactional
installer pass native checks plus clean ARM64 Debian Rust/Node builds. A
standard repository security scan found three medium browser-boundary issues;
all three were fixed before commit `66f57d7`.
