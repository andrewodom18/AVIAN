# Local PEAT demonstration

This starts two real AVIAN mesh agents using PEAT's Automerge persistence and
Iroh QUIC transport. It does not use the in-memory simulation.

## Create a development formation key

Generate this once and give the same protected file to both nodes:

```sh
openssl rand -base64 32 > /tmp/avian-formation.key
chmod 600 /tmp/avian-formation.key
```

The formation ID and secret authenticate membership. A node using a different
pair cannot join the formation. The secret is read from a file and is never
printed by `mesh-agent`.

## First node

```sh
cargo run -p mesh-agent -- \
  --name avian/node-a \
  --bind 127.0.0.1:9001 \
  --storage /tmp/avian-node-a \
  --formation-id avian-local \
  --formation-key-file /tmp/avian-formation.key
```

Copy the printed `Peer spec` value.

## Second node

```sh
cargo run -p mesh-agent -- \
  --name avian/node-b \
  --bind 127.0.0.1:9002 \
  --storage /tmp/avian-node-b \
  --formation-id avian-local \
  --formation-key-file /tmp/avian-formation.key \
  --peer ENDPOINT_ID_FROM_NODE_A@127.0.0.1:9001
```

Each node's PEAT endpoint identity is deterministically derived from its stable
node name and formation secret, so it remains the same after a restart. Do not
reuse a node name for two active machines.

The automated convergence test goes further: it forms an authenticated
connection, writes an AVIAN mission record on one node, and verifies the other
node receives the identical record through PEAT's real persistent sync path.
If a configured peer is offline, `mesh-agent` continues operating and retries
the connection periodically; peer availability is never a startup dependency.
