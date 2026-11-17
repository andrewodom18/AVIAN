# Signed revocation distribution

Requires the isolated engine binaries, Linux `avianctl` and
`avian-revocationd`, and Docker.

```sh
python3 qualification/revocation-distribution/run.py \
  --engine-directory .local/network-policy-final-corrected \
  --service-directory .local/distribution-bin \
  --output qualification/results/revocation-distribution
```

Use `--metered-sync` with the topology-policy engine to declare every physical
link metered. The harness explicitly permits synchronization TCP ports 9180 and
9181, verifies that unrelated HTTP remains blocked, and marks its persistent test
traffic as control traffic. Port 9181 serves intentionally invalid signed-record
responses during the negative tests; it is a fixture, not an application default.

The harness enrolls independently keyed peers once, then starts them without the
administrator signing key. Signed updates travel over the overlay through a relay.
Tests cover invalid signatures, wrong networks, replayed or missing revisions,
partition recovery, durable restart, and revoking
an endpoint while persistent TCP and UDP traffic is active. It never edits peer
trust caches after startup; the distribution service verifies, persists, and
publishes received updates. Test credentials remain in the disposable container.

Reports record binary hashes and preserve failures. Passing these scenarios does
not establish behavior under arbitrary load or authorize hardware deployment.

## Competing traffic

Use the current seven-patch engine to exercise signed distribution under contention:

```sh
python3 qualification/revocation-distribution/run.py \
  --engine-directory .local/congestion-engine \
  --service-directory .local/distribution-bin \
  --priority-forwarding --competing-load \
  --output qualification/results/distribution-load
```

This adds two load-evidence gates to the normal ten checks. Relay egress is shaped
to 2 Mbps while video and bulk each offer 4 Mbps toward the unrevoked administrator.
Synchronization TCP ports 9180 and 9181 are registered as control traffic; the
persistent TCP/UDP test traffic is marked as control. The destination is revoked
through signed overlay HTTP distribution while these flows run. Sender counters,
receiver progress before and during enforcement, worker liveness, and shaper
statistics are recorded. Each class must sustain at least 95% of its stated
offered rate over observed sample intervals, keep sender observation gaps below
one second, and make receiver progress in both windows. An inactive competing
flow fails the load checks.

`--competing-load` requires `--priority-forwarding` and cannot combine with
`--metered-sync`, whose policy blocks video and bulk. The existing metered scenario
retains eleven checks. The runner validates the current seven patch hashes before
priority execution and records both flags in provenance. This isolated workload
does not establish behavior for every capacity, topology, or radio.

Persistent TCP uses newline-delimited sequence frames and retains partial frames
across timeouts. TCP and UDP count valid delayed echoes from earlier sequences as
deliveries and record their receipt times. The two-second revocation bound checks
those receipt times, requires three subsequent completed attempts, and verifies
both generators remain alive. Loaded sampling allows a 500 ms response wait;
normal sampling retains 150 ms. Concurrent JSONL readers ignore incomplete lines.

Run the local sampler regressions without Docker:

```sh
python3 -m unittest discover -s qualification/revocation-distribution -p 'test_*.py'
```
