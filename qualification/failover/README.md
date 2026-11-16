# Warm-alternate recovery

The harness creates an offline four-node diamond with individually keyed,
verified members. It sends sequenced UDP and persistent TCP echo traffic in both
directions, then stops the selected relay or drops its physical traffic without
changing link state. TCP connections are never reopened. Reports record route
change latency separately from the maximum application-delivery gap.

```sh
python3 qualification/failover/run.py --engine-directory .local/authenticated-engine-final \
  --output qualification/results/failover --fault blackhole
```

The optional `failover.patch` profile is enabled only with `--fast-failover` in
this harness (`ET_FAST_FAILOVER=1` in the engine). It probes every 200 ms, expires
a probe after 500 ms, and disconnects after three misses. This increases control
traffic and is unsuitable for paths whose normal latency exceeds that deadline.
Default engine detection remains unchanged. An established alternate is required;
this profile does not make a disconnected destination reachable.

The two-second gate applies to the measured delivery gap of every stream. A
failed run remains failed even if the route eventually converges. Reports do not
qualify other topologies, larger workloads, or hardware. Test addresses are local
fixtures and impose no product membership limit.
