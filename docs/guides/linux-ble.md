# Linux BLE peripheral and mobile testing

With the `std-ble` feature the `std` Altruist Urban advertises the real ROHI Control Service through BlueZ, so a phone
running the ROHI mobile application can discover and operate it without a board. The GATT service, UUIDs, framing and
request/response behaviour are the ones of the ESP32 firmware (`rohi-control-ble`); the mobile application needs no
special mode.

```bash
cargo run -p altruist-urban --bin altruist-urban --features std,std-ble,sensor-simulator
```

## Requirements

| Requirement | Why |
|-------------|-----|
| Linux with a recent BlueZ and `bluetoothd` running | the peripheral is a BlueZ GATT application and advertisement, reached over D-Bus |
| A Bluetooth adapter with LE peripheral (advertising) support | check `btmgmt info`; some virtual machines and containers do not expose an adapter at all |
| Access to the **system** D-Bus and permission to register GATT applications | normally the `bluetooth` group or a D-Bus policy for your user; running as root also works |
| `libdbus-1` development files to build (`libdbus-1-dev`, or `dbus` in the nix shell) | `bluer` links `libdbus` |
| The adapter is not blocked and not in use by another peripheral application | `rfkill unblock bluetooth`; `bluetoothctl power on` |

Check the adapter first:

```bash
systemctl status bluetooth        # bluetoothd is running
bluetoothctl show | grep -E 'Powered|Roles|Name'   # an adapter, Powered: yes
btmgmt info | grep -i 'current settings'         # le is listed
```

## What to expect

The device advertises the service UUID of the ROHI Control Service with the name `ALTRUIST-URBAN-<last two bytes of the
adapter address>`, or the name of the `ble` configuration section. The `ble` service reports `advertising`, then
`connected` while a phone is connected, and `max_payload` from the negotiated MTU.

If BlueZ, the D-Bus socket or the adapter is missing, the device still starts and serves TCP; the `ble` service reports
the failure and bringing BLE up is retried every few seconds. Use `rohi --device tcp://127.0.0.1:9300 status ble`
to see it, or the log (`--log-level debug`).

## Smoke test with a phone

`firmwares/altruist-urban/scripts/ble-smoke.sh` automates the setup checks and starts the device:

```bash
firmwares/altruist-urban/scripts/ble-smoke.sh            # default adapter
firmwares/altruist-urban/scripts/ble-smoke.sh hci1       # another adapter
```

With the device running, in the ROHI mobile application:

1. Scan: `ALTRUIST-URBAN-XXXX` appears.
2. Connect. Open the configuration: read the `wifi`, `network`, `ble`, `time`, `location` and `connectivity` sections.
3. Change a value (for example the `location`) and read it back; restart the device with the same storage and check the
   value persisted.
4. Open the status of every service, including the `sensors` and `identity` diagnostics. With `sensor-simulator`, run
   `printf 'set co2 1500\n' | nc -N 127.0.0.1 9301` (the management port is also on loopback) and watch the value change.
5. Invoke an action (`wifi scan`).
6. Disconnect, then connect again: the device advertises again after every disconnect.

The same steps work with the desktop tool over BLE from another machine or a second adapter: `rohi scan`,
`rohi --device ALTRUIST-URBAN status`.

## Limitations

- **Software integration only.** The automated tests exercise the transport bridge with `rohi-control-ble` on top and
  the process with and without an adapter; they never talk to BlueZ or a phone. A real adapter and a phone are needed for
  the smoke test above, and no result of it is recorded in the support matrix.
- **No pairing or encryption.** The link is reported as unencrypted and unauthenticated and the control dispatcher
  allows everything. Do not treat this as production-secure.
- **Connection detection.** BlueZ does not report an ATT connection to a GATT application. A central counts as connected
  when it subscribes to the TX characteristic or writes the RX characteristic, and as disconnected when its notification
  session ends. A central that never subscribes cannot receive responses and is not tracked.
- **One central at a time**, as on the device.
- **macOS.** CoreBluetooth peripheral support is not implemented; the `std-ble` feature is Linux only. TCP control is
  expected to work on macOS but is not tested there.
