<div align="center">
  
![ROHI Logo](./docs/images/robo-hw-logo-white.png#gh-dark-mode-only)
![ROHI Logo](./docs/images/robo-hw-logo-black.png#gh-light-mode-only)
  
# Robonomics Open Hardware Interface SDK


> The SDK repository provides all the components needed to start build IoT devices
> powered by [Robonomics Network](https://robonomics.network/), a Web3 cloud platform
> designed to manage IoT devices and robotics.

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0) [![Crates.io Version](https://img.shields.io/crates/v/rohi-hal)](https://crates.io/crates/hal) [![Documentation](https://docs.rs/rohi-hal/badge.svg)](https://docs.rs/rohi-hal)

</div>

## 🌟 Features

* Type- & memory-safe using [Rust Language](https://www.rust-lang.org/)
* Fully async code powered by [Embassy Framework](https://embassy.dev/)
* Secure & decentralized infrastructure on [Robonomics Network](https://robonomics.network/)

## 🔌 Supported hardware

* Altruist Urban on **ESP32-C3** and **ESP32-C6** (the C6 pin map is not yet verified on hardware).

## 🧰 Main capabilities

* Sensors: BMP280/BME280, SCD40/SCD41, SDS011, I²S noise microphone, typed measurements.
* Power-fail-safe configuration store of typed sections.
* Wi-Fi station and portable DNS/TCP networking.
* Local BLE control plane and the `rohi` desktop client.

## ⚡ Quickstart

```bash
nix develop                  # or install the toolchain from rust-toolchain.toml
cargo test                   # host tests, no hardware needed
cargo check-esp32c3 && cargo check-esp32c6
cargo flash-esp32c3  # flash a board
```

More: [usage examples](examples/src/bin) and the **[ROHI SDK Book](https://rohi.akagi.dev)**.

## 📚 Documentation

* **[ROHI SDK Book](https://rohi.akagi.dev)** — architecture, concepts, guides, reference and validation
  (sources in [`docs/`](docs/README.md)).
* [API documentation](https://docs.rs/rohi-hal) (`cargo doc --no-deps` for the whole workspace).
* [Robonomics Wiki](https://wiki.robonomics.network/), [Espressif Rust](https://docs.esp-rs.org/book/),
  [Embassy Framework](https://embassy.dev/book/)

## 🗺️ Repository map

| Path | Content |
|------|---------|
| [`crates/`](crates) | SDK libraries: `rohi-hal`, `rohi-config`, `rohi-net`, `rohi-control*` and their ESP platform crates |
| [`firmwares/altruist-urban`](firmwares/altruist-urban) | [Altruist Urban](https://robonomics.network/devices/altruist/) firmware |
| [`examples/`](examples/src/bin) | one hardware example per sensor and a configuration self-test |
| [`tools/rohi`](tools/rohi) | BLE client library and command line tool |
| [`docs/`](docs) | the SDK book (mdBook) |
