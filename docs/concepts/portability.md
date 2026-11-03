# Portability

The SDK separates **portable** code from **platform** code so that most of it can be tested on a developer machine and
reused on another MCU. Portable crates are `no_std`, allocation free and contain no MCU HAL; the `*-esp` crates are the
only ones that depend on `esp-hal`, `esp-radio`, `esp-storage`, `embassy-net` or TrouBLE; the firmware picks the chip and
sizes the resources; the host tool `rohi` is a `std` desktop client.

Portability comes from four habits: traits at the seams (sensors, flash storage, radio and IP backends, the control
backend), ROHI types in public APIs (vendor types and errors are converted inside the platform crate), injected time and
clocks, and a single chip feature selected only by the firmware. Portable crates are fully testable on the host,
including timing, reconnects, cancellation and storage faults; platform crates are only compile-checked for both chips
and need hardware for behaviour ([Host testing](../validation/host-testing.md),
[Hardware testing](../validation/hardware-testing.md)). Porting to another MCU means implementing the seam traits in a
new platform crate; the portable crates should need no change.

Which crates are portable and which features select the chip: [Crate map](crate-map.md),
[Targets and features](../reference/targets-and-features.md).

## Reference

- [`rohi_sdk::portability`](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html) and
  [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk).
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
