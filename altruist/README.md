# ![image](https://github.com/user-attachments/assets/5a6f2c26-36ce-42fb-85f1-b2a17607d5ce) Altruist Firmware
[![License](https://img.shields.io/badge/License-Apache2.0-blue.svg)](LICENSE)

> The Altruist Air Quality Sensor is a decentralized device for monitoring air quality, noise, dust, and temperature, contributing to a DePIN network. [Read more...](https://robonomics.network/devices/altruist/) 

## Features

* Type- & memory-safe by [Rust](https://www.rust-lang.org/)
* Fully async by [Embassy Framework](https://embassy.dev/)
* Secure & decentralized infra by [Robonomics Network](https://robonomics.network/)

## Usage

1. Clone from GitHub

```bash
git clone  https://github.com/akagi-dev/altruist-rs
```

2. Enter nix-shell or use rustup to install dependencies

```bash
cd altruist-rs && nix-shell
```

3. Connect Altruist via TypeC port

4. Build & upload firmware

```bash
cargo run --release
```
