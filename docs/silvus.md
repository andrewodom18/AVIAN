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

The radio type does not change AVIAN's 30,000 ft MSL planning ceiling. Link
selection uses current measurements and geometry rather than assuming that a
higher aircraft always has a better path. Platform profiles will include
antenna placement, radio power/thermal limits, energy cost, usable interfaces,
and a platform-specific ceiling. The lower of the platform ceiling and 30,000
ft MSL applies.

## Range calibration for relay planning

Do not use a single published “radio range” to space the swarm. The SL5200
datasheet specifies parameters such as 2 W native power, -101 dBm sensitivity
at 5 MHz, -107 dBm at optional 1.25 MHz, selectable bandwidth, and several
frequency bands. Those values are inputs to a link budget, but they do not
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
measurements. Hardware execution remains disabled until the adapter can check
the target radio's live capabilities and effective settings.

The current operational workload assumption is one 5.5 MB priority payload
from an airborne StreamCaster to a 4000-series control station over a 20 MHz
channel, with no more than 80% airtime allocated. This is not a Silvus standard
or vendor throughput claim. End-to-end goodput, route depth, retries, queues,
and installed antenna characteristics must be measured before delivery time or
EIRP can be accepted.

- [StreamCaster API manual access page](https://silvustechnologies.com/resources/downloads/api-manual/)

When representative radios are available, the Silvus adapter will normalize
vendor measurements into AVIAN's existing latency,
loss, goodput, signal quality, stability, energy cost, line-of-sight, and
Fresnel-clearance model. Until then, the integration stays standards-based and
does not invent proprietary endpoints.

The companion is ready for that adapter now. A collector running on the same
Linux computer sends one normalized `RelayLinkObservation` JSON datagram to
the agent's local UDP listener; the agent validates it and publishes it as
latest-value PEAT telemetry. Bind the listener to loopback unless the collector
is isolated in a controlled local network namespace:

```sh
cargo run -p mesh-agent -- \
  --name aircraft-017 \
  --formation-key-file ./formation.key \
  --relay-observation-listen 127.0.0.1:9100
```

The collector must provide a rolling **bidirectional** observation rather than
one directional RSSI sample. Its required JSON shape is shown in
[`relay-link-observation.sample.json`](../examples/relay-link-observation.sample.json).
AVIAN checks endpoint shape, sample window, finite metrics, and geometry at
ingress; mission evaluation then checks membership, freshness, and the
mission-specific health policy.

Desired radio settings come only from Arc. `arc-radio-plugin` validates the
grouped 4200/4400/5200 fleet, expands it to deterministic node assignments,
computes routine load and the single-source priority-transfer assessment,
produces a PEAT mission-class record, and emits a dry-run StreamCaster API
sequence. It never includes passwords or encryption keys in the PEAT payload. See
[the Arc radio-plugin guide](arc-radio-plugin.md).

## Remaining handoff work

The current retry loop can reconnect a peer using any advertised address.
Seamless score-driven handoff still requires live interface/radio monitoring,
address advertisement, route-change events, and a multi-underlay test harness.
Those are the next networking milestones after shared membership manifests.
