# ![image](https://github.com/user-attachments/assets/457108cf-d3f8-4f7b-a78e-32420f34fe75) Altruist Urban Firmware
[![License](https://img.shields.io/badge/License-Apache2.0-blue.svg)](../LICENSE)


> The Altruist Air Quality Sensor is a decentralized device for monitoring air quality, noise, dust, and temperature, contributing to a DePIN network. [Read more...](https://robonomics.network/devices/altruist/)

## Usage

1. Clone from GitHub

```bash
git clone  https://github.com/akagi-dev/rohi-sdk
```

2. Enter the Nix shell or use rustup to install dependencies

```bash
cd rohi-sdk && nix develop
```

3. Connect Altruist via TypeC port

4. Build & upload firmware

```bash
# ESP32-C3
cargo run --release -p altruist-urban --bin altruist-urban-firmware --features esp32c3 --target riscv32imc-unknown-none-elf
# ESP32-C6
cargo run --release -p altruist-urban --bin altruist-urban-firmware --features esp32c6 --target riscv32imac-unknown-none-elf
```

The chip is selected by exactly one of the features `esp32c3` / `esp32c6`; without a feature, or with both, the
`esp-hal` build script rejects the build. `cargo check-esp32c3`, `cargo check-esp32c6`, `cargo build-esp32c3` and
`cargo build-esp32c6` are shortcuts for checking and building.

The runner flashes the per-chip partition table (`partitions-esp32c3.csv` or `partitions-esp32c6.csv`), which reserves the `rohi-cfg` region used by the
[configuration store](https://docs.rs/rohi-config) and the `rohi-log` region of the persistent log (`rohi-logging`): the **last partition of the flash**, after the
`factory` application: 240 KiB at `0x3C4000` on the 4 MB ESP32-C3, 1 MiB at `0x700000` on the 8 MB ESP32-C6. Run the command from the repository root.

The `factory` partition starts at `0x20000` (`0x3A0000` bytes on the ESP32-C3, `0x6E0000` on the ESP32-C6); the
release image is about 1.2 MB on the ESP32-C3, and `scripts/image-fits.sh <chip>` checks that the built image fits. On the ESP32-C3 the 16 KiB at
`0x3C0000..0x3C4000` between `factory` and `rohi-log` are left unused on purpose: the app area ends on a 64 KiB boundary, so that two equal,
64 KiB aligned OTA slots can be carved out of it later (#29, the two-slot OTA itself is not implemented).

The 240 KiB of the ESP32-C3 are the partition, not the journal: two 4 KiB sectors hold the format header, and with the 16 KiB segments that are the
chip default (retention 240 KiB) the log retains 14 full segments, 224 KiB. The ESP32-C6 keeps 1 MiB with 64 KiB segments. The defaults of the chip
are set by the firmware (`altruist_urban::logging`), a user override of `retention_bytes` or `segment_bytes` stays in force.

### Upgrading a device that has persistent logging

Flash with the runner, so that the new partition table is written together with the firmware: it moves `rohi-log` to the end of the flash and resizes it from 256 KiB to
1 MiB on the ESP32-C6 and to 240 KiB on the ESP32-C3. Nothing is migrated: the old log history is lost. The first start finds no format header in the new region and **formats** it, that is, it
erases its sectors (60 on the ESP32-C3, 256 on the ESP32-C6) one at a time (the duration is not measured on hardware yet; other tasks keep running between two erases, and boot records wait in the
bounded queue, so a long format can drop some; the console says `Log storage: no valid format header, the region was formatted`). Only the `rohi-log` region is erased, never the partition table, the application, `rohi-cfg`, NVS or PHY, and later starts find the header and keep the
history. `rohi logs clear` physically erases the whole region the same way. The hardware time of both is not measured yet, see
[Hardware testing](../../docs/validation/hardware-testing.md#persistent-logging).

## Sensors

The firmware detects and runs BMP280/BME280, SCD40/SCD41, SDS011 and the I²S noise microphone concurrently, and logs a
`MeasurementsSnapshot` summary every 30 s.

| Sensor | Bus | Cadence |
|--------|-----|---------|
| BMP280 / BME280 (auto-probed at `0x76` / `0x77`) | I²C | 10 s |
| SCD40 / SCD41 | I²C | 5 s |
| SDS011 (multi-sample duty cycle) | UART | ≈2 min |
| Noise microphone (unweighted dB SPL) | I²S | 5 s window |

See [HAL](https://docs.rs/rohi-hal), [Sensors](https://docs.rs/rohi-hal/latest/rohi_hal/sensors/index.html) and [Boards](https://docs.rs/rohi-hal-esp).

## Wi-Fi

The firmware starts a Wi-Fi station through [`rohi-net`](https://docs.rs/rohi-net). If the `wifi` configuration section has overrides (an SSID, set as the effective `WifiConfig`) they are
applied and it connects automatically (and reconnects with bounded backoff); otherwise the state stays `NotConfigured`.
Wi-Fi can be provisioned over BLE with the `rohi` tool, see below.

## Control plane (BLE)

The firmware advertises the ROHI Control Service (device name `ALTRUIST-URBAN-XXXX`, the last two MAC bytes, unless the `ble` section sets a `name`, read at start) and serves a bounded protobuf
control protocol over BLE: device information, capabilities, sensors, typed configuration and Wi-Fi scan / status /
reconnect / disconnect. Wi-Fi and BLE share one radio runtime (`rohi_net_esp::EspWireless`). Use the desktop client:

```bash
cargo run -p rohi -- scan
cargo run -p rohi -- status sensors
cargo run -p rohi -- config wifi ssid=<SSID> password=-   # asks for the password
```

> **Security:** there is no BLE pairing and no authorization policy in this version, so anyone in radio range can read
> sensors and change the configuration. See [Control plane](https://docs.rs/rohi-control).

See [Control plane (BLE)](https://docs.rs/rohi-control) for the protocol, framing, limits and the `rohi` command line.

## Resource budget

The static RAM budget of the wireless stack (wireless heap, IP stack sockets and TCP buffers, BLE queues) is defined in one
place, [`src/resources.rs`](src/resources.rs), with the rationale and exhaustion behaviour of every constant. See
[Resource limits](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html).

## Source layout

The crate is the portable application of the Altruist Urban plus, with a chip feature, its ESP platform:

| Path | Content |
|------|---------|
| `src/{config,bindings,control,net,connectivity,time,identity,location,reboot}.rs`, `src/tasks/` | the portable application: configuration owner and bindings, control device, Wi-Fi manager, telemetry, NTP time, identity, location, measurement store |
| `src/esp/` (features `esp32c3` / `esp32c6`) | the ESP platform: flash, radio, BLE, sensors, resource budget (`resources.rs`) and the Embassy tasks around the portable owners |
| `src/bin/firmware.rs` | the ESP entry point (binary `altruist-urban-firmware`) (requires a chip feature) |
| `src/host/` (feature `std`) | the host platform: composition root, arguments, TCP and BLE control tasks, simulated sensors; entry point `src/bin/main.rs` (binary `altruist-urban`) |

Without a platform feature only the portable part is built; the `std` platform (`cargo run -p altruist-urban --bin altruist-urban --features std,std-tcp,sensor-simulator`, see [the guide](../../docs/guides/altruist-urban-std.md)) runs it on
a native platform. `cargo check-esp32c3` / `cargo check-esp32c6` build the ESP firmware as before.
