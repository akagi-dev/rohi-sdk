# Configuration store

Configuration in ROHI is a set of **independent typed sections**. A feature crate defines its own section (for example
`rohi-net` owns the Wi-Fi and network sections) and the store persists it without knowing anything about it; there is
no central `DeviceConfig`. Only the user's explicit **overrides** of the firmware defaults are persisted, so a new
firmware default reaches every device that did not override it.

The store itself is small and hardware independent. Every update is written as a complete new document into the inactive
one of two A/B flash slots, verified, and committed last, so after a power loss at any point either the previous or the
new configuration loads. Corrupt storage is reported as an error and never as an empty store. The store only persists
bytes: how users see and change a section (`rohi config`, atomic patches, secret fields) is the generic layer described
in [Service pipeline](service.md), and the user side is in [Config](../build/config.md).

Three crates cooperate, and in the Altruist Urban firmware a single configuration task owns the store at runtime
([Runtime ownership](../concepts/runtime-ownership.md), [Async and cancellation](../concepts/async-and-cancellation.md)):

| Crate | Role |
|-------|------|
| `rohi-config` | sections (`ConfigSection`), overrides (`Override`, `ConfigOverrides`, `materialize`), `ConfigStore` over a `ConfigStorage`, the CBOR slot format, `MemoryStorage` with fault injection for host tests |
| `rohi-config-derive` | `#[derive(ConfigSection)]` for a hand written section; `#[derive(Config)]` for a managed config and its overrides (re-exported by `rohi-service`) |
| `rohi-config-esp` | `EspConfigStorage` over a bounded ESP32 flash region, the `rohi-cfg` partition lookup, the Altruist Urban flash layout |

The on-flash encoding, section IDs and versioning rules are **persistent ABI** and are documented once in
[Persistent ABI](../reference/persistent-abi.md); flash region and payload limits are in
[Resource limits](../reference/resource-limits.md#configuration-store); errors in [Errors](../reference/errors.md). To
add a section, follow [Add a ConfigSection](../guides/add-config-section.md).

Status: implemented and host-tested, including write and erase faults at every byte position. The persistent store is
**not verified on hardware**; an on-device self-test exists (`examples/src/bin/config.rs`) but no run is recorded, see
the [support matrix](../support/matrix.md).

## Reference

The crate rustdoc is the authoritative technical documentation: the transaction, the open states (`Empty`, `Valid`,
`Recovered`), recovery with `try_open` and `format`, the storage contract, the flash layout and the runtime ownership
of the firmware.

| Crate | docs.rs | Source |
|-------|---------|--------|
| `rohi-config` | <https://docs.rs/rohi-config> | <https://github.com/akagi-dev/rohi-sdk/tree/master/crates/config> |
| `rohi-config-derive` | <https://docs.rs/rohi-config-derive> | <https://github.com/akagi-dev/rohi-sdk/tree/master/crates/config-derive> |
| `rohi-config-esp` | <https://docs.rs/rohi-config-esp> | <https://github.com/akagi-dev/rohi-sdk/tree/master/crates/config-esp> |

SDK-level concepts: <https://docs.rs/rohi-sdk>. Build the rustdoc locally with
`cargo doc --no-deps --open -p rohi-config` (or `-p rohi-config-derive`, `-p rohi-config-esp`).
