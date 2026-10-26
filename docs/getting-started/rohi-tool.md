# The `rohi` tool

`rohi` is the desktop command line for ROHI devices. It finds a device over Bluetooth LE, shows the status of its
[services](services.md), reads and changes its configuration, and runs its actions. It is the quickest way to check
that a flashed device (see [First device](first-device.md)) is alive and talking.

`rohi` is a thin layer over the [`Device`](../sdk/control.md#rohi-desktop-client) API of the `rohi` library, and the
command tree **knows no device module**: `status`, `config` and `action` take the names of services, sections and
actions that the device lists, and parse values with the field descriptions the device sends. A new firmware module
appears in `rohi` without changing the tool. The tool owns only the terminal side: arguments, device selection,
prompts, output formats and configuration files.

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

`rohi --help` and `rohi <COMMAND> --help` describe every command with examples. Keep the tool and the firmware from the
same revision: the control protocol is under development and a stale `rohi` can show an incomplete view of a newer
device (for example fewer fields of a section).

## First steps

```bash
rohi scan                  # find nearby ROHI devices
rohi status                # device health and every service
rohi status sensors        # one service in detail
rohi config list           # the configuration sections of the device
rohi action list           # what the services can do
```

```text
$ rohi status
Device:  altruist-urban-0.4.0 (urban-esp32c3)
Uptime:  2h 14m
Health:  ok

SERVICE       STATE    HEALTH  READY  SINCE
network        running  ok      yes    2h 14m
connectivity  running  ok      yes    34s
...
```

What the columns mean and how status differs from configuration is explained in
[Services and status](services.md).

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
rohi -d A12F -o json status network | jq -r '.values.ip'
rohi -d A12F -o json status sensors | jq '.values["scd4x.co2"]'
rohi -d A12F -o json status | jq -r '.services[] | select(.health != "ok") | .service'
```

Every JSON document has `"schema_version": 1`. The schemas are owned by the command line, not by the library types.
A closed pipe (`rohi action list | head -1`) ends the command quietly.

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

Destructive commands (`config reset`, `factory-reset`, `config apply`) ask for confirmation on a terminal. Without a
terminal they fail with `confirmation required in non-interactive mode` unless `--yes` is passed. Secrets are never
accepted as command line arguments: a field or argument that is a secret is read with `NAME=-` from stdin, or from a
hidden prompt on a terminal.

## Status

```bash
rohi status                       # device health and one row per service: one compact request
rohi status network                  # one service: values with units, active issues
rohi status --watch               # reprint when something observable changed (Ctrl-C to stop)
rohi status network --watch --interval 5
rohi -o json status network
```

Services are discovered from the device, so `rohi status <name>` works for any service the summary lists. Without a
service the output is the summary table; with `-o json` it is
`{ "device": .., "uptime_ms": .., "health": .., "services": [..] }`. See [Services and status](services.md) for state,
health, readiness, issues and revisions.

## Configuration

`rohi config` works on the **sections** the device lists. Fields are typed and described by the device; values are
parsed against that description (`true`/`false`, numbers, text, hex for bytes, a variant name for enums) and checked
again by the device.

```bash
rohi config list                                   # sections and their versions
rohi config describe connectivity                  # fields: type, access, constraints, description
rohi config get connectivity                       # values; secrets are shown as <configured>
rohi config get connectivity host                  # one field

rohi config set connectivity host=ingress.example.org port=443 enabled=true
rohi config set connectivity interval_s=120 --dry-run
rohi config set network ssid=office password=-         # secret from stdin or a hidden prompt
rohi config unset network password                    # optional field: no value (not "" or 0)
rohi config reset connectivity                     # remove the stored section, restore defaults
```

Format the whole configuration storage of the device with the dedicated `rohi factory-reset` command (not a `config`
subcommand): every section, the device identity and anything else stored in the partition is erased, including section
versions, and a damaged storage is repaired. The device is unprovisioned afterwards; the command then asks it to reboot (`rohi reboot`).

```sh
rohi factory-reset            # asks for confirmation
rohi factory-reset --yes
```

```text
$ rohi config describe connectivity
FIELD       TYPE    ACCESS      CONSTRAINTS  DESCRIPTION
enabled     bool    read-write               Produce and send telemetry
node_id     u64     read-write               Robonomics CPS node identifier
interval_s  u64     read-write  10..86400 s  Time between telemetry frames
host        string  read-write               Ingress host name or IPv4 address
port        u64     read-write  1..65535     Ingress TCP port
```

- **`set` is a patch**: fields that are not mentioned keep their value, and a section that is not stored yet starts
  from its defaults. All mutations of one command are validated together on the device and stored atomically: if
  anything is invalid, nothing is stored. `--dry-run` validates without storing.
- **Secrets** (`write-only` in `describe`) are never returned and never part of exports. Put `NAME=-` instead of a
  value; the tool refuses a secret on the command line.
- **`unset`** is only for optional fields. A required field cannot be unset.
- **Reset** removes the persisted section, so the module defaults apply again.
- A change that needs a restart says so (`A restart is required for the change to take effect`).

### Configuration files

```bash
rohi config export config.json              # or: rohi config export > config.json
rohi config diff config.json                # what applying would change
rohi config apply config.json --dry-run     # validate everything, change nothing
rohi config apply config.json
```

A file maps section names to their fields:

```json
{
  "network": { "ssid": "office" },
  "connectivity": { "enabled": true, "interval_s": 120, "host": "ingress.example.org", "port": 443 },
  "location": { "latitude": 52.52, "longitude": 13.405 }
}
```

| In the file | Effect |
|-------------|--------|
| field missing | left unchanged |
| `"field": null` | unset (optional fields only) |
| `"section": null` | reset the section |

- **Export** writes every *configured* section. Secrets and read-only fields are left out, so applying an exported file
  never clears or overwrites a secret.
- A file cannot contain secrets: `config apply` refuses them and points at `config set NAME=-`.
- **`diff`** compares the file with the device and prints `~` changes, `+` new values, `-` unsets and `reset section`;
  nothing is changed. Fields that already match are not listed.
- **`apply`** checks the whole file against the schemas first (unknown section, unknown or read-only field, wrong type,
  null for a required field fail with no change), then has the device validate **every** section with a dry run, shows
  the changes and asks for confirmation unless `--yes`, then applies the sections one by one. Each section is atomic;
  applying several is **not** atomic as a whole. If the device fails in between, the error lists the sections that
  were applied and the ones that were not.
- The backup of earlier versions (`rohi backup`) is replaced by `export`/`apply`; an old backup file is not understood.

## Actions

```bash
rohi action list                                   # all actions: service, name, read/write, description
rohi action describe network scan                     # arguments and results
rohi action run network scan                          # results as a table, one row per record
rohi action run network reconnect
rohi action run radio tune channel=11              # arguments are NAME=VALUE, all required
rohi -o json action run network scan
```

`write` actions need a link that satisfies the authorization policy of the device. A refusal with a stable reason is
reported as `the device rejected the action: <reason>` (for example `already_provisioned`).

## Identity

The identity is an ordinary service: no dedicated command.

```bash
rohi status identity                                   # state, public key; never the secret
rohi action run identity provision secret=-            # 32-byte secret as hex, from stdin or a hidden prompt
rohi action run identity erase
```

The secret is derived on a trusted machine (`rohi::derive_identity`, see [Device identity](../sdk/identity.md)) and
only the derived secret is sent. `rohi factory-reset` erases it with the rest of the configuration storage. A malformed secret is reported
without echoing it.

## Migrating from earlier versions

| Before | Now |
|--------|-----|
| `rohi sensors` | `rohi status sensors` |
| `rohi time` | `rohi status time` |
| `rohi wifi` (status) | `rohi status network` |
| `rohi wifi scan` | `rohi action run network scan` |
| `rohi wifi reconnect`, `disconnect` | `rohi action run network reconnect` / `disconnect` |
| `rohi wifi connect`, `configure`, `forget` | `rohi config set network ssid=.. password=-`, `rohi config reset network`, then `rohi status network` |
| `rohi config [wifi\|connectivity]` | `rohi config get <section>` |
| `rohi connectivity set --host ..` | `rohi config set connectivity host=..` |
| `rohi backup export` / `import` | `rohi config export` / `apply` |

## Commands

| Command | What it does |
|---------|--------------|
| `scan` | discover nearby devices |
| `status [SERVICE] [--watch]` | device summary or one service |
| `config list`, `describe`, `get`, `set`, `unset`, `reset` | read and change configuration sections |
| `config export`, `diff`, `apply` | configuration files |
| `factory-reset [--yes]` | format the configuration storage, identity included |
| `reboot` | reboot the device |
| `action list`, `describe`, `run` | actions of the services |

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `no ROHI device found` | Bluetooth is on, the device is powered and advertising, no other client holds its connection; raise `--scan-timeout` |
| `timed out waiting for the device` | the device is in range; raise `--timeout` |
| `multiple ROHI devices found` | pass `--device` or set `ROHI_DEVICE`; `rohi scan` lists them |
| `the device has no section …` / `NotFound` | the name is not one the device lists: `rohi config list`, `rohi status`, `rohi action list` |
| a section shows fewer fields than expected | the tool is older than the firmware; rebuild `rohi` from the same revision |
| `the device refused the request: permission denied` | the change needs an encrypted or authenticated link, see [Security](../sdk/control.md#security) |

## Next

How the services, sections and actions are built is in [Service pipeline](../sdk/service.md); the protocol behind the
tool and the library API are in [Control plane](../sdk/control.md).
