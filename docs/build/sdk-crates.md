# SDK crates

**The crate rustdoc is the reference** for each SDK crate: its responsibilities, public interface, contracts and
integration points. This book teaches the concepts and the workflows. Build the API documentation of the whole workspace
with:

```bash
cargo doc --no-deps --open
```

Released crates are also published on docs.rs, for example [`rohi-sdk`](https://docs.rs/rohi-sdk) (the entry point and cross-cutting concepts) and [`rohi-hal`](https://docs.rs/rohi-hal). The source is in [`crates/`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates).

| Crate | What it is |
|-------|-----------|
| `rohi-sensors` | typed measurements, sensor state, the store, the event bus and the `sensors` service |
| `rohi-hal`, `rohi-hal-esp` | physical sensor drivers and their runner; ESP boards |
| `rohi-config`, `rohi-config-derive`, `rohi-config-esp` | transactional configuration store, derive macros and flash backend |
| `rohi-service`, `rohi-service-derive` | the config, status and action model and registries |
| `rohi-net`, `rohi-net-esp`, `rohi-net-std` | Wi-Fi and IP networking, BLE transport traits, ESP backend, host (`std`) backend |
| `rohi-control-proto`, `rohi-control`, `rohi-control-ble` | the control protocol, its dispatcher and its firmware-side BLE binding (the `rohi` tool reuses only its frame and uuid modules) |
| `rohi-control-tcp` | the control protocol over TCP: independent length-prefixed records and the serving loop |
| `rohi-sim` | the simulated world of a `std` application: sensors, Wi-Fi, IP faults, flash file, scenarios |
| `rohi-connectivity`, `rohi-telemetry-robonomics` | signed telemetry |
| `rohi-time`, `rohi-time-ntp` | wall-clock time |
| `rohi-identity` | device identity and signer |
| `rohi-location` | device location |
| `rohi-logging` | persistent logging: the `log` sink, segment format, storage backends and the `logging` service |
| `rohi-system` | the `system` service: reboot and factory reset Actions over a platform adapter |
| `rohi` (`tools/rohi`) | desktop client library and CLI: [Desktop application](../use/desktop/index.md) |
| `rohi-sdk` | umbrella crate: re-exports the crates above behind Cargo features (`service`, `hal`, `net`, `esp`, ...); the firmware depends on it alone |

The dependency rules and "where does new code go" are in the [`rohi-sdk` rustdoc](https://docs.rs/rohi-sdk).
