# ADR 0001: AVIAN Linux, Rust, and PEAT companion baseline

- Status: accepted
- Date: 2026-08-04

## Decision

Use Linux ARM64 as the initial companion environment, Rust for the persistent
mesh agent and vehicle adapters, and PEAT/PEAT Mesh as the decentralized state
and transport foundation.

Payload workloads may use Python, C++, or accelerator-specific runtimes in a
separate process communicating through a versioned local interface.

## Rationale

PEAT is implemented in Rust, so direct integration avoids a language bridge.
Rust provides memory safety without a garbage collector, predictable resource
use, strong concurrency controls, cross-compilation, and a self-contained
deployment artifact. Process isolation prevents an experimental vision
pipeline from destabilizing telemetry or emergency control.

## Consequences

- The mesh service requires a Rust-capable Linux companion.
- Smaller aircraft that cannot carry Linux hardware will need a future
  PEAT-Lite gateway profile.
- Robotics and vision libraries do not need to be rewritten in Rust.
- Rust 1.91 or newer is required by the current PEAT dependency chain.
