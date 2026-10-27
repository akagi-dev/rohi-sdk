# Crate map

Every production workspace crate, what it is for, whether it is portable and the dependency direction it must respect.
"Portable" crates are `no_std`, never allocate and build and test on the host. For the dependency picture see
[Architecture](architecture.md).

| Crate | Purpose | Portable? | `std` / `no_std` | Major public abstractions | May depend on | Does **not** own |
|-------|---------|-----------|------------------|---------------------------|---------------|------------------|
| `rohi-hal` | Typed units and measurements, sensor drivers, sensor runtime, `MeasurementStore` | portable | `no_std` | `MeasurementStore`, `MeasurementsSnapshot`, `SensorDriver`, `Runner`, `EventBus`, `MeasurementHandle` | `embedded-hal(-async)`, Embassy sync, `rohi-service` | MCU peripherals, board pins, networking, serialization |
| `rohi-hal-esp` | ESP32 backend of the HAL: boards, I²C/UART/I²S setup, clock | ESP | `no_std` | `board::altruist_urban::{Hardware, AltruistUrban}` | `rohi-hal`, `esp-hal` | Sensor logic, application tasks |
| `rohi-config` | Transactional CBOR configuration store | portable | `no_std` | `ConfigSection`, `ConfigStore`, `ConfigStorage`, `MemoryStorage` | `minicbor` | Any concrete section, flash access |
| `rohi-config-derive` | `#[derive(ConfigSection)]` | portable (proc-macro) | `std` (build time) | derive macro | `syn`, `quote` | Runtime behaviour |
| `rohi-service` | The generic service pipeline: `Value`, `ManagedConfig` sections with atomic patch, `ManagedService` status with state/health/issues/revisions, actions, tuple registries | portable | `no_std` | `ManagedConfig`, `ManagedSection`, `ConfigRegistry`, `ManagedService`, `Tracked`, `ServiceRegistry`, `ActionDescriptor`, `Value` | `heapless`, Embassy sync | Any concrete section or service, the protocol |
| `rohi-config-esp` | Flash backend of the store | ESP | `no_std` | `EspConfigStorage`, `FlashRegion`, `layout` | `rohi-config`, `esp-storage` | CBOR, sections, any feature's configuration |
| `rohi-identity` | Device identity: Ed25519 `Identity` config section, `Secret`, `Signer`, `IdentitySigner` | portable | `no_std` | `Identity`, `Secret`, `Signer`, `IdentitySigner`, `PublicKey`, `Signature` | `rohi-config`, `rohi-service`, `ed25519-zebra` | Provisioning transport, key derivation (done on the host by `rohi`), protocol semantics of signed messages |
| `rohi-net` | Wi-Fi control plane, IP data plane (DNS, TCP, UDP), BLE transport traits, `WifiConfig`, `NetworkConfig` and `BleSettings` sections and the `wifi`, `network` and `ble` services | portable | `no_std` | `WifiManager`, `WifiHandle`, `WifiBackend`, `NetworkHandle`, `IpBackend`, `UdpSocket`, `NetError`, `BleTransport` | `rohi-config`, `rohi-service`, Embassy sync, `embedded-nal-async`, `embedded-io-async` | Radio drivers, the IP stack, control protocol |
| `rohi-time` | Synchronized wall-clock time: `TimeService`, `TimeSource`, validation, background `Synchronizer` | portable | `no_std` | `TimeService`, `TimeSource`, `TimeSample`, `UnixTime`, `TimeStatus`, `Synchronizer` | `rohi-hal` (monotonic `Clock`, `Backoff`), `rohi-service`, Embassy sync | Network or clock hardware, any concrete source |
| `rohi-time-ntp` | NTP/SNTP `TimeSource` over the IP data plane | portable | `no_std` | `NtpTimeSource`, `NtpConfig`, `NtpServersConfig` (the `time` config section) | `rohi-time`, `rohi-net`, `rohi-config`, `rohi-service`, `sntpc` | The global concept of time, the UDP stack |
| `rohi-location` | The location of the device (user-configured now, GNSS later): `LocationConfig` section, `LocationShared`, `LocationService` | portable | `no_std` | `Location`, `LocationConfig`, `LocationShared`, `LocationSource`, `LocationService` | `rohi-config`, `rohi-service` | GNSS hardware, telemetry |
| `rohi-connectivity-proto` | Bounded bindings of `airalab/connectivity-protocol` (vendored, pinned revision) | portable | `no_std` | `core_v1`, `crypto_v1`, `device_v1`, `sensor_v1`, `limits`, `signing_preimage` | `micropb` | Signing, mapping, transports |
| `rohi-connectivity` | Telemetry: snapshot (+ location) → signed envelope frame, bounded outbox, retry, HTTP transport, `ConnectivityConfig` section, `connectivity` service | portable | `no_std` | `UrbanEncoder`, `EnvelopeBuilder`, `EnvelopeFrame`, `EnvelopeTransport`, `HttpTransport`, `ConnectivityService`, `UtcClock`, `NonceSource`, `FrameSigner` | `rohi-connectivity-proto`, `rohi-hal`, `rohi-identity`, `rohi-location`, `rohi-net`, `rohi-service`, `rohi-time` | Private keys, Wi-Fi lifecycle, sensors, backend logic |
| `rohi-net-esp` | ESP backend of `rohi-net` | ESP | `no_std` | `EspWireless`, `EspWifi`, `EspIp`, `EspBle`, `NetResources`, `BleResources`, `wireless_heap!` | `rohi-net`, `esp-radio`, `embassy-net`, TrouBLE | Product resource sizes, control protocol |
| `rohi-control-proto` | Bounded protobuf schema of the control protocol | portable | `no_std` | generated messages, `PROTOCOL_VERSION`, `limits` | `micropb` | Transport, behaviour |
| `rohi-control` | Transport-agnostic control dispatcher and authorization; serves the registries of `rohi-service` | portable | `no_std` | `Controller`, `ControlBackend`, `Device`, `AuthorizationPolicy` | `rohi-control-proto`, `rohi-service` | BLE, stores, the radio, any knowledge of a module (Wi-Fi, sensors, ...) |
| `rohi-control-ble` | GATT binding: UUIDs, framing, fragmentation, control loop | portable | `no_std` | `ControlBle`, `frame` | `rohi-control`, `rohi-net` | The BLE stack |
| `altruist-urban` (firmware) | Product composition, tasks, resource budget | product | `no_std` | `config`, `net`, `control`, `time`, `connectivity`, `resources`, `tasks` | everything above | Reusable library code |
| `rohi-examples` (`examples/`) | One hardware example per sensor and a config self-test | product | `no_std` | binaries | `rohi-hal(-esp)`, `rohi-config(-esp)` | |
| `rohi` (`tools/rohi`) | Desktop client library and CLI over BLE | desktop | `std` | `Device`, `Session`, `BleLink` | `rohi-control-ble::frame`, `rohi-control-proto`, `rohi-net` (BLE payload size), `btleplug` | Anything that runs on the device |

## Allowed dependency direction

```text
altruist-urban ──► rohi-*-esp ──► rohi-* (portable) ──► embedded-* / embassy-sync
                                                      └─► never esp-*
rohi-control-ble ──► rohi-control ──► rohi-control-proto
rohi-control ──► rohi-control-proto, rohi-service   (nothing else: it knows no module)
rohi-service ──► (nothing of the SDK)       (generic model: Value, ManagedConfig, ManagedService, registries)
rohi-net, rohi-connectivity, rohi-time, rohi-hal, rohi-identity, rohi-location ──► rohi-service   (each implements the traits for its own types)
rohi-location ──► rohi-config, rohi-service   (owns the LocationConfig section)
rohi-net ──► rohi-config                   (owns the WifiConfig, NetworkConfig, BleSettings sections)
rohi-identity ──► rohi-config              (owns the Identity section)
rohi-time-ntp ──► rohi-time, rohi-net, rohi-config, rohi-service   (a source: DNS + UDP samples, owns the `time` section; rohi-time never depends on it)
rohi-connectivity ──► rohi-connectivity-proto, rohi-hal, rohi-identity, rohi-location, rohi-net, rohi-time   (snapshots, location, signing, NetworkHandle, UtcClock)
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
| An absolute-time source (RTC, external time) | A new portable crate implementing `rohi_time::TimeSource` | Consumers keep using `TimeService`; see [Time service](../sdk/time.md) |
| DNS/TCP/UDP behaviour, TLS/HTTP/uplink protocols | A new portable crate over `NetworkHandle` / the `embedded-nal-async` traits | Never use `embassy-net` directly |
| Another IP stack or radio | A platform crate implementing `IpBackend` / `WifiBackend` | Vendor types stay inside |
| A service (status, config, actions) of a runtime | The crate that owns the runtime; register it in the firmware tuples | [Add a service](../guides/add-service.md); no protocol or CLI change |
| A control operation that is none of config, status or action | `rohi-control-proto` (schema), `rohi-control` (dispatch, `ControlBackend`) | [Add a control operation or an action](../guides/add-control-operation.md) |
| Product resource sizes and task wiring | The firmware (`resources.rs`, `tasks/`) | The product owns the RAM budget |
| Anything that needs `esp-hal` / `esp-radio` | A `*-esp` platform crate | Keep the portable API free of vendor types |
