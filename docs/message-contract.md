# AVIAN message contract

This document defines the application-level contract carried by PEAT. The Rust
types in `mesh-core` are authoritative for v0.1.

## Node advertisement

A node advertises a stable identifier, its flight stack, its explicit
capabilities, and its maximum MSL altitude. Consumers must check capabilities
before assigning work. Flight-stack names alone are not authorization.

## Telemetry

The minimal common telemetry record contains:

- position, velocity, attitude, and timestamp;
- MSL, AGL, and above-launch altitude;
- battery state, armed/landed/failsafe state; and
- local control-link quality.

Telemetry is latest-value data. Old telemetry must not be replayed after a
partition heals.

AGL is optional because MAVLink `GLOBAL_POSITION_INT.relative_alt` is relative
to home, not terrain. AVIAN never copies relative altitude into AGL when no
terrain or range estimate exists. Battery, receiver-link quality, and landed
state are also optional instead of inventing healthy defaults for unknown
MAVLink values.

## Relay link observation

A relay link observation is a latest-value telemetry record for one rolling,
bidirectional underlay measurement between two nodes. It carries its timestamp
and sample window, transport, availability, latency, loss, goodput, signal
quality, stability, Fresnel geometry, and optional received power/link margin.
Companions combine these records with current vehicle telemetry and membership
state to construct the shared in-flight relay snapshot. A stale or one-way
observation is not eligible to become a chain hop.

## Emergency command

An emergency command contains:

- globally unique command ID;
- issuer and target node IDs;
- issuance and expiration timestamps;
- issuer-scoped monotonic nonce;
- requested action; and
- Ed25519 signature over a deterministic binary representation.

Receivers verify the trusted issuer key, target, lifetime, signature, and
nonce before execution. A command ID or issuer/nonce pair can be accepted only
once. An acknowledgement is a separate durable record.

Betaflight v0.1 actions are limited to GPS Rescue, return-to-launch mapped to
GPS Rescue, and disarm after the adapter reports a landed state. Raw stick
control is not a mesh command.

## Mission allocation

A mission allocation contains a mission UUID, positive generation, relay
corridor assessment, exact relay and mission pools, and optional operator task
groups. Relay plans include the selected automatic coverage profile, station
positions, range utilization, local failure tolerance, reserved relay count,
remaining payload capacity, feasibility, range evidence, activation readiness,
and warnings. Every station explicitly identifies one active broadcaster and
its receiving standbys. `maximum` automatic coverage uses a
Bluetooth-coordinated active/standby pair; the pair must not broadcast the
same relay role simultaneously. Maximum coverage also carries an explicit
maximum Bluetooth heartbeat age. A matching current heartbeat keeps the
standby receive-only; a stale or missing heartbeat causes its local action to
become `takeover_broadcast_and_receive`.

Task groups target either relay members or remaining mission members. A member
can have only one group instruction in a generation. A one-member group is an
individual assignment. A new allocation or in-flight reallocation increments
the generation rather than silently rewriting an active plan.

## In-flight relay reconfiguration

A relay reconfiguration carries a mission UUID, previous and proposed
generation, observation time, an explicit relay group, ordered per-mission
member hops, any disconnected members, nominated range-discovery candidates,
reserved relay count, remaining mission capacity, and operator-visible
warnings. Each hop includes its transport, current
three-dimensional separation from reported MSL positions, and score against
the supplied health policy.

For maximum coverage, the relay group also carries each active/standby
Bluetooth pair and the standby's measured protected neighbors. A decision
lists any active relays without a qualifying pair separately and does not
commit that single-aircraft path as maximum coverage.

Only a complete observed chain uses a higher proposed generation. When no
fresh bidirectional path meets the policy, automatic mode reports
`begin_range_discovery`; a manual relay list instead reports
`operator_action_required` and is never silently expanded. The live request
includes the currently committed relay members, so an unchanged chain is not
republished and recovered direct links explicitly release relay aircraft in a
new generation.

## Relay runtime configuration

A relay runtime configuration is a durable mission record supplied by ARC UI.
It holds the initial accepted generation and relay set, anchor, required
mission members, candidate role/suitability inventory, health policy, manual
control, maximum position age, coverage profile, optional Bluetooth handover
policy, and current active/standby pair assignments. It holds no dynamic
vehicle position or radio measurement: companions construct their own live
request from this policy plus synchronized telemetry and relay-link
observations.

## Delivery classes

| Class | Durable | Reliable | Redundancy | Lifetime |
| --- | --- | --- | --- | --- |
| Emergency | Yes, for audit and acknowledgement | Yes | Two paths when available | 5 seconds |
| Acknowledgement | Yes | Yes | One path | Until superseded |
| Mission | Yes | Yes | One path | Until superseded |
| Telemetry | No; latest value only | No | One path | 2 seconds |
| Bulk | Yes and resumable | Yes | One path | Until transferred |
