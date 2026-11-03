# Current capabilities

What exists today. Hardware and verification details are authoritative in the [support matrix](../support/matrix.md);
this page is an overview.

| Area | State |
|------|-------|
| Boards | Altruist Urban on ESP32-C3 and ESP32-C6 with the ROHI firmware. The C6 pin map comes from the upstream schematic and is **not verified on hardware**. Altruist Insight is **not** supported by ROHI. |
| Sensors | BMP280/BME280, SCD40/SCD41, SDS011, I²S microphone noise level (unweighted dB SPL, not dBA). See [Sensors](https://docs.rs/rohi-hal/latest/rohi_hal/sensors/index.html). |
| Configuration | typed sections in a power-fail-safe store; only the user's overrides are persisted: [Configuration store](https://docs.rs/rohi-config) |
| Networking | Wi-Fi station, DNS and TCP: [Networking](https://docs.rs/rohi-net) |
| Services | config, status and actions of every module, discovered by name: [Service pipeline](https://docs.rs/rohi-service) |
| Control | local BLE control plane and the `rohi` desktop client, without pairing: [Control plane](https://docs.rs/rohi-control) |
| Telemetry, time, identity, location | [Telemetry](https://docs.rs/rohi-connectivity), [Time](https://docs.rs/rohi-time), [Identity](https://docs.rs/rohi-identity), [Location](https://docs.rs/rohi-location) |
| Logging | persistent, rotated, compressed device log in internal flash, read with `rohi logs`; SD backend not implemented: [Device log](../use/desktop/logs.md), [Logging](https://docs.rs/rohi-logging) |
| Hardware validation | host tests and CI builds for both chips; hardware results are mostly **not yet recorded**: [Hardware testing](../validation/hardware-testing.md) |

## Not available

- A mobile application, other control transports and firmware distribution infrastructure: planned, see
  [One App. Any Firmware.](one-app-any-firmware.md#implemented-and-planned).
- Support for other boards. Adding one is described in [Add or support a board](../guides/add-board.md).

How to read the status words used here (implemented, integrated, built, verified): [Reading the support matrix](../support/overview.md).
