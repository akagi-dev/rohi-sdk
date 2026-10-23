# Targets and features

The single reference for what the SDK builds for, and how.

## Targets

| Target | Chip | Cargo feature | Rust target | Commands |
|--------|------|---------------|-------------|----------|
| Host | developer machine | – | the host triple | `cargo test`, `cargo clippy`, `cargo doc` |
| ESP32-C3 | ESP32-C3 | `esp32c3` | `riscv32imc-unknown-none-elf` | `cargo check-esp32c3`, `cargo build-esp32c3` |
| ESP32-C6 | ESP32-C6 | `esp32c6` | `riscv32imac-unknown-none-elf` | `cargo check-esp32c6`, `cargo build-esp32c6` |

```bash
cargo test                # host: portable crates and desktop tools (default-members)
cargo check-esp32c3
cargo check-esp32c6
cargo build-esp32c3
cargo build-esp32c6
```

The `check-*` / `build-*` aliases are defined in `.cargo/config.toml` and expand to
`cargo [check|build --release] -p altruist-urban --no-default-features --features <chip> --target <triple>`. Examples:
`cargo check -p rohi-examples --no-default-features --features <chip> --target <triple>`. CI additionally runs
`cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` on the host and clippy for the ESP packages per
chip.

## One chip at a time

**Never validate with `--all-features`.** `esp32c3` and `esp32c6` are mutually exclusive: the firmware features forward
to chip-specific `esp-hal` features, and the build scripts / `compile_error!`s reject both (or none) at once. Check each
chip in its own invocation. The repository has no default embedded target: a plain `cargo` command builds for the host
and the chip is always explicit.

## Host default members

`cargo test` and `cargo clippy` at the root run on `default-members`: `crates/*` and `tools/*`. The platform crates
compile on the host without a chip feature (they are empty), so they are covered; firmware and examples are built per
chip only.

## Feature flags of the SDK crates

| Crate | Feature | Meaning |
|-------|---------|---------|
| `altruist-urban` | `esp32c3` / `esp32c6` | chip selection, forwarded to every `*-esp` crate and `esp-*` dependency |
| `rohi-hal-esp`, `rohi-config-esp`, `rohi-net-esp` | `esp32c3` / `esp32c6` | chip selection; no feature = empty crate |
| `rohi-net-esp` | `heap-stats` | track the minimum free wireless heap (small runtime overhead); enabled by the firmware |
| `rohi-net` | `testing` | in-memory `IpBackend` (`rohi_net::testing`) for host tests; not for firmware |

## Network stack features

`embassy-net` is built with **`tcp`, `dns`, `dhcpv4`, `medium-ethernet`** (the Ethernet medium is what `esp-radio` uses).
UDP is not enabled: no current component uses it.

> **Policy.** Platform and network protocol features are enabled only when a current SDK component uses them. A work item
> that needs another feature (UDP, IPv6, mDNS, ...) enables it together with the component and documents why.

## Toolchain

`rust-toolchain.toml` pins the Rust toolchain, `rust-src`, `rustfmt`, `clippy` and both RISC-V targets; CI uses the same
toolchain. `protoc` is needed to build `rohi-control-proto`, `libdbus` and `pkg-config` to build `tools/rohi` on Linux;
the Nix development shell provides all of them.
