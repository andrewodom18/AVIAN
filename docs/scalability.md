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

## Membership and failure behavior

No member has permanent leadership or unique state. A signed membership view
will supply stable node identities and reachable addresses. When membership
changes, each node can recompute the overlay locally. PEAT carries durable
state across the surviving graph and reconciles it after partitions.

The present `mesh-agent` accepts static bootstrap peers and rejects more than
eight. Loading a shared membership manifest, live discovery, and topology
reconciliation are the next implementation steps; until then, the scale
planner is a tested library contract rather than a complete 200-aircraft
deployment mechanism.
