# ARC StreamCaster plugin deployment

The ARC radio plugin is a local PEAT/planning sidecar. It connects to the ARC
Zenoh Unix socket, reads non-secret planning evidence, and uses PEAT for the
durable fleet-plan record. It does not receive a radio management URL or radio
credentials and cannot configure a physical Silvus radio. Operators perform
physical transactions in CHUD's own UI/API; ARC only links to that UI.

## CHUD prerequisite

The ground-side ARC bridge reads `GET /api/radio/devices` from CHUD. A
successful empty `devices` array is a healthy zero-radio bench, not a failure.
CHUD discovery state is translated into vendor-neutral ARC topology; in
particular, `auth-failed` remains visible as an online/reachable radio whose
management access requires a client certificate.

For a known TrellisWare factory address, prefer a static CHUD probe such as
`tw_probe: 10.1.0.2`. Bridge-mode subnet scans can use
`tw_probe_subnet: 10.1.0.0/16`, but require appropriate local routes/interfaces
and raw-network privileges. Linux CHUD deployments performing full passive or
off-subnet discovery require root or `CAP_NET_RAW` plus `CAP_NET_ADMIN`.

Mount approved radio client identities only into CHUD's certificate directory.
CHUD accepts PEM certificate/key pairs, bundled PEM identities, and PKCS#12
bundles. Do not mount those identities into ARC or AVIAN.

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

The external management service owns enrollment, credentials, live capability
inspection, volatile apply, verification, confirmation, persistence, and
rollback. Do not put radio credentials in AVIAN, ARC canonical configuration,
PEAT, compose variables, or logs.
