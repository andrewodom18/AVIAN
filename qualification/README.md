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
