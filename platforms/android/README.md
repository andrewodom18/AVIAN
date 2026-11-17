# Android component

AVIAN-owned ARM64 library for an existing Android host application. Requires macOS,
JDK 17, Android SDK 36/build-tools 36.0.0, NDK 28.2.13676358, Rust 1.95 with
`aarch64-linux-android`, and a host `protoc` executable. No standalone application
or browser UI is included.

```sh
rustup target add --toolchain 1.95 aarch64-linux-android
python3 platforms/android/build.py --source /path/to/EasyTier \
  --protoc /path/to/protoc
```

The builder verifies the pinned source revision, applies the canonical engine
patches followed by the Android socket patch, and produces
`.local/build/avian-android.aar`. `build.json` records source, patch, verifier,
compiler, and artifact hashes. Upstream and AVIAN licenses and the bundled native runtime notices are retained.

The host obtains consent using `AvianRuntime.consentIntent`, then calls
`AvianRuntime.start` or `stop`. Consent, foreground lifecycle, TUN ownership,
socket protection, and Network binding belong to this component. The native
engine uses the mobile `InstanceManager.set_tun_fd` API. It exposes no external
administration listener. Browser access remains through the ARC bridge.

Storage lives under `AvianRuntime.storageDirectory(context)`. Store signed
`trust-root.json` and `membership.json` with owner-only permissions. Use
`ProtectedSecretStore(new KeystoreSecretProtector())` to create `transport.key`
from the canonical base64 transport credential; plaintext envelopes are rejected.
The wrapping key remains in Android Keystore. Native startup independently
verifies enrollment, transport-key ownership, and the assigned overlay address.

`runtime.json` contains `version: 1`, `node_id`, `overlay_address`,
`overlay_network`, absolute `trust_root_file`, `membership_file`, `credential_file`,
`working_directory`, and a `peers` array of explicit TCP/UDP peer URLs. Paths must
stay in AVIAN's private directory. Overlay routing is private IPv4 only. Optional
`avian_bind` and `avian_interface` URL parameters must match an assigned address
and the selected Android Network's interface; mismatches stop that connection.
Hole punching, UPnP, exit-node routing, and other transports are disabled.

Physical interface changes reconcile source-bound TCP/UDP connectors and listeners
through the embedded control API while retaining the instance and TUN. Stable
sockets remain open. A replacement Network handle recreates its affected sockets;
failed binds retry without dropping independent paths. Every physical engine socket is protected and bound to the
selected Android Network before connect, listen, or the first datagram. Overlay
HTTP sockets follow the TUN and never use that physical socket callback.

Metering comes from Android Network capabilities. Updates activate a fresh private
policy file before new physical sockets are admitted; service advertisements cannot
change its permissions. Signed
membership changes are checked every second and accepted leases renew periodically.
Missing keys, invalid membership, and unavailable socket callbacks fail closed.

Bounded capacity probes run on fresh, idle, authorized unmetered links. Sampling
allows at most 512 KiB of payload per 30 seconds, one probe at a time, with a
30-second cooldown per connection. Set `automatic_capacity_probes: false` to
disable sampling. Confirmed delivery rates are lower bounds and expire after
30 seconds.

Local endpoints are registered with `AvianRuntime.registerServices`. Store the
owner's canonical hexadecimal signing seed as Keystore-wrapped `identity.key`.
The caller supplies a service kind, transport, port, and bounded TTL; membership
fixes the owner and overlay address. Applications refresh their own advertisements;
expiry does not renew them automatically. `services` returns verified, unexpired
advertisements and `events` returns bounded pages with an epoch and gap marker.
An advertisement does not prove endpoint reachability.

Optional `exchange_peers` entries contain `node_id`, `service_endpoint`, and
`revocation_endpoint`. Signed service and revocation records are exchanged over
HTTP on the authenticated private overlay; records are checked against enrollment
and the pinned network before persistence. `exchange_port` defaults to 4192.
Persisted replay watermarks survive runtime restart.

## Isolated checks

```sh
python3 platforms/android/qualify.py --keystore-only \
  --output platforms/android/.local/keystore.json
python3 platforms/android/qualify.py \
  --engine-directory /path/to/matching-linux-engine \
  --linux-binary-directory /path/to/linux-services \
  --output platforms/android/.local/native-runtime.json
```

The runner uses a read-only, headless AVD on port 5582. Full checks also require
Docker's `avian-qualification:local` image, matching canonical Linux engine, and
Linux service binaries. Relay and target run inside an internal Docker network.
A transparent Mac loopback/standard-I/O TCP broker carries the emulator connection;
Noise authentication stays end to end. Docker publishes no ports. The runner
checks actual native TUN packet exchange, signed service and revocation exchange,
bounded capacity evidence, foreground operation,
protected socket callbacks, Keystore behavior, and descriptor cleanup. Disposable
fixture keys, APK, emulator, and container are removed afterward. Physical
Wi-Fi/cellular coexistence and system-driven consent withdrawal remain separate
from emulator evidence; reports never authorize hardware integration.
