# AVIAN message contract

This document defines the application-level contract carried by PEAT. The Rust
types in `mesh-core` are authoritative for v0.1.

## Node advertisement

A node advertises a stable identifier, its flight stack, its explicit
capabilities, and its maximum MSL altitude. Consumers must check capabilities
before assigning work. Flight-stack names alone are not authorization.

## Telemetry

The minimal common telemetry record contains:

- position, velocity, attitude, and timestamp;
- MSL, AGL, and above-launch altitude;
- battery state, armed/landed/failsafe state; and
- local control-link quality.

Telemetry is latest-value data. Old telemetry must not be replayed after a
partition heals.

## Emergency command

An emergency command contains:

- globally unique command ID;
- issuer and target node IDs;
- issuance and expiration timestamps;
- issuer-scoped monotonic nonce;
- requested action; and
- Ed25519 signature over a deterministic binary representation.

Receivers verify the trusted issuer key, target, lifetime, signature, and
nonce before execution. A command ID or issuer/nonce pair can be accepted only
once. An acknowledgement is a separate durable record.

Betaflight v0.1 actions are limited to GPS Rescue, return-to-launch mapped to
GPS Rescue, and disarm after the adapter reports a landed state. Raw stick
control is not a mesh command.

## Delivery classes

| Class | Durable | Reliable | Redundancy | Lifetime |
| --- | --- | --- | --- | --- |
| Emergency | Yes, for audit and acknowledgement | Yes | Two paths when available | 5 seconds |
| Acknowledgement | Yes | Yes | One path | Until superseded |
| Mission | Yes | Yes | One path | Until superseded |
| Telemetry | No; latest value only | No | One path | 2 seconds |
| Bulk | Yes and resumable | Yes | One path | Until transferred |
