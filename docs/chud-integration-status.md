# CHUD integration status and AVIAN boundary

The CHUD #112 implementation assessment dated 2026-08-31 reports a working
single-interface discovery core and radio-control APIs, but no versioned
ARC/AVIAN consumer contract. The assessment is source evidence, not bench proof
for the particular CHUD image or host used with this repository.

## Ownership

- **CHUD** is the only authority for physical discovery, credentials, vendor
  management drivers, snapshots, writes, readback, effective state, and
  measured RF-neighbor evidence.
- **ARC** owns operator intent, the guided workflow, transaction journal, and
  presentation of normalized CHUD state.
- **AVIAN** owns radio-planning constraints, drone-to-radio attachment
  assertions, PEAT/logical topology, and consumption of normalized measured
  links. AVIAN never stores CHUD credentials or writes a physical radio.

## Required normalized contract

AVIAN radio discovery v2 records distinguish:

- stable identity: MAC, serial number, and vendor node ID when available;
- reachability from authentication and management-driver availability;
- lifecycle: discovered, reachable, authenticated, managed, connected, stale;
- provenance: `chud_authoritative`, `avian_diagnostic`, or `simulation`;
- monotonic observation revision and explicit expiry.

Only a fresh v2 `chud_authoritative` record that is reachable, authenticated,
managed/connected, and backed by an available driver may enter a configuration
workflow. Only fresh, non-simulated CHUD-authoritative RF-neighbor evidence may
be treated as measured topology. AVIAN diagnostic records and simulations stay
visible for troubleshooting but cannot satisfy either gate.

The v1 discovery and device-observation topics remain available as stripped
compatibility projections. They have no authority metadata and therefore cannot
authorize configuration or measured topology; full provenance is published on
the corresponding v2 topics.

## Candidate discovery intake prepared in AVIAN

AVIAN now defines a strict `radio-discovery-intake` v1 envelope around a fresh
v2 `avian_diagnostic` observation. The envelope carries a bounded observation
ID, source-instance ID, submission timestamp, nonce, and idempotency key. Its
validator rejects stale observations, future or reordered timestamps, simulated
records, CHUD-authoritative claims, unknown fields, and unsupported versions.

This is a candidate contract for CHUD item 2.4/3.2; it does not invent a CHUD
endpoint. Authentication belongs to the transport (a scoped, server-held CHUD
API key), not the payload. CHUD must independently validate and promote accepted
evidence into its own authoritative inventory. The submitting AVIAN record
remains diagnostic even after a successful HTTP response. The JSON fixture at
`apps/arc-radio-plugin/tests/fixtures/radio-discovery-intake.v1.json` is the
cross-repository mock until CHUD publishes the actual endpoint and response
schema.

## External gaps that remain CHUD work

- Publish and version a normalized external inventory-plus-topology contract;
  current `/api/status` and `/api/radio/devices` surfaces are useful inputs but
  do not form that complete contract.
- Expand the existing single-interface capture design to simultaneous physical
  Ethernet, USB-Ethernet, hot-plug, and RF-facing interfaces. Offline operation
  and stale-device expiry already exist in CHUD and must be preserved.
- Supply vendor RF-peer/topology telemetry where supported.
- Deliver and validate the currently absent Microhard management driver.
- Orchestrate radios that share a factory IP without treating IP as identity.
- Land and validate the external API authentication prerequisite, then prove
  certificate selection, write/readback, rollback, and audit behavior on both
  bench radios.

CHUD's current lifecycle maps into the normalized contract as follows:
`Discovered` -> discovered, `Reachable`/`Identified` -> reachable,
`AuthFailed` -> reachable with rejected/certificate-required authentication,
`Confirmed` -> authenticated, `Connected` -> managed/connected, and `Stale` ->
stale. The adapter must retain the original CHUD state so this lossy mapping is
auditable.

Until those gaps are delivered and hardware-validated, AVIAN's native watcher
is a diagnostic fallback only and physical configuration/topology validation
must remain fail-closed.
