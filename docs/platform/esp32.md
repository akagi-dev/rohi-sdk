# ESP32 platform (C3 and C6)

The SDK supports two chips, selected per build by exactly one Cargo feature. This page lists what is common, what
differs and how chip selection works; the commands are in [Targets and features](../reference/targets-and-features.md).

| | ESP32-C3 | ESP32-C6 |
|---|----------|----------|
| Feature | `esp32c3` | `esp32c6` |
| Rust target | `riscv32imc-unknown-none-elf` | `riscv32imac-unknown-none-elf` |
| Flash (Altruist Urban) | 4 MB, `factory` partition `0x3E0000` | 8 MB, `factory` partition `0x7E0000` |
| Partition table | `partitions-esp32c3.csv` | `partitions-esp32c6.csv` |
| Pin map (Altruist Urban) | verified against the pinout | from the schematic, **not yet verified on hardware** ([Boards](boards.md)) |
| Hardware validation | partial, see [Hardware testing](../validation/hardware-testing.md) | compile-checked and built in CI; behaviour largely unvalidated |

The rest of the stack is identical: the same portable crates, the same firmware source, the same configuration region
(`rohi-cfg`, 16 KiB, two 8 KiB slots) and the same resource constants. Differences live in three places only: the pin
module of the board, the partition table and the chip features of the platform crates.

## Chip selection

- The firmware package has the mutually exclusive features `esp32c3` and `esp32c6`; each forwards to `esp-hal`,
  `esp-println`, `esp-backtrace`, `esp-bootloader-esp-idf`, `esp-rtos` and the `rohi-*-esp` crates.
- Platform crates (`rohi-hal-esp`, `rohi-config-esp`, `rohi-net-esp`) are **empty without a chip feature**, so they stay
  buildable on the host; `rohi-net-esp` rejects both features together with a `compile_error!`.
- The workspace dependencies never select a chip globally: `esp-hal` and friends are pinned in the root `Cargo.toml`,
  chips come from the firmware features. This is why `--all-features` cannot validate both chips: it would enable
  both features at once.

## Runtime on ESP32

Embassy runs on `esp-rtos` (thread-mode executor, 1 MHz `embassy-time` driver), which `esp-radio` requires. Both chips
are single-core RISC-V; Wi-Fi and BLE share one radio ([Wireless runtime](wireless.md)). Flash erase and programming
stall the whole CPU (cache disabled), see [Configuration store](../sdk/config.md#runtime-ownership-altruist-urban).

## Wireless features

`esp-radio` is built with `wifi`, `ble`, `coex`, `esp-alloc` and `unstable`. WPA3-only networks are not supported by the
station configuration currently used ([Networking](../sdk/net.md#out-of-scope)).
