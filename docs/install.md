# Installation

> For blazing-fast dependency installation, use the [Nix package manager](https://nixos.org).
> If you already have Nix, skip to the [Nix Way](#nix-way) section.

Clone the SDK repository first.

```bash
git clone https://github.com/akagi-dev/rohi-sdk && cd rohi-sdk
```

## Prerequisites

**Rust & Cargo**

Use the [rustup.rs](https://rustup.rs/) script for a quick Rust compiler installation.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The required toolchain, components and both RISC-V targets (`riscv32imc-unknown-none-elf` for the ESP32-C3 and
`riscv32imac-unknown-none-elf` for the ESP32-C6) are listed in `rust-toolchain.toml`.
Run `rustup` inside the repository to install them (add `rustup` to your PATH if you've just installed it).

```bash
rustup toolchain install
```

**Espflash**

This tool is required for flashing ESP hardware. The SDK prepares the firmware and launches the flashing process, but
the `espflash` binary must be available in your PATH. For setup details, see the official
[installation instructions](https://docs.esp-rs.org/book/tooling/espflash.html).

## Nix Way

The repository is a Nix flake. The development shell provides the pinned Rust toolchain, `espflash`, `mdbook` and the
linker, nothing else has to be installed.

```bash
nix develop
```

<script src="https://asciinema.org/a/9VksaqQ2h0pln6posA04IxCrL.js" id="asciicast-9VksaqQ2h0pln6posA04IxCrL" async="true"></script>

## Build examples

To verify that all dependencies are installed correctly, try building the ROHI examples package.

```bash
cargo build --release -p rohi-examples --features esp32c3 --target riscv32imc-unknown-none-elf
```

The repository has no default embedded target: a plain `cargo` command builds for the developer host, and the ESP chip
is always selected explicitly (`esp32c3` with `riscv32imc-unknown-none-elf`, `esp32c6` with
`riscv32imac-unknown-none-elf`).

## Build firmware

The Altruist Urban firmware lives in `firmwares/altruist-urban`.

The firmware is a single package; the chip is selected by the mutually exclusive features `esp32c3` and `esp32c6`.
Use the aliases from `.cargo/config.toml`:

```bash
cargo check-esp32c3
cargo check-esp32c6

cargo build-esp32c3
cargo build-esp32c6
```

Connect the board via USB and flash it with
`cargo run --release -p altruist-urban --features esp32c3 --target riscv32imc-unknown-none-elf` (ESP32-C6:
`--features esp32c6 --target riscv32imac-unknown-none-elf`).

## Run the tests

The hardware independent crates are tested on the host, no board needed:

```bash
cargo test
```

Do not use `--all-features` to validate the chips: it would enable `esp32c3` and `esp32c6` together. Each chip is
checked in its own invocation (`cargo check-esp32c3`, `cargo check-esp32c6`).
