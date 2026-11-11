<!-- Generated from device-matrix.toml by `cargo run -p rohi-devices -- render`. Do not edit. -->

# Support matrix

Reviewed on 2026-11-03, schema version 1. The data is published as [`device-matrix.toml`](https://github.com/akagi-dev/rohi-sdk/blob/master/device-matrix.toml); how to read the statuses is explained in [Reading the matrix](overview.md).

## Boards

| Board | Revision | Chip | ROHI support | Firmware | Upstream documentation |
|-------|----------|------|--------------|----------|------------------------|
| Altruist Urban | ES-ALTRUIST-URB-1, ESP32-C6-WROOM-1-N8 | esp32c6 | **supported** | ROHI altruist-urban (ROHI SDK)<br>altruist-firmware (other project) | upstream [airalab/hardware: Altruist Urban](https://github.com/airalab/hardware/tree/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban) |
| Altruist Urban (legacy) | legacy ESP32-C3 Urban | esp32c3 | **supported** | ROHI altruist-urban (ROHI SDK)<br>altruist-firmware (other project) | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme) |
| Altruist Insight | ESP32-C6 (airalab/hardware, Altruist/Insight) | esp32c6 | **other-firmware-only** | altruist-firmware (other project) | upstream [airalab/hardware: Altruist Insight](https://github.com/airalab/hardware/tree/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Insight) |

- **Altruist Urban** (esp32c6): Outdoor air-quality station: SDS011, BME280 and ICS-43434 microphone. The ROHI C6 pin map is taken from the upstream schematic and is not yet verified on hardware.
- **Altruist Urban (legacy)** (esp32c3): Legacy ESP32-C3 variant of the Urban. The ROHI pin map is checked against the Urban pinout, but airalab/hardware documents only the ESP32-C6 board, so there is no upstream schematic for this variant.
- **Altruist Insight** (esp32c6): Indoor station with a display and an SCD4x CO2 sensor. ROHI has no Insight board module, firmware or display driver; only the independent altruist-firmware targets it.

## Firmware

| Firmware | Maintainer | Chips | Location |
|----------|------------|-------|----------|
| ROHI altruist-urban | ROHI SDK | esp32c3, esp32c6 | [`firmwares/altruist-urban`](https://github.com/akagi-dev/rohi-sdk/tree/master/firmwares/altruist-urban) |
| altruist-firmware | other project | esp32c3, esp32c6 | <https://github.com/airalab/altruist-firmware> |

- **ROHI altruist-urban**: Rust firmware of this repository, selected per chip by the `esp32c3` / `esp32c6` feature.
- **altruist-firmware**: Independent PlatformIO firmware of the Altruist devices with its own feature set; ROHI has not assessed it and its claims are not ROHI support.

## Capabilities per board and firmware

### Altruist Urban with ROHI altruist-urban (esp32c6)

| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |
|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|
| BMP280 / BME280 environment | documented | implemented | integrated | passing | none | ci-built | **not-verified** | source [bmx280.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/bmx280.rs), test [bmx280 tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/bmx280.rs), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C6 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c6.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| SCD40 / SCD41 CO2 | not-on-board | implemented | integrated | passing | none | ci-built | **not-verified** | source [scd4x.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/scd4x.rs), test [scd4x tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/scd4x.rs), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C6 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c6.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md) |
| SDS011 particulate matter | documented | implemented | integrated | passing | none | ci-built | **not-verified** | source [sds011](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/sds011), test [sds011 tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/sds011), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C6 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c6.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| I²S microphone noise level | documented | implemented | integrated | passing | none | ci-built | **not-verified** | source [noise.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/noise.rs), test [noise tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/noise.rs), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C6 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c6.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md), source [Sensors](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/mod.rs) |
| Wi-Fi station and IP networking | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-net](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/net), test [rohi-net tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/net/tests), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Time synchronisation (NTP) | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-time-ntp](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/time-ntp), test [rohi-time-ntp tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/time-ntp/src/tests.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Telemetry (connectivity) | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-connectivity](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/connectivity), test [rohi-connectivity tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/connectivity/tests), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| BLE control plane | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-control-ble](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-ble), test [rohi-control-ble tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-ble/src/tests.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Persistent configuration store | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-config-esp](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/config-esp), test [rohi-config store tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/config/tests/store.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Device identity | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-identity](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/identity), test [rohi-identity tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/identity/src/tests.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Persistent logging | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-logging](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/logging), source [altruist-urban logging task and log backend](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/logging.rs), test [rohi-logging service and storage tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/logging/src/service_tests.rs), test [altruist-urban host process: logging service, file-backed log flash, restart](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/tests/std_logging.rs), test [rohi logs against the real logging service](https://github.com/akagi-dev/rohi-sdk/blob/master/tools/rohi/tests/logs.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |

Limitations of BMP280 / BME280 environment:

- The ESP32-C6 pin map comes from the upstream schematic and is not verified on hardware.
- The driver handles BMP280 (no humidity) and BME280; the Urban documents a BME280.

Limitations of SCD40 / SCD41 CO2:

- The Urban sensor list has no SCD4x; the firmware starts the driver anyway and reports no measurement and an unhealthy sensor state when none answers on the I²C bus.
- Attaching an external SCD4x is not documented upstream and not verified.

Limitations of SDS011 particulate matter:

- The ESP32-C6 pin map comes from the upstream schematic and is not verified on hardware.
- The schematic names the SDS011 nets PM_TX and PM_RX without a viewpoint; swap the pins if the sensor stays silent.
- The SDS011 accuracy is ±10 µg/m³ or ±15 % (upstream datasheet figure).

Limitations of I²S microphone noise level:

- The ESP32-C6 pin map comes from the upstream schematic and is not verified on hardware.
- Unweighted dB SPL (not dBA) from the nominal ICS-43434 sensitivity (-26 dBFS at 94 dB SPL); no per-device calibration, so values include the microphone response and the device's own noise.
- Not comparable to a calibrated, A-weighted sound level meter.

Limitations of Wi-Fi station and IP networking:

- Scan, connect, reconnect, wrong password, heap minimum and 24 h stability are listed as not yet measured on hardware.

Limitations of Time synchronisation (NTP):

- Depends on a working Wi-Fi connection; no hardware run recorded.

Limitations of Telemetry (connectivity):

- Depends on Wi-Fi, time and a provisioned device identity; no hardware run against a connectivity provider is recorded.
- The independent altruist-firmware reports to Robonomics through its own mechanism; the two are not interchangeable.

Limitations of BLE control plane:

- One heap measurement exists (Wi-Fi and BLE running, chip not recorded); advertising, coexistence, MTU and 24 h stability are not measured.
- No BLE pairing or encryption yet.

Limitations of Persistent configuration store:

- The flash self-test (example-config) exists, but no run is recorded.

Limitations of Device identity:

- The hardware checklist (provision, reboot, erase, re-provision) has no recorded results.

Limitations of Persistent logging:

- Only the internal-flash backend exists; the SD backend is not implemented (issue #26 is implemented only in part) and `storage = sd` reports `storage_unavailable`.
- Flash erase and programming stall the whole CPU for tens of milliseconds per sector; the effect on Wi-Fi, BLE and the microphone DMA ring is not measured.
- Persistence across reset, power-loss recovery (including payload checksum validation and the history generation), flash rotation, coexistence with the configuration partition and the transfer speed of `rohi logs download` over BLE are covered by host emulation only; no hardware run is recorded.

### Altruist Urban with altruist-firmware (esp32c6)

| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |
|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|
| BMP280 / BME280 environment | documented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md) |
| SDS011 particulate matter | documented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md) |
| I²S microphone noise level | documented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme), upstream [Altruist Urban sensor specification](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Urban/README.md) |

Limitations of BMP280 / BME280 environment:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.

Limitations of SDS011 particulate matter:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.

Limitations of I²S microphone noise level:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.
- Reports the level in dBA, a different metric from the unweighted dB SPL of ROHI; the values must not be compared.

### Altruist Urban (legacy) with ROHI altruist-urban (esp32c3)

| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |
|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|
| BMP280 / BME280 environment | undocumented | implemented | integrated | passing | none | ci-built | **not-verified** | source [bmx280.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/bmx280.rs), test [bmx280 tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/bmx280.rs), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C3 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c3.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| SCD40 / SCD41 CO2 | undocumented | implemented | integrated | passing | none | ci-built | **not-verified** | source [scd4x.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/scd4x.rs), test [scd4x tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/scd4x.rs), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C3 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c3.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml) |
| SDS011 particulate matter | undocumented | implemented | integrated | passing | none | ci-built | **not-verified** | source [sds011](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/sds011), test [sds011 tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/sds011), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C3 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c3.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| I²S microphone noise level | undocumented | implemented | integrated | passing | none | ci-built | **not-verified** | source [noise.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/noise.rs), test [noise tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/noise.rs), source [firmware sensor tasks](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/sensors.rs), source [C3 board module](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/esp32c3.rs), source [Boards](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal-esp/src/board/altruist_urban/mod.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Sensors](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/mod.rs) |
| Wi-Fi station and IP networking | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-net](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/net), test [rohi-net tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/net/tests), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Time synchronisation (NTP) | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-time-ntp](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/time-ntp), test [rohi-time-ntp tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/time-ntp/src/tests.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Telemetry (connectivity) | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-connectivity](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/connectivity), test [rohi-connectivity tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/connectivity/tests), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| BLE control plane | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-control-ble](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-ble), test [rohi-control-ble tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/control-ble/src/tests.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Persistent configuration store | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-config-esp](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/config-esp), test [rohi-config store tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/config/tests/store.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Device identity | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-identity](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/identity), test [rohi-identity tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/identity/src/tests.rs), source [firmware startup](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/bin/firmware.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |
| Persistent logging | not-applicable | implemented | integrated | passing | none | ci-built | **not-verified** | source [rohi-logging](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/logging), source [altruist-urban logging task and log backend](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/src/esp/logging.rs), test [rohi-logging service and storage tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/logging/src/service_tests.rs), test [altruist-urban host process: logging service, file-backed log flash, restart](https://github.com/akagi-dev/rohi-sdk/blob/master/firmwares/altruist-urban/tests/std_logging.rs), test [rohi logs against the real logging service](https://github.com/akagi-dev/rohi-sdk/blob/master/tools/rohi/tests/logs.rs), ci [CI](https://github.com/akagi-dev/rohi-sdk/blob/master/.github/workflows/ci.yml), source [Hardware testing status](https://github.com/akagi-dev/rohi-sdk/blob/master/docs/validation/hardware-testing.md) |

Limitations of BMP280 / BME280 environment:

- The ESP32-C3 pin map is checked against the Urban pinout, but this variant has no schematic in airalab/hardware and no recorded hardware run.
- The driver handles BMP280 (no humidity) and BME280; the Urban documents a BME280.

Limitations of SCD40 / SCD41 CO2:

- No upstream documentation of the variant's sensors; the firmware starts the SCD4x driver and reports no measurement and an unhealthy sensor state when none answers.

Limitations of SDS011 particulate matter:

- The ESP32-C3 pin map is checked against the Urban pinout, but this variant has no schematic in airalab/hardware and no recorded hardware run.
- The UART wiring was not recorded in a hardware report.
- The SDS011 accuracy is ±10 µg/m³ or ±15 % (upstream datasheet figure).

Limitations of I²S microphone noise level:

- The ESP32-C3 pin map is checked against the Urban pinout, but this variant has no schematic in airalab/hardware and no recorded hardware run.
- Unweighted dB SPL (not dBA) from the nominal ICS-43434 sensitivity (-26 dBFS at 94 dB SPL); no per-device calibration, so values include the microphone response and the device's own noise.
- Not comparable to a calibrated, A-weighted sound level meter.

Limitations of Wi-Fi station and IP networking:

- Scan, connect, reconnect, wrong password, heap minimum and 24 h stability are listed as not yet measured on hardware.

Limitations of Time synchronisation (NTP):

- Depends on a working Wi-Fi connection; no hardware run recorded.

Limitations of Telemetry (connectivity):

- Depends on Wi-Fi, time and a provisioned device identity; no hardware run against a connectivity provider is recorded.
- The independent altruist-firmware reports to Robonomics through its own mechanism; the two are not interchangeable.

Limitations of BLE control plane:

- One heap measurement exists (Wi-Fi and BLE running, chip not recorded); advertising, coexistence, MTU and 24 h stability are not measured.
- No BLE pairing or encryption yet.

Limitations of Persistent configuration store:

- The flash self-test (example-config) exists, but no run is recorded.

Limitations of Device identity:

- The hardware checklist (provision, reboot, erase, re-provision) has no recorded results.

Limitations of Persistent logging:

- Only the internal-flash backend exists; the SD backend is not implemented (issue #26 is implemented only in part) and `storage = sd` reports `storage_unavailable`.
- Flash erase and programming stall the whole CPU for tens of milliseconds per sector; the effect on Wi-Fi, BLE and the microphone DMA ring is not measured.
- Persistence across reset, power-loss recovery (including payload checksum validation and the history generation), flash rotation, coexistence with the configuration partition and the transfer speed of `rohi logs download` over BLE are covered by host emulation only; no hardware run is recorded.

### Altruist Urban (legacy) with altruist-firmware (esp32c3)

| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |
|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|
| BMP280 / BME280 environment | undocumented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme) |
| SDS011 particulate matter | undocumented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme) |
| I²S microphone noise level | undocumented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme) |

Limitations of BMP280 / BME280 environment:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.

Limitations of SDS011 particulate matter:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.

Limitations of I²S microphone noise level:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.
- Reports the level in dBA, a different metric from the unweighted dB SPL of ROHI; the values must not be compared.

### Altruist Insight: no firmware targets this board (esp32c6)

| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |
|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|
| SCD40 / SCD41 CO2 | documented | implemented | not-integrated | passing | none | not-built | **not-verified** | source [scd4x.rs](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/scd4x.rs), test [scd4x tests](https://github.com/akagi-dev/rohi-sdk/blob/master/crates/hal/src/sensors/scd4x.rs), upstream [Insight SCD4x datasheet](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Insight/Docs/Datasheets/CD_DS_SCD4x_Datasheet_D1.pdf) |
| Display | documented | not-implemented | not-integrated | none | none | not-built | **not-verified** | upstream [airalab/hardware: Altruist Insight](https://github.com/airalab/hardware/tree/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Insight) |

Limitations of SCD40 / SCD41 CO2:

- The driver exists, but no ROHI firmware or Insight board module wires it.

Limitations of Display:

- ROHI has no display driver.

### Altruist Insight with altruist-firmware (esp32c6)

| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |
|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|
| SCD40 / SCD41 CO2 | documented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme), upstream [Insight SCD4x datasheet](https://github.com/airalab/hardware/blob/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Insight/Docs/Datasheets/CD_DS_SCD4x_Datasheet_D1.pdf) |
| Display | documented | external | not-applicable | none | none | not-applicable | **not-verified** | upstream [airalab/altruist-firmware README](https://github.com/airalab/altruist-firmware#readme), upstream [airalab/hardware: Altruist Insight](https://github.com/airalab/hardware/tree/73a4d5cd97b85a514f8f8a19c6285d21fb3797e4/Altruist/Insight) |

Limitations of SCD40 / SCD41 CO2:

- Claimed by the altruist-firmware README; ROHI has not built or verified it.

Limitations of Display:

- Claimed by the altruist-firmware README (display and QR code); ROHI has not built or verified it.

## Use cases

| Use case | Verdict | Summary |
|----------|---------|---------|
| Measure CO2 with Altruist Urban | **unsupported** | The Urban board carries no SCD4x. The firmware runs the SCD4x driver and would read a sensor wired to the I²C bus, but no wiring is documented upstream and nothing was verified. |
| Measure environmental noise with ROHI | **partial** | The Urban microphone yields unweighted dB SPL from the nominal ICS-43434 sensitivity. There is no per-device calibration and no A-weighting, so it is not dBA and not comparable to a calibrated sound level meter. |
| Run ROHI on Altruist Insight | **unsupported** | No ROHI firmware or board module exists for the Insight and no display driver exists. The SCD4x driver exists in the SDK, but nothing integrates it on this board. |
| Build a water-quality monitor with existing ROHI firmware | **unsupported** | ROHI has no water-quality sensor driver (pH, turbidity, conductivity, temperature probes), no such board and no firmware. A new device would reuse the SDK services but needs all sensing work. |

Still missing for "Measure CO2 with Altruist Urban":

- An upstream-documented way to attach an SCD4x to the Urban
- A hardware report with an SCD4x on the Urban

Still missing for "Measure environmental noise with ROHI":

- A-weighting (dBA)
- Per-device calibration against a reference meter
- Hardware verification on a board

Still missing for "Run ROHI on Altruist Insight":

- Insight board module in rohi-hal-esp
- Display driver
- Insight firmware
- Hardware verification

Still missing for "Build a water-quality monitor with existing ROHI firmware":

- Sensor drivers
- A board definition and firmware
- Calibration procedures
- Hardware verification
