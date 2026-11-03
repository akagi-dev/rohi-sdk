# Add a ConfigSection

A configuration section is a typed group of settings that a user can read and change with `rohi config`. It is owned by
the **feature crate** that interprets it. What is stored on the device is only what the user overrides; everything else
follows the firmware default. The stored bytes are a persistent ABI, so read the rules in the
[`rohi-config` rustdoc](https://docs.rs/rohi-config) and the
[versioning rules](https://docs.rs/rohi-sdk/latest/rohi_sdk/versioning/index.html) before choosing names and ids.

1. **Define the typed section** in the feature crate with `#[derive(Config)]`: bounded fields, a `Default` that is the
   portable baseline, a stable numeric id per field, and the public section and field names, units and ranges. One
   declaration generates the stored overrides, the schema clients discover and the typed access. The attributes are
   documented in the [`rohi-config-derive` rustdoc](https://docs.rs/rohi-config-derive) and the model in the
   [`rohi-service` rustdoc](https://docs.rs/rohi-service).
2. **Validate the whole result.** Write the validation for the complete effective configuration, and make "not
   configured" or "disabled" a valid state, because it runs on every change.
3. **Pin the stored encoding** with a golden test on the exact bytes, as the existing sections do (for example the
   Wi-Fi section in `rohi-net`).
4. **Register it in the firmware**: describe the section once as a `Binding` (`firmwares/altruist-urban/src/bindings.rs`:
   the firmware default overrides and the consumer that gets the effective value) and list it in `firmware_configs!`
   (`firmwares/altruist-urban/src/config.rs`). That one list drives registration, boot, commit, removal and factory
   reset; a section that needs a restart is published at boot only.
5. **Use it** in the runtime through the configuration handle. The configuration task is the only owner of the store;
   the runtime receives the effective value as desired state right after each commit (see the
   [cancellation rules](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html#durable-mutation)).
6. **Check it in a client.** `rohi config <section>` and `rohi config <section> FIELD=VALUE` work at once: the
   protocol, the client and the CLI need no change. `FIELD=none` sets an optional field to an explicit no value;
   `FIELD=null` removes the override so the field follows the firmware default again. Secrets are write-only.
7. **Later changes.** Ids are only ever added, never reused or renumbered. Renaming the config type changes the stored
   section id and orphans the stored data unless the id is pinned explicitly. Changing the stored layout means a new
   version and, if old data must stay readable, a migration.
8. **Docs.** Document the section in the rustdoc of its crate and, if a user can set it, in the
   [Configuration](../use/desktop/configuration.md) page.

Per-section removal never touches sections of other features (for example the identity); there is deliberately no
"erase everything" in the configuration handle.
