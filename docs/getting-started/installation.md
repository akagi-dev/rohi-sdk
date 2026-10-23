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

## Verify the installation

Build and test the SDK as described in [Build and test](build-and-test.md): `cargo test` on the host, and
`cargo check-esp32c3` / `cargo check-esp32c6` for the two chips. Documentation tooling (`mdbook`, link check) is covered
in [Documentation style and policy](../contributing/docs-style.md); the Nix shell provides it.
