# Formation membership

## Purpose

A membership manifest lets every AVIAN companion receive the same aircraft
list and independently select its direct PEAT neighbors. It is a provisioning
document, not a leader election or a ground-station dependency.

The current schema contains 5-200 aircraft. Each entry has:

- the stable AVIAN node name;
- the PEAT endpoint ID derived from that name and the formation secret; and
- one to eight ordered IP addresses across Silvus and alternate underlays.

See [the sample manifest](../examples/membership.sample.json).
Its endpoint IDs and addresses are placeholders; replace them with the values
printed by the provisioned companions and the deployment's routed addresses.

## Start an aircraft

```sh
cargo run -p mesh-agent -- \
  --name aircraft-001 \
  --formation-id avian-demo \
  --formation-key-file ./formation.key \
  --membership-file ./examples/membership.sample.json \
  --bind 0.0.0.0:9000
```

At startup, the agent rejects the manifest if its schema or formation is
wrong, its size is outside 5-200, identities are duplicated, addresses are
invalid, the local aircraft is absent, or the locally derived PEAT endpoint
does not match the local entry. It then computes at most eight direct peers.

`--max-mesh-peers` may reduce the degree to 2, 4, or 6. All aircraft in a
formation must use the same value or they will compute asymmetric overlays.

## Generations and changes

`generation` must be a positive, monotonically increasing deployment value.
The current agent loads one generation at process start. Coordinated live
updates, conflict handling, and graceful peer replacement still need to be
implemented before changing membership during flight.

The manifest is currently trusted as a locally provisioned file and is not
cryptographically signed. File permissions and deployment tooling must protect
it. PEAT formation authentication still protects network admission, and the
local endpoint-ID check prevents assigning the aircraft a different identity
by mistake. Signed manifest distribution is planned.
