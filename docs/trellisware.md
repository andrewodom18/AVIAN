# TrellisWare TW-950 integration

CHUD is the operational discovery and configuration authority for TrellisWare
radios. ARC consumes CHUD's read-only `/api/radio/devices` inventory, and opens
the selected hardware MAC in CHUD for configuration. AVIAN does not replace
CHUD's driver, certificate store, transaction engine, or operator workflow.

AVIAN also includes a read-only TW-950 bench probe. It can read a radio through
its HTTPS TNC agent API and normalize the result into the vendor-neutral
observation used by ARC's Devices page. This command is diagnostic support for
radio-in-the-loop development, not the production configuration path.

AVIAN can also publish credential-independent discovery records to
`local/link/radio/discovery/v1`. These identify the physical radio by MAC and
report network reachability separately from management authentication, so ARC
can show `certificate required` instead of hiding a reachable radio.

Observed fields include the physical device ID/MAC, model, serial number,
firmware, system alias, operating state, battery level, active preset, and
transmit-power override. The HTTPS contract does not expose trustworthy RSSI,
SNR, throughput, GPS position, or RF-neighbor records. Those require the
TrellisWare MQTT interface and must not be estimated.

## Safety boundary

The initial adapter is read-only. TW-950 writes take effect immediately and the
available API has no separate durable-save step. Do not add configuration writes
until the exact firmware, client certificate, local recovery procedure, and
readback behavior have been verified on both bench radios.

## CHUD discovery and certificate configuration

The CHUD wiki documents both TrellisWare OUIs (`00:1E:3F` and `20:9B:60`) and
management-IP probing for bridge-mode radios. For the current one-radio bench,
configure the known address instead of scanning an entire `/16`:

```yaml
radios.0: TW; TrellisWare; 00:1E:3F; 100; 100; 10.1.0.0/16
tw_probe: 10.1.0.2
```

Use `tw_probe_subnet: 10.1.0.0/16` only when scanning is operationally required.
Subnet probing needs local reachability to the management network and elevated
network privileges. Linux deployments need root or `CAP_NET_RAW` plus
`CAP_NET_ADMIN`; macOS needs `sudo` for raw discovery and off-subnet aliases.

When the radio requires mutual TLS, CHUD accepts any of these formats:

```yaml
# PEM certificate and separate private key
radio_cert_TW_0_cert: /etc/chud/certs/tw-client.pem
radio_cert_TW_0_key: /etc/chud/certs/tw-client.key

# Or a bundled PEM containing the certificate and private key
radio_cert_TW_1_cert: /etc/chud/certs/tw-client-bundle.pem

# Or PKCS#12
radio_cert_TW_2_p12: /etc/chud/certs/tw-client.p12
radio_cert_TW_2_password: <retrieve-from-approved-secret-store>
```

Certificate indexes must be dense starting at zero. CHUD loads up to ten
certificates per radio type and cycles through them, which permits two radios
with different issued identities. Mount the certificate directory read-only
into CHUD and keep passwords outside ARC configuration, AVIAN, PEAT, source
control, and logs. The filenames and passwords above are placeholders; the
repository does not contain an authorized TrellisWare client identity.

CHUD's device lifecycle distinguishes `reachable`, `identified`,
`auth-failed`, `confirmed`, `connected`, and `stale`. ARC intentionally keeps an
`auth-failed` radio visible as reachable and labels its management access
`certificate required`; this is not a generic fetch failure.

## AVIAN diagnostic discovery before credentials are available

On Windows or Linux, inspect the local neighbor table and stimulate the known
factory management address without changing host routes or adapter settings:

```powershell
cargo run -p arc-radio-plugin -- trellisware-discover `
  --probe-ip 10.1.0.2 `
  --watch `
  --zenoh-endpoint tcp/127.0.0.1:7447
```

TrellisWare OUIs `00:1E:3F` and `20:9B:60` are recognized. Each discovery is
keyed by its full MAC rather than its IPv4 address. AVIAN also derives the
unique scoped IPv6 link-local management endpoint, which prevents two radios
using the same factory IPv4 address from being represented as one device.

With no radio attached, a one-shot discovery returns an empty JSON list. That
is a valid empty inventory, not an error.

## Probe one radio

```powershell
cargo run -p arc-radio-plugin -- trellisware-probe `
  --radio-url https://10.1.0.11 `
  --source tw-ground-1 `
  --client-identity-pem C:\secure\tw-client-identity.pem `
  --ca-certificate-pem C:\secure\tw-radio-ca.pem
```

For a self-signed lab certificate, `--accept-invalid-server-certificate` is an
explicit temporary alternative to `--ca-certificate-pem`. It does not disable
client-certificate authentication when the radio requires mTLS.

To poll continuously and publish into ARC:

```powershell
cargo run -p arc-radio-plugin -- trellisware-probe `
  --radio-url https://10.1.0.11 `
  --source tw-ground-1 `
  --client-identity-pem C:\secure\tw-client-identity.pem `
  --accept-invalid-server-certificate `
  --zenoh-endpoint tcp/127.0.0.1:7447 `
  --watch
```

Run one probe per physical radio with a unique `--source`. ARC displays each
fresh observation as a TrellisWare node. PEAT/IP connections appear as dashed
overlay links. The UI will not invent a solid RF edge without a measured
TrellisWare neighbor record.

## Published hardware facts used for planning

The manufacturer describes the TW-950 as an IPv4/IPv6 TSM MANET radio with up
to eight RF hops, 400+ nodes in one RF channel, 1.2/3.6/10/20/40 MHz channel
widths, up to 2 W transmit power, and up to 16 Mbps one-hop point-to-point data.
These are capability bounds, not promises for an installed antenna or RF
environment.

- [TW-900/950 product data sheet](https://www.trellisware.com/wp-content/uploads/2021/03/TW-900-950-TSM-Shadow-Product-Datasheet.pdf)
- [TW Shadow 950 product page](https://www.trellisware.com/trellisware-radios/tw-shadow-950/)
