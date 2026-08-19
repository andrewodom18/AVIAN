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

If `[radio].enabled = false`, the link monitor exits successfully; the mesh
agent remains operational. On stardogOS the installer also adds `rolex` to the
`avian` group so the image-trigger service can write the payload socket. Restart
that service after installation so it receives the updated group membership.

## Normal operation

```sh
avianctl status
avianctl status --json
avianctl status --json --require-ready
avianctl records --class telemetry
avianctl records --class bulk
avianctl records --class mission
avianctl records --class acknowledgement
```

`--require-ready` exits nonzero when a configured peer, required MAVLink lock,
or required fresh radio observation is missing. The unqualified status command
still returns the full degraded state. Record classes expose remote telemetry,
image manifests, detections, and acknowledgements without copying JPEG bytes.

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
