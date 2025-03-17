# Installation

> For blazing-fast dependency installation, use the [Nix package manager](https://nixos.org). 
> If you already have Nix, skip to the [Nix Way](#nix-way) section.

## Prerequisites

Before installing the SDK, ensure you have these dependencies:

**Rust & Cargo**

Use the [rustup.rs](https://rustup.rs/) script for a quick Rust compiler installation.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

All required Rust components are listed in the `rust-toolchain.yml` file. Clone the SDK repository.

```bash
git clone https://github.com/akagi-dev/rohi-sdk && cd rohi-sdk
```

And run `rustup` to install them (remember to add `rustup` to your PATH if you've just installed it).

```bash
rustup toolchain install
```

**Espflash**

This tool is required for flashing ESP hardware. The SDK fully supports firmware preparation and launches the flashing process, but the `espflash` binary must be available in your PATH. For setup details, see the official [installation instructions](https://docs.esp-rs.org/book/tooling/espflash.html).

### Nix Way

<script src="https://asciinema.org/a/9VksaqQ2h0pln6posA04IxCrL.js" id="asciicast-9VksaqQ2h0pln6posA04IxCrL" async="true"></script>

## Build examples

To verify that all dependencies are installed correctly, try building the ROHI examples package.

```bash
cargo build --release -p rohi-examples
```

