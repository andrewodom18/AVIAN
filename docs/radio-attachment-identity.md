# Drone-to-radio attachment identity

AVIAN can publish a vendor-neutral assertion describing the radio explicitly provisioned on the same aircraft as `mesh-agent`. This lets ARC compare three identities without collapsing them:

- ARC Fleet `drone_id`
- AVIAN `node_id`
- CHUD radio MAC and optional vendor node ID

An IP address is reachability data only and is never accepted as radio identity. AVIAN does not discover or configure radios in this workflow; CHUD remains authoritative for those operations.

## Aircraft configuration

Add the optional block below to the aircraft's existing `[radio]` configuration:

```toml
[radio.attachment]
drone_id = "aircraft-001"
mac_address = "00:1e:3f:20:9a:10"
radio_node_id = "1"
radio_serial_number = "TW950-123"
```

`mesh-agent` validates the MAC strictly and refuses attachment assertions on non-aircraft roles. At startup it publishes a durable `RadioAttachmentAssertion` record under `radio-attachment/<avian-node-id>` through the authenticated PEAT formation.

The assertion is evidence of the locally configured tuple, not proof of RF topology, radio health, or command authorization. ARC must compare MAC, vendor node ID, and serial number when present with the operator assignment and a fresh CHUD-authoritative inventory record before marking an association verified. A shared, expired, diagnostic, simulated, or unauthenticated transport must not be treated as equivalent evidence.

## Current integration status

- Contract and aircraft publisher: implemented and locally testable.
- ARC operator assignment, Fleet projection, and exact-match/conflict reconciliation: implemented on the paired ARC feature branch.
- Automatic ARC session transport consumer: pending; ARC remains fail-closed as `operator_assigned` until an authenticated Fleet/PEAT path invokes the reconciliation logic.
- Physical attachment and RF topology validation: pending hardware testing through CHUD.
