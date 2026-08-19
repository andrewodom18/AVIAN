# Production deployment

AVIAN installs three binaries on Linux:

- `mesh-agent`, the PEAT node and local control/payload service;
- `avianctl`, the local operator CLI; and
- `avian-link-monitor`, an isolated read-only radio/probe process.

The installer uses `/etc/avian`, `/var/lib/avian`, and `/run/avian`. It creates
the `avian` system account/group, preserves an existing `/etc/avian/avian.toml`,
and does not require a peer, MAVLink source, payload producer, or radio to be
available when systemd starts the agent.

## Install or upgrade

Run the installer from a trusted AVIAN checkout. Without `--bin-dir`, it makes
a locked release build before installing:

```sh
sudo ./deploy/install.sh
```

To install already-built artifacts:

```sh
cargo build --workspace --release --locked
sudo ./deploy/install.sh --bin-dir ./target/release
```

The installer replaces binaries, unit files, the documentation copy of the
example, and `/etc/avian/avian.toml.example`. It preserves the live TOML and
all locally provisioned keys/credentials. Review configuration changes before
restarting an upgraded node.

## Provision a node

1. Copy either [`aircraft.toml.example`](../config/aircraft.toml.example) or
   [`ground.toml.example`](../config/ground.toml.example) to
   `/etc/avian/avian.toml`, then replace every deployment placeholder.
2. Create one shared 32-byte formation secret on a trusted provisioning host:

   ```sh
   umask 077
   openssl rand -base64 32 > formation.key
   sudo install -o avian -g avian -m 0600 formation.key /etc/avian/formation.key
   ```

   Transfer that same secret out of band to authorized peers. Do not put it in
   Git, PEAT records, logs, or acceptance evidence.
3. Keep private command keys and radio credential files owned by `avian` with
   mode `0600`. Public verification keys may be `0644`.
4. Keep `/etc/avian/avian.toml` owned by `root:avian` with mode `0640`.
5. Start once without `[[peers]]` entries if endpoint IDs are not known. Read
   the stable endpoint from `journalctl -u avian-mesh-agent`, exchange it out of
   band, add the peer blocks, and restart.

Enable services after provisioning:

```sh
sudo systemctl enable --now avian-mesh-agent.service
sudo systemctl enable --now avian-link-monitor.service
```

If `[radio].enabled = false`, vendor radio API collection stays disabled. The
link monitor still runs when `probe_listen` or `[[radio.probes]]` is configured,
so ordinary Ethernet or ZeroTier links can be measured without pretending that
a managed radio is present. With neither passive probe feature configured it
waits without contacting a radio API. On stardogOS the installer also adds
`rolex` to the `avian` group so the image-trigger service can write the payload
socket. Restart that service after installation so it receives the updated
group membership.

## Normal operation

```sh
sudo avianctl status
sudo avianctl status --json
sudo avianctl status --json --require-ready
sudo avianctl records --class telemetry
sudo avianctl records --class bulk
sudo avianctl records --class mission
sudo avianctl records --class acknowledgement
```

`--require-ready` exits nonzero when a configured peer, required MAVLink lock,
or required fresh radio observation is missing. The unqualified status command
still returns the full degraded state. Record classes expose remote telemetry,
image manifests, detections, and acknowledgements without copying JPEG bytes.
The control and link-observation sockets are owner-only (`avian`, mode `0600`)
because they can issue commands or affect operational status. The payload
socket is the only group-writable ingress (`0660`) so the stardogOS `rolex`
service can submit strict image/detection metadata. Linux operators therefore
run `avianctl` through `sudo`; the optional
[AVIAN Ground UI](https://github.com/andrewodom18/avian-ground-ui) follows the
same owner-only boundary and exposes no command endpoint. Install that
companion repository on the ground operator device with `sudo
./deploy/install.sh --enable`, then open `http://127.0.0.1:4178/` on that
device. It is loopback-only, observational, and not required for mesh
operation. Its dedicated aircraft view reads telemetry records synchronized to
the local ground agent; it does not tunnel HTTP to the aircraft. Aircraft data
refreshes every 2 seconds, status every 10 seconds, and bounded log/record feeds
every 30 seconds. Background tabs pause polling. Last-known flight data remains
visible with an explicit stale warning during a link interruption. Active
warnings are expandable, and events are searchable, filterable, ordered, and
paginated.

To view a dashboard installed on a separate ground computer without exposing
another network listener, forward that ground computer's loopback port over
SSH:

```sh
ssh -N -L 4178:127.0.0.1:4178 avian-operator@ground-device
```

Open `http://127.0.0.1:4178/` on the laptop. If the underlying Ethernet,
Wi-Fi, or overlay path changes, the dashboard and AVIAN services continue on
the ground device, but the existing SSH session ends and must be re-established
through a reachable address. For flight, run the Ground UI on the same operator
Mac as the local ground agent and browser so an aircraft SSH session is never
part of the display path. `jq` is optional operator tooling and is not an AVIAN
or dashboard runtime dependency.

Useful service diagnostics:

```sh
systemctl status avian-mesh-agent avian-link-monitor
journalctl -u avian-mesh-agent -u avian-link-monitor --since today
systemd-analyze security avian-mesh-agent.service
systemd-analyze security avian-link-monitor.service
```

## macOS field peer

The systemd installer is Linux-only. On a Mac, build with `cargo build
--release --locked -p mesh-agent --bins`, copy the ground example, and change
storage and socket paths to locations writable by the operator. Relative paths
resolve from the TOML directory; for example:

```toml
[peat]
storage = "state"
formation_key_file = "formation.key"

[sockets]
control = "run/control.sock"
payload = "run/payload-events.sock"
link_observation = "run/link-observations.sock"
max_message_bytes = 65536
```

Run both processes from separate terminals:

```sh
./target/release/mesh-agent --config ./field/ground.toml
./target/release/avian-link-monitor --config ./field/ground.toml
```

Point `avianctl --socket` at the configured control socket.

For unattended field use, run both binaries as user LaunchAgents with
`RunAtLoad` and `KeepAlive`, using absolute paths to the config, binaries,
sockets, and log files. Run `avian-ground-ui` as a third LaunchAgent with the
same local control socket and `--disable-journal`. Verify all three processes
after login before flight; the HTTP listener must remain on `127.0.0.1`.

The Mac peer should list the aircraft's preferred direct/Silvus address first
and its ZeroTier-over-Starshield address second. The aircraft should list the
corresponding Mac addresses in the same order. This symmetric configuration
avoids relying on which endpoint wins PEAT's deterministic connection
initiation. A cable removal can then interrupt the direct session and trigger a
retry through the satellite-tagged address without taking down the local UI.

## Failure recovery

- A missing peer, Cube/MAVLink feed, payload producer, or radio API degrades
  status but does not stop PEAT or the other local services.
- Identity remains stable when node name, formation secret, and persistent
  storage are retained.
- If configuration is rejected, fix the named strict-TOML field; do not remove
  schema/version or unknown-field validation.
- If a socket path is left behind, the service replaces it only when it is a
  Unix socket. It refuses to overwrite another file type.
- Restore a missing Silvus path at the network/radio layer. AVIAN retries the
  ordered peer address set and can reconnect through the tagged satellite
  (ZeroTier-over-Starshield) address; it does not reconfigure the radio or
  Starshield terminal.

See the [field runbooks](field-runbooks.md), [RTL procedure](emergency-rtl.md),
and [implementation status](implementation-status.md) before acceptance work.
