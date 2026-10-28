# The `rohi` tool

`rohi` is the desktop command line for ROHI devices. It finds a device over Bluetooth LE, shows the status of its
[services](services.md), reads and changes its configuration, and runs its actions. It is the quickest way to check
that a flashed device (see [First device](first-device.md)) is alive and talking.

`rohi` is a thin layer over the [`Device`](../sdk/control.md#rohi-desktop-client) API of the `rohi` library, and the
command tree **knows no device module**: `status`, `config` and `action` take the names of services, sections and
actions that the device lists, and parse values with the field descriptions the device sends (type, unit, enum variants; the wire carries ids and numbers, the tool translates them with the schema). A new firmware module
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
rohi config                # the configuration sections of the device
rohi action                # what the services can do
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
rohi -d A12F -o json config wifi | jq '.overridden'   # {"section", "values", "overridden": [field names]}
```

Every JSON document has `"schema_version": 1`. The schemas are owned by the command line, not by the library types.
A closed pipe (`rohi action | head -1`) ends the command quietly.

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

`rohi config` works on the **sections** the device lists. The configuration of a section is its **effective**
configuration: the defaults of the firmware plus the **overrides** you set explicitly. Fields are typed and described by
the device; values are parsed against that description (`true`/`false`, numbers, text, hex for bytes, a variant name for
enums) and checked again by the device.

```bash
rohi config                                        # sections and their versions
rohi config connectivity                           # the effective values; secrets are shown as <redacted>
rohi config connectivity --help                    # fields: type, unit, allowed values (from the device)

rohi config connectivity host=ingress.example.org port=443 enabled=true
rohi config connectivity interval_s=120 --dry-run
rohi config wifi ssid=office password=-               # secret from stdin or a hidden prompt
rohi config wifi ssid=none                            # disable Wi-Fi (no ssid is the default)
rohi config wifi scan_timeout=null                    # remove the override: back to the firmware default
rohi config wifi password=none                        # explicit "no value": an open network
rohi config reset connectivity                     # remove every override of the section
```

A name that is also a command (`reset`, `export`, `diff`, `apply`) is not a valid section name. `rohi config --help`
and `rohi action --help` need no device; `rohi config SECTION --help` connects and is generated from the schema.

```text
$ rohi config wifi
ssid             office
password         <redacted>
scan_timeout     20000 ms
connect_timeout  30000 ms
backoff_initial  1000 ms
backoff_max      60000 ms
link_refresh     10000 ms
```

An optional field without a value shows as `<unset>` when it follows a default that has none, and as `<none>` when you
set it to no value explicitly. After a change `rohi` reads the section
again and prints what the device reports, so the output is always the effective result.

Format the whole configuration storage of the device with the dedicated `rohi factory-reset` command (not a `config`
subcommand): every section, the device identity and anything else stored in the partition is erased, including section
versions, and a damaged storage is repaired. The device is unprovisioned afterwards; the command then asks it to reboot (`rohi reboot`).

```sh
rohi factory-reset            # asks for confirmation
rohi factory-reset --yes
```

```text
$ rohi config connectivity --help
Usage:
  rohi config connectivity [FIELD=VALUE...]

Fields:
  enabled     bool
  node_id     u64
  interval_s  u64 s
  host        string
  port        u64

Special values:
  FIELD=null  clear the override and use the field default
...
```

- **Effective = defaults + overrides.** A field you never set keeps following the firmware default, also after a
  firmware update changes it; a field you set stays as you set it.
- **`FIELD=VALUE` is a patch** of overrides: fields that are not mentioned are not touched. All mutations of one command
  are applied to a copy, the complete effective result is validated on the device and stored atomically: if anything
  is invalid, nothing is stored. `--dry-run` validates without storing.
- **The result of every change is validated** as `defaults + overrides`; an unconfigured or disabled section (no
  `ssid` means Wi-Fi is disabled, no location, connectivity disabled) is a valid state. Fields that belong together are therefore set in one
  command: `rohi config connectivity enabled=true` alone fails until the `host` is set,
  `rohi config connectivity enabled=true host=ingress.example.org` works.
- **Three states for a nullable field.** `FIELD=value` is an explicit value, `FIELD=none` is an explicit "no value"
  (`rohi config wifi password=none` configures an open network, `rohi config time server2=none` disables the second NTP
  server) and `FIELD=null` removes the override, so the field follows the **current firmware default** again
  (`rohi config time server2=null` restores the firmware's second server). `none` is only valid for fields that
  `--help` marks `nullable`; `rohi` refuses it for any other field before contacting the device. `FIELD=null` works for
  any writable field, required or not.
- **Secrets** (`secret` in `--help`) are never returned and never part of exports. Put `NAME=-` instead of a value;
  the tool refuses a secret on the command line. `password=none` and `password=null` are allowed: they carry no secret.
- **Reset** removes every override of the section, so the firmware defaults apply again.
- A change that needs a restart says so (`A restart is required for the change to take effect`).

### Configuration files

A file describes your **overrides**, not the defaults:

```bash
rohi config export config.json              # or: rohi config export > config.json
rohi config diff config.json                # what applying would change
rohi config apply config.json --dry-run     # validate everything, change nothing
rohi config apply config.json
```

A file maps section names to their fields:

```json
{
  "wifi": { "ssid": "office" },
  "connectivity": { "enabled": true, "interval_s": 120, "host": "ingress.example.org", "port": 443 },
  "location": { "latitude": 52.52, "longitude": 13.405 }
}
```

| In the file | Effect |
|-------------|--------|
| `"field": value` | set the override (even if the value equals the default) |
| `"field": null` | remove the override (follow the firmware default) |
| `"field": {"none": true}` | explicit "no value" of a nullable field (distinct from `null`) |
| field missing | left unchanged |
| `"section": null` | reset the section (remove every override) |

- **Export** writes the explicit overrides only, never materialized defaults, so a later firmware default still
  reaches fields you did not override. A nullable field you set to no value is written as `{"none": true}`. Sections without overrides are omitted; secrets and read-only fields are left
  out, so applying an exported file never clears or overwrites a secret.
- A file cannot contain secrets: `config apply` refuses them and points at `config SECTION NAME=-`.
- **`diff`** compares the file with your overrides on the device and prints `~` changes, `+` new overrides, `-` removed
  overrides and `reset section`; nothing is changed. Fields that already match are not listed.
- **`apply`** checks the whole file against the schemas first (unknown section, unknown or read-only field, wrong type
  fail with no change), then has the device validate **every** section with a dry run, shows
  the changes and asks for confirmation unless `--yes`, then applies the sections one by one. Each section is atomic;
  applying several is **not** atomic as a whole. If the device fails in between, the error lists the sections that
  were applied and the ones that were not. Every section is applied against the revision its diff was computed from, so
  a section that another client changed after the diff was shown fails with a conflict instead of being overwritten.
- The backup of earlier versions (`rohi backup`) is replaced by `export`/`apply`; an old backup file is not understood.

## Actions

```bash
rohi action                                        # all actions: service, name, privileged
rohi action network                                # the actions of one service with their arguments
rohi action network scan --help                    # arguments and results (generated from the device)
rohi action network scan                           # results as a table, one row per record
rohi action network reconnect
rohi action radio tune channel=11                  # arguments are NAME=VALUE, all required
rohi -o json action network scan
```

`-o json` with `--help` prints the schema. Privileged actions need a link that satisfies the authorization policy of
the device. A refusal with a stable reason is reported as `the device rejected the action: <reason>` (for example
`already_provisioned`).

## Identity

The identity is an ordinary service: no dedicated command.

```bash
rohi status identity                                   # state, public key; never the secret
rohi action identity provision secret=-               # 32-byte secret as hex, from stdin or a hidden prompt
rohi action identity erase
```

The secret is derived on a trusted machine (`rohi::derive_identity`, see [Device identity](../sdk/identity.md)) and
only the derived secret is sent. `rohi factory-reset` erases it with the rest of the configuration storage. A malformed secret is reported
without echoing it.

## Migrating from earlier versions

| Before | Now |
|--------|-----|
| `rohi sensors` | `rohi status sensors` |
| `rohi time` | `rohi status time` |
| `rohi wifi` (status) | `rohi status wifi` |
| `rohi wifi scan` | `rohi action wifi scan` |
| `rohi wifi reconnect`, `disconnect` | `rohi action wifi reconnect` / `disconnect` |
| `rohi wifi connect`, `configure`, `forget` | `rohi action wifi connect ssid=- password=-`, `rohi config wifi ssid=.. password=-`, `rohi config reset wifi`, then `rohi status wifi` |
| `rohi connectivity set --host ..` | `rohi config connectivity host=..` |
| `rohi config list`, `config describe <section>` | `rohi config`, `rohi config <section> --help` |
| `rohi config get <section>`, `config set <section> ..` | `rohi config <section>`, `rohi config <section> FIELD=VALUE` |
| `rohi config unset <section> <field>` | `rohi config <section> FIELD=null` (removes the override) |
| `rohi action list`, `describe`, `run` | `rohi action`, `rohi action <service> <action> --help`, `rohi action <service> <action>` |
| `rohi backup export` / `import` | `rohi config export` / `apply` |

## Commands

| Command | What it does |
|---------|--------------|
| `scan` | discover nearby devices |
| `status [SERVICE] [--watch]` | device summary or one service |
| `config [SECTION] [FIELD=VALUE...]`, `config reset SECTION` | list, show and change configuration sections |
| `config export`, `diff`, `apply` | configuration files (your overrides) |
| `factory-reset [--yes]` | format the configuration storage, identity included |
| `reboot` | reboot the device |
| `action [SERVICE] [ACTION] [ARG=VALUE...]` | list and run the actions of the services |

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `no ROHI device found` | Bluetooth is on, the device is powered and advertising, no other client holds its connection; raise `--scan-timeout` |
| `timed out waiting for the device` | the device is in range; raise `--timeout` |
| `multiple ROHI devices found` | pass `--device` or set `ROHI_DEVICE`; `rohi scan` lists them |
| `the device has no section …` / `NotFound` | the name is not one the device lists: `rohi config`, `rohi status`, `rohi action` |
| a section shows fewer fields than expected | the tool is older than the firmware; rebuild `rohi` from the same revision |
| `the device refused the request: permission denied` | the change needs an encrypted or authenticated link, see [Security](../sdk/control.md#security) |

## Next

How the services, sections and actions are built is in [Service pipeline](../sdk/service.md); the protocol behind the
tool and the library API are in [Control plane](../sdk/control.md).
