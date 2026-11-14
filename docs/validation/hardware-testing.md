# Hardware testing

What the host tests cannot prove — real radios, flash, buses and long-term stability — is validated on devices. The
test pyramid and what must be covered without hardware is in [Host testing](host-testing.md). This page lists the
hardware checks, their procedure and their **current status**. Anything not listed as measured has **not** been
validated on hardware yet. The per-board, per-capability status and evidence is in the [support matrix](../support/overview.md).

## Procedure

Build and flash with the per-chip commands in [Targets and features](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html#targets-and-features); the runner
flashes the chip's partition table. Examples (one per sensor and a configuration self-test) live in `examples/` and print
typed results; the firmware logs a snapshot summary every 30 s.

```bash
EXAMPLE="cargo run --release -p rohi-examples --features esp32c3 --target riscv32imc-unknown-none-elf"
$EXAMPLE --bin example-bmx280
$EXAMPLE --bin example-scd4x
$EXAMPLE --bin example-sds011
$EXAMPLE --bin example-noise
$EXAMPLE --bin example-all-sensors
$EXAMPLE --bin example-config        # configuration persistence self-test
```

(ESP32-C6: `--features esp32c6 --target riscv32imac-unknown-none-elf`.)

## Sensors

Combined test checklist: BMx280 keeps updating; SCD4x keeps updating; SDS011 completes its duty cycle and sleeps; noise
windows keep arriving; no sensor starves another. Hot-reconnect check: unplug and replug an I²C sensor and confirm
`Error → Initializing → Ready` without a reboot; repeat for the SDS011 UART. The ESP32-C6 pin map is **not yet verified
on hardware** (see [Boards](https://docs.rs/rohi-hal-esp)).

## Configuration store

`example-config` verifies persistence: every boot checks what the previous boot stored (typed write + reboot + read,
several sections, replacement preserving unrelated sections, repeated write cycles) and logs `SELFTEST PASS` or
`SELFTEST FAIL`. On the third boot it corrupts the newest slot on purpose and checks that the store reports `Recovered`
and falls back to the previous generation.

## Persistent logging

**Not validated on hardware.** The log ring is host-tested on an emulated NOR flash (4-byte write words, a word programmed at
most twice, power-loss injected at every write) and on a RAM medium with power loss injected at every write and every
sector erase of a format and of a clear. The log lives in the last partition of the flash (`rohi-log`: 256 KiB at `0x3C0000` on ESP32-C3,
1 MiB at `0x700000` on ESP32-C6), see the [firmware README](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/README.md). Checklist for a device
(record the results here; the issue is #67):

0. **Flash and first boot (ESP32-C3):** `cargo flash-esp32c3` from a device that ran the 256 KiB layout, so that the partition table and the
   firmware are rewritten. Read the partition table back (`espflash read-flash 0x8000 0xC00 pt.bin` and `espflash partition-table pt.bin`) and confirm `factory` at `0x20000` (`0x3A0000` bytes) and `rohi-log` at `0x3C0000`
   (`0x40000` bytes). The console shows `No valid format header, the region was formatted` (target `logging`) once and no panic; measure
   the time from boot to `Running` of `rohi status logging`, and watch Wi-Fi, BLE and the sensors meanwhile. Reset: no second format message, the
   earlier records and the `history` number are still there. Record whether boot records were dropped (`dropped_records`).
1. `rohi logs` shows boot records; reboot (`rohi system reboot`) and confirm the earlier records are still there and the sequence continues.
2. Pull power while `rohi config logging level=debug` is active and the device logs heavily; after power-up `rohi logs`
   shows a contiguous history, `rohi status logging` has no `storage_corrupt` issue or only the one for the cut segment.
   Repeat with the power cut during the first format (erase a fresh board with `espflash erase-flash` first, then flash and cut
   power a few seconds after boot) and during `rohi logs clear`: the next start must finish the work and no old record may come back.
3. `rohi logs download -d DIR` while Wi-Fi is connected: all files decode with `lz4 -d`, and record the time it takes over BLE (16 KiB segments on ESP32-C3, 64 KiB on ESP32-C6).
4. Watch Wi-Fi, BLE and the microphone (noise windows keep arriving) while the log fills its budget (256 KiB on ESP32-C3, 1 MiB on ESP32-C6) and the oldest segments are erased: measure
   how long a flush stalls the CPU.
5. Fill beyond the retention, confirm `dropped_records` stays 0 at the default `warn` level in normal operation.
6. `rohi logs clear --yes` with a full log: record how long it takes (it must stay below the 40 s action timeout of the client, also over
   BLE), that Wi-Fi/BLE/sensors keep running meanwhile, that the status shows an empty log and a new `history`, and that logging continues after a reset.

## Wi-Fi and IP

**Not yet measured on hardware:** scan, connect/DHCP, reconnect after reboot, wrong password, access point loss,
configuration change while connecting/backing off/connected, DNS and TCP through `NetworkHandle` (no component uses it
yet), BLE + Wi-Fi coexistence, minimum free wireless heap (idle, scanning, connected), 24 h stability. Record results
here together with the final `WIRELESS_HEAP_SIZE`.

## Device identity

The hardware checklist (provision, reboot, erase, re-provision) is in the [`rohi-identity` rustdoc](https://docs.rs/rohi-identity) (see also [Device identity](https://docs.rs/rohi-identity)); its results are not yet recorded.

## BLE control

**First hardware data point** (96 KiB heap, Wi-Fi + BLE running, chip not recorded): 14 656 B free, 11 120 B minimum
free, i.e. about 87 KB peak use, too little margin, hence 128 KiB. Re-measure the minimum free with the larger heap
during BLE connected + Wi-Fi scan/reconnect.

**Not yet measured:** BLE advertising with Wi-Fi disconnected and connected, BLE connected with Wi-Fi connected /
scanning / reconnecting, minimum free wireless heap for each, MTU negotiation, and a 24 h (preferably 72 h) stability
run exercising BLE connect/disconnect, sensor reads, config reads and updates, Wi-Fi status, scan and reconnect.
Expected outcome: no panic, no allocator exhaustion, no deadlock, no permanent BLE or Wi-Fi failure.

## Control plane stack

The stack high-water mark of the control path (`rohi status` with Wi-Fi and identity configured and all services active,
config and action requests, factory reset) is **not yet measured**. The procedure, the scenarios and the table for the
results are in [Resource budget](resource-budget.md#measuring-the-control-task).

Static evidence (task pools, main-stack allocation, function frames) comes from `scripts/memcheck.sh` and is not a
measurement of stack use. A measured peak is attached to its report as a hardware observation, see
[Task memory and stack budget](resource-budget.md#task-memory-and-stack-budget-memcheck).

## Sensor stability test

Run `altruist-urban` without networking for at least 24 h (preferably 72 h) with all sensors attached and record the
following. The table is filled in after each hardware run:

| Item | Result |
|------|--------|
| Duration | |
| Unexpected reboots / panics | |
| Sensor errors per sensor | |
| Successful recoveries | |
| `dropped events` (log) | |
| Task stalls (snapshots stop updating) | |
| Stale measurements | |
| Memory stability | |
