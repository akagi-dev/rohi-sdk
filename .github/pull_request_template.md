## Summary

## Checklist

- [ ] Tests pass (`cargo test`) and the book builds (`mdbook build docs`).
- [ ] The [support matrix](device-matrix.toml) is updated if the change touches a board, sensor driver, firmware wiring or hardware result (then run `cargo run -p rohi-devices -- render --write`).
- [ ] The SDK book is updated if the change touches public behaviour (see `docs/contributing/docs-style.md`).
- [ ] Public API or crate documentation changes follow the [rustdoc guide](docs/contributing/rustdoc-for-agents.md) (`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`, `cargo test --doc`).
