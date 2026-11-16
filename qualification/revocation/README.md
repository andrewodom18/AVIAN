# Live revocation

Apply `live-revocation.patch` after membership and authenticated-forwarding patches.
The connection watchdog rechecks local and neighbor keys every 100 ms, invalidates
sessions, and stops both directions after admission is lost. Data forwarding also
checks origin and destination membership, including traffic through healthy relays.
Trust-file metadata checks are shared and throttled per process.

```sh
python3 qualification/revocation/check.py \
  --engine-directory /path/to/candidate-binaries \
  --output qualification/results/revocation
```

The isolated Linux test keeps TCP connections and UDP traffic active while an
atomic public-cache update removes an endpoint, removes every endpoint, or expires
a lease. Separate scenarios inform only the source or only the relay. It also
checks malformed replacement caches stop working when the last valid lease expires,
and that timely renewal preserves traffic. Every scenario uses fresh engine state;
revoked identities are not reinstated during a scenario.

These checks prove local enforcement after verified cache delivery. They do not
prove network-wide distribution of signed revocations or trust revision checkpoints.
The Rust verifier remains responsible for signatures and revision ordering before
publishing the protected engine cache.
