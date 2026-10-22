# Network (Wi-Fi)

`rohi-net` is the single network abstraction of the SDK. It provides a platform-independent Wi-Fi *station* API and
hides the ESP32-C3 radio stack behind a private backend.

```text
 firmware / control plane / future uplink
                 │   WifiHandle  (ROHI types only)
            WifiManager  ── state machine, backoff, commands
                 │   WifiBackend  (trait)
      ESP32-C3 backend  (feature `esp`, private)
                 │
   esp-radio · esp-rtos · embassy-net (DHCPv4, DNS) · radio heap
```

No `esp-radio`, `esp-hal` or `embassy-net` type appears in the public API; vendor types and errors are converted in
`backend/esp.rs`. The only platform-specific entry points are `init_radio_heap()` and `EspWifi::new()`, called once
by the board firmware.

## Public types

| Type | Role |
|------|------|
| `WifiConfig` | Persistent section `wifi` (`ssid`, optional `password`); open network = `password: None` |
| `WifiState` / `WifiStatus` | `NotConfigured`, `Idle`, `Scanning`, `Connecting`, `Connected`, `Disconnected`, `Error` (+ last `WifiError`) |
| `NetworkInfo` | SSID, RSSI, IPv4, gateway, DNS. Never contains credentials |
| `WifiNetwork` / `WifiScanResults` | Bounded scan result (`heapless::Vec`), strongest first |
| `WifiError` | Stable typed errors (`AuthenticationFailed`, `DhcpFailed`, `Busy`, ...) |
| `WifiShared` / `WifiHandle` | `static` shared state and the cheap handle other tasks use to command the manager |

`Connected` means *network ready*: association **and** DHCP have completed.

## Behaviour

- No stored configuration: state `NotConfigured`, no retry loop.
- Boot: the firmware loads `WifiConfig` from the `ConfigStore` and sends `apply_config` to the manager.
- Failure or link loss: bounded exponential backoff (1 s doubling up to 60 s). The backoff is reset only after the
  link stayed up for one info refresh interval (10 s), so a flapping link keeps backing off. Failures never panic or
  reboot; sensors keep running.
- A new configuration, `reconnect`, `disconnect` and `forget` (`apply_config(None)`) are applied without a reboot.
- Only one scan can be in flight; the command queue is bounded and returns `Busy` when full.
- Scans never abort or delay reconnection: a scan requested while connecting is served after the attempt, and a
  scan during the backoff wait or while connected does not restart the backoff or refresh timer.

## Limits

| Constant | Value |
|----------|-------|
| `MAX_SSID_LEN` | 32 |
| `MAX_WIFI_PASSWORD_LEN` | 64 (WPA passphrases are at least 8) |
| `MAX_WIFI_SCAN_RESULTS` | 16 |
| `WIFI_COMMAND_QUEUE_DEPTH` | 4 |
| `RADIO_HEAP_SIZE` | 72 KiB (initial value, to be tuned from hardware measurements) |
| IP stack sockets | 6 (DHCP, DNS, headroom) |

## Memory policy

Application and runtime code is heapless. The Espressif wireless stack needs dynamic allocation, so the ESP backend
owns a **fixed-size** radio heap (`esp-alloc`, no growth). Heap-backed vendor results (e.g. the scan `Vec`) are
converted into bounded ROHI types inside the backend and dropped there. `radio_heap_free()` reports the free bytes.

## Security

The Wi-Fi password is secret: `WifiPassword` has a redacting `Debug`, nothing logs configuration contents, and
`NetworkInfo` has no credential field. Only the backend reads the secret (`expose_secret`). Field-level redaction
and write-only access in the control API belong to the control plane (WP4).

## Hardware validation (to do)

Not yet measured on hardware: scan, connect/DHCP, reboot reconnect, wrong password, AP loss, BLE + Wi-Fi coexistence,
minimum free radio heap (idle, scanning, connected), 24 h stability. Record results here, together with the final
`RADIO_HEAP_SIZE`.

## Out of scope

SoftAP, HTTP(S)/TLS, NTP, Connectivity Protocol uplink, multiple saved networks, static IP, WPA enterprise.
`WifiConfig` authentication currently selects WPA/WPA2 personal; WPA3-only networks are not yet supported by the
`esp-radio` station configuration used here.
