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

Dependency direction: `rohi-control-proto ← rohi-control ← rohi-control-ble → rohi-net ← rohi-net-esp ← altruist-urban`
(see [Crate map](../concepts/crate-map.md)).
`rohi-net*` never depends on `rohi-control*`, and there is no ESP specific control crate: the ESP part only exposes a
generic byte pipe.

## Crates

| Crate | Role |
|-------|------|
| `rohi-net` (`ble`) | `BleTransport`, `BleState`, `BleConnectionInfo`, `BleConfig`: portable BLE API, no vendor types |
| `rohi-net-esp` | `EspWireless` (shared radio runtime), `EspWifi`, `EspIp`, `EspBle` |
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
| `UpdateConfig` | `CONFIG` (`CONNECTIVITY` for the telemetry section) | **yes** |
| `ScanWifi` | `WIFI_SCAN` | no |
| `GetWifiStatus` | `WIFI_STATUS` | no |
| `ReconnectWifi`, `DisconnectWifi` | `WIFI_CONTROL` | **yes** |
| `GetIdentity` | `IDENTITY` | no |
| `GetTimeStatus` | `TIME` | no |
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

Wi-Fi update sequence: validate (SSID 1–32 bytes, password 8–64 bytes or open) → `ControlServices::set_wifi_config`,
which persists the section **and** publishes it to the Wi-Fi runtime as the desired configuration (the firmware's
configuration owner task does both in one step, so there is no cancellable gap between commit and publication, see
[Async and cancellation](../concepts/async-and-cancellation.md#durable-mutation)) → success. **Success means the
configuration is stored and handed to the Wi-Fi runtime, not that the access point was reached**; observe
`GetWifiStatus` (`NOT_CONFIGURED`, `IDLE`, `SCANNING`, `CONNECTING`, `CONNECTED`, `DISCONNECTED`, `ERROR`).

Telemetry configuration (`CAPABILITY_CONNECTIVITY`) is a second, independent section of the same two requests:
`GetConfigResponse.connectivity` and the `connectivity_update` oneof of `UpdateConfigRequest`
(`update_connectivity` is a partial update of `enabled`, `node_id`, `interval_s`, `host`, `port`; `clear_connectivity`
removes the section, so the device falls back to its build-time defaults). Without a stored section an update starts from a
disabled configuration, so it needs a host. The merged result is validated (`ConnectivityConfig::validate`:
host 1–64 letters, digits, `.`, `-`; port 1–65535; interval 10 s … 24 h) before anything is stored, and `INVALID_ARGUMENT`
leaves everything unchanged, also the Wi-Fi section of the same request. The sections are then stored one by one, Wi-Fi
first, so a failure of the second leaves the first stored. A stored configuration takes effect **without a restart**: the
configuration owner task hands it to the telemetry task in the commit step, which rebuilds the service (frames still in
the outbox are dropped), see [Connectivity](connectivity.md#configuration). `GetConfig` reports the *stored* section, not the
build-time defaults.

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
| `MAX_HOST_LEN` | 64 |

The limits are defined once (`rohi-control-proto/build.rs`), applied to the generated types and checked by tests
against the worst-case encoded size. This table is the canonical list of protocol limits; the BLE queue depths belong to
the product and are in [Resource limits](../reference/resource-limits.md#ble).

### Security

`rohi-net` reports `encrypted` / `authenticated` for the current link. `rohi-control` turns that into decisions
with an `AuthorizationPolicy` (`Privilege::Read` or `Privilege::Mutate`): `OpenPolicy`, `RequireEncryption`,
`RequireAuthenticated`. The Altruist Urban firmware currently runs `OpenPolicy`, and the BLE backend does not pair yet
(TrouBLE's `security` feature is not enabled), so every link reports unencrypted. Anyone in radio range can use the
control plane until a policy is configured, **including identity provisioning and erase**, which use their own
`Privilege::Provision` so that a stricter policy can protect them through the same hook. The Wi-Fi password still crosses the air in clear text until pairing is
enabled.

## `rohi` desktop client

`tools/rohi` is one package: the **library** `rohi` carries the device protocol, the **binary** `rohi` is a thin
command line layer.

```text
rohi library
  Device         typed async API: info, capabilities, sensors, config, update_wifi, forget_wifi,
                 update_connectivity, forget_connectivity,
                 wifi_scan, wifi_status, wifi_reconnect, wifi_disconnect,
                 identity, provision_identity, erase_identity  (checks capabilities first)
  Session        framing, request ids, timeouts, typed error mapping   (reuses rohi-control-ble::frame)
  ClientTransport  chunk link trait; BleLink (btleplug) implements it, tests use an in-memory one
  model          plain Rust types (no protobuf), optional serde, canonical units
  format         human readable rendering
```

The command line has one authoritative page, [The rohi tool](../getting-started/rohi-tool.md): commands, device selection
(`--device`, `ROHI_DEVICE`, `ble://` and `tcp://` URIs), the output contract (results on stdout, prompts and
diagnostics on stderr, `-o json`, exit codes), Wi-Fi workflows and configuration backup and restore. It is a layer over
`Device`: parsing, device choice, prompts, output and backup files live in `tools/rohi/src/cli`, protocol behaviour stays
in `Device` and below, and the command dispatch (`rohi::cli::execute`) is generic over `ClientTransport`, so it runs
unchanged against the in-memory transport of the tests.

The Wi-Fi password is never printed, and a normal configuration backup never contains it. On Linux the build needs
`libdbus` and `pkg-config` (provided by `nix develop`); the protobuf compiler `protoc` is needed to build
`rohi-control-proto`.

## Build, test and measurements

Commands are listed once in [Build and test](../getting-started/build-and-test.md) and
[Targets and features](../reference/targets-and-features.md). The control plane is covered by host tests (protocol,
dispatcher, framing, `rohi` client end to end); flash, RAM and heap measurements are in
[Resource budget](../validation/resource-budget.md) and the open hardware checks in
[Hardware testing](../validation/hardware-testing.md). The shared radio runtime that serves BLE and Wi-Fi is described in
[Wireless runtime](../platform/wireless.md).
