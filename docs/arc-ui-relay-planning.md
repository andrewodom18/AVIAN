# ARC UI relay and mission planning

## Operator workflow

ARC UI supplies the mission ID, marked objective area, available aircraft,
current platform suitability, and a calibrated radio range or link budget.
AVIAN returns a proposed relay corridor before the mission is activated.

The response tells the operator:

- how many relay stations are needed;
- which aircraft are reserved as relays;
- how many aircraft remain for the payload mission;
- the proposed latitude, longitude, and MSL altitude of every relay station;
- maximum planned hop distance and utilization of the derated radio range;
- whether the range is `field_calibrated` or only a `free_space_model`;
- whether the plan is eligible for activation; and
- minimum aircraft and node-failure tolerance at each station; and
- whether the proposal is `healthy`, `degraded`, or `infeasible`.

The planner is deterministic. Given the same inventory, geometry, policy, and
override, ARC UI and any AVIAN node obtain the same assignment. It does not
elect a leader or make a ground station necessary after activation.

## Range evidence: measured, not guessed

AVIAN does not have a universal drone spacing or a default “Silvus range.” The
same radio has different usable distance with its selected band and bandwidth,
TX setting, antennas, airframe installation, altitude, terrain/obstructions,
interference, payload traffic, and desired loss/latency margin.

The request therefore carries one of two range sources:

| Source | Meaning | Can activate a mission? |
| --- | --- | --- |
| `field_calibrated` | A derated usable segment measured for the actual radio, airframe, terrain class, altitude band, and traffic class | Yes, when feasibility is `healthy` |
| `free_space_link_budget` | A calculated first pass from frequency, transmit power, antenna gains, sensitivity, known losses, and fade margin | No; ARC UI must show the proposal as planning-only |

`activation_ready` is false for every free-space-only result, even if its
spacing and redundancy are otherwise healthy. This prevents a manufacturer
specification or an ideal path-loss calculation from being mistaken for a
guaranteed field range.

The SL5200 published data is useful as an input, not a final answer: its
datasheet gives 2 W native transmit power, receive sensitivity of -101 dBm at
5 MHz or -107 dBm at optional 1.25 MHz, selectable 1.25–20 MHz channels, and
several frequency-band options. AVIAN includes an SL5200 5 MHz link-budget
helper using the published 33 dBm native power and -101 dBm sensitivity, but
the operator must provide the actual center frequency, antenna gain,
installation/terrain loss, and fade margin. [SL5200 datasheet](https://silvustechnologies.com/wp-content/uploads/2026/02/StreamCaster-LITE-5200-SL5200-OEM-Module-Datasheet.pdf)

For each aircraft/radio/antenna configuration, calibration should collect
bidirectional packet loss, latency, goodput, received signal quality, and link
margin across altitude, range, terrain/obstruction, and motion bins. ARC UI
uses the lowest range that meets the mission delivery target for the current
bin, then retains additional margin before making a `field_calibrated` plan.

## Automatic coverage profiles

ARC UI exposes exactly two automatic relay choices through
`policy.coverage`:

| Profile | Reservation | Station behavior | Capacity effect |
| --- | --- | --- | --- |
| `minimum` | One aircraft per measured-required station | That aircraft is the station's sole relay broadcaster. | Preserves the most aircraft for mission tasks. |
| `maximum` | Two aircraft per measured-required station | Both are assigned to receive relay traffic. One is the active broadcaster; the other is a Bluetooth-coordinated receiving standby. | Reserves twice as many relay aircraft, but tolerates one station-member failure after handover. |

The pre-mission response names the active broadcaster, standby receiver(s),
and `peer_coordination` for every station. Automatic `maximum` creates an
exact pair at every station; a manual relay-count or member override can
still deliberately create an incomplete or extra-staffed station, which ARC
UI reports as `degraded` when it falls below the selected profile.

Only the active member broadcasts relay traffic. A standby keeps receiving
the relevant traffic and maintains a local Bluetooth peer connection, so it
has the information needed to assume broadcast duty when the active peer is
declared failed. `maximum` therefore requires an explicit
`paired_handover.max_bluetooth_heartbeat_age_ms`; a plan without it is
rejected. The designated standby switches to broadcast-and-receive only after
its matching active-peer heartbeat has aged beyond that value. The active and
all additional manually assigned standbys remain receive-capable, but only
the designated pair can automatically change broadcast ownership. The planner
never asks both members to broadcast the same relay role simultaneously.

The heartbeat deadline must be calibrated for the actual Bluetooth companion
link and mission latency objective; the sample's 1,500 ms is illustrative,
not a universal recommendation. A local radio adapter applies the resulting
`broadcast_and_receive`, `receive_only`, or
`takeover_broadcast_and_receive` action. Hardware transmit interlocks and
the adapter-specific controller are still required before physical radio
actuation.

The planner ranks eligible aircraft by:

1. higher relay suitability;
2. lower mission utility, preserving the best payload aircraft where possible;
3. stable node ID for deterministic tie-breaking.

ARC UI or a future onboard estimator calculates normalized suitability scores
from radio compatibility, antenna installation, battery/endurance, thermal
limits, payload value, and platform restrictions.

## Automatic and manual controls

The `allocation` object supports three modes:

| Mode | Result |
| --- | --- |
| `automatic` | AVIAN selects the exact one-per-station or active/standby-pair reservation defined by `policy.coverage` |
| `relay_count` | Operator sets the number of relays and optionally the number of stations |
| `relay_members` | Operator selects exact aircraft and optionally the number of stations |

If a lower relay count can still span the corridor but leaves some stations
with fewer aircraft than the selected profile, the result is `degraded`. If
the resulting hops exceed the usable range, it is `infeasible`. Increasing the
station count can shorten hops; adding aircraft without adding stations adds
receiving standbys but never adds a second broadcaster to a station.

ARC UI can send several `relay_count_previews` in one request. The response
contains the full impact of each choice, allowing a slider or stepper to show
the communications and mission-capacity tradeoff before the operator accepts
it. ARC should first present the two automatic profile results, then use these
previews only for a deliberate manual change. An invalid preview is returned
with an error without discarding the main proposal.

## In-mission chain discovery and regrouping

Pre-mission corridors are useful when calibrated range is already known, but
they are not enough for a moving swarm. During an active mission, every Linux
companion can run the same `InFlightRelayPlanner` over a shared live snapshot.
There is no elected coordinator: a peer can publish the deterministic outcome
as a `RelayReconfiguration` mission record, and all peers can independently
verify it before accepting its next generation.

The runtime request requires these inputs; none has a hidden generic default:

| Input | Why it is required |
| --- | --- |
| Fresh MSL positions and, when available, AGL for the ground anchor and every aircraft | Calculates each current three-dimensional hop and retains the altitude context needed for RF assessment. |
| Current available/relay-eligible/mission-eligible state, suitability, and mission utility | Selects relay candidates while keeping operator-required mission aircraft out of hidden relay duty. |
| Per-underlay rolling observations: both endpoint IDs, time, sample window, bidirectional confirmation, availability, latency, loss, goodput, signal quality, stability, Fresnel clearance, and optional received power/link margin | Establishes whether a link is truly usable now. A one-way or stale observation is not a chain edge. |
| Mission-specific health policy: freshness, latency/loss ceilings, goodput/signal/stability/Fresnel floors, and optional margin floor | Defines "reliable" for this mission and radio configuration. |
| Coverage profile and, for `maximum`, a positive Bluetooth heartbeat deadline | Minimum accepts one live relay per chain hop. Maximum additionally requires a distinct Bluetooth-linked standby with measured non-Bluetooth receive-ready links to every neighbor protected by its active relay. |
| Automatic allocation or an exact manual relay-member list | Lets the operator preserve control. An unsatisfied manual list never causes AVIAN to take an unlisted aircraft. |

For every active mission member, runtime planning searches only direct links or
paths whose intermediate nodes are relay-eligible aircraft. It returns ordered
hops with measured underlay, current calculated separation, and health score.
It also returns a single explicit relay group plus the mission members it
serves. That is the grouping ARC UI needs to show a chain or branch and the
number of payload aircraft affected. Every decision includes
`reserved_relay_count` and `mission_drones_remaining`; only a complete
`form_relay_chain` reserves aircraft, so partial discovery paths do not
silently reduce mission capacity.

For `maximum`, a runtime relay group also includes one
`broadcast_pairs` entry per active relay: the active broadcaster, its distinct
Bluetooth standby, and the named neighboring hops whose current non-Bluetooth
links the standby is ready to receive and take over. If that pairing evidence
is incomplete, AVIAN reports `unpaired_active_relays`, begins measured
discovery (or requests operator action for a manual list), and does **not**
commit a single-aircraft route as maximum coverage.

The runtime request also carries `current_relay_members`. That makes the
decision stateful without assigning a coordinator: unchanged chains are
maintained instead of republished on every evaluation, while recovered direct
links produce an explicit `release_relay_chain` update.

| Runtime outcome | Meaning |
| --- | --- |
| `maintain_direct` | All required mission members currently have healthy direct anchor links. |
| `maintain_relay_chain` | The currently committed relay group still matches the observed routes, so no new generation is published. |
| `form_relay_chain` | Live observations show a complete multi-hop path. The result reserves its exact relay group and proposes the next mission generation. |
| `release_relay_chain` | Healthy direct paths returned, so the prior relay group is released in the next generation and mission capacity increases. |
| `begin_range_discovery` | A current path is missing and automatic allocation is enabled. AVIAN nominates available relay candidates for a measured probing workflow; it does **not** invent a chain from an untested distance. |
| `operator_action_required` | The exact manual group cannot form all required paths. AVIAN reports the affected mission members and leaves unlisted aircraft untouched. |

Range discovery is intentionally conservative. A live chain can be formed only
from fresh bidirectional observations that meet the mission policy. If those
observations do not exist, the system reports the gap, exposes candidates for
probing, and waits for measured results to be shared; it does not claim that a
radio specification, 25,000 ft MSL ceiling, or free-space calculation proves
an unobserved hop will work.

## Synthetic planner test

The test suite retains the earlier 50-aircraft, one-mile scenario only to
exercise manual-control behavior. Its 136 m usable segment is a synthetic
field-calibrated input; it is not a recommendation for any radio or airframe.
With that deliberately supplied input, the planner shows:

| Operator choice | Stations | Payload aircraft left | Result |
| ---: | ---: | ---: | --- |
| Minimum (11 relays) | 11 | 39 | Healthy: one broadcaster per station, no local standby |
| Maximum (22 relays) | 11 | 28 | Healthy: active/standby Bluetooth pair at every station |
| 10 relays | 10 | 40 | Infeasible: about 146 m per hop |
| 20 relays | 11 | 30 | Degraded for `maximum`: two stations have only one aircraft |
| 24 relays, 12 stations | 12 | 26 | Healthy: shorter hops of about 124 m |

Real mission values come from the range source described above. ARC UI must
display its calibration ID, measured conditions, age, and range assumptions,
then recalculate when altitude, terrain, interference, antenna configuration,
or measured link health changes.

## Individual and group instructions

`task_groups` assign named instructions to explicit aircraft. A group targets
either the `relay` pool or the remaining `mission` pool. A one-aircraft group
is an individual instruction. The contract rejects empty instructions,
unknown members, cross-pool assignments, duplicate group IDs, and assignment
of one aircraft to two groups in the same mission generation.

The accepted pre-mission result is a versioned `MissionAllocation` with a
mission UUID and positive generation. A runtime chain decision is a
`RelayReconfiguration` mission payload. Both are serializable through PEAT;
new complete relay chains increment the mission generation instead of silently
rewriting an active allocation. The ARC-to-PEAT submission endpoint is not
implemented yet; the current milestone supplies the validated planning
contract and JSON planning engine.

## Run the planning engine

```sh
cargo run -p mission-planner -- \
  --input ./examples/arc-relay-request.sample.json \
  --output ./relay-plan.json
```

Omit `--input` or `--output` to use standard input or standard output. The
sample contains five aircraft to keep the file readable. It uses published
SL5200 5 MHz parameters at a representative in-band frequency solely as a
free-space planning example; it intentionally returns `activation_ready: false`
until replaced with a field calibration.

`arc-inflight-relay-request.sample.json` demonstrates a live Silvus chain for
two mission members. It returns `form_relay_chain` and a new generation with
two relay aircraft. Replace the sample observations with current measurements;
they are illustrative input only.

## Onboard evaluation

ARC UI distributes one durable `RelayRuntimeConfiguration` for the accepted
mission generation. It contains the anchor, mission members, candidate
eligibility/suitability, health policy, allocation control, maximum position
age, coverage profile, Bluetooth handover policy when required, and any relay
members/pair assignments already committed by the pre-mission plan. It does
not contain live positions or radio values.

Each Linux companion combines that shared policy with synchronized AVIAN
telemetry and `RelayLinkObservation` records. A missing telemetry record is an
input error; a stale position keeps the aircraft in the inventory but marks it
unavailable for relay selection. This prevents a crashed or stale aircraft
from being silently chosen as a repeater.

`mesh-agent` enables the loop with a local copy of the shared configuration:

```sh
cargo run -p mesh-agent -- \
  --name aircraft-017 \
  --formation-key-file ./formation.key \
  --relay-runtime-config ./examples/relay-runtime-config.sample.json
```

The agent evaluates the common snapshot once per second by default. It emits a
mission-class `RelayReconfiguration` only when a relay group must form or
release, or when it needs to report discovery/manual-action status; unchanged
healthy states are not repeatedly written. Record IDs include the publishing
node, so redundant peers can report the same deterministic conclusion without
creating a "mother" drone. `--relay-evaluation-ms` changes the cadence.

Radio adapters feed the loop through the agent's optional local
`--relay-observation-listen 127.0.0.1:9100` UDP listener. The normalized
bidirectional observation schema is in
[`relay-link-observation.sample.json`](../examples/relay-link-observation.sample.json).

## Current boundary

Pre-mission allocation and the deterministic in-flight decision core are
implemented. Complete closed-loop operation still requires:

- terrain and obstruction-aware corridor routing instead of one straight
  geodesic corridor;
- a Silvus/alternate-underlay collector that continuously supplies the live
  observation snapshot to the onboard companion;
- the local Bluetooth heartbeat transport and radio-adapter control that
  applies the paired-station receive-only/takeover action;
- onboard station-hold execution and relay-health reporting;
- automatic physical backfill when a relay degrades or crashes;
- mission-generation reconciliation while aircraft are moving; and
- ARC UI approval, pause, manual reassignment, and abort workflows.

Any automatic reallocation must publish a higher generation and preserve
operator visibility. It must not silently take payload aircraft away from a
task without reporting the changed mission capacity.
