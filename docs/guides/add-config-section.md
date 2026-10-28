# Add a ConfigSection

A persistent configuration section is owned by the **feature crate** that interprets it (for example `rohi-net` owns
`NetworkConfig`), never by `rohi-config`. The stored bytes are a [persistent ABI](../reference/persistent-abi.md); read it
before choosing names and field numbers.

A section has two types. The **typed section** (`UplinkConfig`) is the *effective* configuration the runtime uses:
every field has a value. What is **persisted** is a generated **overrides** struct (`UplinkOverrides`) that holds only
what the user explicitly set. The effective configuration is the **firmware defaults** plus the explicit overrides; the
firmware (the product or build) provides the defaults, the section type does not decide them.

1. **Define the typed section** in the feature crate with bounded fields (`heapless`, fixed arrays) and implement
   `Default` as a portable baseline (the firmware defaults start from it). It needs no `minicbor` derive and is not
   persisted:

   ```rust,ignore
   #[derive(Clone, PartialEq, Eq)]
   pub struct UplinkConfig {
       pub endpoint: Endpoint,
       pub interval_s: u32,
   }

   impl Default for UplinkConfig {
       fn default() -> Self { /* the portable baseline */ }
   }
   ```

   - Secrets: do not derive `Debug`; implement it by hand with redaction (see `WifiPassword`).
   - A field that may have no value is `Option<T>`. Its override has three states: `Value(Some(v))`, an explicit
     `Value(None)` and `Default` (follow the firmware default).
2. **Declare the overrides** next to it with `rohi_config::config_overrides!`:

   ```rust,ignore
   rohi_config::config_overrides! {
       /// Persisted overrides of the uplink section.
       pub struct UplinkOverrides for UplinkConfig, version 1 {
           1 => endpoint: Endpoint,
           2 => interval_s: u32,
       }
   }
   ```

   - Every line maps a stable **field id** (the CBOR map key) to a field of the section type with the same name and
     type; nullable fields are `Option<T>` in both places. Ids are only appended, never reused or renumbered.
   - The macro derives the CBOR map codec, `ConfigSection` and `ConfigOverrides<UplinkConfig>`. The section ID is the
     FNV-1a hash of the **section type name** (`UplinkConfig`): **renaming the type orphans the stored data**.
   - Call it next to the section type: it reads the fields of the type.
3. **Pin the encoding** with a golden test (`assert_eq!` on the exact bytes of an overrides value, `ID` and `VERSION`);
   see `the_cbor_encoding_is_stable` for `WifiOverrides`. Also test the empty map `a0`, the round trip and the
   maximum-size value against `MAX_SECTION_PAYLOAD`.
4. **Validate** in the typed section (`validate()` of the complete effective value), at the boundary that accepts new
   values and again in the consumer. It runs on `firmware defaults + overrides` for every patch, so make "unconfigured" or
   "disabled" a valid state (see `LocationConfig`, valid without a position, and `ConnectivityConfig`, valid while
   disabled).
5. **Use it** from the firmware through `ConfigHandle::{get, set, remove}` of the **overrides** type
   (`get::<UplinkOverrides>()`); the configuration task is the only owner of the store. If the runtime must react to a
   change (as Wi-Fi does), publish the **effective** config (the firmware defaults with `overrides.apply_to(..)`, also
   after a removal) as **desired state** from the owner task right after the commit, with no `.await` in between
   ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)). List the section's defaults provider in
   one place (`defaults.rs` of the firmware) and use it for reads, validation and publication alike.
6. **Expose it to clients** (optional, but this is how users set it): implement `ManagedConfig` for the typed section in
   the same crate (`NAME`, `VERSION`, `FIELDS`, `type Overrides`, `read_field`, `field_is_set`, `set_field`, `set_none` for the optional fields, `validate`;
   see [Service pipeline](../sdk/service.md#config-sections)), implement `SectionStore<UplinkConfig>` (with
   `defaults()`) over the configuration handle in the firmware and add `ManagedSection<C, Store<C>, RawMutex>` to the config
   registry tuple in `control.rs`. The protocol, the client and the `rohi` CLI need **no change**: `rohi config
   <section>` and `rohi config <section> FIELD=VALUE` work at once. `FIELD=none` sets an optional field to no
   value explicitly and `FIELD=null` removes the override of any writable field so that it follows the firmware default
   again. `read_field` returns `None` for a field without a value (an optional field that is none, or a default that is
   not a value, such as the `NaN` coordinates of `LocationConfig`). Mark secrets `secret()`: the registry never calls `read_field` for them, it only
   asks `field_is_set`, so a secret is never copied into a generic value to be discarded; answer `field_is_set` from
   the typed field and never log the secret.
7. **Versioning**: changing the stored layout later means bumping the `version` of `config_overrides!` and, if old data
   must stay readable, a migration through `decode_versioned` (`rohi_config::decode_exact` decodes older layouts
   strictly).
8. **Docs**: add the section to the persistent ABI examples if it introduces a new rule, and note it in
   [Configuration store](../sdk/config.md).

A destructive "erase all" is deliberately not part of `ConfigHandle`; per-section removal must never touch sections of
other features (a future identity).
