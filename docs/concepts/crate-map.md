# Crate map

Every production workspace crate, what it is for, whether it is portable and the dependency direction it must respect.
"Portable" crates are `no_std`, never allocate and build and test on the host. For the dependency picture see
[Architecture](architecture.md).

| Crate | Purpose | Portable? | `std` / `no_std` | Major public abstractions | May depend on | Does **not** own |
|-------|---------|-----------|------------------|---------------------------|---------------|------------------|
| `rohi-hal` | Typed units and measurements, sensor drivers, sensor runtime, `MeasurementStore` | portable | `no_std` | `MeasurementStore`, `MeasurementsSnapshot`, `SensorDriver`, `Runner`, `EventBus`, `MeasurementHandle` | `embedded-hal(-async)`, Embassy sync | MCU peripherals, board pins, networking, serialization |
| `rohi-hal-esp` | ESP32 backend of the HAL: boards, I²C/UART/I²S setup, clock | ESP | `no_std` | `board::altruist_urban::{Hardware, AltruistUrban}` | `rohi-hal`, `esp-hal` | Sensor logic, application tasks |
| `rohi-config` | Transactional CBOR configuration store | portable | `no_std` | `ConfigSection`, `ConfigStore`, `ConfigStorage`, `MemoryStorage` | `minicbor` | Any concrete section, flash access |
| `rohi-config-derive` | `#[derive(ConfigSection)]` | portable (proc-macro) | `std` (build time) | derive macro | `syn`, `quote` | Runtime behaviour |
| `rohi-config-esp` | Flash backend of the store | ESP | `no_std` | `EspConfigStorage`, `FlashRegion`, `layout` | `rohi-config`, `esp-storage` | CBOR, sections, any feature's configuration |
| `rohi-identity` | Device identity: Ed25519 `Identity` config section, `Secret`, `Signer`, `IdentitySigner` | portable | `no_std` | `Identity`, `Secret`, `Signer`, `IdentitySigner`, `PublicKey`, `Signature` | `rohi-config`, `ed25519-zebra` | Provisioning transport, key derivation (done on the host by `rohi`), protocol semantics of signed messages |
| `rohi-net` | Wi-Fi control plane, IP data plane, BLE transport traits, `WifiConfig` section | portable | `no_std` | `WifiManager`, `WifiHandle`, `WifiBackend`, `NetworkHandle`, `IpBackend`, `NetError`, `BleTransport` | `rohi-config`, Embassy sync, `embedded-nal-async`, `embedded-io-async` | Radio drivers, the IP stack, control protocol |
| `rohi-net-esp` | ESP backend of `rohi-net` | ESP | `no_std` | `EspWireless`, `EspWifi`, `EspIp`, `EspBle`, `NetResources`, `BleResources`, `wireless_heap!` | `rohi-net`, `esp-radio`, `embassy-net`, TrouBLE | Product resource sizes, control protocol |
| `rohi-control-proto` | Bounded protobuf schema of the control protocol | portable | `no_std` | generated messages, `PROTOCOL_VERSION`, `limits` | `micropb` | Transport, behaviour |
| `rohi-control` | Control dispatcher, capabilities, authorization | portable | `no_std` | `Controller`, `ControlServices`, `Capabilities`, `AuthorizationPolicy` | `rohi-control-proto`, `rohi-hal`, `rohi-net`, `rohi-identity` | BLE, stores, the radio |
| `rohi-control-ble` | GATT binding: UUIDs, framing, fragmentation, control loop | portable | `no_std` | `ControlBle`, `frame` | `rohi-control`, `rohi-net` | The BLE stack |
| `altruist-urban` (firmware) | Product composition, tasks, resource budget | product | `no_std` | `config`, `net`, `control`, `resources`, `tasks` | everything above | Reusable library code |
| `rohi-examples` (`examples/`) | One hardware example per sensor and a config self-test | product | `no_std` | binaries | `rohi-hal(-esp)`, `rohi-config(-esp)` | |
| `rohi` (`tools/rohi`) | Desktop client library and CLI over BLE | desktop | `std` | `Device`, `Session`, `BleLink` | `rohi-control-ble::frame`, `rohi-control-proto`, `rohi-net` (types), `btleplug` | Anything that runs on the device |

## Allowed dependency direction

```text
altruist-urban ──► rohi-*-esp ──► rohi-* (portable) ──► embedded-* / embassy-sync
                                                      └─► never esp-*
rohi-control-ble ──► rohi-control ──► rohi-control-proto
rohi-control ──► rohi-net, rohi-hal, rohi-identity   (types only: WifiConfig, MeasurementsSnapshot, Identity)
rohi-net ──► rohi-config                   (owns the WifiConfig section)
rohi-identity ──► rohi-config              (owns the Identity section)
```

Forbidden: a portable crate depending on an `*-esp` crate; `rohi-net*` depending on `rohi-control*`; `rohi-hal`
depending on `rohi-net`; a platform crate depending on the firmware.

## Where does new code go?

| You are adding... | Put it in | Notes |
|-------------------|-----------|-------|
| A new sensor driver | `rohi-hal` (`sensors/`) | [Add a sensor](../guides/add-sensor.md); a new board only needs wiring in `rohi-hal-esp` |
| Pins or bus setup for a board | `rohi-hal-esp` (`board/`) | [Add a board](../guides/add-board.md) |
| A new persistent configuration section | The crate of the feature that interprets it | [Add a ConfigSection](../guides/add-config-section.md); never in `rohi-config` |
| Flash or storage backend | `rohi-config-esp` (or a new `*-esp` crate) | Implement `ConfigStorage` |
| DNS/TCP behaviour, TLS/HTTP/uplink protocols | A new portable crate over `NetworkHandle` / the `embedded-nal-async` traits | Never use `embassy-net` directly |
| Another IP stack or radio | A platform crate implementing `IpBackend` / `WifiBackend` | Vendor types stay inside |
| A control operation | `rohi-control-proto` (schema), `rohi-control` (dispatch, `ControlServices`), firmware adapter | [Add a control operation](../guides/add-control-operation.md) |
| Product resource sizes and task wiring | The firmware (`resources.rs`, `tasks/`) | The product owns the RAM budget |
| Anything that needs `esp-hal` / `esp-radio` | A `*-esp` platform crate | Keep the portable API free of vendor types |
