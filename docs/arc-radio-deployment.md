# ARC StreamCaster plugin deployment

The ARC radio plugin is a local edge sidecar. It connects only to the ARC
Zenoh Unix socket, reads the local protected credential/evidence mounts, and
uses PEAT for the durable fleet-plan record. The live vendor adapter is
implemented but remains disabled by canonical configuration until bench and
radio-in-the-loop qualification are complete.

Prepare requests contain no ARC flight-safety claims. Activation requires a
fresh, timestamped authorization from Link Manager for the same prepared
generation. The sidecar accepts only an explicitly authorized maintenance
window with known-landed state and a preserved alternate control bearer, then
combines those ARC-owned facts with its local capability and evidence gates.

Build the deployment image from the AVIAN repository root:

```sh
docker build -f apps/arc-radio-plugin/Dockerfile -t avian-arc-radio-plugin:latest .
docker save -o avian-arc-radio-plugin.tar avian-arc-radio-plugin:latest
```

Place the tarball in ARC's `infra/images/` before running the normal ARC
deployment. Enable `arc_streamcaster_plugin_enabled` only after provisioning:

- `/etc/arc/streamcaster-credentials/radio.json` as root-owned mode 0600;
- `/etc/arc/radio-evidence/regulatory.json` for the exact frequency/width;
- `/etc/arc/radio-evidence/installations/<profile>.json` for approved antenna
  installation/calibration evidence;
- `/etc/arc/keys/peat-radio.key` as the out-of-band PEAT formation key;
- an isolated management interface and a separate operational data interface.

Start one sidecar per locally attached StreamCaster. Supply that radio's actual
management URL with `--radio-url`; the observation publisher exposes only its
sanitized host/IP. Set `--source` to the stable ARC device identity used for
that companion. Supply each intended PEAT relationship with one or more
`--peat-peer NAME=ENDPOINT_ID@IP:PORT[,IP:PORT...]` arguments. The UI marks a PEAT
link connected only when the transport reports that endpoint as connected.
Configured but disconnected peers remain visible as disconnected; no link is
fabricated for an unobserved relationship. Offline peers do not block sidecar
startup; the sidecar retries missing sessions with a bounded connection timeout.
The `--radio-url` host must exactly match the canonical per-device management
address or the sidecar blocks the generation as a binding mismatch.

Use [the radio mesh bootstrap command](arc-radio-bootstrap.md) to derive every
endpoint ID and generate bounded, per-host Ansible peer variables without
starting the sidecars first.

The peer address is the PEAT/ARC reachability address, which may differ from the
StreamCaster management IP. Fresh fused ARC `local/telemetry` supplies node
position when available. Documented `network_status`, `nbr_rssi`, `nbr_mcs`,
and `nbr_mcs_rx` calls supply direct RF SNR/RSSI/MCS. Throughput is not probed
periodically because the vendor test adds traffic. Operators must not treat the
logical PEAT topology as a radio propagation map.

Activation requires independent management reachability, complete capability,
regulatory, installation, credential, landed, and alternate-control-bearer
gates. It applies volatile state first. Persist only through the separate
confirm operation after effective-state verification; lack of confirmation for
60 seconds triggers an automatic rollback attempt.

Do not put credentials in ARC config, PEAT, compose variables, or logs.
