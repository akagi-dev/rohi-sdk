# Firmware distribution

> **Not implemented.** ROHI has no firmware registry, update service or signing infrastructure. This page says what
> exists today and what a firmware author can do.

## Today

- The source of the reference firmware is this repository. Build it for your chip and flash it with `cargo run`, see
  [Quick start](../build/quick-start.md). No pre-built images are published.
- A custom firmware needs no approval to be operated with `rohi`: it only has to implement the
  [control protocol](../build/control-protocol.md) ([Compatibility](compatibility.md)).
- To share a firmware, publish its source and say which board and chip it targets and which ROHI revision it was built
  against. The control protocol is still changing, so the revision matters.
- To make hardware claims visible, record validation as described in [Validation](validation.md) and, for boards and
  capabilities of this repository, update the [support matrix](../support/overview.md).

## Planned

Distribution of verified and community firmware, with provenance and update mechanisms, is a goal of the ecosystem
and is outside this SDK for now. When it exists it will keep the three questions of
[Compatibility](compatibility.md) separate.
