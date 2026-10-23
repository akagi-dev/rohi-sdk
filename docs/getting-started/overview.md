# What is the ROHI SDK?

**ROHI** (Robonomics Open Hardware Initiative) SDK is a set of Rust libraries, firmware and desktop tools for building
IoT devices powered by the [Robonomics](https://robonomics.network/) network. It targets the ESP32-C3 and ESP32-C6
(RISC-V) and is built on:

```text
Embassy            async executor, time, sync primitives
   ↓
esp-rtos           scheduler / time base required by the radio
   ↓
esp-hal / esp-radio  peripherals, Wi-Fi, BLE
```

ROHI does not introduce another runtime or RTOS abstraction. It adds its own **SDK contracts** on top: typed sensor
measurements, a transactional configuration store, a Wi-Fi control plane, a portable IP data plane, a BLE control
protocol, and the rules that make them safe to combine.

## What is in the box

| Area | What you get | Read |
|------|--------------|------|
| Sensors | Typed measurements, drivers (BMx280, SCD4x, SDS011, noise), runtime, `MeasurementStore` | [Sensor HAL](../sdk/hal.md), [Sensors](../sdk/sensors.md) |
| Configuration | CBOR, A/B-slot, power-fail-safe store of independent typed sections | [Configuration store](../sdk/config.md) |
| Networking | Wi-Fi control plane, DNS/TCP data plane, ESP backend | [Networking](../sdk/net.md) |
| Control | Local BLE control protocol, `rohi` desktop client | [Control plane](../sdk/control.md) |
| Identity | Provisioned Ed25519 device identity and signer | [Device identity](../sdk/identity.md) |
| Firmware | Altruist Urban, a complete product built from the above | [Boards](../platform/boards.md) |

## Design principles

- **Portable core, thin platform crates.** Everything except `*-esp` crates, the firmware and examples builds and tests
  on the host. See [Portability](../concepts/portability.md).
- **One owner per stateful service.** Other tasks use bounded handles. See
  [Runtime ownership](../concepts/runtime-ownership.md).
- **Bounded and static.** Application code does not allocate; every queue and buffer has a documented limit chosen by
  the product. See [Memory model](../concepts/memory-model.md).
- **Cancellation is normal.** Public async APIs document their cancellation semantics. See
  [Async and cancellation](../concepts/async-and-cancellation.md).
- **Persistent data is an ABI.** See [Persistent ABI](../reference/persistent-abi.md).

## Reading path

1. [Installation](installation.md), [Workspace](workspace.md), [Build and test](build-and-test.md).
2. [First device](first-device.md): run a sensor example on hardware.
3. [Architecture](../concepts/architecture.md) and [Crate map](../concepts/crate-map.md): where things live.
4. The concepts, then the SDK component pages you need, then the [guides](../guides/add-sensor.md).
5. [Reference](../reference/resource-limits.md) and [Validation](../validation/host-testing.md) when you need exact
   numbers or want to prove a change.

Exact Rust APIs are in the generated API documentation (`cargo doc --no-deps`, published on docs.rs for released
crates); this book explains architecture, contracts and workflows.
