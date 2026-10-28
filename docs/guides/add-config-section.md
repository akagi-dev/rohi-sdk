# Add a ConfigSection

A persistent configuration section is owned by the **feature crate** that interprets it (for example `rohi-net` owns
`NetworkConfig`), never by `rohi-config`. The stored bytes are a [persistent ABI](../reference/persistent-abi.md); read it
before choosing names and field numbers.

A section has two types. The **typed section** (`UplinkConfig`) is the *effective* configuration the runtime uses:
every field has a value. What is **persisted** is a generated **overrides** struct (`UplinkOverrides`) that holds only
what the user explicitly set. The effective configuration is `C::default()`, plus the **firmware overrides** (a value of the same generated type,
compiled into the product, never persisted), plus the user overrides.

1. **Define the typed section** in the feature crate with bounded fields (`heapless`, fixed arrays), implement
   `Default` as a portable baseline, and derive its persisted overrides with `#[derive(ConfigOverrides)]`. The field
   types need `Clone` and the `minicbor` traits (the overrides hold exactly these types); the struct itself is never
   encoded:

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

   - Secrets: do not derive `Debug` on the field types; implement it by hand with redaction (see `WifiPassword`). The
     generated overrides print their values only through the `Debug` of the field types.
   - A field that may have no value is `Option<T>`. Its override has three states: `Value(Some(v))`, an explicit
     `Value(None)` and `Default` (follow the default).
   - Every field needs a stable **id** (the CBOR map key). Ids are only appended, never reused or renumbered.
     Duplicate or missing ids, generic types and structs without named fields are compile errors.
   - The derive generates `UplinkOverrides` (with the visibility of the struct and an `Override<T>` per field), its
     CBOR map codec, `ConfigSection`, `ConfigOverrides<UplinkConfig>` and `HasOverrides for UplinkConfig`. The section
     ID is the FNV-1a hash of the **config type name** (`UplinkConfig`, not `UplinkOverrides`): **renaming the config
     type orphans the stored data**; keep the ID with `#[config(id = 0x...)]` if it must be renamed.
   - The effective configuration is `rohi_config::materialize(&firmware, &user)`: `UplinkConfig::default()`, then the
     firmware overrides, then the user overrides.
2. **Pin the encoding** with a golden test (`assert_eq!` on the exact bytes of an overrides value, `ID` and `VERSION`);
   see `the_cbor_encoding_is_stable` for `WifiOverrides`. Also test the empty map `a0`, the round trip and the
   maximum-size value against `MAX_SECTION_PAYLOAD`.
3. **Validate** in the typed section (`validate()` of the complete effective value), at the boundary that accepts new
   values and again in the consumer. It runs on `default + firmware overrides + user overrides` for every patch, so make "unconfigured" or
   "disabled" a valid state (see `LocationConfig`, valid without a position, and `ConnectivityConfig`, valid while
   disabled).
4. **Use it** from the firmware through `ConfigHandle::{get, set, remove}` of the **overrides** type
   (`get::<UplinkOverrides>()`); the configuration task is the only owner of the store. If the runtime must react to a
   change (as Wi-Fi does), publish the **effective** config (`materialize(&firmware, &user)`, also
   after a removal) as **desired state** from the owner task right after the commit, with no `.await` in between
   ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)). Describe the section once as a firmware `Binding`
   (`bindings.rs`: the firmware overrides and the consumer to publish to) and list it in `firmware_configs!`
   (`config.rs`); that one list drives the control-plane registration, boot, commit, removal and factory-reset
   publication, and a restart-required section (`RESTART_REQUIRED`) is published at boot only.
5. **Expose it to clients** (optional, but this is how users set it): implement `ManagedConfig` for the typed section in
   the same crate (`NAME`, `FIELDS`, `read_field`, `field_is_set`, `set_field`, `set_none` for the optional fields, `validate`;
   see [Service pipeline](../sdk/service.md#config-sections)), implement `SectionStore<UplinkConfig>` (with
   `firmware_overrides()`) over the configuration handle in the firmware (`Stored<YourBinding>`, generated into the config registry by
   `firmware_configs!`). The protocol, the client and the `rohi` CLI need **no change**: `rohi config
   <section>` and `rohi config <section> FIELD=VALUE` work at once. `FIELD=none` sets an optional field to no
   value explicitly and `FIELD=null` removes the override of any writable field so that it follows the firmware override or default
   again. `read_field` returns `None` for a field without a value (an optional field that is none, or a default that is
   not a value, such as the `NaN` coordinates of `LocationConfig`). Mark secrets `secret()`: the registry never calls `read_field` for them, it only
   asks `field_is_set`, so a secret is never copied into a generic value to be discarded; answer `field_is_set` from
   the typed field and never log the secret.
6. **Versioning**: changing the stored layout later means bumping the `version` of the `#[config(overrides = .., version = ..)]` attribute and, if old data
   must stay readable, a migration through `decode_versioned` (`rohi_config::decode_exact` decodes older layouts
   strictly).
7. **Docs**: add the section to the persistent ABI examples if it introduces a new rule, and note it in
   [Configuration store](../sdk/config.md).

A destructive "erase all" is deliberately not part of `ConfigHandle`; per-section removal must never touch sections of
other features (a future identity).
