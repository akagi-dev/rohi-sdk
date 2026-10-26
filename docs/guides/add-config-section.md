# Add a ConfigSection

A persistent configuration section is owned by the **feature crate** that interprets it (for example `rohi-net` owns
`WifiConfig`), never by `rohi-config`. The stored bytes are a [persistent ABI](../reference/persistent-abi.md); read it
before choosing names and field numbers.

1. **Define the type** in the feature crate with bounded fields (`heapless`, fixed arrays) and explicit CBOR layout:

   ```rust,ignore
   use minicbor::{CborLen, Decode, Encode};
   use rohi_config::ConfigSection;

   #[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen, ConfigSection)]
   #[cbor(array)]               // explicit representation: part of the ABI
   #[config(version = 1)]       // required
   pub struct UplinkConfig {
       #[n(0)] pub endpoint: Endpoint,
       #[n(1)] pub interval_s: u32,
   }
   ```

   - The section ID is the FNV-1a hash of the **struct name**. **Renaming the type orphans the stored data**; pick the
     name once, or pin the ID with `#[config(id = 0x..., version = N)]`.
   - Field numbers (`#[n(..)]`) are the schema: only append, never reuse or renumber.
   - Secrets: do not derive `Debug`; implement it by hand with redaction (see `WifiPassword`).
2. **Pin the encoding** with a golden test (`assert_eq!` on the exact bytes, `T::ID` and `T::VERSION`); see
   `cbor_encoding_is_stable` for `WifiConfig`. Also test the round trip and the maximum-size value against
   `MAX_SECTION_PAYLOAD`.
3. **Validate** in the type (`validate()`), at the boundary that accepts new values and again in the consumer.
4. **Use it** from the firmware through `ConfigHandle::{get, set, remove}`; the configuration task is the only owner of
   the store. If the runtime must react to a change (as Wi-Fi does), publish the new value as **desired state** from the
   owner task right after the commit, with no `.await` in between
   ([Durable mutation](../concepts/async-and-cancellation.md#durable-mutation)).
5. **Expose it to clients** (optional, but this is how users set it): implement `ManagedConfig` for the type in the same crate
   (`NAME`, `VERSION`, `FIELDS`, `defaults`, `read_field`, `set_field`, `unset_field`, `validate`; see
   [Service pipeline](../sdk/service.md#config-sections)), implement `SectionStore` over the configuration handle in the
   firmware and add `ManagedSection<C, Store<C>, RawMutex>` to the config registry tuple in `control.rs`. The protocol,
   the client and the `rohi` CLI need **no change**: `rohi config describe/get/set` work at once. Make a `defaults()` that
   is not a valid value when fields must be given together (see `LocationConfig`), mark secrets `secret()`: `read_field` may return the value (the registry
   never sends it to a client), but never log it.
6. **Versioning**: changing the layout later means bumping `version` and, if old data must stay readable, adding a
   `decode_versioned` hook (`rohi_config::decode_exact` decodes older layouts strictly).
7. **Docs**: add the section to the persistent ABI examples if it introduces a new rule, and note it in
   [Configuration store](../sdk/config.md).

A destructive "erase all" is deliberately not part of `ConfigHandle`; per-section removal must never touch sections of
other features (a future identity).
