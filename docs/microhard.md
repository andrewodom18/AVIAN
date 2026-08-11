# Microhard radio integration

Microhard radios are an AVIAN network underlay. ARC remains the desired-state
authority, AVIAN/PEAT carries application state over reachable IP paths, and a
Microhard adapter translates device observations into the vendor-neutral radio
contract in `mesh-core`.

Microhard and Silvus use different vendor waveforms. They must not be described
as one RF mesh. A deployment can use both underlays when an IP gateway or an
AVIAN node has reachability through each network.

## Implemented foundation

- `mesh-core::RadioDeviceObservation` provides a vendor-neutral PEAT payload.
- `mesh-core::RadioCapabilities` represents vendor-specific frequency ranges,
  channel widths, management surfaces, modes, power, and antenna counts without
  forcing StreamCaster enums onto other radios.
- `microhard-control` identifies documented Microhard models and normalizes
  published capability evidence.
- The Microhard reader parses documented, read-only `AT+MSSYSI`, `AT+MSGMR`,
  `AT+MWRSSI`, `AT+MWTXPOWER?`, and `AT+MSTR=0` responses.
- A deterministic simulator exercises the same read boundary without a radio.
- `arc-radio-plugin microhard-probe` normalizes captured responses into the
  generic observation contract, providing the integration seam for bench data.
- PEAT's default transport policy recognizes `quic-microhard` as a primary IP
  underlay alongside `quic-silvus`.

No production management transport or hardware-write implementation is enabled.
The public manuals document Telnet and an AT command interface, but they do not
establish that a secure CLI transport, write behavior, or rollback procedure is
identical across current firmware and every candidate model.

Exercise the integration seam without hardware:

```sh
cargo run -p arc-radio-plugin -- microhard-probe \
  --input examples/microhard-command-responses.sample.json \
  --source air-001 \
  --management-ip 192.168.168.1 \
  --observed-at-ms 1000
```

## Initial model evidence

| Model | Published bands | Channel widths represented | Role in evaluation |
| --- | --- | --- | --- |
| pMDDL2460 | 2.402-2.478 GHz, 5-6 GHz | 4, 5, 8, 10, 20, 40 MHz | Primary 20 MHz/video candidate |
| pMDDL4000 | 3.2-4.8 GHz | 4, 8, 18 MHz | S/C-band candidate |
| fDDL1624 | Six ranges from 1.625-2.5 GHz | 1, 2, 4, 8 MHz | Low-SWaP airborne candidate |
| fDDL9324 | Eight ranges from 902 MHz-2.5 GHz | 1, 2, 4, 8 MHz | Low-SWaP, multiband candidate |
| pMDDL900 | 902-928 MHz | 4, 8 MHz | Lower-band candidate |

Published throughput values are retained only where the public brochure labels
a measured IPerf result. In particular, the pMDDL2460 profile does not convert
its wider-channel headline table into measured application goodput. Live
capabilities and field measurements must replace planning evidence.

Sources:

- [Microhard Digital Data Links](https://www.microhardcorp.com/Digital_Data_Link.php)
- [pMDDL2460 brochure](https://microhardcorp.com/brochures/pMDDL2460.Brochure.Rev.0.0.3.pdf)
- [pMDDL4000 product page](https://www.microhardcorp.com/pMDDL4000.php)
- [fDDL1624 brochure](https://www.microhardcorp.com/brochures/fDDL1624.Brochure.Rev.1.0.2.pdf)
- [fDDL9324 product page](https://www.microhardcorp.com/fDDL9324.php)

## Required evidence before live hardware support

Obtain and archive evidence for the exact model and firmware:

1. Current operating/API manual and supported command inventory.
2. SNMP MIB, object access levels, traps, and SNMPv3 requirements.
3. Secure CLI availability and host-key behavior; Telnet is not an acceptable
   production write transport.
4. Discovery protocol wire format for the documented UDP 20097 service.
5. Neighbor and routing-table commands or MIB objects, including peer ID/IP,
   RSSI, SNR, rates, route metric, and timestamps.
6. Volatile versus persistent configuration semantics, reboot behavior,
   confirmation window, backup/restore, and recovery procedure.
7. Model/firmware-reported frequency and channel capabilities plus local
   regulatory authorization.
8. Maximum tested mesh size, recommended neighbor/hop limits, convergence time,
   and measured per-hop TCP/UDP goodput and latency.
9. Antenna part numbers, cable loss, conducted power, installed EIRP, power,
   thermal, vibration, and aircraft integration evidence.
10. Encryption provisioning and rotation behavior without exporting key
    material into ARC, AVIAN, logs, or PEAT records.

## Safe implementation sequence

1. Capture read-only command and SNMP responses from a bench radio.
2. Add redacted fixtures and exact firmware/model identification tests.
3. Implement secure discovery and observation publishing.
4. Verify topology against real neighbor/routing tables.
5. Add desired-state validation against device-reported capabilities.
6. Add prepare/apply/confirm/rollback only after the recovery procedure is
   demonstrated on representative hardware.
7. Run two-node, multi-hop, mixed-underlay, failure/rejoin, and scale tests
   before permitting flight use.
