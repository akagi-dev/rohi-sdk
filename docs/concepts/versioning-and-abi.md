# Versioning and ABI

Several things in the SDK are contracts between independently updated parties: a firmware and the data an older
firmware wrote, a device and its desktop client. Each has its own version rule; ordinary Rust API changes follow semver
of the crates.

| Contract | Rule in short | Authoritative |
|----------|---------------|---------------|
| Persistent storage | new firmware reads old data; renaming, renumbering or changing a representation is an ABI break | [Persistent ABI](../reference/persistent-abi.md) |
| Control protocol | field numbers and enum values only ever added | `rohi-control-proto` rustdoc |
| BLE framing | frame version byte; incompatible changes bump it | `rohi-control-ble` rustdoc |
| Rust APIs | semver | rustdoc of each crate |
| Partition table | constants mirrored in `rohi-config-esp`, kept in sync by a host test | `rohi-config-esp` rustdoc |

Never reuse or renumber a persistent or protobuf field, pin persistent encodings with golden tests, and update the docs
in the same change ([Documentation policy](../contributing/docs-style.md)). The full rules of thumb are in the rustdoc.

## Reference

- [`rohi_sdk::versioning`](https://docs.rs/rohi-sdk/latest/rohi_sdk/versioning/index.html) and
  [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk).
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
