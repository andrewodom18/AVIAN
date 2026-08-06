# ARC radio mesh bootstrap

The `arc-radio-plugin bootstrap` command derives the exact stable PEAT endpoint
ID for every declared ARC surrogate and generates a bounded peer map before any
sidecar starts. This removes the staged "start, read endpoint IDs, edit peer
lists, restart" process.

Bootstrap is an offline, fail-closed generator. It does not scan a network,
contact a surrogate, expose the formation secret, deploy files, or write a
radio. The operator supplies the authoritative node names and dialable Silvus
IP addresses, reviews the generated map, and then deploys it through ARC.

## Prepare the inventory

Copy `examples/arc-radio-bootstrap-inventory.sample.json` outside the repository
and replace every sample value:

- `name` must exactly match the stable ARC Ansible inventory hostname used by
  the sidecar's `--source` argument;
- `radio_url` is the credential-free management base URL for that node's local
  StreamCaster; and
- `addresses` contains one to eight concrete, dialable PEAT addresses for the
  surrogate. Use the operational IP carried by Silvus, not `0.0.0.0` and not
  the radio-management address unless that address is actually routed between
  companions.

The port in each advertised address normally matches `peat_bind`, currently
UDP 4747. Multiple addresses may be supplied when a node has controlled
alternate underlays.

Create one 32-byte shared formation key and provision the same protected file
to every node. Do not commit the key or copy it into the inventory:

```sh
openssl rand -base64 32 > /secure/operator/peat-radio.key
chmod 600 /secure/operator/peat-radio.key
```

## Generate the bundle

From the AVIAN repository root:

```sh
cargo run -p arc-radio-plugin -- bootstrap \
  --inventory /secure/operator/arc-radio-nodes.json \
  --formation-id arc-radio \
  --formation-key-file /secure/operator/peat-radio.key \
  --output-dir /secure/operator/arc-radio-bootstrap
```

The output directory must be empty or absent. The command refuses to overwrite
an existing bundle. It creates:

- `bootstrap-summary.json`: identities, selected peers, edge count, and file
  routing for operator review;
- `membership.json`: a complete versioned manifest using the AVIAN mesh-agent
  membership schema (the mesh agent accepts operational formations of five to
  200 nodes; smaller benches use the generated sidecar host variables); and
- `host_vars/<node>.json`: ARC Ansible variables that enable the sidecar and
  set the radio URL, PEAT formation, bind address, and named peer descriptors.

The formation key is never written to any generated file. Endpoint IDs are
public transport identities, not secrets.

## Review and deploy

Review `bootstrap-summary.json` for the expected names, addresses, and peer
counts. Then copy the generated JSON host-variable files into the ARC checkout:

```sh
mkdir -p /path/to/arc-uas/infra/ansible/host_vars
cp /secure/operator/arc-radio-bootstrap/host_vars/*.json \
  /path/to/arc-uas/infra/ansible/host_vars/
```

Ansible accepts JSON as host-variable input. Keep deployment-specific address
files out of source control unless the program explicitly approves them for
the repository.

Before deploying, separately provision on every surrogate:

- `/etc/arc/streamcaster-credentials/radio.json` with mode 0600;
- `/etc/arc/keys/peat-radio.key` containing the same formation key;
- `/etc/arc/radio-evidence/regulatory.json`; and
- the `avian-arc-radio-plugin:latest` image for the surrogate architecture.

Use the normal ARC Ansible deployment after those prerequisites are present.
The generated host variables deliberately enable the sidecar, so ARC's deploy
role will stop if its protected prerequisite files are absent.

## Topology behavior

One to four nodes use a deterministic bench ring. A two-node bench has one
edge; three nodes form a triangle; four nodes form a four-edge ring. Formations
of five to 200 nodes use AVIAN's deterministic ring-and-chord planner with no
more than eight direct PEAT neighbors per node by default.

The generated graph is an AVIAN/PEAT application overlay. Silvus still owns RF
neighbor selection and multi-hop IP routing. A PEAT edge may traverse multiple
StreamCaster RF hops, so the ARC topology must not be interpreted as a vendor
RF-neighbor map.

The radio sidecar retries unavailable configured peers on a bounded interval.
It can therefore start before the other surrogates and reconnect after a radio
path or peer container returns without requiring a manual restart.
