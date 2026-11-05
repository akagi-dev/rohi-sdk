# Workspace layout

```text
rohi-sdk/
├── crates/
│   ├── rohi-hal, rohi-hal-esp                 sensors, measurements, runtime / ESP board backend
│   ├── rohi-config, -derive, -esp             configuration store / derive macro / ESP flash backend
│   ├── rohi-net, rohi-net-esp                 Wi-Fi control plane, IP data plane, BLE transport / ESP backend
│   ├── rohi-time, rohi-time-ntp               synchronized wall-clock time and its NTP source
│   ├── rohi-identity                          device identity: Ed25519 section and signer
│   ├── rohi-control-proto, -control, -ble     control protocol: schema / dispatcher / BLE binding
│   ├── rohi-control-tcp                       control protocol over TCP: length-prefixed records
│   ├── rohi-net-std                           host DNS, TCP, UDP and the BlueZ BLE peripheral
│   └── rohi-sim                               simulated world for `std`: sensors, Wi-Fi, flash, scenarios
├── firmwares/altruist-urban                   product firmware: portable application + ESP and `std` platforms
├── examples                                   one hardware example per sensor, config self-test
├── tools/rohi                                 desktop client library and CLI
└── docs                                       this book (mdBook)
```

The crate responsibilities and the allowed dependency directions are in [Crate map](https://docs.rs/rohi-sdk).

## Default members and targets

`cargo test` and `cargo clippy` at the repository root cover the **portable crates, desktop tools and the portable part of `altruist-urban`** (`default-members`). The `std` platform and its end-to-end tests need features: `cargo test -p altruist-urban --features std,std-tcp,sensor-simulator`. ESP firmware and examples are built per chip with the aliases from `.cargo/config.toml`; the repository
has no default embedded target. The chip is always selected explicitly by the `esp32c3` or `esp32c6` feature, never
globally. See [Targets and features](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html#targets-and-features).

## Toolchain

`rust-toolchain.toml` pins the toolchain, components and both RISC-V targets. The Nix flake provides the same toolchain
plus `espflash`, `mdbook`, `protoc` and the other tools, see [Installation](installation.md).
