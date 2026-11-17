# Measured forwarding

Build and run from the repository root:

```sh
python3 qualification/build.py --measured-routing --output .local/measured-engine
python3 qualification/measured-routing/run.py --engine-directory .local/measured-engine \
  --output qualification/results/measured-routing
python3 qualification/measured-routing/run.py --engine-directory .local/measured-engine \
  --scenario growth --growth-seconds 30 --memory-budget-mib 512 --load-seconds 10 \
  --output qualification/results/measured-growth
python3 qualification/measured-routing/run.py --engine-directory .local/measured-engine \
  --scenario blackhole --output qualification/results/measured-blackhole
python3 qualification/measured-routing/run.py --engine-directory .local/measured-engine \
  --scenario relay-stop --output qualification/results/measured-stop
```

Docker runs privileged namespaces with no external network. Temporary credentials,
interfaces, processes, and packets stay inside the disposable container. The fixture
uses synthetic public trust; signed enrollment and distribution have separate checks.
Growth uses time and RSS admission budgets, without a membership target or ceiling.

Actual forwarding and connection selection use the shared routing crate. Control and
telemetry rank reliability, then latency and jitter. Video and bulk first maximize
the measured bottleneck, then reliability and delay. Unknown unmetered paths remain
usable with explicit degraded status. Capacity evidence is a bounded probe's delivered
rate lower bound, never spare bandwidth inferred from passive traffic. The compatibility
field `available_capacity_bps` retains this lower-bound meaning. Advertised capacity
rounds down into ten-percent buckets, including low-rate links.

Quality uses a three-second local sample window. Authenticated topology expires twenty
seconds after receipt of a newer origin version; duplicate versions cannot renew it.
Each edge carries its actual sample age and generation. Measured publications refresh
at least once per second. Successful synchronization does not enter failure backoff. Probe capacity expires thirty seconds after the last observed
reply. Healthy improvements require a sustained five-second hold and a meaningful
quality or capacity gain. Minor ranking changes between healthy parallel connections
retain the pending path after current policy and quality are rechecked. Failed,
revoked, or prohibited paths bypass the hold.
`improvement_hold_ms` reports the actual hold of the last healthy change.

Inside an isolated candidate process, the native observation commands are:

```sh
easytier-cli -o json route measured --limit 100 --class video
easytier-cli -o json route links --limit 100
easytier-cli -o json route probe --peer-id PEER --connection-id UUID \
  --duration-ms 500 --offered-bps 8000000
```

Observation pages return `items`, `next_cursor`, and a process-local `revision`.
Pass the opaque cursor with `--after`. Limits apply to a page, not membership:
default 100 items, maximum 1000, and a 256 KiB serialized response budget. Path summaries
include hop counts instead of unbounded path arrays. Revision changes reflect selected
paths or degraded/permission state, rather than every metric fluctuation. Stable node
identifiers come only from active, unambiguous local verified-cache entries matched to
authenticated transport keys; missing mappings return null. Connection byte counters
are cumulative observations and do not supply capacity estimates.

Probes require priority forwarding, fresh quality, low loss/jitter, and empty send
queues. Each process permits one active probe; sessions have a thirty-second cooldown.
Requests allow 50–500 ms, 64 kbit/s–8 Mbit/s, and at most 512 KiB payload. Probe frames
use bulk service and bounded echo budgets. Automatic hosts start at 64 kbit/s for
50 ms. Complete delivery within twice that window permits a next offer bounded by
twice both the previous offer and confirmed delivery rate. Longer round trips can
keep this conservative short probe at its initial rate; an explicit 500 ms request
provides a longer measurement window. Exploratory offers are never capacity evidence.
Metered probes require an explicit request
and local bulk authorization at both ends. All peers use the incompatible Noise domain
`avian-peerconn-noise-hop-measured-v1`.

`AVIAN_TRAFFIC_POLICY_FILE` takes precedence over startup policy JSON. The runtime
publishes complete JSON atomically with private ownership/permissions. The engine
checks current policy before sealing queued packets; cache refresh is bounded to
100 ms. Missing, malformed, or unsafe input denies media and probes. Managed mode
also restricts unresolved physical egress. The file schema retains explicit
`metered_addresses`, `control_tcp_ports`, `allow_metered_video`, and
`allow_metered_bulk`. Received announcements cannot change this local authorization.
Unix ownership is checked in the engine; Windows ACL protection belongs to the
supervising runtime.
