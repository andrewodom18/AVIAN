# ADR 0001: Linux, Rust, and the AVIAN transport baseline

- Status: accepted
- Date: 2026-08-04
- Updated: 2026-10-16

## Decision

Use Linux ARM64 as the initial companion environment, Rust for the persistent
mesh agent and vehicle adapters, and the AVIAN-owned `mesh-transport` crate
for authenticated Iroh QUIC networking and transactional record reconciliation.

Payload workloads may use Python, C++, or accelerator-specific runtimes in a
separate process communicating through a versioned local interface.

## Rationale

The networking and persistence libraries integrate directly with Rust.
Rust provides memory safety without a garbage collector, predictable resource
use, strong concurrency controls, cross-compilation, and a self-contained
deployment artifact. Process isolation prevents an experimental vision
pipeline from destabilizing telemetry or emergency control.

## Consequences

- The mesh service requires a Rust-capable Linux companion.
- Smaller aircraft that cannot carry Linux hardware will need a future
  lightweight gateway profile.
- Robotics and vision libraries do not need to be rewritten in Rust.
- The workspace pins Rust 1.91.1 for reproducible builds.
- Transport identity and database format upgrades require formation-wide
  reprovisioning; follow the transport upgrade guide before deployment.
