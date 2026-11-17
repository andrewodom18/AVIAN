# Engine tests

Requires Docker, Python 3, Git, internet access for the first build, and a local
checkout of EasyTier v2.6.4. Builds verify the pinned source revision; downloaded
release binaries are checksum verified. Runtime tests use disposable Linux network
namespaces with no external network access.

## Build

```sh
mkdir -p .local
python3 qualification/build.py --traffic-policy --live-revocation --failover \
  --output .local/network-policy
```

The combined build applies membership, authenticated forwarding, traffic policy,
live revocation, and failover patches in order. `build.json` records patch and
binary hashes. The experimental engine is not deployed by these commands.

## Run

```sh
python3 qualification/authenticated-forwarding/check.py --network-policy \
  --source /path/to/easytier-v2.6.4 --output qualification/results/engine-tests/runtime.json
python3 qualification/traffic-policy/run.py --engine-directory .local/network-policy \
  --output qualification/results/policy
python3 qualification/revocation/check.py --engine-directory .local/network-policy \
  --output qualification/results/revocation
python3 qualification/failover/run.py --engine-directory .local/network-policy \
  --fast-failover --fault blackhole --output qualification/results/blackhole
python3 qualification/failover/run.py --engine-directory .local/network-policy \
  --fast-failover --fault relay_stop --output qualification/results/relay-stop
cargo run -p avian-routing --release --example measure_growth -- 5 67108864
```

Results distinguish passed, failed, and untested checks. The growth example measures
route calculation rather than packet throughput. Private test keys stay in the
container temporary filesystem; results and binaries are ignored by Git.

## Operating notes

For topology-wide policy, build with
`python3 qualification/build.py --topology-policy --output .local/topology-engine`.
This includes directed per-connection eligibility and measured latency.

```sh
python3 qualification/topology-policy/run.py --engine-directory .local/topology-engine \
  --output qualification/results/topology
python3 qualification/revocation-distribution/run.py \
  --engine-directory .local/topology-engine --service-directory .local/distribution-bin \
  --metered-sync --output qualification/results/distribution
```

The distribution harness requires Linux `avianctl` and `avian-revocationd` binaries
in `--service-directory`. A macOS build cannot run inside its Linux container.

Use `ET_REQUIRE_VERIFIED_MEMBERSHIP=1` with protected public trust files produced
by the membership verifier. Invalid initial caches stop startup; invalid replacements
retain accepted leases only until expiry. Ordinary members do not need administrator
secrets. The Rust verifier checks signatures and revision ordering before publishing
an engine cache.

Patched peers use a versioned Noise handshake domain and reject incompatible peers.
Do not mix engine variants in one mesh. The forwarding envelope adds 40 bytes;
native interface configuration reserves that budget. Externally supplied virtual
interfaces require equivalent MTU settings. Relays enforce local policy and remain
trusted members.

[Traffic policy](traffic-policy/README.md), [revocation](revocation/README.md), and
[failover](failover/README.md) describe configuration and test scope.

## Runtime, load, and platform checks

```sh
python3 qualification/build.py --priority-forwarding --output .local/priority-engine
```

This build includes topology policy and the opt-in `ET_PRIORITY_FORWARDING=1`
profile: bounded per-class forwarding queues with weighted service and an 8 KiB
TCP send buffer. Every class has reserved queue capacity; a full queue returns a
buffer-full error instead of blocking shared dispatch. Overload can drop packets
in any class. This bounds admission work rather than guaranteeing lossless control.
It does not reserve physical-link bandwidth or eliminate TCP
head-of-line blocking.

Run the exact patched engine unit checks with:

```sh
python3 qualification/authenticated-forwarding/check.py --priority-forwarding \
  --source /path/to/easytier-v2.6.4 --output qualification/results/priority-unit/runtime.json
```

See [runtime control](runtime-control/README.md), [load tests](load/README.md), and
[platform checks](platforms/README.md) for commands, prerequisites, and measured scope.

## Managed connection profile

```sh
python3 qualification/build.py --managed-network --output .local/managed-engine
python3 qualification/measured-routing/run.py \
  --engine-directory .local/managed-engine \
  --output qualification/results/measured
python3 qualification/network-helper/check.py \
  --output qualification/results/network-helper.json
python3 qualification/native-runtime/run.py \
  --engine-directory .local/managed-engine \
  --service-directory /path/to/linux-service-binaries \
  --priority-forwarding --services \
  --output qualification/results/native-runtime
```

The managed profile includes measured routing, bound underlays, private RPC, and
scoped Linux network resources. `build.json` binds the shared routing and helper
client sources as well as every binary. Peers must use the same protocol profile.
Helper tests exercise unprivileged packet forwarding inside disposable namespaces;
they do not qualify an installed native service or authorize hardware integration.

The native-runtime runner requires Linux `aviand`, `avianctl`, and
`avian-revocationd` binaries matching the current sources. It runs unprivileged
managers and engines with a root helper inside disposable namespaces, including
helper-crash continuity, observed class routes, automatic capacity evidence,
signed services, revocation, and cleanup. Installed-service and hardware checks
remain separate qualification gates.
