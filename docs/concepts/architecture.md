# Architecture

The SDK is layered: portable crates define typed models, contracts and logic; thin `*-esp` platform crates implement
those contracts on the ESP32; the firmware composes them and owns the product decisions (resource sizes, which
features run). This page shows the complete dependency model; [Crate map](crate-map.md) lists every crate.

```text
                    firmwares/altruist-urban   (product: composition, tasks, resources.rs)
 ┌──────────────────────────────┼───────────────────────────────────────────┐
 │                              │                                           │
 sensors + runtime        configuration                  networking / control
 ─────────────────        ─────────────                  ────────────────────
 rohi-hal                 rohi-config                    rohi-control-ble ─► rohi-control ─► rohi-control-proto
   drivers, Runner          ConfigStore                    (BLE binding)      (dispatcher)      (protobuf schema)
   MeasurementStore         ConfigSection, rohi-identity         │                  │
   MeasurementHandle        (derive: rohi-config-derive)         ▼                  ▼
       │                         │                          rohi-net ◄────── rohi-hal (measurement types)
       ▼                         ▼                            WifiHandle / WifiManager     (Wi-Fi control plane)
 rohi-hal-esp              rohi-config-esp                    NetworkHandle / IpBackend    (IP data plane)
   board pins, I²C,          flash region,                    BleTransport                 (BLE transport)
   UART, I²S, clock          esp-storage                            │
       │                         │                          rohi-net-esp  (EspWireless, EspWifi, EspIp, EspBle)
       └────────────┬────────────┴────────────────────────────────┘
                    ▼
        Embassy  ─  esp-rtos  ─  esp-hal / esp-radio / embassy-net / TrouBLE
```

Rules that keep this picture true:

1. **Dependencies point from platform and product towards portable crates.** `rohi-net` never depends on
   `rohi-net-esp` or on `rohi-control*`; `rohi-hal` never depends on `rohi-net`; portable crates never depend on
   `esp-*` crates. See [Portability](portability.md).
2. **Feature crates own their own `ConfigSection`.** `rohi-config` never learns the set of application sections:
   `rohi-net` owns `WifiConfig`, `NetworkConfig` and `BleSettings`, `rohi-identity` owns the `Identity` section. A section type lives next to the code that
   interprets it, and the firmware only wires the section to its runtime. See
   [Add a ConfigSection](../guides/add-config-section.md).
3. **Stateful services have one owner task** and are reached through bounded handles:
   see [Runtime ownership](runtime-ownership.md).
4. **Platform types never cross a portable API.** Vendor types and errors are converted into ROHI types inside the
   platform crate.
5. **The product, not the platform crate, decides how much RAM the wireless stack takes**: see
   [Memory model](memory-model.md).

## Data flows

```text
sensor tasks ─► SensorEvent ─► EVENTS (bounded) ─► store task ─► MeasurementStore
                                                       ▲                │
                                       MeasurementHandle (snapshot)     ▼
                                                          rohi-control ─► BLE ─► rohi client

BLE write ─► rohi-net-esp (EspBle, bounded RX) ─► rohi-control-ble (reassemble) ─► Controller
          ◄─ bounded TX ◄─ fragment ◄─ response ◄─────────────────────────────────┘
                                           │ ControlBackend (registries of rohi-service)
                  config sections (ConfigHandle) · services (MeasurementHandle, WifiHandle, ...)

ConfigHandle::set(WifiConfig) ─► config owner task ─ commit ─► publish desired Wi-Fi config ─► WifiManager
                                                                                                 │
                                          application ◄── NetworkHandle (DNS, TCP) ◄── EspIp ◄───┘ (link + DHCP)
```

## Where does new code go?

See the table at the end of [Crate map](crate-map.md#where-does-new-code-go).
