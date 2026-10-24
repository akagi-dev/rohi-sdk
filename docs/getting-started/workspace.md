# Workspace layout

```text
rohi-sdk/
├── crates/
│   ├── rohi-hal, rohi-hal-esp                 sensors, measurements, runtime / ESP board backend
│   ├── rohi-config, -derive, -esp             configuration store / derive macro / ESP flash backend
│   ├── rohi-net, rohi-net-esp                 Wi-Fi control plane, IP data plane, BLE transport / ESP backend
│   ├── rohi-time, rohi-time-ntp               synchronized wall-clock time and its NTP source
│   ├── rohi-identity                          device identity: Ed25519 section and signer
│   └── rohi-control-proto, -control, -ble     control protocol: schema / dispatcher / BLE binding
├── firmwares/altruist-urban                   product firmware
├── examples                                   one hardware example per sensor, config self-test
├── tools/rohi                                 desktop client library and CLI
└── docs                                       this book (mdBook)
```

The crate responsibilities and the allowed dependency directions are in [Crate map](../concepts/crate-map.md).

## Default members and targets

`cargo test` and `cargo clippy` at the repository root cover the **portable crates and desktop tools** only
(`default-members`). Firmware and examples are built per chip with the aliases from `.cargo/config.toml`; the repository
has no default embedded target. The chip is always selected explicitly by the `esp32c3` or `esp32c6` feature, never
globally. See [Targets and features](../reference/targets-and-features.md).

## Toolchain

`rust-toolchain.toml` pins the toolchain, components and both RISC-V targets. The Nix flake provides the same toolchain
plus `espflash`, `mdbook`, `protoc` and the other tools, see [Installation](installation.md).
