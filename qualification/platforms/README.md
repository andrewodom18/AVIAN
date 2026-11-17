# Platform checks

Run native CLI, authenticated loopback API, and private-file checks:

```sh
cargo build -p avian --bins --locked
python3 qualification/platforms/run.py \
  --output qualification/results/platforms/runtime.json
```

The runner does not configure host network interfaces. `--windows-host odom-pc`
adds read-only SSH inventory. A compiler check for the pure Rust core is separate:

```sh
rustup target add x86_64-pc-windows-gnu
python3 qualification/platforms/run.py --cross-check-windows \
  --output qualification/results/platforms/windows-check.json
```

A cross-check does not establish native Windows networking, protected storage,
or installation behavior.

## Android API probe

Requires JDK 17, Android SDK platform 36, build-tools 36.0.0, and an existing arm64
AVD. The APK is compiled locally; its temporary signing key is discarded.

```sh
python3 qualification/platforms/run.py --android-avd Medium_Phone \
  --output qualification/results/platforms/android.json
```

The runner starts the AVD headlessly and read-only on dedicated port 5580, installs
a temporary probe, grants VPN consent inside that emulator, and destroys the
emulator session afterward. It never takes over a physical device or changes the
Mac's routes. Port 5580 must be unused.

The probe creates only a private IPv6 TUN route. It explicitly allows the IPv4
family, waits for a validated non-VPN default network, binds and protects UDP
sockets, and exchanges echo packets with a temporary loopback server. Tests include
screen-off foreground service operation for three additional echo samples, tunnel
cleanup, and restart. Explicit IPv4 family allowance prevents the IPv6-only VPN
from blocking unrelated IPv4 traffic.

This is a platform API test, without the AVIAN packet engine. Emulated
networks do not qualify physical simultaneous Wi-Fi/cellular binding, radio
hardware, or an end-to-end Android mesh. Reports keep those cases `not_run` and
remain unqualified for hardware integration.
