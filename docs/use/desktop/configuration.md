# Configuration

The firmware defines which settings exist, their types, units, defaults and validation; `rohi` discovers them from the device, so a setting of a new firmware module appears without changing the tool. How a firmware defines a section is in [Config](../../build/config.md).


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

## Configuration files

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

