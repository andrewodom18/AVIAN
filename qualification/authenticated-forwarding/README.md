# Authenticated forwarding qualification

Run the candidate tests against a disposable checkout of the pinned engine:

```sh
python3 qualification/authenticated-forwarding/check.py \
  --source /path/to/easytier-v2.6.4 \
  --output qualification/results/hop/runtime.json
```

The runner applies membership and hop patches in order, records their hashes,
and requires all nine forwarding tests plus the existing secure-connection
regressions to execute and pass. Exit 2 means a gate failed; exit 1 means
the build or test execution failed. An empty test selection cannot pass.

Authentication between hops detects changes by parties without that hop's keys.
It does not stop an authorized relay from creating a different authenticated
packet. Relays must enforce local membership, traffic, and link policy; hop authentication does not establish that a relay behaves correctly.

The upstream metadata test in `../routing-extension/` is a baseline regression.
Candidate tests inject changes after hop sealing to exercise the outer envelope.
