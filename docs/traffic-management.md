# Swarm traffic management

AVIAN treats airtime as a shared mission resource. Routine data is limited at
the source before it enters PEAT, so the limit applies equally over Silvus,
Wi-Fi, cellular, satellite, or any future underlay. Emergency and mission
records are never held behind routine telemetry.

## Default behavior

Without an explicit policy, every companion uses:

| Stream | Default | Exception |
| --- | ---: | --- |
| Routine individual telemetry | One update every 2 seconds | A failsafe, armed/landed, or low-battery state transition is passed immediately. |
| Live relay-candidate telemetry | Up to one update every 500 ms | Required while the aircraft is listed in a live relay-runtime configuration. |
| Unchanged radio-link observation | One update every 500 ms per endpoint pair and underlay | Availability or bidirectionality changes are passed immediately. |
| Operator swarm summary | Up to three rotating publishers every second | Publishers rotate deterministically from the membership view; no drone has a permanent gateway role. |

At 200 aircraft, ordinary source telemetry is bounded to roughly 100 updates
per second before relay-specific priority needs. That is a source bound, not a
claim about over-the-air capacity: real radio testing must still set the
policy from measured goodput and congestion. If ARC configures all aircraft as
live relay candidates, AVIAN keeps their timely position reports instead of
silently suppressing information needed for safe relay decisions.

## Operator feed

The rotating peers publish `SwarmStatusSummary` records containing membership,
fresh/stale counts, and bounded lists of failsafe or low-battery aircraft.
ARC UI should consume these compact summaries as its normal swarm display
rather than rendering every aircraft's detailed stream continuously. The
summary does not contain a full position feed.

Individual records remain latest-value, short-lived mesh control-plane data
for relaying, collision/mission logic, and fault response. Current PEAT
replication is formation-wide, so AVIAN’s implemented enforcement point is
source publication rate and compact operator presentation; per-peer
subscription filtering and operator-requested detail streams are future work.

## Mission policy

ARC can provide a shared JSON policy through `--traffic-policy-file`. Its
summary interval may not exceed the current two-second latest-value telemetry
lifetime:

```sh
cargo run -p mesh-agent -- \
  --name aircraft-017 \
  --formation-key-file ./formation.key \
  --traffic-policy-file ./examples/swarm-traffic-policy.sample.json
```

The policy sets routine and priority intervals, radio-observation interval,
summary interval and replica count, summary freshness, low-battery threshold,
and maximum number of attention identifiers. It is rejected if zero or
inconsistent intervals would undermine the stated bound.
