# StreamCaster integration evidence checklist

AVIAN separates confirmed hardware facts, regulatory constraints, live radio
capabilities, and field calibration. A value from a datasheet or integration
guide is not automatically a safe operational value for every SKU, firmware,
country, antenna, or airframe.

## Radio expert evidence

Current planning inputs are approximately 34.44 dBm installed EIRP airborne
and 33 dBm installed EIRP ground. Preserve these as estimates until the
following evidence reconstructs and validates each value per active RF path.

Capture the following for one representative unit of every 4200, 4400, and
5200 variant before hardware apply is enabled:

- complete Silvus part number, installed options/licenses, firmware version,
  regulatory domain, and country of operation;
- raw `supported_frequency_profiles` and `print_all_settings` responses;
- confirmation whether API `power_dBm` is conducted power per RF port,
  aggregate power, or another calibrated quantity;
- authorized center-frequency, bandwidth, antenna-mask, and conducted-power
  tuples for the installed SKU;
- receiver sensitivity and required SNR by bandwidth and MCS;
- measured UDP goodput, latency, loss, and retry behavior by MCS and hop count;
- login/session flow, cookie lifetime, reconnect behavior, persistence rules,
  and safe rollback sequence;
- available neighbor, route, RSSI/SNR, MCS, retry, airtime, queue, spectrum,
  temperature, and thermal-throttle telemetry; and
- encryption-profile handling without exporting passwords or RF key material.

## Aircraft integration evidence

For every aircraft installation, record:

- power rail, fuse, reverse-polarity protection, surge/brownout behavior, and
  measured startup and peak current;
- heatsink, conductive interface, airflow, and case/internal temperature across
  the expected duty cycle and environment;
- antenna part number, gain, pattern, polarization, port use, spacing, and
  isolation from other transmitters;
- cable type, length, measured insertion loss, and connector loss per port;
- antenna position, airframe shadowing, attitude-dependent loss, and co-site
  interference; and
- altitude, temperature, pressure, vibration, shock, condensation, and EMI/EMC
  qualification evidence for the intended flight envelope.

## Traffic and mission evidence

Confirm whether the priority workload is a 5.5 MB discrete object, a 5.5 Mb/s
continuous stream, or both. Record production rate, delivery deadline,
concurrent sources, acceptable loss, retry behavior, and gateway fan-in. Also
confirm the 3 KiB routine-packet interval and unicast/multicast behavior.

## Activation rule

Free-space link budgets and published peak rates remain planning-only. A plan
becomes activation-ready only after live capabilities match the requested
tuple and field calibration covers the actual radio, antennas, airframe,
altitude band, environment, traffic class, and required delivery margin.
