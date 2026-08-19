# Ground aircraft connection

AVIAN Ground supports a one-code operator workflow for adding an aircraft to a
pre-provisioned ground formation. The operator does not edit TOML, copy peer
IDs, use SSH, or restart services.

## Provision an aircraft code

The formation ID and shared formation key must already be provisioned on the
aircraft and authorized ground installation through the existing protected,
out-of-band process. The connection code does **not** contain that key.

On the aircraft, generate a code using every public address the ground node may
use. Put the preferred local path first and the satellite overlay last:

```sh
sudo avianctl connection-code \
  --address ethernet=192.168.2.2:9000 \
  --address wifi=192.168.2.89:9000 \
  --address satellite=10.210.122.229:9000
```

The command reads the running agent's stable public endpoint identity and
formation ID through its owner-only control socket, then prints one `AVIAN1.`
code. It never reads or emits the formation key. Deliver the code with the
aircraft through a trusted provisioning channel or print it as a QR/text label.

## Operator workflow

1. Power on the aircraft computer.
2. Put the ground device on the aircraft's local network or approved overlay.
3. Open AVIAN Ground and select **Manage aircraft**.
4. Paste the `AVIAN1.` code and select **Connect aircraft**.

The local ground agent validates the code's version, formation, aircraft name,
64-character endpoint identity, address count, underlay names, and unicast
socket addresses. It writes only the public peer descriptor to
`paired-peers.json` under the agent's existing private state directory, then
attempts the connection immediately. The write is atomic and mode `0600`.
Paired peers survive agent and ground-device restarts.
Pasting a corrected code for the same dynamically paired aircraft name safely
replaces the prior public identity and addresses. If the old aircraft is still
connected, AVIAN requires the replacement endpoint to connect successfully
before it commits the change or drops the working link.

Codes for another formation, malformed or secret-bearing codes, loopback or
multicast addresses, duplicate identities, name conflicts, managed-membership
nodes, non-ground nodes, and peers beyond the configured maximum are rejected.
The browser endpoint remains loopback-only and requires an exact same-origin
request plus an explicit setup header. Flight and emergency operations remain
outside the website.

## Remove a saved aircraft

1. Open **Manage aircraft** in AVIAN Ground.
2. Under **Saved aircraft**, select **Remove** for the aircraft.
3. Select **Confirm remove**.

Removal is a ground-local operation. AVIAN atomically removes the public peer
descriptor from `paired-peers.json`, stops outbound reconnect attempts, and
closes the currently tracked transport session. It does not change
configuration on the aircraft, revoke formation membership or credentials, or
erase records that already synchronized to the ground device. An authorized
aircraft or another formation peer can therefore establish an inbound or
relayed mesh path and continue synchronizing records. AVIAN Ground nevertheless
filters its aircraft overview to the agent's currently configured peers, so a
removed aircraft's peer row, cached or live telemetry, and payload summaries
disappear immediately and remain hidden after refresh or restart. Paste the
aircraft's connection code again to restore the ground-initiated direct pairing
and overview data.

Only aircraft added through a connection code are shown as removable. Static
TOML peers and signed managed-membership peers cannot be removed through the
website.

Removing a saved peer is not an aircraft revocation mechanism. Revocation
requires the approved formation membership or credential-rotation process on
every affected node.

## Boundary

This workflow adds or removes a ground-side direct peer. Aircraft connections
still need the same formation credential, a reachable AVIAN UDP listener, and
network policy that permits the advertised addresses. Large managed formations
continue to use signed membership manifests instead of local connection codes.
