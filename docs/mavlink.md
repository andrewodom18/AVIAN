# MAVLink telemetry

`mesh-agent` can ingest the MAVLink common dialect from ArduPilot or PX4,
normalize it, and publish one latest-value PEAT record for the aircraft.

## UDP or TCP

Add these options to the normal `mesh-agent` command:

```sh
--mavlink-address udpin:0.0.0.0:14550 \
--flight-stack ardupilot \
--telemetry-hz 2
```

For PX4, use `--flight-stack px4`. The address also accepts `udpout`, `tcpin`,
and `tcpout` forms supported by
[rust-mavlink](https://github.com/mavlink/rust-mavlink). The receiver reconnects
after transport loss without stopping PEAT.

## Direct serial

Direct serial is an optional build feature because Linux serial enumeration can
require platform packages:

```sh
cargo run -p mesh-agent --features direct-serial -- \
  ... \
  --mavlink-address serial:/dev/ttyUSB0:115200 \
  --flight-stack ardupilot
```

## Safety and data behavior

- The first matching flight-controller heartbeat locks the adapter to one
  MAVLink system ID.
- A PX4/ArduPilot mismatch is rejected rather than silently decoded.
- AGL, landed state, battery, and receiver RSSI remain absent until explicitly
  reported.
- Publication is capped at 2 Hz by default. New samples replace the prior
  telemetry record instead of accumulating history.
- Real flight-controller configurations default to signed command `dry_run` and
  send no command packet. `execute` is accepted only when the strict TOML also
  declares `environment = "sitl"`; the bounded sender then correlates
  `COMMAND_ACK` for `MAV_CMD_NAV_RETURN_TO_LAUNCH`.

stardogOS supplies AVIAN from its dedicated MAVProxy output on UDP 14553. This
does not replace the independent RFD900 path. See the [signed RTL
procedure](emergency-rtl.md) before testing commands.
