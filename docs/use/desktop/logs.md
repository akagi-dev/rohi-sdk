# Device log

The device keeps a persistent log: what its firmware reports through the standard Rust `log` facade (Wi-Fi came up,
a request timed out, a storage write failed), kept in a bounded ring in internal flash (a file next to the configuration flash for the simulated device), so it survives a reset. `rohi logs`
reads it. Nothing here is a special protocol: the log is the `logging` [service](status.md) with its own status,
configuration section and three [actions](actions.md), and `rohi logs` is a convenience over them.

> **Status.** The behaviour below is covered by host tests, including an emulated flash with power-loss injection, and by
> end-to-end tests of the `altruist-urban` host process. It has not been run on a device: see the
> [support matrix](../../support/overview.md). The SD card backend is **not implemented**: internal flash is the only
> storage, and issue #26 is implemented only in part.

```bash
rohi logs                       # the latest 20 records, oldest first
rohi logs -n 100                # the latest 100
rohi logs -f                    # follow new records like `tail -f` (Ctrl-C to stop)
rohi logs -n 100 -f             # the latest 100, then follow

rohi logs download              # save every retained log file to the current directory
rohi logs download -d ./logs    # ... to another directory
rohi logs clear                 # remove the history of the active storage (asks first)
rohi -o json logs -n 50
```

A record is one line:

```text
42 12345 2026-11-03T05:24:43.221Z WARN wifi: connected to home
```

the sequence number, milliseconds since boot, the wall-clock time in UTC (`-` while the device has no synchronized
time), the level (`*` after it when the message was cut), the target and the message. Sequence numbers only grow within a
*history*, also across resets, so they tell a gap from a quiet device. A history ends when it is cleared or lost to damage;
the next one is identified by a new `history` number in `rohi status logging` and numbers its records from 1 again. A sequence is
a cursor into one history only. `rohi logs -f` notices the change, says so on stderr and shows the new history from its start
instead of waiting for sequences it has already passed. `-o json` prints `{ "schema_version", "records": [..] }`; with `-f` it
prints one such document per line.

## What is kept

`rohi config logging` shows and changes what the device persists. A change applies at once, without a reboot.

| Field | Meaning |
|-------|---------|
| `level` | `off`, `error`, `warn`, `info` or `debug`. The default is `warn` in a release build and `info` in a development build. Only the persistent log is affected, not the serial console |
| `storage` | an enum: `internal` (0, flash; a file on the host) or `sd` (1). `sd` is selectable but this firmware has no card: the service then reports `storage_unavailable` and does **not** fall back to flash |
| `compression` | an enum: `none` (0) or `lz4` (1), for **new** segments only; old segments keep their form |
| `retention_bytes`, `segment_bytes` | the budget of the history and the size at which a file is closed; the oldest files are dropped when the budget is used. The chip sets the defaults, and a value you set stays in force: on the ESP32-C3 the `rohi-log` partition is 240 KiB, so the default is 240 KiB in 16 KiB segments (the partition is not the journal: two 4 KiB sectors hold the format header, so 14 full segments, 224 KiB, are retained); on the ESP32-C6 and the host it is 1 MiB in 64 KiB segments, below the limit of 24 segments. A value you set is kept as it is, and one that is only valid next to the former 8 KiB segment (a budget below 128 KiB) makes the section invalid until you also set `segment_bytes` or remove it |
| `flush_interval` | the longest a record waits in RAM before it is written |

Records are written in small batches and files are compressed one batch at a time, so a power loss costs at most the
records not yet written, never the history. When the queue between the firmware and the log task is full, new records are dropped and
counted, and the application is never slowed down.

`rohi status logging` shows the state: `capacity_bytes`, `used_bytes` and `free_bytes` of the active storage, the number of
retained segments and records, `dropped_records`, the oldest and newest sequence and the `history`. `free_bytes = 0` is normal
for a full ring: the next write replaces the oldest file. At most 24 segments are retained, so with small `segment_bytes` the
count, not `retention_bytes`, limits `capacity_bytes`.

When the device starts, every sealed segment is checked against the checksum stored with it. A segment whose content does
not match is **dropped whole** (the rest of the history stays) and the service reports `storage_corrupt`. A `read` that meets a
damaged segment fails with the reason `segment_corrupt` and the service reports `read_failed` until a read succeeds; a read that
only stops at its record limit is not an error.

## Download

`rohi logs download` saves the retained files exactly as the device keeps them, oldest first: `rohi1.log.lz4`,
`rohi2.log.lz4`, ... (`.log` for segments written with `compression=none`). They are standard LZ4 files of UTF-8 text:

```bash
lz4 -d rohi1.log.lz4            # or: lz4 -dc rohi*.log.lz4 | less
```

The device closes the file being written, starts a new one for the records that come next, and holds the retained files
until the download ends, so records logged meanwhile are not part of it. `rohi` does not overwrite existing files unless you pass
`--force`, and removes a file it could not finish.

## Clear

`rohi logs clear` removes the history of the active storage after you confirm (`--yes` skips the question, a script
must pass it). It does not touch the configuration, the identity or another storage. It **destroys all retained log history**
and physically erases the whole log region of the flash, one sector at a time, not only the headers of the files, so it takes
longer than the other actions (the time on a device is not measured yet). The `history` number grows, the log starts again
empty and keeps working; a reset in the middle of a clear leaves either the old history or an empty one, never a part of it.
A clear is refused as busy while a download holds files.

## First start and upgrades

The log region carries a *format header* in its last two sectors. A region without a valid one (a new device, a partition table that
moved the region over other data, an interrupted format) is erased once, sector by sector, and then the header is written; a region with a valid
header is never erased by a start, whatever else happens to be damaged in it. Only the `rohi-log` partition is formatted. The region
moved to the end of the flash (the last 240 KiB on the ESP32-C3, the last 1 MiB, grown from 256 KiB, on the ESP32-C6): the old history is **not** migrated and the first start of the new firmware formats the new region
(see the [firmware README](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/README.md)). Records logged while it runs wait in a bounded queue.

## Secrets

The log can be downloaded by anyone who can reach the device, so firmware must never log secrets. The Altruist Urban
firmware logs no Wi-Fi passwords, keys or provisioning secrets. Third-party drivers can log at `debug`, which is the highest level the persistent log can be set to: keep it at
`warn` or `info` unless you are diagnosing a problem, and review a download before you share it.
