# Config

**Config** is what a user can set up: typed settings with defaults, validation and persistence. A firmware defines a
section as a Rust struct; clients discover and edit it without knowing it.

- The struct is the **effective** configuration the runtime uses; its default is the firmware baseline.
- Only what the user **overrides** is stored. A setting the user never touched keeps following the firmware default,
  also after an update changes it.
- Field ids are stable and only appended: they are the stored layout, a persistent ABI.
- The whole effective result of every change is validated, so "unconfigured" must be a valid state.
- The store is transactional and power-fail-safe: a change is stored completely or not at all.

Once the firmware lists a section, it appears in every generic client; for a section named `uplink`, `rohi` shows:

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
| The store, transactions, recovery, flash layout | [`rohi-config` rustdoc](https://docs.rs/rohi-config) |
| The section and field model, validation | [`rohi-service` rustdoc](https://docs.rs/rohi-service) |
