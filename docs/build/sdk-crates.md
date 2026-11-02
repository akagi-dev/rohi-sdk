# SDK crates

The SDK is a set of Rust crates. **The crate rustdoc is the reference** for APIs, contracts and integration points; this
book teaches the concepts and the workflows. Build the API documentation of the whole workspace with:

```bash
cargo doc --no-deps --open
```

Released crates are also published on [docs.rs](https://docs.rs/rohi-hal).

| Crate | What it is | Concept page |
|-------|-----------|--------------|
| `rohi-hal` | typed measurements, sensor drivers and runtime | [Sensor HAL](../sdk/hal.md), [Sensors](../sdk/sensors.md) |
| `rohi-config`, `rohi-config-esp` | transactional configuration store and its flash backend | [Configuration store](../sdk/config.md) |
| `rohi-service` | the config, status and action model and registries | [Service pipeline](../sdk/service.md) |
| `rohi-net`, `rohi-net-esp` | Wi-Fi and IP networking, BLE transport traits, ESP backend | [Networking](../sdk/net.md), [Wireless runtime](../platform/wireless.md) |
| `rohi-control-proto`, `rohi-control`, `rohi-control-ble` | the control protocol, its dispatcher and its BLE binding | [Control protocol](control-protocol.md), [Control plane](../sdk/control.md) |
| `rohi-connectivity`, `rohi-connectivity-proto` | signed telemetry | [Telemetry](../sdk/connectivity.md) |
| `rohi-time`, `rohi-time-ntp` | wall-clock time | [Time service](../sdk/time.md) |
| `rohi-identity` | device identity and signer | [Device identity](../sdk/identity.md) |
| `rohi-location` | device location | [Location](../sdk/location.md) |
| `rohi` (`tools/rohi`) | desktop client library and CLI | [Desktop application](../use/desktop/index.md) |

The full list with dependency rules is the [Crate map](../concepts/crate-map.md).

## Technical reference in this book

Until the detailed contracts live in the crate rustdoc, the following pages remain the technical reference. They describe
the current implementation and should not be copied elsewhere:

- [What is the ROHI SDK?](../getting-started/overview.md) and the concepts:
  [Architecture](../concepts/architecture.md), [Runtime ownership](../concepts/runtime-ownership.md),
  [Async and cancellation](../concepts/async-and-cancellation.md), [Portability](../concepts/portability.md),
  [Memory model](../concepts/memory-model.md), [Versioning and ABI](../concepts/versioning-and-abi.md).
- The component pages listed above.
- [Platform](../platform/esp32.md) and [Reference](../reference/resource-limits.md) pages.
