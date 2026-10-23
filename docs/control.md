# Control plane (BLE)

The control plane lets a desktop (or any BLE central) inspect and configure an Altruist Urban locally: device
information, capabilities, sensors, typed configuration, Wi-Fi and the device identity ([provisioning](identity.md)). It is **local control only**:
uplink telemetry, HTTP and SoftAP are out of scope.

```text
 BLE radio
     │
 rohi-net-esp::ble         ESP32 backend (esp-radio HCI + TrouBLE GATT)       platform
     │  BleTransport
 rohi-control-ble          Control Service, framing, fragmentation            portable
     │  bytes
 rohi-control              dispatcher, capabilities, authorization            portable
     │  rohi-control-proto messages (bounded protobuf)
 ControlServices           firmware adapter (UrbanControlServices)
     ├── MeasurementHandle   on-demand MeasurementsSnapshot
     ├── ConfigHandle        typed config section store
     ├── IdentityHandle      Ed25519 identity (Identity config section)
     └── WifiHandle          Wi-Fi manager

 rohi (library + CLI)      desktop client, btleplug
```

Dependency direction: `rohi-control-proto ← rohi-control ← rohi-control-ble → rohi-net ← rohi-net-esp ← altruist-urban`.
`rohi-net*` never depends on `rohi-control*`, and there is no ESP specific control crate: the ESP part only exposes a
generic byte pipe.

## Crates

| Crate | Role |
|-------|------|
| `rohi-net` (`ble`) | `BleTransport`, `BleState`, `BleConnectionInfo`, `BleConfig`: portable BLE API, no vendor types |
| `rohi-net-esp` | `EspWireless` (shared radio runtime), `EspWifi`, `EspBle` |
| `rohi-control-proto` | `control.proto`, bounded generated types (`micropb`), `PROTOCOL_VERSION`, `limits` |
| `rohi-control` | `Controller`, `ControlServices`, `Capabilities`, `AuthorizationPolicy` |
| `rohi-identity` | `Identity` config section, `Secret`, `Signer`, `IdentitySigner`; see [Device identity](identity.md) |
| `rohi-control-ble` | Service/characteristic UUIDs, `frame` (fragmenter, reassembler), `ControlBle` control loop |
| `rohi` (`tools/rohi`) | Desktop library (`Device`, `Session`, `BleLink`, model) and the `rohi` command line tool |

Everything except `rohi-net-esp`, the firmware and `tools/rohi` builds and is tested on the host.

## Protocol

One protobuf request, one protobuf response, correlated by `request_id`. The schema is
`crates/rohi-control-proto/proto/rohi/control/v1/control.proto`; field numbers and enum values are stable and only
ever added. `protocol_version` is `1` in every message. Unknown requests, a wrong version or a missing capability
return a typed error; the link is never dropped because of a bad request.

| Request | Capability | Needs secure link policy |
|---------|-----------|---------------------------|
| `GetInfo`, `GetCapabilities` | – | no |
| `GetSensors` | `SENSORS` | no |
| `GetConfig` | `CONFIG` | no |
| `UpdateConfig` | `CONFIG` | **yes** |
| `ScanWifi` | `WIFI_SCAN` | no |
| `GetWifiStatus` | `WIFI_STATUS` | no |
| `ReconnectWifi`, `DisconnectWifi` | `WIFI_CONTROL` | **yes** |
| `GetIdentity` | `IDENTITY` | no |
| `ProvisionIdentity`, `EraseIdentity` | `IDENTITY` | **yes** (`Privilege::Provision`) |

Errors: `INVALID_ARGUMENT`, `NOT_FOUND`, `NOT_SUPPORTED`, `BUSY`, `UNAVAILABLE`, `PERMISSION_DENIED`, `INTERNAL`, `IDENTITY_NOT_PROVISIONED`, `IDENTITY_ALREADY_PROVISIONED`.
A response with `request_id = 0` means the device could not parse the request at all.

Clients must read `GetCapabilities` and must not infer features from the firmware version.

### Sensors

The firmware keeps the strongly typed `rohi-hal` model. Only at the control boundary it becomes a generic list:
`Sensor { name, state, measurements[], error }`, `Measurement { type, value, state, age_ms }`. `Sensor.name` is the
physical sensor (`bmx280`, `scd4x`, `sds011`, `microphone`, `zmod4510`, `ags3871`, `radsens`). Observations of one
quantity from different sensors are never merged. Every snapshot is generated when the request is served, with the
current monotonic time, so `valid`/`stale` and `age_ms` are correct at request time. Measurements that were never
observed have no `value`.

Canonical units are fixed per type:

| Type | Unit | Type | Unit |
|------|------|------|------|
| `TEMPERATURE` | °C | `O3` | ppb |
| `HUMIDITY` | % RH | `NO2` | ppb |
| `PRESSURE` | Pa | `CO` | ppm |
| `CO2` | ppm | `AQI_FAST`, `AQI_EPA` | index |
| `PM2_5`, `PM10` | µg/m³ | `RADIATION` | µSv/h |
| `NOISE_AVERAGE`, `NOISE_MAXIMUM` | dB | `PULSE_COUNT` | pulses |

### Configuration

The API is typed and per logical field; raw CBOR, section ids, flash offsets and store records never appear on the
wire. Updates are partial:

| In the request | Effect |
|----------------|--------|
| field omitted | stored value preserved |
| field provided | value replaced |
| explicit clear (`clear_password`, `clear_wifi`) | value removed |

A client never needs to read a secret to change something else. The Wi-Fi password is never returned or logged; the
response only says `password_configured`. There is no generic reset: configuration operations are per section, so
they can never touch the device identity (see [Device identity](identity.md)). A destructive reset, if ever added, has to be a separate privileged
operation.

Wi-Fi update sequence: validate (SSID 1–32 bytes, password 8–64 bytes or open) → persist with `ConfigHandle` →
`WifiHandle::apply_config` (waits for queue room, never fails with `BUSY`) → success. **Success means the
configuration is stored and handed to the Wi-Fi runtime, not that the access point was reached**; observe
`GetWifiStatus` (`NOT_CONFIGURED`, `IDLE`, `SCANNING`, `CONNECTING`, `CONNECTED`, `DISCONNECTED`, `ERROR`).

## BLE binding

One GATT service with two characteristics:

| | UUID | Direction |
|---|------|-----------|
| ROHI Control Service | `726f6869-0001-4d00-8000-636f6e74726f` | |
| RX | `726f6869-0002-4d00-8000-636f6e74726f` | central writes a chunk (write / write without response) |
| TX | `726f6869-0003-4d00-8000-636f6e74726f` | peripheral notifies a chunk |

The device advertises the service UUID and the name `ALTRUIST-URBAN-XXXX` (scan response), where `XXXX` are the last two bytes of the device MAC in hex.

A message is split into frames of at most one ATT payload (MTU − 3, between 20 and 244 bytes). The framing does not
know protobuf:

```text
| version u8 | msg id u8 | index u8 | count u8 | total len u16 LE | payload chunk |
```

Frames arrive strictly in order; a new first fragment (`index 0`) starts a new message. `count` and `total len` must
not change within a message. Bad frames (bad version, invalid or duplicate index, a skipped fragment, a changed
header, an oversized message, a length mismatch) are rejected: the partial message is dropped, the rest of that
message is skipped, and the device answers once with `INVALID_ARGUMENT` (`request_id 0`). Disconnecting drops all
partial state; a response is never sent to a different connection than the one that asked.

One connection, one request in flight, one response. Nothing runs in a radio callback: GATT writes are copied into a
bounded queue (the ATT response waits for room, which is the backpressure), the control task reassembles and
dispatches, and responses leave through a bounded TX queue.

### Limits

| Constant | Value |
|----------|-------|
| `MAX_REQUEST_LEN` | 256 B (worst-case encoded request: 116 B) |
| `MAX_RESPONSE_LEN` | 2048 B (worst-case encoded response: 1906 B) |
| ATT payload | 20 – 244 B, header 6 B, at most 255 fragments |
| `MAX_SENSORS` / `MAX_MEASUREMENTS_PER_SENSOR` | 8 / 4 |
| `MAX_WIFI_SCAN_RESULTS` | 16 |
| `MAX_SSID_LEN` / `MAX_WIFI_PASSWORD_LEN` | 32 / 64 |
| BLE RX / TX queue | 4 / 4 chunks |

The limits are defined once (`rohi-control-proto/build.rs`), applied to the generated types and checked by tests
against the worst-case encoded size.

### Security

`rohi-net` reports `encrypted` / `authenticated` for the current link. `rohi-control` turns that into decisions
with an `AuthorizationPolicy` (`Privilege::Read` or `Privilege::Mutate`): `OpenPolicy`, `RequireEncryption`,
`RequireAuthenticated`. The Altruist Urban firmware currently runs `OpenPolicy`, and the BLE backend does not pair yet
(TrouBLE's `security` feature is not enabled), so every link reports unencrypted. Anyone in radio range can use the
control plane until a policy is configured, **including identity provisioning and erase**, which use their own
`Privilege::Provision` so that a stricter policy can protect them through the same hook. The Wi-Fi password still crosses the air in clear text until pairing is
enabled.

## Shared wireless runtime

Wi-Fi and BLE share one radio, the PHY, the coexistence arbiter and one allocator. `rohi_net_esp::EspWireless` is the
only way to create the drivers:

```rust
let mut wireless = EspWireless::new(peripherals.WIFI, peripherals.BT); // adds the single wireless heap
esp_rtos::start(...);
let (wifi, net_runner) = wireless.wifi()?;
let (ble, ble_runner) = wireless.ble(BLE_CONFIG)?;
```

Neither driver initializes radio state or an allocator on its own. `WIRELESS_HEAP_SIZE` is 128 KiB (was 72 KiB for
Wi-Fi only; 96 KiB left only 11 KB minimum free on hardware with Wi-Fi and BLE running) and has to be confirmed with `wireless_heap()` (free and, with the `heap-stats` feature, minimum free)
while both radios are busy, see [Validation](#validation).

## `rohi` desktop client

`tools/rohi` is one package: the **library** `rohi` carries the device protocol, the **binary** `rohi` is a thin
command line layer.

```text
rohi library
  Device         typed async API: info, capabilities, sensors, config, update_wifi, forget_wifi,
                 wifi_scan, wifi_status, wifi_reconnect, wifi_disconnect,
                 identity, provision_identity, erase_identity  (checks capabilities first)
  Session        framing, request ids, timeouts, typed error mapping   (reuses rohi-control-ble::frame)
  ClientTransport  chunk link trait; BleLink (btleplug) implements it, tests use an in-memory one
  model          plain Rust types (no protobuf), optional serde, canonical units
  format         human readable rendering
```

```bash
rohi scan                       # find ROHI devices
rohi info                       # firmware, board, features
rohi status                     # info + Wi-Fi status
rohi sensors                    # current measurements
rohi config                     # stored configuration (never the password)
rohi wifi                       # = rohi wifi status
rohi wifi scan
rohi wifi configure --ssid <SSID>                      # asks for the password (hidden; empty keeps the stored one)
rohi wifi configure --ssid <SSID> --open               # open network, no prompt
rohi wifi configure --ssid <SSID> --password <PASSWORD>
rohi wifi reconnect
rohi wifi disconnect
rohi wifi forget [--yes]
rohi identity                  # = rohi id: identity state and public key
rohi identity provision -n <NUMBER> --seed <PATH>
rohi identity erase [--yes]
```

Global options: `--json` (desktop only, the firmware never speaks JSON), `--device <ID>` (identifier from
`rohi scan`), `--scan-timeout`, `--timeout`, `--payload` (chunk size if the link negotiated a larger MTU; `btleplug`
does not report the MTU, so the safe default is 20 bytes).

**`--password` is risky**: command line arguments stay in the shell history and are visible to other users in the
process list. Prefer the hidden prompt, or the `ROHI_WIFI_PASSWORD` environment variable. Without `--password`/`--open`,
`configure` prompts; an empty answer keeps the stored password, so `rohi wifi configure --ssid X` can change only the
network name. Use `--open` to remove the password.

The password is never printed. On Linux the build needs `libdbus` and `pkg-config` (provided by `nix develop`); the
protobuf compiler `protoc` is needed to build `rohi-control-proto`.

## Build and test

```bash
cargo test                      # host: all portable crates, framing, dispatcher, rohi library e2e
cargo check-esp32c3 && cargo check-esp32c6
cargo build-esp32c3 && cargo build-esp32c6
```

Never validate several chips with `--all-features`.

## Resource budget

Measured with `size` on the release build (`cargo build-esp32c3` / `cargo build-esp32c6`), baseline = Wi-Fi only,
before WP4:

| | ESP32-C3 before | ESP32-C3 after | ESP32-C6 before | ESP32-C6 after |
|---|---:|---:|---:|---:|
| `.text` (flash) | 970 092 | 1 149 952 | 1 012 322 | 1 335 452 |
| `.data` | 10 024 | 15 640 | 12 876 | 18 396 |
| `.bss` | 106 984 | 181 248 | 111 288 | 185 056 |
| wireless heap (in `.bss`) | 73 728 | 131 072 | 73 728 | 131 072 |
| `.bss` without heap | 33 256 | 50 176 | 37 560 | 53 984 |

Statically allocated control buffers: reassembly 256 B + response 2 048 B + chunk 244 B (control task), BLE RX/TX
queues 4 × ~250 B each, TrouBLE host resources, and the task futures. The application code stays heapless; only
the radio drivers use the wireless heap.

## Validation

Host tests cover the protocol (golden bytes, bounds, unknown fields/requests), the dispatcher (capabilities,
authorization, partial updates, persist-before-apply, secrets), the framing (every case of the checklist: single and
multiple fragments, maximum size, one byte oversized, truncated, invalid index, duplicate, missing, disconnect in the
middle of a request, reset after disconnect, exact protobuf byte preservation) and the `rohi` client end to end
against the real dispatcher.

**First hardware data point** (96 KiB heap, Wi-Fi + BLE running, chip not recorded): `14 656 B` free, `11 120 B` minimum free, i.e. about 87 KB peak use, too little margin, hence 128 KiB. Re-measure the minimum free with the larger heap during BLE connected + Wi-Fi scan/reconnect.

**Not yet measured on hardware** (needs a device; record results here): BLE advertising with Wi-Fi disconnected and
connected, BLE connected with Wi-Fi connected / scanning / reconnecting, minimum free wireless heap for each, MTU
negotiation, and a 24 h (preferably 72 h) stability run exercising BLE connect/disconnect, sensor reads, config
reads and updates, Wi-Fi status, scan and reconnect. Expected outcome: no panic, no allocator exhaustion, no
deadlock, no permanent BLE or Wi-Fi failure.
