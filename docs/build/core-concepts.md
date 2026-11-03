# Core concepts: Config, Status, Action

A ROHI firmware offers three things to the outside world. Everything a generic client can show or do with your device is
one of them.

| Concept | Responsibility | Typical user question |
|---------|----------------|-----------------------|
| **[Config](config.md)** | typed settings, defaults, validation and persistence | what is the device set up to do? |
| **[Status](status.md)** | runtime state, health, readiness, metrics and diagnostic issues | what is it doing now? |
| **[Action](action.md)** | discoverable operations with typed arguments and results | what can I ask it to do? |

They are separate on purpose: configuration is **desired, persistent** state, status is **observed, read-only** state and
an action is an **explicit operation**. An action is never smuggled into status, and a setting never doubles as a
status value.

## Composition in a firmware

Each feature crate (Wi-Fi, time, telemetry, sensors, identity, location, your own) describes its own settings, state and
operations. The firmware lists them once, and everything else is generated or generic: the control protocol serves them
over BLE to any client.

The reference composition is in [`firmwares/altruist-urban`](https://github.com/akagi-dev/rohi-sdk/tree/master/firmwares/altruist-urban)
(`config.rs`, `control.rs`).

## Schema-driven discovery

Nothing in the client is written for a particular module. The device publishes, on request:

- the **sections** it has and, per section, a schema of its fields (name, type, unit, allowed values, writable, secret,
  optional);
- the **services** it has, a schema of their status fields and the names of their issue codes;
- the **actions** it has with their arguments and results.

A client fetches a schema when it needs it and renders and validates values by it. Ids are stable and only ever added.

## Independent identities

Configuration does not depend on service identity: a section and a service are registered separately. A service can
exist without a section (a read-only sensor service), a section can exist without a service (a stored setting a runtime
reads), and one service can use several sections. By convention they share a name when they belong together
(`location`, `connectivity`). Renaming a persisted config type orphans its stored data unless its id is pinned: see
[Add a ConfigSection](../guides/add-config-section.md).

## Where the details are

This book teaches the concepts and the workflow. The contracts (traits, limits, ownership, cancellation) are in the crate
rustdoc, see [SDK crates](sdk-crates.md), starting with the [`rohi-service` rustdoc](https://docs.rs/rohi-service) and
the [`rohi-config` rustdoc](https://docs.rs/rohi-config). How the concepts reach a client is the
[control protocol](control-protocol.md).
