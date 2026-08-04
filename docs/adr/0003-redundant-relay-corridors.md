# ADR 0003: Plan redundant relay corridors as mission resources

- Status: accepted
- Date: 2026-08-04

## Context

Some missions operate beyond direct ground or cloud connectivity. Aircraft
must be reserved to relay AVIAN traffic, and the operator needs to understand
the payload-capacity cost and communication effect before launch. A simple
single-aircraft chain would make each relay a single point of failure.

## Decision

1. ARC UI requests a relay assessment before activating a remote mission.
2. AVIAN models a corridor as evenly spaced relay stations between the base
   and the objective entry point.
3. The default desired station redundancy is two aircraft, configurable by
   policy.
4. Automatic allocation prefers aircraft with high relay suitability and low
   mission utility, using node ID as a deterministic tie-breaker.
5. Operators can override relay count, exact members, and station count.
6. Every proposal and override reports feasibility, hop utilization,
   station-failure tolerance, reserved relay count, and payload aircraft left.
7. Accepted individual and group instructions are explicit node assignments
   in a versioned mission allocation.
8. During a mission, all companions can derive a relay decision from the same
   fresh bidirectional link-observation snapshot. A complete observed path
   reserves a named relay group and increments the mission generation.
9. Missing or stale observations trigger measured range discovery, not a
   guessed physical chain. Exact manual relay overrides are preserved.

## Consequences

- Relay aircraft are visible mission resources rather than hidden networking
  behavior.
- No coordinator is required to reproduce a plan.
- Reducing relay count may first reduce local failure tolerance and then make
  coverage infeasible.
- Increasing relay count can improve redundancy or shorten hops depending on
  station count.
- Straight-line planning is only the first stage; terrain-aware routing and
  physical station-hold/backfill execution remain required.
