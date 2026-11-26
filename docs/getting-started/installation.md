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
linker, nothing else has to be installed. Intel Mac (`x86_64-darwin`) uses
[`nixpkgs-26.05-darwin`](https://github.com/NixOS/nixpkgs/tree/nixpkgs-26.05-darwin): Nixpkgs 26.11 dropped that
platform, and [`nix-systems/default`](https://github.com/nix-systems/default) no longer lists it. `dbus` is Linux-only
(BLE on macOS uses Core Bluetooth). The `e2e` shell (`edge`) is not built for Intel Mac.

```bash
nix develop
```

Intel Mac (`x86_64-darwin`): macOS Bash is 3.2 and cannot enter `nix develop` directly. From the repository root this is the same shell:

```bash
nix run .
```

If the shell says `command not found: nix` after a multi-user install, Nix is on disk but not on `PATH` (a non-login terminal, or the installer snippet missing from `zshrc`). This is the default daemon profile, not a machine-specific path:

```bash
. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
```

A single-user install uses `~/.nix-profile/etc/profile.d/nix.sh` instead. Put the matching line in `~/.zshrc` so new terminals see `nix`.

If the clone path contains non-ASCII characters, keep build artifacts off that path: `CARGO_TARGET_DIR=/tmp/rohi-target cargo test`. A path like `~/src/rohi-sdk` does not need that.

## Verify the installation

Run the [First device](first-device.md) example, then build and test the SDK as described in [Build and test](build-and-test.md): `cargo test` on the host, and
`cargo check-esp32c3` / `cargo check-esp32c6` for the two chips. Documentation tooling (`mdbook`, link check) is covered
in [Documentation style and policy](../contributing/docs-style.md); the Nix shell provides it.
