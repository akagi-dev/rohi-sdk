# Hardware testing

What the host tests cannot prove — real radios, flash, buses and long-term stability — is validated on devices. The
test pyramid and what must be covered without hardware is in [Host testing](host-testing.md). This page lists the
hardware checks, their procedure and their **current status**. Anything not listed as measured has **not** been
validated on hardware yet.

## Procedure

Build and flash with the per-chip commands in [Targets and features](../reference/targets-and-features.md); the runner
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
on hardware** (see [Boards](../platform/boards.md)).

## Configuration store

`example-config` verifies persistence: every boot checks what the previous boot stored (typed write + reboot + read,
several sections, replacement preserving unrelated sections, repeated write cycles) and logs `SELFTEST PASS` or
`SELFTEST FAIL`. On the third boot it corrupts the newest slot on purpose and checks that the store reports `Recovered`
and falls back to the previous generation.

## Wi-Fi and IP

**Not yet measured on hardware:** scan, connect/DHCP, reconnect after reboot, wrong password, access point loss,
configuration change while connecting/backing off/connected, DNS and TCP through `NetworkHandle` (no component uses it
yet), BLE + Wi-Fi coexistence, minimum free wireless heap (idle, scanning, connected), 24 h stability. Record results
here together with the final `WIRELESS_HEAP_SIZE`.

## Device identity

The hardware checklist (provision, reboot, erase, re-provision) is in [Device identity](../sdk/identity.md#validation-checklist-hardware); its results are not yet recorded.

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
