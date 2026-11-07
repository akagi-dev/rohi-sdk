# Instructions for AI coding agents

ROHI SDK: Rust libraries, firmware and desktop tools for Robonomics Open Hardware Interface (ROHI) devices
(ESP32-C3 / ESP32-C6). This file is the entry point; the repository-wide onboarding is tracked in #50.

## Build and test

`cargo` may not be on `PATH`: run it as `nix develop --command bash -c '<cargo command>'`.

```bash
cargo test                                     # host: crates/*, tools/* and the portable part of firmwares/altruist-urban (includes doctests)
cargo test -p altruist-urban --features std,std-tcp,sensor-simulator   # the std platform, incl. end-to-end tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo check-esp32c3 && cargo check-esp32c6   # firmware, one chip per invocation
mdbook build docs && mdbook test docs        # the book
cargo run -p altruist-urban --bin altruist-urban --features std,std-tcp,sensor-simulator   # Altruist Urban on std, see docs/guides/altruist-urban-std.md
```

Never use `--all-features` or `--workspace`: `esp32c3` and `esp32c6` are mutually exclusive, and so are `std` and the ESP
features of `altruist-urban`. The std platform uses one reactor (tokio, inside `rohi-net-std`); do not add `async-io` or
`blocking`.
See [Targets and features](crates/sdk/src/portability.rs) (rustdoc of `rohi-sdk`).

## Documentation

- **Writing or changing rustdoc (`//!`, `///`) or doctests: follow the
  [Rustdoc authoring guide](docs/contributing/rustdoc-for-agents.md).** It defines the crate page structure, the
  doctest policy, the agent workflow and the validation commands. Do not invent behaviour, examples or hardware results.
- Where each kind of documentation lives: [Documentation style and policy](docs/contributing/docs-style.md).
- Hardware support and verification evidence: [support matrix](support-matrix/matrix.toml); update it with any change to a
  board, sensor driver, firmware wiring or hardware result.

## Exception safety (no panics in firmware)

Firmware must not panic: `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!` and `unimplemented!` are allowed **in tests only**.
Handle the error instead: log it and degrade (a task that cannot start, a board that cannot initialize), return a typed error,
or park. This is enforced with `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic,
clippy::unreachable, clippy::todo, clippy::unimplemented))]` in `altruist-urban` (ESP and std platform), `rohi-hal-esp`,
`rohi-net-esp`, `rohi-config-esp` and `rohi-examples`; the CI clippy jobs of both chips and the host fail on a violation. Start
Embassy tasks with `altruist_urban::tasks::spawn`, never `spawner.spawn(task().unwrap())`. Code in a new firmware or ESP crate
carries the same attribute.

## Before you finish

- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`, `cargo test`, `cargo clippy --all-targets -- -D warnings` pass.
- Report what you verified and what you could not validate (hardware, chip-only code).
