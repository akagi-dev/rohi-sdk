# Discovery & connection

Commands that operate on a device first find it and open a Bluetooth LE link to it. This page covers how the device is chosen and what to check when that fails. No Wi-Fi or cloud connectivity is needed: the link is local BLE.

```bash
rohi scan                  # list nearby ROHI devices
```

## Choose a device

| Situation | What `rohi` does |
|-----------|------------------|
| One device found | uses it |
| Several devices, interactive terminal | shows a numbered list on stderr and asks |
| Several devices, no terminal | fails and lists them; it never picks one for you |
| `--device <DEVICE>` | uses the device that matches; fails if none or more than one does |

`--device` (`-d`) accepts the exact platform ID, the exact advertised name, or a unique prefix or suffix of either
(`A12F` for `ALTRUIST-URBAN-A12F`). Setting `ROHI_DEVICE` is the same as passing `--device`.

```bash
rohi -d A12F status
export ROHI_DEVICE=ALTRUIST-URBAN-A12F
```

The value may also be a URI, so the transport is explicit: `ble://ALTRUIST-URBAN-A12F`. Without a scheme the transport
follows from the content: `host:port` is TCP, anything else is a Bluetooth name or ID. `tcp://HOST:PORT` is accepted
by the parser but **not implemented yet**: `rohi` reports that the transport is not supported.

`--scan-timeout <SECONDS>` bounds the search for the device and `--timeout <SECONDS>` the wait for each response. A scan
without `--device` keeps looking for a couple of seconds after the first device appears, so a second device can show up.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `no ROHI device found` | Bluetooth is on, the device is powered and advertising, no other client holds its connection; raise `--scan-timeout` |
| `timed out waiting for the device` | the device is in range; raise `--timeout` |
| `multiple ROHI devices found` | pass `--device` or set `ROHI_DEVICE`; `rohi scan` lists them |
| `the device has no section …` / `NotFound` | the name is not one the device lists: `rohi config`, `rohi status`, `rohi action` |
| a section shows fewer fields than expected | the tool is older than the firmware; rebuild `rohi` from the same revision |
| `the device refused the request: permission denied` | the change needs an encrypted or authenticated link, see [Control plane](https://docs.rs/rohi-control) |

