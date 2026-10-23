# Glossary

| Term | Meaning |
|------|---------|
| **ABI (persistent)** | The stored byte format other firmware versions rely on: container, section IDs, CBOR payloads. See [Persistent ABI](persistent-abi.md) |
| **Backend** | A platform implementation of a portable trait: `WifiBackend`, `IpBackend`, `BleTransport`, `ConfigStorage` |
| **BleTransport** | Portable BLE peripheral API: one connected central as a bounded byte pipe |
| **Capability** | A feature a device advertises through `GetCapabilities`; clients must not infer features from the firmware version |
| **Command** | A transient, bounded request to an owner task (scan, reconnect). Contrast *desired state* |
| **ConfigHandle / ConfigStore / ConfigSection** | Handle to the configuration owner task / the transactional store / one typed section owned by a feature crate |
| **Control plane (BLE)** | The local protobuf protocol served over BLE by `rohi-control` + `rohi-control-ble` |
| **Control plane (Wi-Fi)** | `WifiHandle` / `WifiManager`: configuration, scan, status, reconnect, disconnect. Not DNS or TCP |
| **Desired state** | The latest value a runtime should converge to (for example the desired `WifiConfig`); published, not queued. See [Runtime ownership](../concepts/runtime-ownership.md#state-versus-command) |
| **EspWireless** | The single owner of the ESP radio peripherals and the wireless heap; creates the Wi-Fi and BLE drivers |
| **Golden test** | A test pinning exact encoded bytes or IDs so accidental ABI changes fail |
| **Handle** | A small copyable value giving other tasks bounded access to a service owned by one task |
| **Identity** | The device's single Ed25519 key, provisioned from the host (never generated on the device) and stored as the `Identity` config section. See [Device identity](../sdk/identity.md) |
| **IP data plane** | `NetworkHandle`: DNS and TCP through `embedded-nal-async` / `embedded-io-async` |
| **MeasurementStore / MeasurementHandle / snapshot** | Owner-side store of typed observations / handle for on-demand requests / immutable `MeasurementsSnapshot` generated at request time |
| **Owner** | The one task (or runtime object) that holds the mutable state of a service |
| **Product** | A firmware (Altruist Urban) that composes the SDK and owns its resource budget |
| **Reconciliation** | The Wi-Fi manager continuously moving the actual radio state towards the latest desired configuration |
| **Section ID** | FNV-1a 64-bit hash of the Rust struct name identifying a `ConfigSection` |
| **SensorDriver / Runner** | Driver trait for one sensor / the generic task loop handling probing, polling, backoff and recovery |
| **Slot (A/B)** | One of the two flash slots of the configuration region; updates go to the inactive one and commit last |
| **Wireless heap** | The fixed heap shared by the Wi-Fi and BLE controllers; the only heap in the firmware |
