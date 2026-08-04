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

The radio type does not change AVIAN's 25,000 ft MSL system ceiling. Link
selection uses current measurements and geometry rather than assuming that a
higher aircraft always has a better path. Platform profiles will include
antenna placement, radio power/thermal limits, energy cost, usable interfaces,
and a platform-specific ceiling. The lower of the platform ceiling and 25,000
ft MSL applies.

## Vendor telemetry boundary

AVIAN currently recognizes `silvus` as a transport type and can give PEAT
multiple IP addresses. It does not yet read StreamCaster neighbor, signal,
throughput, or route statistics. Silvus publishes a StreamCaster API manual
entry, but the public page does not provide enough schema detail to implement
and verify a client without the manual or hardware.

- [StreamCaster API manual access page](https://silvustechnologies.com/resources/downloads/api-manual/)

When the supported API and a representative radio are available, a Silvus
adapter will normalize vendor measurements into AVIAN's existing latency,
loss, goodput, signal quality, stability, energy cost, line-of-sight, and
Fresnel-clearance model. Until then, the integration stays standards-based and
does not invent proprietary endpoints.

## Remaining handoff work

The current retry loop can reconnect a peer using any advertised address.
Seamless score-driven handoff still requires live interface/radio monitoring,
address advertisement, route-change events, and a multi-underlay test harness.
Those are the next networking milestones after shared membership manifests.
