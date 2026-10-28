# Glossary

| Term | Meaning |
|------|---------|
| **ABI (persistent)** | The stored byte format other firmware versions rely on: container, section IDs, CBOR payloads. See [Persistent ABI](persistent-abi.md) |
| **Backend** | A platform implementation of a portable trait: `WifiBackend`, `IpBackend`, `BleTransport`, `ConfigStorage` |
| **BleTransport** | Portable BLE peripheral API: one connected central as a bounded byte pipe |
| **Action** | An explicit operation a service declares (scan, reconnect, provision) with typed arguments and result records; listed and run through the control plane. Status never changes anything. See [Service pipeline](../sdk/service.md#actions) |
| **Config section** | A named group of typed, described fields of persistent desired state (`wifi`, `connectivity`, `location`), patched atomically; owned by a feature crate via `ManagedConfig`. What is persisted are its *overrides* |
| **Command** | A transient, bounded request to an owner task (scan, reconnect). Contrast *desired state* |
| **ConfigHandle / ConfigStore / ConfigSection** | Handle to the configuration owner task / the transactional store / one persisted section (the overrides struct for a user-configurable section) owned by a feature crate |
| **Control plane (BLE)** | The local protobuf protocol served over BLE by `rohi-control` + `rohi-control-ble`: info, config, status and actions, driven by schemas the device publishes |
| **Control plane (Wi-Fi)** | `WifiHandle` / `WifiManager`: configuration, scan, status, reconnect, disconnect. Not DNS or TCP |
| **Desired state** | The latest value a runtime should converge to (for example the desired `WifiConfig`); published, not queued. See [Runtime ownership](../concepts/runtime-ownership.md#state-versus-command) |
| **Effective configuration** | What a runtime uses and `rohi config SECTION` shows: `C::default()`, the firmware overrides compiled into the product and the user's overrides, in that order (`materialize`). A changed firmware override or default reaches every field that is not overridden. See [Service pipeline](../sdk/service.md#config-sections) |
| **EspWireless** | The single owner of the ESP radio peripherals and the wireless heap; creates the Wi-Fi and BLE drivers |
| **Golden test** | A test pinning exact encoded bytes or IDs so accidental ABI changes fail |
| **Device health** | The overall `ok` / `degraded` / `error` of the device, decided by the firmware from its services, never by a client |
| **Handle** | A small copyable value giving other tasks bounded access to a service owned by one task |
| **Identity** | The device's single Ed25519 key, provisioned from the host (never generated on the device) and stored as the `Identity` config section. See [Device identity](../sdk/identity.md) |
| **IP data plane** | `NetworkHandle`: DNS and TCP through `embedded-nal-async` / `embedded-io-async`, and bounded UDP sockets |
| **Issue** | A currently active problem of a service with a stable machine-readable code, a severity and an age; history is not an issue |
| **Location** | Where the device is (WGS84): set by the user now, GNSS later; sent in telemetry. See [Location](../sdk/location.md) |
| **MeasurementStore / MeasurementHandle / snapshot** | Owner-side store of typed observations / handle for on-demand requests / immutable `MeasurementsSnapshot` generated at request time |
| **Owner** | The one task (or runtime object) that holds the mutable state of a service |
| **Override** | An explicit user value of one config field (`Override::Value`), as opposed to `Override::Default` (the field follows the firmware default). Only overrides are persisted, as a CBOR map keyed by field id; `FIELD=null` removes one, `FIELD=none` sets a nullable field to an explicit no value (`Value(None)`). See [Persistent ABI](persistent-abi.md#section-payload) |
| **Product** | A firmware (Altruist Urban) that composes the SDK and owns its resource budget |
| **Reconciliation** | The Wi-Fi manager continuously moving the actual radio state towards the latest desired configuration |
| **Section ID** | FNV-1a 64-bit hash of the Rust struct name identifying a `ConfigSection` (for an overrides struct: the name of the section type) |
| **SensorDriver / Runner** | Driver trait for one sensor / the generic task loop handling probing, polling, backoff and recovery |
| **Service (status)** | A runtime part exposed through `ManagedService`: state, health, readiness, active issues, values and actions, by name; see [Services and status](../getting-started/services.md) |
| **Status revision** | A counter that changes when a service's state, health, readiness or issues change; lets a poller fetch details only when needed |
| **Slot (A/B)** | One of the two flash slots of the configuration region; updates go to the inactive one and commit last |
| **Time anchor** | The accepted absolute time together with the monotonic instant it was accepted at; wall-clock time is `anchor + monotonic elapsed`. See [Time service](../sdk/time.md) |
| **TimeService / TimeSource** | The single source of optional wall-clock time / an abstract producer of absolute-time samples (NTP, future RTC) |
| **State / health / ready** | The three independent parts of a service status: lifecycle, operational quality, and whether it can do its job now |
| **Wall-clock time** | `UnixTime`: absolute time, available only after a valid sample and allowed to step; never used for durations (those use monotonic time) |
| **Wireless heap** | The fixed heap shared by the Wi-Fi and BLE controllers; the only heap in the firmware |
