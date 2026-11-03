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
| `rohi-hal`, `rohi-hal-esp` | typed measurements, sensor drivers and runtime; ESP boards |
| `rohi-config`, `rohi-config-derive`, `rohi-config-esp` | transactional configuration store, derive macros and flash backend |
| `rohi-service`, `rohi-service-derive` | the config, status and action model and registries |
| `rohi-net`, `rohi-net-esp` | Wi-Fi and IP networking, BLE transport traits, ESP backend |
| `rohi-control-proto`, `rohi-control`, `rohi-control-ble` | the control protocol, its dispatcher and its firmware-side BLE binding (the `rohi` tool reuses only its frame and uuid modules) |
| `rohi-connectivity`, `rohi-connectivity-proto` | signed telemetry |
| `rohi-time`, `rohi-time-ntp` | wall-clock time |
| `rohi-identity` | device identity and signer |
| `rohi-location` | device location |
| `rohi` (`tools/rohi`) | desktop client library and CLI: [Desktop application](../use/desktop/index.md) |
| `rohi-sdk` | umbrella crate: re-exports the crates above behind Cargo features (`service`, `hal`, `net`, `esp`, ...); the firmware depends on it alone |

The dependency rules and "where does new code go" are in the [`rohi-sdk` rustdoc](https://docs.rs/rohi-sdk).
