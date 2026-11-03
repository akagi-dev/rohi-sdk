# Instructions for AI coding agents

ROHI SDK: Rust libraries, firmware and desktop tools for Robonomics Open Hardware Interface (ROHI) devices
(ESP32-C3 / ESP32-C6). This file is the entry point; the repository-wide onboarding is tracked in #50.

## Build and test

`cargo` may not be on `PATH`: run it as `nix develop --command bash -c '<cargo command>'`.

```bash
cargo test                                   # host: crates/* and tools/* (includes doctests)
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo check-esp32c3 && cargo check-esp32c6   # firmware, one chip per invocation
mdbook build docs && mdbook test docs        # the book
```

Never use `--all-features` or `--workspace`: `esp32c3` and `esp32c6` are mutually exclusive.
See [Targets and features](crates/sdk/src/portability.rs) (rustdoc of `rohi-sdk`).

## Documentation

- **Writing or changing rustdoc (`//!`, `///`) or doctests: follow the
  [Rustdoc authoring guide](docs/contributing/rustdoc-for-agents.md).** It defines the crate page structure, the
  doctest policy, the agent workflow and the validation commands. Do not invent behaviour, examples or hardware results.
- Where each kind of documentation lives: [Documentation style and policy](docs/contributing/docs-style.md).
- Hardware support and verification evidence: [support matrix](support-matrix/matrix.toml); update it with any change to a
  board, sensor driver, firmware wiring or hardware result.

## Before you finish

- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`, `cargo test`, `cargo clippy --all-targets -- -D warnings` pass.
- Report what you verified and what you could not validate (hardware, chip-only code).
