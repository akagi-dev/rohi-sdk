# Portability

The SDK separates **portable** code from **platform** code so that most of it can be tested on a developer machine and
reused on another MCU.

| Category | Crates | Properties |
|----------|--------|------------|
| Portable | `rohi-hal`, `rohi-config`, `rohi-config-derive`, `rohi-net`, `rohi-control-proto`, `rohi-control`, `rohi-control-ble` | `no_std`, no allocation, no MCU HAL, builds and tests on the host |
| ESP-specific | `rohi-hal-esp`, `rohi-config-esp`, `rohi-net-esp` | `no_std`, depend on `esp-hal` / `esp-radio` / `esp-storage` / `embassy-net` / TrouBLE; empty without a chip feature, so they stay buildable on the host |
| Product | `altruist-urban`, `examples` | choose the chip, size the resources, spawn the tasks |
| Desktop | `tools/rohi` | `std`; talks to a device over BLE |

## How portability is achieved

- **Traits at the seams.** `SensorDriver` + `embedded-hal(-async)` for sensors; `ConfigStorage` for flash; `WifiBackend`,
  `IpBackend` and `BleTransport` for the radio and IP stack; `ControlBackend` and the `rohi-service` registries for the control plane. Portable code is
  generic over them and ships a fake or in-memory implementation for tests (`MemoryStorage`, `rohi_net::testing::FakeIp`,
  mocks in the test modules).
- **ROHI types in public APIs.** Vendor types and errors are converted into ROHI types inside the platform crate; a
  portable signature never mentions `esp-*` or `embassy-net`. `NetworkHandle` is generic over `IpBackend`, and higher
  layers should depend on the standard `embedded-nal-async` / `embedded-io-async` traits.
- **Time and clocks are injected.** Portable code takes a `Clock` / `DelayNs`; nothing reads a hardware timer or wall
  clock.
- **One chip feature at the edge.** The chip (`esp32c3` or `esp32c6`) is selected only by the firmware's features and
  forwarded to the platform crates; it never leaks into portable crates.

## What can and cannot be tested on the host

Portable crates are fully testable on the host, including timing (mock delays), reconnect logic (mock backends),
cancellation (dropped futures) and storage faults (`MemoryStorage` fault injection). Platform crates are only
**compile-checked** for both chips; their behaviour needs hardware, see [Host testing](../validation/host-testing.md) and
[Hardware testing](../validation/hardware-testing.md).

## Porting to another platform

Implement the seam traits in a new platform crate (for example `rohi-hal-<mcu>`, `rohi-net-<mcu>`), keep vendor types
inside, and add a firmware or board module that composes them. The portable crates should need no change; if one does,
the seam was leaking a platform detail and should be fixed there.
