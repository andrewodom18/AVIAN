# Silvus StreamCaster integration

## Integration decision

Silvus is an AVIAN network underlay, not the owner of swarm state or vehicle
authority. StreamCaster forms and heals the RF MANET; PEAT carries AVIAN state
over the IP connectivity that the radio exposes.

This matches the published StreamCaster interface: Silvus describes the
radios as carrying IP data, exposing Ethernet/USB/RS232 interfaces, and
forming a self-healing/self-forming MANET. The compact SL5200 is specifically
positioned for UAV integration and publishes Ethernet as an I/O interface.

- [StreamCaster radio family](https://silvustechnologies.com/products/streamcaster-radios/)
- [StreamCaster LITE 5200](https://silvustechnologies.com/products/streamcaster-lite-5200/)
- [Unmanned systems application](https://silvustechnologies.com/applications/unmanned-systems/)

## Companion connection

The Linux companion computer connects to the radio through Ethernet or a
USB-presented network interface. PEAT/Iroh uses QUIC over UDP. The default
AVIAN listener is UDP port 9000.

For a companion listening on all local IPv4 paths:

```sh
cargo run -p mesh-agent -- \
  --name aircraft-017 \
  --formation-key-file ./formation.key \
  --bind 0.0.0.0:9000 \
  --peer ENDPOINT_ID@10.40.0.18:9000,172.22.0.18:9000
```

In that peer entry, `10.40.0.18` may be the other aircraft's Silvus-side IP
and `172.22.0.18` an alternate underlay. A peer accepts one to eight ordered,
unique addresses. On connection and every reconnect, AVIAN gives PEAT the
whole set. The peer keeps the same cryptographic identity across paths.

Deployments must ensure that UDP port 9000 is reachable on each advertised
path and must advertise concrete interface addresses, not `0.0.0.0`.

## Two layers of mesh

Silvus and AVIAN solve different problems:

| Layer | Responsibility |
| --- | --- |
| Silvus MANET | RF links, radio neighbors, waveform adaptation, IP packet routing |
| AVIAN bounded overlay | At most eight direct PEAT peers per node for 5-200 aircraft |
| PEAT state | Durable mission, detection, telemetry, and command synchronization |
| AVIAN link policy | Path health, altitude/RF geometry, failover, delivery redundancy |

AVIAN must not open a direct PEAT session to every other aircraft. The Silvus
radio already routes multi-hop packets, while AVIAN's bounded overlay limits
application session and synchronization overhead.

## Altitude and aircraft variety

The radio type does not change AVIAN's 30,000 ft MSL planning ceiling. This is
a software planning ceiling, not evidence that an SL5200 installation is
environmentally qualified for 30,000 ft. Link
selection uses current measurements and geometry rather than assuming that a
higher aircraft always has a better path. Platform profiles will include
antenna placement, radio power/thermal limits, energy cost, usable interfaces,
and a platform-specific ceiling. The lower of the platform ceiling and 30,000
ft MSL applies.

## Range calibration for relay planning

Do not use a single published “radio range” to space the swarm. The SL5200
datasheet specifies parameters such as a 2 W total power class, -101 dBm
sensitivity at 5 MHz, -107 dBm at optional 1.25 MHz, selectable bandwidth, and
several frequency bands. Those values are inputs to a link budget, but they do not
capture the installed antennas, selected center frequency, vehicle attitude,
terrain, clutter, interference, traffic demand, or required availability.
[SL5200 OEM datasheet](https://silvustechnologies.com/wp-content/uploads/2026/02/StreamCaster-LITE-5200-SL5200-OEM-Module-Datasheet.pdf)

AVIAN separates a free-space model from a `field_calibrated` usable segment
range. The free-space calculation is useful for early capacity planning, but
its relay plan is not activation-ready. ARC UI must obtain a calibration from
measurements for the particular radio/antenna/airframe and current environment
before it presents a chain as mission-ready. The relay planner returns this
distinction as `range_evidence` and `activation_ready`.

While a mission is active, the in-flight relay planner does not extrapolate
from either number. It requires fresh bidirectional link observations that
meet the mission health policy, including timing, loss, goodput, signal
quality, stability, Fresnel clearance, and optional measured link margin.
It uses current MSL positions to report each actual relay hop. When a needed
path is not observed, AVIAN enters measured range discovery rather than
claiming that a data-sheet range proves a new chain will work.

## Vendor telemetry boundary

AVIAN recognizes `silvus` as a transport type and can give PEAT multiple IP
addresses. The supplied StreamCaster 4000-series user and API manuals now
ground the radio configuration contract, dry-run JSON-RPC sequence, and the
available neighbor, signal, throughput, route, queue, airtime, and spectrum
measurements. Physical execution belongs to the external CHUD-backed
radio-management API, which must check the target radio's live capabilities
and effective settings before accepting a transaction.

The current operational workload assumption is one 5.5 MB priority payload
from an airborne StreamCaster to a 4000-series control station over a 20 MHz
channel, with no more than 80% airtime allocated. This is not a Silvus standard
or vendor throughput claim. End-to-end goodput, route depth, retries, queues,
and installed antenna characteristics must be measured before delivery time can
be accepted. Approximate installed-system inputs are currently 34.44 dBm EIRP
airborne and 33 dBm EIRP ground; they remain planning estimates until the
underlying conducted power, gain, loss, array method, and calibration are
captured.

The SL5200/LC5200 OEM Integration Manual v1.1 confirms the two-port SL5220
power split as 1 W (30 dBm) per port. Its FCC 2.4 GHz modular profile permits
20 MHz at 2440 MHz with no more than 27 dBm conducted power per antenna. AVIAN
models total radio power separately from per-path conducted power and rejects
other 20 MHz center frequencies when `fcc_sl52_245_oem` is selected for an
exact SL5210 or SL5220. Generic/estimated 5200 profiles cannot claim that
grant. Other bands, countries, and radio families remain live-capability and
operator-authorization dependent.

- [StreamCaster API manual access page](https://silvustechnologies.com/resources/downloads/api-manual/)

`avian-link-monitor` now uses the existing allowlisted read-only StreamCaster
client. It reads capabilities/model/firmware, effective settings, network
neighbors, RSSI, SNR, and transmit/receive MCS. Credentials stay in a local
mode-`0600` JSON file and are reduced to sanitized availability errors before
status or PEAT publication. The monitor has no radio mutation call path.

It also runs bounded UDP echo probes for each configured peer/underlay and
normalizes latency, loss, achieved probe goodput, and rolling stability. The
default local path to `mesh-agent` is the group-controlled Unix datagram
`/run/avian/link-observations.sock`. The existing normalized
`RelayLinkObservation` UDP listener remains available for compatibility; bind
that compatibility listener to loopback unless it is isolated in a controlled
local network namespace:

```sh
cargo run -p mesh-agent -- \
  --name aircraft-017 \
  --formation-key-file ./formation.key \
  --relay-observation-listen 127.0.0.1:9100
```

The compatibility collector must provide a rolling **bidirectional**
observation rather than one directional RSSI sample. Its required JSON shape is shown in
[`relay-link-observation.sample.json`](../examples/relay-link-observation.sample.json).
AVIAN checks endpoint shape, sample window, finite metrics, and geometry at
ingress; mission evaluation then checks membership, freshness, and the
mission-specific health policy.

The built-in monitor similarly publishes a relay-eligible observation only
when both radio API directions and every mission measurement are present. It
does not derive geometry from a plausible-looking guess. Missing distance,
line of sight, Fresnel clearance, RF node IDs, SNR bounds, receiver sensitivity,
energy calibration, latency, loss, goodput, or rolling stability produces an
explicit degradation reason.

Desired radio settings come only from Arc. `arc-radio-plugin` validates the
grouped 4200/4400/5200 fleet, expands it to deterministic node assignments,
computes routine load and the single-source priority-transfer assessment,
produces a PEAT mission-class record, and emits a dry-run StreamCaster API
sequence. It never includes passwords or encryption keys in the PEAT payload. See
[the Arc radio-plugin guide](arc-radio-plugin.md).

## Silvus loss and satellite fallback

Production peer TOML tags the Silvus IP first and the peer's ZeroTier IP over
Starshield second as `satellite`. PEAT receives the complete ordered set at
every connection attempt. When the operator/network layer removes the Silvus
route, the existing session may drop and reconnect through the reachable
ZeroTier address. Status and transition logs report the interruption, selected
fallback, and preferred-path recovery.

AVIAN neither mutates StreamCaster configuration nor changes Starshield
terminal/GPS state. RFD900 remains an independent direct-MAVLink safety path.
This milestone does not provide make-before-break handoff or AVIAN transport
over RFD900. Follow the [field runbook](field-runbooks.md) for physical proof;
the behavior remains unaccepted until the SL5200/4200 and route-loss evidence is
recorded in the [status ledger](implementation-status.md).
