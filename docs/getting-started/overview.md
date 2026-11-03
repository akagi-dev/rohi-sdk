# What is the ROHI SDK?

This page is the technical overview of the SDK. For the product view, start with [Why ROHI?](../intro/why-rohi.md).

**ROHI** (Robonomics Open Hardware Interface) SDK is a set of Rust libraries, firmware and desktop tools for building
IoT devices powered by the [Robonomics](https://robonomics.network/) network. Its platform crates target the ESP32-C3 and
ESP32-C6 (RISC-V) and are built on Embassy (executor, time, sync primitives), `esp-rtos` (scheduler and time base for
the radio) and `esp-hal` / `esp-radio` (peripherals, Wi-Fi, BLE).

ROHI does not introduce another runtime or RTOS abstraction. It adds its own **SDK contracts** on top: typed sensor
measurements, a transactional configuration store, a Wi-Fi control plane, a portable IP data plane, a BLE control
protocol, and the rules that make them safe to combine.

## What is in the box

| Area | What you get | Read |
|------|--------------|------|
| Sensors | Typed measurements, drivers, runtime, `MeasurementStore` | [Sensor HAL](../sdk/hal.md), [Sensors](../sdk/sensors.md) |
| Configuration | CBOR, A/B-slot, power-fail-safe store of typed sections | [Configuration store](../sdk/config.md) |
| Networking | Wi-Fi control plane, DNS/TCP/UDP data plane, ESP backend | [Networking](../sdk/net.md) |
| Services | Config sections, live status and actions of every module | [Service pipeline](../sdk/service.md) |
| Control | Generic local BLE control protocol, `rohi` desktop client | [Control plane](../sdk/control.md), [Desktop application](../use/desktop/index.md) |
| Identity, location, time, telemetry | Device identity, location, wall-clock time, signed telemetry | [Identity](../sdk/identity.md), [Location](../sdk/location.md), [Time](../sdk/time.md) |
| Firmware | Altruist Urban, a complete product built from the above | [Boards](../platform/boards.md) |

## Design principles

- **Portable core, thin platform crates**: [Portability](../concepts/portability.md).
- **One owner per stateful service**: [Runtime ownership](../concepts/runtime-ownership.md).
- **Bounded and static**: [Memory model](../concepts/memory-model.md).
- **Cancellation is normal**: [Async and cancellation](../concepts/async-and-cancellation.md).
- **Persistent data is an ABI**: [Persistent ABI](../reference/persistent-abi.md).

## Reading path

1. [Installation](installation.md) and [Workspace](workspace.md).
2. [First device](first-device.md): run a sensor example on hardware, then [Build and test](build-and-test.md).
3. [Architecture](../concepts/architecture.md) and [Crate map](../concepts/crate-map.md): where things live.
4. The concepts, then the SDK component pages you need, then the [guides](../guides/add-sensor.md).
5. [Reference](../reference/resource-limits.md) and [Validation](../validation/host-testing.md) when you need exact
   numbers or want to prove a change.

## Reference

The technical documentation is the crate rustdoc: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk) has the overview, crate
map and cross-cutting contracts. Source:
[`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk). Locally:
`cargo doc --no-deps --open -p rohi-sdk`.
