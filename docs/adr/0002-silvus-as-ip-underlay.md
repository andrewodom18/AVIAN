# ADR 0002: Treat Silvus as an IP MANET underlay

- Status: accepted
- Date: 2026-08-04

## Context

AVIAN must support formations of 5-200 aircraft and continue across changing
radio connectivity. Silvus StreamCaster radios already provide a mobile ad hoc
network and IP data interfaces. AVIAN also has to remain usable on aircraft
with different radios and multiple simultaneous links.

## Decision

1. StreamCaster supplies an IP underlay; it does not own AVIAN membership,
   mission state, command authority, or vehicle behavior.
2. Silvus is a first-class `TransportKind` and a preferred PEAT/PACE path.
3. Each PEAT peer may publish multiple ordered IP addresses for Silvus and
   alternate underlays while retaining one stable endpoint identity.
4. AVIAN keeps at most eight direct application peers instead of building a
   full mesh on top of the Silvus MANET.
5. The core remains independent of proprietary Silvus APIs. A vendor metrics
   adapter will be added only against an available, versioned API contract and
   tested radio.

## Consequences

- Existing PEAT synchronization can traverse StreamCaster through standard IP.
- Reconnection can try Silvus and alternate peer addresses without replacing
  AVIAN identities.
- Aircraft without Silvus remain compatible.
- Radio-level roaming is distinct from AVIAN membership and state convergence.
- Live metrics, automatic address advertisement, and make-before-break path
  switching remain required for complete seamless handoff.
