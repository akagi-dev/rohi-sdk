# Crate map

Every production workspace crate, what it is for and where it lives. "Portable" crates are `no_std`, never allocate and
build and test on the host; the `*-esp` crates need the chip for their behaviour. All crates listed here are
implemented. For the layering see [Architecture](architecture.md).

| Crate | Role | Where |
|-------|------|-------|
| [`rohi-hal`](https://docs.rs/rohi-hal) | typed measurements, sensor drivers and runtime, `MeasurementStore` | `crates/hal` |
| [`rohi-hal-esp`](https://docs.rs/rohi-hal-esp) | ESP32 backend of the HAL: boards, buses, clock | `crates/hal-esp` |
| [`rohi-config`](https://docs.rs/rohi-config) | transactional CBOR configuration store | `crates/config` |
| [`rohi-config-derive`](https://docs.rs/rohi-config-derive) | derive macros for configuration sections | `crates/config-derive` |
| [`rohi-config-esp`](https://docs.rs/rohi-config-esp) | flash backend of the store | `crates/config-esp` |
| [`rohi-service`](https://docs.rs/rohi-service) | generic service pipeline: config sections, status, actions, registries | `crates/service` |
| [`rohi-service-derive`](https://docs.rs/rohi-service-derive) | `#[derive(Service)]` | `crates/service-derive` |
| [`rohi-identity`](https://docs.rs/rohi-identity) | Ed25519 device identity and signer | `crates/identity` |
| [`rohi-net`](https://docs.rs/rohi-net) | Wi-Fi control plane, IP data plane, BLE transport traits | `crates/net` |
| [`rohi-net-esp`](https://docs.rs/rohi-net-esp) | ESP backend of `rohi-net` | `crates/net-esp` |
| [`rohi-time`](https://docs.rs/rohi-time) | synchronized wall-clock time | `crates/time` |
| [`rohi-time-ntp`](https://docs.rs/rohi-time-ntp) | NTP/SNTP time source | `crates/time-ntp` |
| [`rohi-location`](https://docs.rs/rohi-location) | device location (user-configured; no GNSS runtime yet) | `crates/location` |
| [`rohi-connectivity-proto`](https://docs.rs/rohi-connectivity-proto) | bounded bindings of the connectivity protocol | `crates/connectivity-proto` |
| [`rohi-connectivity`](https://docs.rs/rohi-connectivity) | telemetry: signed frames, outbox, HTTP transport | `crates/connectivity` |
| [`rohi-control-proto`](https://docs.rs/rohi-control-proto) | bounded protobuf schema of the control protocol | `crates/control-proto` |
| [`rohi-control`](https://docs.rs/rohi-control) | transport-agnostic control dispatcher | `crates/control` |
| [`rohi-control-ble`](https://docs.rs/rohi-control-ble) | firmware-side BLE peripheral binding (GATT, framing); the host tool reuses only its `frame` and `uuid` modules | `crates/control-ble` |

Outside these 18 crates: the product firmware `altruist-urban` (`firmwares/altruist-urban`), the hardware `examples`,
the host tool `rohi` (`tools/rohi`) and the documentation-only [`rohi-sdk`](https://docs.rs/rohi-sdk) crate
(`crates/sdk`).

## Dependency rule

Dependencies point `altruist-urban → rohi-*-esp → rohi-* (portable) → embedded-* / embassy-sync`; portable crates never
depend on `esp-*`. `rohi-net*` never depends on `rohi-control*`, `rohi-hal` never depends on `rohi-net`, and a platform
crate never depends on the firmware. `rohi-control` knows no module: it serves the registries of `rohi-service`. The
exact per-crate edges are in the rustdoc.

## Where does new code go?

The table (new sensor, board, configuration section, storage backend, time source, network protocol, service, control
operation) is in the rustdoc; the step-by-step [guides](../guides/add-sensor.md) cover each case.

## Reference

- [`rohi_sdk` crate map](https://docs.rs/rohi-sdk/latest/rohi_sdk/index.html#crate-map) and
  [`architecture`](https://docs.rs/rohi-sdk/latest/rohi_sdk/architecture/index.html).
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
