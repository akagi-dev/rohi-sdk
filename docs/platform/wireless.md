# Wireless runtime (Wi-Fi and BLE)

Wi-Fi and BLE on the ESP32 share one radio, one PHY, the coexistence arbiter and **one allocator**. They are therefore
brought up by one owner, `rohi_net_esp::EspWireless`, and not by two independent drivers. This page describes that
runtime and how Wi-Fi and BLE interact; the portable APIs are in [Networking](../sdk/net.md) and
[Control plane](../sdk/control.md).

```text
EspWireless::new(WIFI, BT, heap)     one owner of radio peripherals and the wireless heap
  ├── wifi(&mut NetResources)  ->  EspWifi + NetRunner + EspIp   (Wi-Fi control plane + IP data plane)
  └── ble(config, &mut BleResources) ->  EspBle + BleRunner      (BLE peripheral transport)
```

## Bring-up

```rust,ignore
let heap = rohi_net_esp::wireless_heap!(WIRELESS_HEAP_SIZE);    // adds the single wireless heap
let mut wireless = EspWireless::new(peripherals.WIFI, peripherals.BT, heap);
esp_rtos::start(...);
let (wifi, net_runner, ip) = wireless.wifi(NET_RESOURCES.init(NetResources::new()))?;
let (ble, ble_runner) = wireless.ble(BLE_CONFIG, BLE_RESOURCES.init(Default::default()))?;
```

Rules:

- `wireless_heap!` and `EspWireless::new` are called once, before `esp_rtos::start` and before any radio use. The
  peripherals and the heap region are moved in, so a second call cannot compile.
- `wifi()` and `ble()` each hand out their driver once (`WirelessError::AlreadyTaken` / `WifiError::DriverError`
  otherwise) and need the `esp-rtos` scheduler to be running.
- Neither driver initializes radio state or an allocator on its own. Nothing else in the firmware may add heap regions
  for the radio.
- A Wi-Fi or BLE failure must never stop the sensors: the firmware logs the error and carries on without that radio.

The sizes — the heap, the IP stack sockets, TCP buffers and the BLE queues — are chosen by the firmware in one place,
for Altruist Urban `firmwares/altruist-urban/src/resources.rs`, see [Resource limits](../reference/resource-limits.md).

## How Wi-Fi and BLE interact

- **Shared heap.** Both controllers allocate from the same fixed wireless heap; see
  [Memory model](../concepts/memory-model.md). `wireless_heap()` reports the free bytes and, with the `heap-stats`
  feature, the minimum free bytes, which must be checked with both radios busy.
- **Coexistence.** `esp-radio` runs with the `coex` feature so the radio arbitrates between the two protocols. Bringing
  the radio up, and the first Wi-Fi association in particular, keeps the core busy long enough to overrun the
  microphone DMA ring, so the firmware starts the sensor tasks after the first Wi-Fi attempt (bounded wait).
- **Independent control.** The BLE control plane configures Wi-Fi through the generic `wifi` config section and service of `rohi-service`; the Wi-Fi manager never
  depends on BLE and BLE never touches the radio driver of Wi-Fi.
- **Independent failure.** A BLE host error restarts advertising; a Wi-Fi failure backs off and retries. Neither stops
  the other or the sensors.

## Hardware status

Coexistence behaviour (BLE connected while Wi-Fi scans or reconnects) and the minimum free heap with both radios busy
are tracked in [Hardware testing](../validation/hardware-testing.md); they are not yet fully measured.
