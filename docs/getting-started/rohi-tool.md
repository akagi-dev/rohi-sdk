# The `rohi` tool

`rohi` is the desktop command line for ROHI devices. It finds a device over Bluetooth LE, shows its status and
sensors, manages its Wi-Fi and configuration, and backs the configuration up. It is the quickest way to check that a
flashed device (see [First device](first-device.md)) is alive and talking.

`rohi` is a thin layer over the [`Device`](../sdk/control.md#rohi-desktop-client) API of the `rohi` library: every
command is one or a few control-plane requests, and the tool owns only the terminal side: arguments, device selection,
prompts, output formats and backup files.

> The behaviour described here is covered by host tests against an in-memory device. It has not been exercised with a
> wide range of adapters and operating systems.

## Install and run

`rohi` is a package of the workspace, `tools/rohi`. From the repository root (inside `nix develop` if you use Nix):

```bash
cargo install --path tools/rohi     # installs `rohi` into ~/.cargo/bin
cargo run -p rohi -- status         # or run it without installing
```

On Linux the build needs `libdbus` and `pkg-config` (provided by `nix develop`) and `protoc`; see
[Targets and features](../reference/targets-and-features.md). Bluetooth must be enabled, and the device must be
advertising and not connected to another client: a device accepts a single connection at a time.

`rohi --help` and `rohi <COMMAND> --help` describe every command with examples.

## First steps

```bash
rohi scan                  # find nearby ROHI devices
rohi status                # connection, firmware, board, capabilities, Wi-Fi, time, identity
rohi sensors               # current measurements
```

`rohi status` is the single command for the overall snapshot of a device:

```text
$ rohi status
Connection
  URI         ble://ALTRUIST-URBAN-A12F

Device
  Firmware    altruist-urban-0.4.0
  Board       urban-esp32c3

Capabilities
  sensors
  config
  wifi scan
  wifi status
  wifi control
  identity
  time

Wi-Fi
  State       connected
  Network     office
  Signal      -51 dBm
  Address     192.168.1.42

Time
  State       synchronized
  Unix time   1780000000.250
  Source      ntp
  Last sync   90 s ago
  Accuracy    +-40 ms

Identity
  State       provisioned
  Address     4FHZCUoTq7LqeeaVhgmk3WfwL4wAtRfM45aUV11XeARGeEju
  Public key  88dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0ee
```

Sections the device does not support are left out; a device without Wi-Fi still has a status. The identity `Address`
is the device public key as a Robonomics (SS58) address.

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

## Scripting

Command results go to stdout. Prompts, progress, warnings and diagnostics go to stderr, so redirects and pipes stay
clean. `-o json` (`--output json`) prints a JSON document with a stable, versioned schema instead of text:

```bash
rohi -d A12F -o json status | jq -r '.wifi.address'
rohi -d A12F -o json sensors | jq '.sensors[].measurements[] | select(.type == "co2")'
```

Every JSON document has `"schema_version": 1`. The schemas are owned by the command line, not by the library types.
A closed pipe (`rohi sensors | head -1`) ends the command quietly.

| Exit code | Meaning |
|-----------|---------|
| `0` | success |
| `1` | failure of the device, the link or the command (including a declined prompt) |
| `2` | command line usage error |
| `130` | interrupted with Ctrl-C |

Failures print an `error:` line and, where there is something to try, `hint:` lines:

```text
$ rohi status
error: no ROHI device found

hint: make sure Bluetooth is enabled and the device is advertising
hint: try `rohi scan`
```

Destructive commands (`wifi forget`, `identity erase`, `backup import`) ask for confirmation on a terminal. Without a
terminal they fail with `confirmation required in non-interactive mode` unless `--yes` is passed.

## Wi-Fi

```bash
rohi wifi                          # Wi-Fi status
rohi wifi scan                     # networks the device sees (takes up to half a minute)
rohi wifi connect office           # store the network and wait until the device is connected
rohi wifi connect guests --open    # open network
rohi wifi forget                   # remove the stored configuration
```

`rohi wifi connect <SSID>` stores the configuration, then follows the Wi-Fi status until the device is connected, reports
a failure, or `--wait <SECONDS>` (default 30) expires. A stored configuration does not mean the network was reached, and
the command says which of the two happened; `--no-wait` returns once the configuration is stored.

Passwords are not taken as command line arguments by `connect`: they are read from a hidden prompt, from the first line
of stdin with `--password-stdin`, or from the `ROHI_WIFI_PASSWORD` environment variable. An empty answer at the prompt
keeps the stored password. `wifi configure` is the lower-level operation that only stores values: omitted settings are
kept, and `--open` removes the password.

## Telemetry

```bash
rohi config connectivity                                   # stored telemetry configuration
rohi connectivity set --host ingress.example.org --node-id 42 --enable
rohi connectivity set --interval 120                       # omitted settings are kept
rohi connectivity set --disable                            # stop sending, keep the configuration
```

`connectivity set` takes any combination of `--host`, `--port` (default 80), `--node-id`, `--interval` (10 to 86400
seconds), `--enable` and `--disable`, and needs at least one. While nothing is stored a host is required. The device
validates the result and applies it at once, without a restart. Telemetry is signed with the device identity, so
provision one first (see [Identity](#identity)). A device without the `connectivity` capability answers
`this device does not support connectivity`. The settings are part of `rohi backup`.

## Time

```bash
rohi time                  # wall-clock time and synchronization state
```

The device has wall-clock time only after a source such as NTP delivered a plausible sample: until then the section shows
`State unsynchronized` and **no time**; the device never reports its uptime or a placeholder date instead. `Last sync`,
`Accuracy`, `Source` and the last failure (`Failures`) are shown when known. The time may step when a later sample
corrects it, and NTP time is not authenticated. See [Time service](../sdk/time.md).

## Back up and restore the configuration

```bash
rohi backup export backup.json     # or: rohi backup export > backup.json
rohi backup import backup.json --dry-run
rohi backup import backup.json
```

A backup is a versioned JSON file of the **stored configuration**, not of the runtime state:

```json
{
  "schema_version": 1,
  "source": { "firmware": "altruist-urban-0.4.0", "board": "urban-esp32c3" },
  "config": {
    "wifi": { "ssid": "office", "credential": "required" },
    "connectivity": { "enabled": true, "node_id": 42, "interval_s": 60, "host": "ingress.example.org", "port": 80 }
  }
}
```

- The Wi-Fi password cannot be read back from a device, so it is never in the file. `"credential": "required"` records
  that a password is set, `"none"` an open network, and `"wifi": null` that nothing was configured. Identity keys are not
  part of a backup either.
- The `connectivity` section is only exported by a device that has the capability. As with Wi-Fi, `null` means nothing was
  configured and removes a stored section on restore, an absent section is left alone.
- `import` validates the whole file and compares it with the device before changing anything. A malformed file, an
  unsupported `schema_version`, or a section the device cannot restore fails with no change. It shows what would change,
  asks for the missing password (hidden prompt, `--wifi-password-stdin`, or `ROHI_WIFI_PASSWORD`), and asks for
  confirmation unless `--yes` is passed.
- `--dry-run` prints the planned changes and exits without touching the device.
- A backup can be restored onto another compatible device; the firmware and board of the source are only reported.
- The control protocol has no atomic multi-section update, so a restore is not atomic. If it stops in the middle, the
  error lists the sections that were applied and the ones that were not.

## Identity

```bash
rohi identity                                          # state and address (alias: rohi id)
rohi identity provision --number 42 --seed master.suri
rohi identity erase
```

`provision` derives the identity of the device from a master secret URI in a file on this machine; only the derived
secret is sent to the device. See [Device identity](../sdk/identity.md) for the model and its limits.

## Commands

| Command | What it does |
|---------|--------------|
| `scan` | discover nearby devices |
| `status` | overall device snapshot |
| `sensors` | current measurements |
| `config [wifi\|connectivity]` | stored configuration (never secrets) |
| `connectivity set` | change the telemetry configuration |
| `wifi` and its subcommands `status`, `scan`, `connect`, `configure`, `reconnect`, `disconnect`, `forget` | Wi-Fi |
| `backup export [FILE]`, `backup import FILE` | configuration backup and restore |
| `time` | wall-clock time and synchronization state |
| `identity` and its subcommands `provision`, `erase` | device identity |

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `no ROHI device found` | Bluetooth is on, the device is powered and advertising, no other client holds its connection; raise `--scan-timeout` |
| `timed out waiting for the device` | the device is in range; raise `--timeout` |
| `multiple ROHI devices found` | pass `--device` or set `ROHI_DEVICE`; `rohi scan` lists them |
| `this device does not support …` | the firmware lacks that [capability](../sdk/control.md); see `rohi status` |

## Next

The protocol behind the tool and the library API are in [Control plane](../sdk/control.md).
