# Field integration runbooks

These runbooks collect sanitized evidence; they do not declare success before
the hardware actions are performed. Record UTC timestamps, AVIAN/stardogOS
commit IDs, OS versions, radio model/firmware, and pass/fail notes in the
[implementation status ledger](implementation-status.md). Never record keys,
passwords, cookies, absolute imagery paths, or Starshield credentials.

## Shared preparation

1. Run `just verify` on the Mac and Linux/Pi checkout.
2. Install AVIAN on each Pi with the [deployment procedure](deployment.md).
3. Provision the same formation ID/key and stable, unique node names.
4. Start nodes once without peers, record their endpoint IDs, then configure
   ordered addresses: `silvus` first and the ZeroTier-over-Starshield address
   tagged `satellite` second.
5. Permit AVIAN UDP ports `9000` (PEAT) and `9200` (bounded probe responder) on
   both underlays. Do not route AVIAN through RFD900.

## Raspberry Pi to Mac

On the stardogOS Pi, set the following in `/home/rolex/config.txt`:

```sh
AVIAN_ENABLED=true
AVIAN_MAVLINK_OUT="--out 127.0.0.1:14553"
AVIAN_PAYLOAD_SOCKET="/run/avian/payload-events.sock"
```

Configure the aircraft agent with `udpin:0.0.0.0:14553`, `flight_stack =
"ardupilot"`, `commands.mode = "dry_run"`, and `commands.environment =
"hardware"`. Restart MAVProxy, image trigger, and AVIAN. Leave RFD900, GPS
Guard, MediaMTX, and KLV in their existing independent paths.

On the Mac, run the ground agent as described in [deployment](deployment.md).
Then verify:

```sh
avianctl --socket ./field/run/control.sock status --json --require-ready
avianctl --socket ./field/run/control.sock records --class telemetry
avianctl --socket ./field/run/control.sock records --class bulk
```

The remote telemetry record must identify the aircraft node and update from the
real Cube. The bulk record must contain an image manifest with a relative
imagery reference, byte count, SHA-256, and geotag status, but no JPEG bytes or
absolute path.

If no supported camera is attached, place an approved JPEG fixture under the
stardogOS imagery root and invoke the same failure-isolated notifier as user
`rolex`:

```sh
cd /home/rolex
/home/rolex/venv/bin/python - <<'PY'
from datetime import datetime, timezone
from pathlib import Path
from camera_scr.avian import AvianPayloadNotifier

root = Path("/home/rolex/imagery")
fixture = root / "fixtures" / "acceptance.jpg"
AvianPayloadNotifier(root, enabled=True).emit_image(
    fixture, datetime.now(timezone.utc), "fixture", None, "not_attempted"
)
PY
```

An absent AVIAN socket must produce one warning and must not fail or delete a
camera capture. Restore the socket and confirm one recovery message.

## Raspberry Pi to Raspberry Pi

Use the Cube-connected Pi as `aircraft-001` and the second Pi as an `observer`.
Configure both as persistent systemd peers; optionally retain the Mac as a
third peer. Verify on each Pi:

```sh
sudo systemctl is-active avian-mesh-agent.service
avianctl status --json
```

Record the endpoint IDs, restart both services, and confirm the endpoint IDs do
not change. Confirm the observer receives Pi 1 telemetry and the image manifest.

Next, stop the Mac/ground peer. Both Pi services must remain active; status may
correctly become degraded because a configured peer is absent. Confirm the two
Pis remain connected to each other. Restart one Pi agent and verify durable
manifests reconcile while stale telemetry does not reappear as current data.

## Silvus and backup-link acceptance

Provision radio credentials only in `/etc/avian/secrets/*.json`, owned by
`avian` with mode `0600`:

```json
{"username":"REDACTED","password":"REDACTED"}
```

Use the SL5200 airborne management API and the 4200-series ground management
API. Confirm `avian-link-monitor` reports model, firmware, effective settings,
direct RF neighbors, RSSI/SNR/MCS, API freshness, and probe metrics from both
sides. AVIAN must not call a mutation endpoint.

Relay eligibility must remain absent until both API directions, latency, loss,
probe goodput, rolling stability, and field-owned geometry/calibration are
present. A missing item must appear as a degradation reason.

Using the operator-owned network/radio procedure, disable only the Silvus data
route. Do not modify the radio through AVIAN. Capture:

```sh
avianctl status --json
journalctl -u avian-mesh-agent -u avian-link-monitor --since "5 minutes ago"
```

PEAT should disconnect and reconnect using the peer's tagged `satellite`
address over ZeroTier/Starshield. Record interruption and recovery timestamps.
Restore Silvus and confirm it becomes the selected reachable preferred path.
During both transitions, verify image capture, GPS Guard, RFD900, and the plain
flight video remain unaffected. AVIAN must never change terminal GPS state.

Make-before-break handoff, JPEG transfer, AVIAN-over-RFD900, dynamic in-flight
membership, and hardware RTL are outside this milestone.
