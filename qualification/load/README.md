# Isolated load checks

Requires Docker with privileged Linux network namespaces. Run from the repository root:

```sh
python3 qualification/build.py --priority-forwarding --output .local/priority-engine
python3 qualification/load/run.py --engine-directory .local/priority-engine \
  --priority-forwarding --output qualification/results/load \
  --growth-seconds 30 --memory-budget-mib 512 --load-seconds 10
python3 qualification/load/run.py --engine-directory .local/priority-engine \
  --priority-forwarding --scenario failover --fault blackhole \
  --output qualification/results/load-blackhole
python3 qualification/load/run.py --engine-directory .local/priority-engine \
  --priority-forwarding --scenario failover --fault relay-stop \
  --output qualification/results/load-stop
```

The container has no external network. Nodes, temporary keys, and application
traffic stay in disposable namespaces. Membership uses a synthetic public trust
fixture; these checks do not establish signed enrollment or signed revocation
delivery. Results and binary provenance are written to the output directory.

Growth admits real engine processes until the time or aggregate RSS admission
budget is exhausted. Each admitted join receives up to 15 seconds to converge.
The next admission reserves the largest observed process RSS plus 32 MiB.
These are test resource budgets, not membership limits. Reports include CPU time,
RSS, background underlay bytes, direct peers, local session counts, join times,
and cold leave/rejoin convergence. Underlay bytes include transport overhead.

The default workload offers 4 Mbps video and 4 Mbps bulk through a shared 2 Mbps
relay egress, with 20 Hz CS5 control echo probes. The control gate requires at
least 95% delivery, p95 round trip at most 500 ms, and round trip and delivery gaps
at most two seconds. Video and bulk must both advance across the workload midpoint.
Generators must complete their configured duration and successfully send at least 95% of
the configured video/bulk bytes. Revocation removes public cache membership
while control probes continue; successful control delivery must cease within two seconds.
Competing video/bulk use a separate authorized sink through the same relay during
revocation, so removing the control destination does not remove the congestion.
This phase requires at least one admitted leaf within the supplied resource budgets.

Failover uses a diamond with a 250 ms slower alternate. A control-only socket burst
and physical relay counters identify the preferred path; competing workload counters
must corroborate it before the fault. The alternate must carry traffic, video and
bulk must progress across the fault, and control delivery gaps must stay within
two seconds. Ambiguous path observations fail the check.

`--priority-forwarding` requires matching binary hashes and the current seven-patch
manifest. It enables reserved bounded queues, weighted packet service in the ratio
8 control : 4 telemetry : 2 video : 1 bulk, and an 8 KiB TCP send buffer. Every full
class queue rejects excess packets immediately so shared dispatch remains bounded.
FIFO order is preserved within each class. Omit the flag for a baseline comparison.
Measurements qualify the recorded topology, transport, rates, resources, and duration;
they do not establish a population ceiling or general platform performance guarantee.
