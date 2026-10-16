# AVIAN transport upgrade

The current transport is implemented in `crates/mesh-transport`. It uses
authenticated Iroh QUIC connections and an AVIAN record protocol with
transactional redb storage. This is a coordinated formation upgrade: the
wire protocol, identity derivation, configuration names, and database format
are new.

## Prepare existing installations

1. Stop every affected mesh agent and radio sidecar. Retain protected backups
   of configuration, formation keys, stored mission data, paired peers, and
   command replay journals. Keep a matching binary/configuration backup for
   rollback.
2. Use the `[mesh]` TOML section and `max_mesh_peers` setting shown in
   `config/aircraft.toml.example` and `config/ground.toml.example`. Keep the
   existing command-state file and issuer keys: replay protection must survive
   the transport upgrade.
3. Select a new empty transport storage directory. An unknown populated
   directory is rejected rather than silently discarding its records. There
   is no automatic conversion of older transport databases. Restore required
   mission intent through its authorized producer before field use.
4. Keep the same provisioned formation ID/key and unique node names, but
   regenerate public endpoint descriptors, membership manifests, and aircraft
   connection codes with the new binaries. Old endpoint IDs do not identify
   the new listeners. Re-enroll local aircraft pairings instead of copying
   descriptors with obsolete endpoint IDs.
5. Update radio sidecars to `--mesh-formation-id`,
   `--mesh-formation-key-file`, `--mesh-bind`, `--mesh-storage`, and
   `--mesh-peer`. Bootstrap inventories now use `mesh_bind`; generated
   Ansible variables use `arc_streamcaster_mesh_*`. One-shot responses expose
   `mesh_record`. Mesh observations use schema version 2 and the
   `local/link/radio/streamcaster/mesh-observations/v2` topic, with
   `mesh_endpoint_id` and `mesh_connected_peers` fields. Update downstream consumers together; aliases for
   retired transport-specific names are intentionally absent.
6. Run `just verify`, then verify startup, status, peer identity, command dry
   runs, and partition/reconnect behavior on the target systems. Repeat the
   hardware acceptance runs before flight. Loopback tests do not establish
   RF performance or flight readiness.

The companion and ground control/status message shapes remain versioned.
The independent Ground UI repository must consume the current configuration
and enrollment outputs when provisioning agents.

## Replication behavior

Each class/record-ID pair stores one whole record. Conflicts converge by
publication timestamp, sequence, source, and canonical payload digest.
Telemetry producers reuse stable IDs; command producers use distinct IDs.
No transport merge bypasses application signature, expiry, or replay checks.
Record inventories and requested batches have explicit size limits, and
peers forward synchronized records across the bounded overlay.

The initial store limits are 1 MiB per encoded record, 64 MiB total, and
65,536 records. Inventory pages contain at most 64 versions. Emergency and
acknowledgement exchanges use a separate priority stream while normal
catch-up continues. These are implementation bounds, not measured RF latency
or capacity guarantees. There is no automatic durable-record eviction;
high-volume image metadata producers need an explicit retention plan before
long-running operation. Storage exhaustion returns an error instead of
silently deleting mission or command state.

## Rollback

Stop the complete upgraded formation before restoring its matching previous
binaries, configuration, endpoint descriptors, and database backups. Retain
current command replay journals so rollback cannot re-enable already consumed
commands. Do not mix wire-protocol generations in one formation.
