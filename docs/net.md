# Network (Wi-Fi)

`rohi-net` is the single network abstraction of the SDK. It provides a platform-independent Wi-Fi *station* API and
keeps the ESP32 radio stack in the separate platform crate `rohi-net-esp`.

```text
 firmware / control plane / future uplink
                 │   WifiHandle  (ROHI types only)
            WifiManager  ── state machine, backoff, commands
                 │   WifiBackend  (trait)
      rohi-net-esp  (ESP32-C3 / ESP32-C6, features `esp32c3` / `esp32c6`)
                 │
   esp-radio · esp-rtos · embassy-net (DHCPv4, DNS) · radio heap
```

No `esp-radio`, `esp-hal` or `embassy-net` type appears in the public API; vendor types and errors are converted in
`rohi-net-esp`. The only platform-specific entry point is `EspWireless` (see [Control plane](control.md)), called once by the board
firmware: it owns the radio heap and the radio peripherals shared with BLE and creates the Wi-Fi driver. `rohi-net` itself has no ESP dependency and builds and tests on the host.

## Public types

| Type | Role |
|------|------|
| `WifiConfig` | Persistent CBOR section `WifiConfig` (`ssid`, optional `password`); open network = `password: None` |
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
- Only one scan can be in flight; the command queue is bounded and `reconnect`, `disconnect` and scan requests return
  `Busy` when it is full.
- `apply_config` is different: it is `async` and waits for queue space, so a configuration that was already persisted
  always reaches the manager. Completion means the manager accepted it, not that the access point was reached;
  follow `status()` (`Connecting` / `Connected` / error) for that.
- Scans never abort or delay reconnection: a scan requested while connecting is served after the attempt, and a
  scan during the backoff wait or while connected does not restart the backoff or refresh timer.

## Limits

| Constant | Value |
|----------|-------|
| `MAX_SSID_LEN` | 32 |
| `MAX_WIFI_PASSWORD_LEN` | 64 (WPA passphrases are at least 8) |
| `MAX_WIFI_SCAN_RESULTS` | 16 |
| `WIFI_COMMAND_QUEUE_DEPTH` | 4 |
| `WIRELESS_HEAP_SIZE` | 96 KiB, shared by Wi-Fi and BLE (was 72 KiB for Wi-Fi only; to be tuned from hardware measurements) |
| IP stack sockets | 6 (DHCP, DNS, headroom) |

## Memory policy

Application and runtime code is heapless. The Espressif wireless stack needs dynamic allocation, so the ESP backend
owns a **fixed-size** wireless heap (`esp-alloc`, no growth) that Wi-Fi and BLE share. Heap-backed vendor results (e.g. the
scan `Vec`) are converted into bounded ROHI types inside the backend and dropped there. `wireless_heap()` reports the
free bytes and, with the `heap-stats` feature, the minimum free bytes.

## Security

The Wi-Fi password is secret: `WifiPassword` has a redacting `Debug`, nothing logs configuration contents, and
`NetworkInfo` has no credential field. Only the backend reads the secret (`expose_secret`). The control API (see [Control plane](control.md)) never returns the password and accepts it write-only.

## Hardware validation (to do)

Not yet measured on hardware: scan, connect/DHCP, reboot reconnect, wrong password, AP loss, BLE + Wi-Fi coexistence,
minimum free wireless heap (idle, scanning, connected), 24 h stability. Record results here, together with the final
`WIRELESS_HEAP_SIZE`.

## Out of scope

SoftAP, HTTP(S)/TLS, NTP, Connectivity Protocol uplink, multiple saved networks, static IP, WPA enterprise.
`WifiConfig` authentication currently selects WPA/WPA2 personal; WPA3-only networks are not yet supported by the
`esp-radio` station configuration used here.
