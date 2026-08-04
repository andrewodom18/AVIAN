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

## Why relay stations are redundant

A single-file chain fails when any middle aircraft is lost. AVIAN instead
plans relay stations. The default policy requests two aircraft per station, so
one aircraft can fail at a station without immediately removing that part of
the corridor. Deployments can request a different redundancy level.

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
| `automatic` | AVIAN selects the recommended relay count and members |
| `relay_count` | Operator sets the number of relays and optionally the number of stations |
| `relay_members` | Operator selects exact aircraft and optionally the number of stations |

If a lower relay count can still span the corridor but leaves some stations
with fewer aircraft than requested, the result is `degraded`. If the resulting
hops exceed the usable range, it is `infeasible`. Increasing the station count
can shorten hops; adding aircraft without adding stations increases local
redundancy or supplies hot spares.

ARC UI can send several `relay_count_previews` in one request. The response
contains the full impact of each choice, allowing a slider or stepper to show
the communications and mission-capacity tradeoff before the operator accepts
it. An invalid preview is returned with an error without discarding the main
proposal.

## Synthetic planner test

The test suite retains the earlier 50-aircraft, one-mile scenario only to
exercise manual-control behavior. Its 136 m usable segment is a synthetic
field-calibrated input; it is not a recommendation for any radio or airframe.
With that deliberately supplied input, the planner shows:

| Operator choice | Stations | Payload aircraft left | Result |
| ---: | ---: | ---: | --- |
| 10 relays | 10 | 40 | Infeasible: about 146 m per hop |
| 20 relays | 11 | 30 | Degraded: coverage works, but two stations have only one aircraft |
| 22 relays | 11 | 28 | Healthy: two aircraft at every station |
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

The accepted result is a versioned `MissionAllocation` with a mission UUID and
positive generation. It is serializable as an AVIAN mission payload and can be
stored through PEAT. The ARC-to-PEAT submission endpoint is not implemented
yet; the current milestone supplies the validated planning contract and JSON
planning engine.

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

## Current boundary

This is pre-mission geometry and allocation. Complete closed-loop operation
still requires:

- terrain and obstruction-aware corridor routing instead of one straight
  geodesic corridor;
- live Silvus and alternate-link measurements feeding the range model;
- onboard station-hold execution and relay-health reporting;
- automatic backfill when a relay degrades or crashes;
- mission-generation updates distributed while aircraft are moving; and
- ARC UI approval, pause, manual reassignment, and abort workflows.

Any automatic reallocation must publish a higher generation and preserve
operator visibility. It must not silently take payload aircraft away from a
task without reporting the changed mission capacity.
