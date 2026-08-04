# AVIAN scalability contract

## Supported formation size

AVIAN's current design target is 5-200 aircraft. Ground and cloud endpoints
may join the same PEAT formation, but they cannot be required for aircraft to
keep exchanging state.

The overlay is not an application-level full mesh. Given the same membership
view, every node independently builds the same undirected graph from a ring
and long-range chords. The default direct-neighbor ceiling is eight.

| Aircraft | Full-mesh edges | AVIAN maximum edges |
| ---: | ---: | ---: |
| 5 | 10 | 10 |
| 25 | 300 | 100 |
| 100 | 4,950 | 400 |
| 200 | 19,900 | 800 |

The AVIAN column is the lower of the possible complete graph and `N * 8 / 2`;
duplicate and self edges reduce the actual number. This bounds direct
connection state and repeated application-level synchronization work to
linear growth.

## Automated checks

The `mesh-core` test suite verifies that overlays for 5, 25, 100, and 200
nodes are connected, have no more than eight neighbors per node, stay within
the linear edge budget, and have a diameter no greater than 16 hops. It also
checks that:

- removing any single aircraft from a 25-node overlay leaves it connected;
- a 200-node overlay remains connected after a distributed 20-aircraft loss;
- sizes below 5 or above 200 are rejected by the planner; and
- duplicate node identities are rejected.

These are topology guarantees, not a claim that 200 physical radios have been
field-tested. Hardware validation still needs a staged PEAT soak test at 5,
25, 100, and 200 processes, followed by radio-in-the-loop tests measuring
convergence time, airtime, packet loss, memory, CPU, and recovery under motion.

The traffic governor separately bounds routine source telemetry and unchanged
radio observations, while three rotating peers publish compact operator
summaries by default. See [traffic management](traffic-management.md). Relay
candidate and emergency-state updates deliberately bypass the routine bound;
their field-measured airtime remains part of the radio-in-the-loop validation.

## Membership and failure behavior

No member has permanent leadership or unique state. A versioned, locally
provisioned membership manifest supplies stable node identities and reachable
addresses. Every aircraft validates that its locally derived PEAT identity
matches its entry, then computes its neighbors independently. When membership
changes, each node can recompute the overlay locally. PEAT carries durable
state across the surviving graph and reconciles it after partitions.

The present `mesh-agent` can load the manifest at startup or accept static
bootstrap peers, and it rejects more than eight direct neighbors. Live
discovery, signed manifest distribution, generation reconciliation, and
in-flight topology changes remain future work.
