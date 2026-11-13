# Desktop application


`rohi` is the desktop command line for ROHI devices. It finds a device over Bluetooth LE, shows the status of its
[services](status.md), reads and changes its configuration, and runs its actions. It is the quickest way to check
that a flashed device (see [First device](../../getting-started/first-device.md)) is alive and talking.

`rohi` is a thin layer over the [`Device`](https://github.com/akagi-dev/rohi-sdk/tree/master/tools/rohi) API of the `rohi` library, and the
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
[Targets and features](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html#targets-and-features). Bluetooth must be enabled, and the device must be
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
[Services and status](status.md).

## Commands

| Command | What it does |
|---------|--------------|
| `scan` | discover nearby devices |
| `status [SERVICE] [--watch]` | device summary or one service |
| `config [SECTION] [FIELD=VALUE...]`, `config reset SECTION` | list, show and change configuration sections |
| `config export`, `diff`, `apply` | configuration files (your overrides) |
| `action [SERVICE] [ACTION] [ARG=VALUE...]` | list and run the actions of the services |
| `SERVICE [ACTION] [ARG...]` | the same actions with positional arguments, for example `system reboot` and `system factory_reset` |
| `logs [-n N] [-f]`, `logs download [-d DIR]`, `logs clear` | read, follow, save and clear the persistent device log |

Each command has its own page: [Discovery & connection](discovery.md), [Configuration](configuration.md), [Status & diagnostics](status.md), [Actions](actions.md), [Device log](logs.md) and [Automation](automation.md).

## Migrating from earlier versions

| Before | Now |
|--------|-----|
| `rohi sensors` | `rohi status sensors` |
| `rohi time` | `rohi status time` |
| `rohi wifi` (status) | `rohi status wifi` |
| `rohi wifi scan` | `rohi action wifi scan` |
| `rohi wifi reconnect`, `disconnect` | `rohi action wifi reconnect` / `disconnect` |
| `rohi wifi configure`, `forget` | `rohi config wifi ssid=.. password=-`, `rohi config reset wifi`, then `rohi status wifi` |
| `rohi connectivity set --host ..` | `rohi config connectivity host=..` |
| `rohi config list`, `config describe <section>` | `rohi config`, `rohi config <section> --help` |
| `rohi config get <section>`, `config set <section> ..` | `rohi config <section>`, `rohi config <section> FIELD=VALUE` |
| `rohi config unset <section> <field>` | `rohi config <section> FIELD=null` (removes the override) |
| `rohi action list`, `describe`, `run` | `rohi action`, `rohi action <service> <action> --help`, `rohi action <service> <action>` |
| `rohi backup export` / `import` | `rohi config export` / `apply` |
| `rohi reboot` | `rohi system reboot` |
| `rohi factory-reset [--yes]` | `rohi system factory_reset`: no confirmation prompt, and no automatic reboot (run `rohi system reboot` when `restart_required` says so) |

The two last rows are a breaking change of Control Protocol version 1 (which stays `1`): the dedicated reboot and
factory reset requests were removed from the protocol, so a `rohi` and a firmware built before it do not work together.

