# Build and test

All commands run from the repository root (inside `nix develop` if you use Nix). The authoritative description of
targets, chip features and why they are never combined is [Targets and features](../reference/targets-and-features.md).

## On the host

```bash
cargo test                 # portable crates, framing, dispatcher, rohi client end to end
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

No board is needed. What the host tests cover and what they cannot is in [Host testing](../validation/host-testing.md).

## For the ESP chips

```bash
cargo check-esp32c3        # ESP32-C3 (riscv32imc-unknown-none-elf)
cargo check-esp32c6        # ESP32-C6 (riscv32imac-unknown-none-elf)
cargo build-esp32c3        # release build
cargo build-esp32c6
```

## Flash and run

Connect the board via USB:

```bash
cargo run --release -p altruist-urban --features esp32c3 --target riscv32imc-unknown-none-elf
# ESP32-C6
cargo run --release -p altruist-urban --features esp32c6 --target riscv32imac-unknown-none-elf
```

The runner flashes the chip's partition table, which reserves the configuration region
([Flash layout](../sdk/config.md#flash-layout-altruist-urban-esp32-c3-4-mb--esp32-c6-8-mb)). Examples are built the same
way with `-p rohi-examples --features esp32c3 --target riscv32imc-unknown-none-elf --bin example-<name>`.

## Documentation

```bash
mdbook build docs          # the book
mdbook test docs           # plain Rust snippets of the book
cargo doc --no-deps        # API reference of the host crates
```

See [Documentation style and policy](../contributing/docs-style.md) for the checks that CI runs.

## Next

[First device](first-device.md) runs a sensor example on hardware; [Hardware testing](../validation/hardware-testing.md)
lists what is validated on devices.
