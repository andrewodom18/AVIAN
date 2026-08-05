# ARC StreamCaster plugin deployment

The ARC radio plugin is a local edge sidecar. It connects only to the ARC
Zenoh Unix socket, reads the local protected credential/evidence mounts, and
uses PEAT for the durable fleet-plan record. Live vendor writes are not
implemented; only the simulator implements the write trait.

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

Do not put credentials in ARC config, PEAT, compose variables, or logs.
