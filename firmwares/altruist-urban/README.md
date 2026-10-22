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
cargo run --release -p altruist-urban
```

The runner flashes the partition table `partitions.csv`, which reserves the `rohi-cfg` region used by the
[configuration store](../../docs/config-store.md). Run the command from the repository root.

## Sensors

The firmware detects and runs BMP280/BME280, SCD40/SCD41, SDS011 and the I²S noise microphone concurrently, and logs a
`MeasurementsSnapshot` summary every 30 s.

| Sensor | Bus | Cadence |
|--------|-----|---------|
| BMP280 / BME280 (auto-probed at `0x76` / `0x77`) | I²C | 10 s |
| SCD40 / SCD41 | I²C | 5 s |
| SDS011 (multi-sample duty cycle) | UART | ≈2 min |
| Noise microphone (unweighted dB SPL) | I²S | 10 s window |

See [Sensor Core](../../docs/sensor-core.md), [Sensors](../../docs/sensors.md) and [Boards](../../docs/boards.md).

## Wi-Fi

The firmware starts a Wi-Fi station through [`rohi-net`](../../docs/net.md). If a `wifi` configuration section is
stored it connects automatically (and reconnects with bounded backoff); otherwise the state stays `NotConfigured`.
Provisioning over BLE arrives with the control plane (WP4).
