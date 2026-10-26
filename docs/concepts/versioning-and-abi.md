# Versioning and ABI

Several things in the SDK are contracts between independently updated parties — a firmware and the data an older
firmware wrote, a device and its desktop client. They have their own version rules; ordinary Rust API changes follow
semver of the crates.

| Contract | Compatibility rule | Canonical page |
|----------|--------------------|----------------|
| **Persistent storage** (container, section IDs, CBOR payloads) | New firmware reads old data; old firmware preserves unknown sections. Renaming a type, renumbering a field or changing a representation is an ABI break | [Persistent ABI](../reference/persistent-abi.md) |
| **Control protocol** (protobuf messages, `PROTOCOL_VERSION`) | Field numbers and enum values are stable and only ever added; clients discover sections, services and actions with the `List...` requests instead of inferring features from the firmware version; the protocol is under development and `PROTOCOL_VERSION` is not bumped yet | [Control plane](../sdk/control.md#protocol) |
| **BLE framing** (frame header, GATT UUIDs) | Frame `version` byte; incompatible changes bump it | [Control plane](../sdk/control.md#ble-binding) |
| **Rust APIs of the crates** | Semver; `ControlBackend`, the `rohi-service` traits and the backend traits are extension points and change deliberately (see changelogs in commit history) | rustdoc |
| **Partition table / flash layout** | Mirrored by constants in `rohi_config_esp::layout`; a host test keeps both in sync | [Configuration store](../sdk/config.md) |

## Rules of thumb

- Never reuse or renumber a persistent field or protobuf field number; only append.
- Pin every persistent encoding with a golden test; a failing golden test is a decision, not an inconvenience.
- A schema change that old data must survive needs a section version bump and a migration hook
  (`decode_versioned`).
- Version policy is explicit: the default decoder accepts only `stored version == T::VERSION`; CBOR's tolerance of
  unknown fields does not replace it.
- Secrets are write-only on every interface and never printed (Wi-Fi password).
- Update the SDK book in the same change when you touch any of these ([Documentation policy](../contributing/docs-style.md)).
