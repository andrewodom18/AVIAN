# ARC main compatibility

This document records how AVIAN coexists with the current ARC mainline without
modifying ARC source. It separates capabilities that AVIAN can use immediately
from the CHUD-backed radio workflow that still requires ticketed ARC changes.

## Reviewed baselines

- ARC UAS main: `f212949b` (`feat(spatial): serve every pack layer...`)
- ARC UI commit pinned by ARC UAS main: `88bbce2`
- AVIAN main: `4e9c767`

Commit identifiers are evidence for this review, not permanent version pins.
Repeat the comparison when either main branch advances.

## Mainline ARC capabilities AVIAN can use

### Multi-drone sessions and active-drone selection

ARC now addresses commands, events, and data by admitted `drone_id` and fences
reconnected sessions with `session_epoch`. This aligns with AVIAN's stable node
identity and newly added ground-side paired-aircraft path management. AVIAN
must keep MAC identity, AVIAN node identity, and ARC `drone_id` as distinct
fields; none may be inferred from a transient IP address.

### Identification-image delivery

ARC serves a selected drone's JPEG crop through
`GET /api/drones/{drone_id}/vision/crop/{observation_id}` and resolves the bytes
over that drone's admitted Zenoh session. AVIAN provides the available network
path but does not proxy, replicate, cache, or embed the JPEG in PEAT.

The existing AVIAN radio traffic profile can model the largest operator-
requested crop or crop burst with `priority_transfer_bytes` and the number of
simultaneous source aircraft with `priority_source_nodes`. A duration estimate
is not credible until `calibrated_end_to_end_goodput_bps` is measured on the
installed radio, antenna, airframe, route, and environment. The normal 20%
airtime reserve remains intact.

### Waypoint on-arrival actions

ARC locations can carry an optional `on_arrival` action. This remains an ARC
mission/flight-control contract. AVIAN transports authenticated mission and
command traffic reliably but must not reinterpret the action, invent a default,
or downgrade it to routine telemetry.

### Spatial map and landing APIs

ARC serves bulk spatial layers and landing analysis through dev-bridge HTTP
from local packs. This data does not belong in AVIAN's replicated mesh state.
Only compact mission results selected by ARC should enter mission traffic.

## Boundary that remains open

The reviewed ARC main baseline contains no `/api/radio/*` routes, no
`RadioSwarmBuilder`, and no native consumer for AVIAN's radio observation
topics. AVIAN can start safely beside ARC main, and both systems can use the
same approved underlay, but ARC main will not display or configure radios from
AVIAN by itself.

The CHUD-guided workflow remains tracked by AVIAN issue #42 and the local ARC
and ArcUI integration worktrees. Bringing that workflow to ARC main requires a
reviewed ARC change. This AVIAN branch deliberately does not patch, overlay, or
silently rewrite ARC source.

## Validation levels

1. **AVIAN compatibility:** AVIAN formatting, lint, tests, build, simulator,
   and radio-plugin contract fixture pass.
2. **ARC coexistence:** unmodified ARC main dev-bridge and UI start while the
   AVIAN sidecar starts without topic, port, or process failure.
3. **Guided workflow:** the ticket #42 ARC integration starts and exercises
   CHUD inventory/snapshot/operation routes. This is not available on
   unmodified ARC main.
4. **Hardware validation:** physical CHUD reads/writes, restart recovery,
   duplicate factory-IP handling, and RF peer evidence pass on radios. No
   earlier level may be reported as hardware validation.
