# ARC StreamCaster plugin deployment

The ARC radio plugin is a local PEAT/planning sidecar. It connects to the ARC
Zenoh Unix socket, reads non-secret planning evidence, and uses PEAT for the
durable fleet-plan record. It does not receive a radio management URL or radio
credentials and cannot configure a physical Silvus radio. ARC's guided
swarm-builder may orchestrate normalized CHUD API transactions through
dev-bridge, but ARC never contacts a radio or implements a vendor write.

## CHUD prerequisite

CHUD currently exposes device snapshots through `/api/status` and radio devices
with driver availability through `/api/radio/devices`. The ground-side ARC
bridge uses the latter, but must pin and validate the response shape because
CHUD does not yet publish a unified, versioned ARC/AVIAN inventory-plus-topology
contract. A successful empty device array is a healthy zero-radio bench, not a
failure, and authentication failure must remain visible as reachable rather
than becoming a generic fetch error.

CHUD's discovery core performs passive capture and active heartbeat probing on
one selected interface. Known-address and subnet-probe settings still require
bench confirmation in the running image. Appropriate routes and raw-network
privileges may be required, but the current design does not simultaneously
cover a second USB-Ethernet or RF-facing interface.

Mount approved radio client identities only into CHUD's certificate directory.
CHUD accepts PEM certificate/key pairs, bundled PEM identities, and PKCS#12
bundles. Do not mount those identities into ARC or AVIAN.

See [CHUD integration status and AVIAN boundary](chud-integration-status.md)
for the authority gates and external gaps.

Build the deployment image from the AVIAN repository root:

```sh
docker build -f apps/arc-radio-plugin/Dockerfile -t avian-arc-radio-plugin:latest .
docker save -o avian-arc-radio-plugin.tar avian-arc-radio-plugin:latest
```

Place the tarball in ARC's `infra/images/` before running the normal ARC
deployment. Enable `arc_streamcaster_plugin_enabled` only after provisioning:

- `/etc/arc/radio-evidence/regulatory.json` for the exact frequency/width;
- `/etc/arc/radio-evidence/installations/<profile>.json` for approved antenna
  installation/calibration evidence;
- `/etc/arc/keys/peat-radio.key` as the out-of-band PEAT formation key;
- a protected PEAT/ARC operational data interface.

Start one sidecar per ARC/PEAT node. Set `--source` to its stable ARC device
identity. Supply each intended PEAT relationship with one or more
`--peat-peer NAME=ENDPOINT_ID@IP:PORT[,IP:PORT...]` arguments. The UI marks a PEAT
link connected only when the transport reports that endpoint as connected.
Configured but disconnected peers remain visible as disconnected; no link is
fabricated for an unobserved relationship. Offline peers do not block sidecar
startup; the sidecar retries missing sessions with a bounded connection timeout.

Use [the radio mesh bootstrap command](arc-radio-bootstrap.md) to derive every
endpoint ID and generate bounded, per-host Ansible peer variables without
starting the sidecars first.

The peer address is the PEAT/ARC reachability address, not a StreamCaster
management endpoint. Fresh fused ARC `local/telemetry` supplies node position
when available. Physical RF telemetry comes from the external management API.
Operators must not treat the logical PEAT topology as a radio propagation map.

CHUD owns enrollment, credentials, live capability inspection, physical apply,
verification, confirmation, persistence, audit, and compensating rollback.
ARC owns only the operator workflow and its durable journal. Do not put radio
credentials in AVIAN, ARC canonical configuration, PEAT, compose variables, or
logs.

See [ARC single-plug radio swarm onboarding](arc-radio-swarm-builder.md) for
the sequential workflow, recovery behavior, and the distinction between
configuration readback and RF-topology validation.

## ARC main compatibility

AVIAN may run beside an unmodified ARC main checkout and publish its local
planning and observation topics without changing ARC source. ARC main's fleet
sessions, identification-image proxy, spatial HTTP APIs, and waypoint
on-arrival actions do not change the AVIAN sidecar protocol. Identification
JPEG bytes remain on ARC's addressed Zenoh/HTTP data path and must never be
copied into AVIAN's replicated PEAT document.

ARC main does not currently expose the CHUD-backed `/api/radio/*` routes or the
guided radio-swarm UI described above. Those surfaces remain ticketed
integration work. Starting the sidecar beside ARC main is therefore a valid
transport/coexistence smoke test, not proof that the ARC radio workflow is
available. See [ARC main compatibility](arc-main-compatibility.md).
