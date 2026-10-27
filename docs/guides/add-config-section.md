# Add a ConfigSection

A persistent configuration section is owned by the **feature crate** that interprets it (for example `rohi-net` owns
`NetworkConfig`), never by `rohi-config`. The stored bytes are a [persistent ABI](../reference/persistent-abi.md); read it
before choosing names and field numbers.

A section has two types. The **typed section** (`UplinkConfig`) is the *effective* configuration the runtime uses:
`UplinkConfig::default()` is the firmware default and every field has a value. What is **persisted** is a generated
**overrides** struct (`UplinkOverrides`) that holds only what the user explicitly set. The effective configuration is
`C::default()` plus the explicit overrides.

1. **Define the typed section** in the feature crate with bounded fields (`heapless`, fixed arrays) and implement
   `Default` with the firmware defaults. It needs no `minicbor` derive and is not persisted:

   ```rust,ignore
   #[derive(Clone, PartialEq, Eq)]
   pub struct UplinkConfig {
       pub endpoint: Endpoint,
       pub interval_s: u32,
   }

   impl Default for UplinkConfig {
       fn default() -> Self { /* the firmware defaults */ }
   }
   ```

   - Secrets: do not derive `Debug`; implement it by hand with redaction (see `WifiPassword`).
   - A field that may have no value is `Option<T>`; its default (usually `None`) is what removing the override restores.
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
   values and again in the consumer. A section without overrides is inactive and is **not** validated, so the
   defaults need not be a valid configuration (see `LocationConfig`, whose default has no position).
5. **Use it** from the firmware through `ConfigHandle::{get, set, remove}` of the **overrides** type
   (`get::<UplinkOverrides>()`); the configuration task is the only owner of the store. If the runtime must react to a
   change (as Wi-Fi does), publish the **effective** config (`overrides.effective()`) as **desired state** from the
   owner task right after the commit, with no `.await` in between
   ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)); no overrides means inactive.
6. **Expose it to clients** (optional, but this is how users set it): implement `ManagedConfig` for the typed section in
   the same crate (`NAME`, `VERSION`, `FIELDS`, `type Overrides`, `read_field`, `field_is_set`, `set_field`, `validate`;
   see [Service pipeline](../sdk/service.md#config-sections)), implement `SectionStore<UplinkOverrides>` over the
   configuration handle in the firmware and add `ManagedSection<C, Store<C::Overrides>, RawMutex>` to the config
   registry tuple in `control.rs`. The protocol, the client and the `rohi` CLI need **no change**: `rohi config
   <section>` and `rohi config <section> FIELD=VALUE` work at once. `FIELD=null` removes the
   override of any writable field and the field follows the default again. `read_field` returns `None` for a field
   without a value (an optional field whose default is none, or a default that is not a value, such as the `NaN`
   coordinates of `LocationConfig`). Mark secrets `secret()`: the registry never calls `read_field` for them, it only
   asks `field_is_set`, so a secret is never copied into a generic value to be discarded; answer `field_is_set` from
   the typed field and never log the secret.
7. **Versioning**: changing the stored layout later means bumping the `version` of `config_overrides!` and, if old data
   must stay readable, a migration through `decode_versioned` (`rohi_config::decode_exact` decodes older layouts
   strictly).
8. **Docs**: add the section to the persistent ABI examples if it introduces a new rule, and note it in
   [Configuration store](../sdk/config.md).

A destructive "erase all" is deliberately not part of `ConfigHandle`; per-section removal must never touch sections of
other features (a future identity).
