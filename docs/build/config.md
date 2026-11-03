# Config

**Config** is what a user can set up: typed settings with defaults, validation and persistence. A firmware defines a
section as a Rust struct; clients discover and edit it without knowing it.

## A small example

```rust,ignore
use rohi_config::ConfigOverrides;

#[derive(Clone, PartialEq, Eq, ConfigOverrides)]
#[config(overrides = UplinkOverrides, version = 1)]
pub struct UplinkConfig {
    #[config(id = 1)]
    pub endpoint: Endpoint,
    #[config(id = 2)]
    pub interval_s: u32,
}

impl Default for UplinkConfig {
    fn default() -> Self { /* the portable baseline */ }
}
```

- The struct is the **effective** configuration the runtime uses. `Default` is the firmware baseline.
- Only what the user **overrides** is stored (`UplinkOverrides`, generated). A setting the user never touched keeps
  following the firmware default, also after an update changes it.
- Field ids are stable and only appended: they are the stored layout, a [persistent ABI](../reference/persistent-abi.md).
- `validate()` checks the **whole effective result** of every change, so "unconfigured" must be a valid state.
- The store is transactional and power-fail-safe: a change is stored completely or not at all.

To make the section visible to clients, implement `ManagedConfig` for it (names, field descriptors, typed access) and list
it in the firmware. From then on the section appears in every generic client; for a section named `uplink`, `rohi` shows:

```bash
rohi config uplink                      # effective values
rohi config uplink interval_s=120       # set an override
rohi config uplink interval_s=null      # back to the firmware default
```

The user's view of these operations is [Configuration](../use/desktop/configuration.md).

## Go deeper

| Topic | Page |
|-------|------|
| Step by step | [Add a ConfigSection](../guides/add-config-section.md) |
| The store, transactions, recovery, flash layout | [Configuration store](../sdk/config.md) |
| The section/field model and validation | [Service pipeline](../sdk/service.md) |
| Stored encoding rules | [Persistent ABI](../reference/persistent-abi.md) |
