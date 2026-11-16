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
