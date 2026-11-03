///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////
//! Derive macros of `rohi-config`: persisted sections and managed configs.
//!
//! # Overview
//!
//! This is a `proc-macro` crate of the Robonomics Open Hardware Interface
//! (ROHI) SDK. It generates metadata only: it never inspects the encoding of a
//! field (that is `minicbor`'s job) and never touches storage. Use the derives
//! through the runtime crates that re-export them, [`ConfigSection`] from
//! `rohi-config` and [`Config`] from `rohi-service`.
//!
//! | Derive | Generates | Used for |
//! |--------|-----------|----------|
//! | [`ConfigSection`] | `rohi_config::ConfigSection`: section ID, version and an optional migration hook | a hand written persisted section such as `Identity` |
//! | [`Config`] | the persisted overrides struct, `ConfigOverrides`, `HasOverrides`, `ConfigSection` of the overrides and `rohi_service::Config` of the typed config | a typed, user configurable section such as `WifiConfig` |
//!
//! # Quick example
//!
//! A hand written section; the store itself is [`rohi_config::ConfigStore`]:
//!
//! ```
//! use minicbor::{CborLen, Decode, Encode};
//! use rohi_config::{ConfigSection, ConfigStore, MemoryStorage, SectionId};
//!
//! #[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
//! #[cbor(array)]
//! #[config(version = 1)]
//! pub struct Counter {
//!     #[n(0)]
//!     pub value: u32,
//! }
//!
//! assert_eq!(Counter::ID, SectionId::from_name("Counter"));
//! assert_eq!(Counter::VERSION, 1);
//!
//! let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
//! store.set(&Counter { value: 7 }).unwrap();
//! assert_eq!(store.get::<Counter>().unwrap(), Some(Counter { value: 7 }));
//! ```
//!
//! A managed config, whose persisted form is the generated overrides struct
//! (see [`Config`] for the attributes and the generated items):
//!
//! ```
//! use rohi_config::{ConfigOverrides, Override, materialize};
//! use rohi_service::Config;
//!
//! #[derive(Clone, Debug, PartialEq, Config)]
//! #[config(name = "sampler", overrides = SamplerOverrides, version = 1)]
//! pub struct SamplerConfig {
//!     #[config(id = 1)]
//!     pub enabled: bool,
//!     #[config(id = 2, name = "interval", unit = S, min = 10, max = 86_400)]
//!     pub interval_s: u32,
//! }
//!
//! impl Default for SamplerConfig {
//!     fn default() -> Self {
//!         Self { enabled: true, interval_s: 60 }
//!     }
//! }
//!
//! // The user overrides one field; everything else follows the default.
//! let user = SamplerOverrides { interval_s: Override::Value(300), ..Default::default() };
//! let effective: SamplerConfig = materialize(&SamplerOverrides::default(), &user);
//! assert_eq!(effective, SamplerConfig { enabled: true, interval_s: 300 });
//! assert_eq!(user.is_overridden(2), Some(true));
//! ```
//!
//! # Core concepts
//!
//! **Hand written section.** The type derives `minicbor::Encode`, `Decode`
//! and `CborLen` with explicit `#[n(..)]` field numbers, plus
//! [`ConfigSection`]. Declare the CBOR representation explicitly
//! (`#[cbor(array)]` or `#[cbor(map)]`) instead of relying on the `minicbor`
//! default, because the encoding is persistent ABI; pin it with a golden byte
//! test. The `#[config(...)]` keys of [`ConfigSection`]:
//!
//! - `version = N` (required, `u16`): section schema version;
//! - `id = N` (`u64`, optional): explicit section ID, used to keep an
//!   existing ID across a type rename, to resolve a hash collision or to
//!   import a legacy ID;
//! - `decode_versioned = "path::to::fn"` (optional): migration hook with the
//!   signature `fn(version: u16, payload: &[u8]) -> Result<Self, SectionError>`,
//!   called instead of the default decoder, which accepts only `version` and
//!   rejects trailing bytes. A hook can migrate older versions of this
//!   section only.
//!
//! **Managed config.** [`Config`] generates everything from one declaration of
//! the fields, so each field's id, name and type are written once. The typed
//! struct is the *effective* configuration and is not persisted; what the
//! store holds is the generated overrides, one `Override<T>` per persisted
//! field. [`rohi_config::materialize`] builds the effective value from
//! `C::default()`, the firmware overrides and the user overrides.
//!
//! **Section IDs.** Unless `id` is given, the section ID is
//! [`rohi_config::SectionId::from_name`] of the Rust struct name: the FNV-1a
//! 64-bit hash of its UTF-8 bytes, without module or crate path and without a
//! `r#` prefix. The algorithm and the struct name are part of the persistent
//! storage ABI, so renaming a persisted type changes its ID. For [`Config`]
//! the ID is the hash of the name of the *config* type (`SamplerConfig`),
//! never of the generated overrides type, and the `name` attribute (the
//! public name in the control protocol) is not hashed.
//!
//! # Architecture and integration
//!
//! ```text
//! #[derive(ConfigSection)] ──► impl rohi_config::ConfigSection      (ID, VERSION)
//!                                      │
//!                         ConfigStore::set / get / remove           (rohi-config)
//!
//! #[derive(Config)] ──► struct XOverrides (CBOR map keyed by field ids)
//!                   ├─► impl ConfigSection, ConfigOverrides<X> for XOverrides
//!                   ├─► impl HasOverrides for X
//!                   └─► impl rohi_service::Config for X            (schema, field access)
//!                                      │
//!                         Section<X, Store, M> ──► ConfigRegistry   (rohi-service)
//!                                      │
//!                         Controller / `rohi config`                (rohi-control)
//! ```
//!
//! The expansion names the runtime crates as `::rohi_config` and
//! `::rohi_service` (or their renamed dependency names), so a crate that
//! derives [`Config`] depends on both `rohi-config` and `rohi-service`; one
//! that derives only [`ConfigSection`] needs `rohi-config`. The persisted
//! layers are in [`rohi_config`](../rohi_config/index.html); how clients see
//! and patch a managed section belongs to
//! [`rohi_service`](../rohi_service/index.html).
//!
//! # API navigation
//!
//! - [`ConfigSection`] - derive for a hand written persisted section.
//! - [`Config`] - derive for a managed config and its overrides.
//! - [`rohi_config::ConfigSection`], [`rohi_config::ConfigOverrides`],
//!   [`rohi_config::HasOverrides`], [`rohi_config::Override`]: the traits and
//!   types the expansions implement.
//! - [`rohi_service::Config`](../rohi_service/trait.Config.html): the trait
//!   `Config` implements.
//!
//! # Guarantees and limitations
//!
//! - All validation is at compile time; invalid declarations are compile
//!   errors with a span at the offending attribute: a missing `version`, an
//!   unknown, duplicate or malformed key, a missing field `id`, duplicate
//!   field ids or names, names outside `[a-z][a-z0-9_]*`, `min` without
//!   `max`, a `secret` or `required` field that is not an `Option`, and
//!   generic or non-named-field structs for [`Config`].
//! - [`ConfigSection`] types with type or const parameters need an explicit
//!   `id`, because all instantiations would otherwise share the name-derived
//!   one. Lifetime parameters are accepted.
//! - Section and field names longer than `rohi_service::MAX_NAME_LEN` fail the
//!   build through a `const` assertion in the generated code.
//! - A section holds at most `rohi_config::MAX_SECTION_PAYLOAD` bytes encoded;
//!   the derive cannot check this, the store reports it at write time.
//! - The rules of section versioning and the container format are persistent
//!   ABI, documented in
//!   [`rohi-config`](https://docs.rs/rohi-config);
//!   the step by step recipe is the
//!   [add a ConfigSection guide](https://rohi.akagi.dev/guides/add-config-section.html).
//!
//! [`rohi_config::ConfigStore`]: ../rohi_config/struct.ConfigStore.html
//! [`rohi_config::materialize`]: ../rohi_config/fn.materialize.html
//! [`rohi_config::SectionId::from_name`]: ../rohi_config/struct.SectionId.html#method.from_name
//! [`rohi_config::ConfigSection`]: ../rohi_config/trait.ConfigSection.html
//! [`rohi_config::ConfigOverrides`]: ../rohi_config/trait.ConfigOverrides.html
//! [`rohi_config::HasOverrides`]: ../rohi_config/trait.HasOverrides.html
//! [`rohi_config::Override`]: ../rohi_config/enum.Override.html

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::{DeriveInput, LitInt, LitStr, Path, parse_macro_input, spanned::Spanned};

mod access;
mod model;
mod overrides;

const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit hash, identical to `rohi_config::SectionId::from_name`.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV64_OFFSET;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

/// Derives [`rohi_config::ConfigSection`](../rohi_config/trait.ConfigSection.html)
/// for a hand written persisted section.
///
/// The type must also implement `minicbor::Encode`, `Decode` and `CborLen`;
/// the encoded size of a section comes from `CborLen`, there is no size
/// attribute.
///
/// Keys of `#[config(...)]`: `version = N` (required, `u16`), `id = N`
/// (optional explicit `u64` section ID) and `decode_versioned = "path"`
/// (optional migration hook). Without `id` the section ID is the FNV-1a
/// 64-bit hash of the struct name.
///
/// ```
/// use minicbor::{CborLen, Decode, Encode};
/// use rohi_config::{ConfigSection, SectionError, SectionId, decode_exact};
///
/// #[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
/// #[cbor(array)]
/// #[config(version = 2, decode_versioned = "decode_counter")]
/// pub struct Counter {
///     #[n(0)]
///     pub value: u32,
///     #[n(1)]
///     pub step: u32,
/// }
///
/// // Version 1 stored only `[value]`.
/// fn decode_counter(version: u16, payload: &[u8]) -> Result<Counter, SectionError> {
///     match version {
///         2 => decode_exact(payload),
///         1 => {
///             let (value,) = decode_exact::<(u32,)>(payload)?;
///             Ok(Counter { value, step: 1 })
///         }
///         _ => Err(SectionError::UnsupportedSectionVersion { id: Counter::ID, version }),
///     }
/// }
///
/// assert_eq!(Counter::ID, SectionId::from_name("Counter"));
/// // [7] as written by version 1.
/// assert_eq!(
///     Counter::decode_versioned(1, &[0x81, 0x07]),
///     Ok(Counter { value: 7, step: 1 })
/// );
/// assert!(Counter::decode_versioned(3, &[]).is_err());
/// ```
///
/// # Errors
///
/// Compile errors for a missing `version`, an unknown or duplicate key,
/// a `version` outside `0..=65535`, a malformed `decode_versioned` path, the
/// unsupported key `name`, and generic types without an explicit `id`.
#[proc_macro_derive(ConfigSection, attributes(config))]
pub fn derive_config_section(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derives the persisted overrides and the
/// [`rohi_service::Config`](../rohi_service/trait.Config.html) implementation
/// of a typed config section from one declaration.
///
/// The struct is the *effective* configuration and is not persisted; its
/// `Default` is the portable baseline. What the store holds is the user's
/// explicit *overrides*, generated from the struct itself.
///
/// ```
/// use rohi_config::{ConfigOverrides, ConfigSection, Override, SectionId, Variant, materialize};
/// use rohi_service::{
///     Config, EnumDescriptor, EnumVariant, FieldError, Value, ValueKind,
/// };
///
/// const MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
///     1,
///     &[EnumVariant::new(0, "off"), EnumVariant::new(1, "eco"), EnumVariant::new(2, "full")],
/// )];
///
/// fn check(config: &PumpConfig) -> Result<(), FieldError> {
///     if config.mode == 2 && config.interval_s < 60 {
///         return Err(FieldError::Invalid);
///     }
///     Ok(())
/// }
///
/// #[derive(Clone, Debug, PartialEq, Config)]
/// #[config(name = "pump", overrides = PumpOverrides, version = 1, validate = check, enums = MODES)]
/// pub struct PumpConfig {
///     #[config(id = 1, with = Variant, enum_id = 1)]
///     pub mode: u32,
///     #[config(id = 2, name = "interval", unit = S, min = 10, max = 3_600)]
///     pub interval_s: u32,
///     #[config(id = 3, secret)]
///     pub pin: Option<u32>,
///     #[config(id = 4, read_only)]
///     pub runs: u32,
/// }
///
/// impl Default for PumpConfig {
///     fn default() -> Self {
///         Self { mode: 1, interval_s: 120, pin: None, runs: 0 }
///     }
/// }
///
/// // The section identity is the hash of the config type name, not of `PumpOverrides`.
/// assert_eq!(<PumpOverrides as ConfigSection>::ID, SectionId::from_name("PumpConfig"));
/// // A read-only field has a descriptor but no override.
/// assert_eq!(PumpConfig::FIELDS.len(), 4);
/// assert_eq!(<PumpOverrides as ConfigOverrides<PumpConfig>>::FIELD_IDS, &[1, 2, 3]);
/// assert_eq!(PumpConfig::FIELDS[1].field.kind, ValueKind::U64);
///
/// // Generic field access, as used by the control plane.
/// let mut config = PumpConfig::default();
/// assert_eq!(config.set_field(2, Value::U64(30)), Ok(()));
/// assert_eq!(config.set_field(4, Value::U64(9)), Err(FieldError::ReadOnly));
/// assert_eq!(config.read_field(3), Err(FieldError::Secret));
/// assert_eq!(config.field_is_set(3), Ok(false));
/// assert_eq!(config.set_none(2), Err(FieldError::NotOptional));
///
/// // Persisted state is the overrides; `materialize` overlays them on the defaults.
/// let mut user = PumpOverrides::default();
/// assert!(user.capture(&config, 2));
/// let effective: PumpConfig = materialize(&PumpOverrides::default(), &user);
/// assert_eq!(effective.interval_s, 30);
/// assert_eq!(effective.mode, 1);
/// assert_eq!(user.is_overridden(1), Some(false));
/// assert_eq!(user.interval_s, Override::Value(30));
/// ```
///
/// # Container attributes
///
/// - `name = "..."` (required): public name of the section in the control
///   protocol, matching `[a-z][a-z0-9_]*`; it is not hashed into the ID;
/// - `overrides = Name` (required): name of the generated overrides type;
/// - `version = N` (required, `u16`): section schema version;
/// - `id = N` (optional, `u64`): explicit section ID, as for
///   [`ConfigSection`]; otherwise the hash of the name of the *config* type;
/// - `validate = path` (optional; a string literal path is accepted too): a
///   `fn(&Self) -> Result<(), FieldError>` for the rules across fields.
///   Without it every section is valid. It is called for every patch, so an
///   unconfigured or disabled section must be a valid state;
/// - `enums = CONST` (optional): the `&[EnumDescriptor]` the enum fields refer
///   to; empty by default.
///
/// # Field attributes
///
/// Every field needs `#[config(id = N)]`, a stable `u32` that is the CBOR map
/// key of its override and is only ever added. Optional keys:
///
/// - `name = "..."`: public field name, the identifier by default; same
///   character rule as the section name;
/// - `with = Codec`: the `rohi_config::Codec<T>` that converts the type to and
///   from a `Value`; `rohi_config::Standard` (booleans, integers, `f32`,
///   `heapless::String`) by default. A domain type uses `Text`, `Variant` or
///   its own codec;
/// - `unit = Name`: a variant of `rohi_service::Unit`;
/// - `min = EXPR`, `max = EXPR`: inclusive bounds of a numeric field, both
///   required together, evaluated against the kind of the codec (`U64`,
///   `I64` or `F32`); on any other kind the generated schema panics at
///   compile time (constant evaluation);
/// - `enum_id = EXPR`: the id of the `EnumDescriptor` of an enum field;
/// - flags `secret`, `required` and `read_only`.
///
/// An `Option<T>` field is optional: it can be set to "no value" and the codec
/// converts `T`; `required` keeps it required. A `secret` field must be an
/// `Option` and is only ever asked whether it is set, its value is never
/// read; a write wipes the copy it was handed. A `read_only` field is shown to
/// clients but not persisted and has no override. `secret` and `read_only`
/// exclude each other. Field types need `Clone` and, for persisted fields, the
/// `minicbor` `Encode`, `Decode` and `CborLen` traits; the config itself needs
/// `Clone` and `Default`, which `rohi_service::Config` requires.
///
/// # Generated items
///
/// Generated next to the config, with its visibility:
///
/// - the struct named by `overrides`, with a public `Override<T>` per
///   persisted field with the type taken from the config (`Override::Default`:
///   the user did not override it, `Override::Value(v)`: explicit). A nullable
///   field is `Override<Option<T>>`, so `Value(None)` differs from `Default`.
///   The doc comments of the fields are copied;
/// - its CBOR codec as a **map** keyed by the field ids. A section without
///   overrides encodes as the empty map and a field appended later reads as
///   `Default` in older data;
/// - `Clone`, `PartialEq`, `Default` (nothing overridden) and a `Debug` that
///   redacts `secret` fields;
/// - `ConfigSection` for the overrides, with the ID and `version` above;
/// - `ConfigOverrides<Config>` (`FIELD_IDS`, `apply_to`, `is_overridden`,
///   `capture`, `clear`, `is_empty`) and `HasOverrides for Config`, which
///   links the config to its overrides;
/// - `rohi_service::Config for Config`: `NAME`, `FIELDS`, `ENUMS`,
///   `read_field`, `field_is_set`, `set_field`, `set_none` and `validate`.
///
/// The generated code only overlays explicit overrides on a base value; it
/// never decides the base. `rohi_config::materialize` is that rule:
/// `C::default()`, then the firmware overrides, then the user overrides.
///
/// # Errors
///
/// Compile errors for: a missing `name`, `overrides` or `version`; unknown,
/// duplicate or malformed keys; generic types, enums, unions, tuple or unit
/// structs and structs without fields; a field without `id`; duplicate field
/// ids or names; names outside `[a-z][a-z0-9_]*`; `min` without `max` or the
/// reverse; `secret` or `required` on a non-`Option` field; `secret` together
/// with `read_only` or `required` with `read_only`; a field type without a
/// codec; and (through the schema checks of `rohi_service`) names longer than
/// `rohi_service::MAX_NAME_LEN`.
#[proc_macro_derive(Config, attributes(config))]
pub fn derive_config(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_config(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand_config(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let model = model::parse(input)?;
    let overrides = overrides::expand(input, &model);
    let access = access::expand(input, &model);
    Ok(quote!(#overrides #access))
}

#[derive(Default)]
struct Attrs {
    version: Option<(u16, Span)>,
    id: Option<(u64, Span)>,
    hook: Option<Path>,
}

fn duplicate(key: &str, span: Span) -> syn::Error {
    syn::Error::new(span, format!("duplicate `{key}` attribute"))
}

fn parse_attrs(input: &DeriveInput) -> syn::Result<Attrs> {
    let mut attrs = Attrs::default();
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("config")) {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            if meta.path.is_ident("version") {
                if attrs.version.is_some() {
                    return Err(duplicate("version", span));
                }
                let lit: LitInt = meta.value()?.parse()?;
                let value = lit.base10_parse::<u16>().map_err(|_| {
                    syn::Error::new(lit.span(), "`version` must be an integer in 0..=65535")
                })?;
                attrs.version = Some((value, lit.span()));
            } else if meta.path.is_ident("id") {
                if attrs.id.is_some() {
                    return Err(duplicate("id", span));
                }
                let lit: LitInt = meta.value()?.parse()?;
                let value = lit.base10_parse::<u64>().map_err(|_| {
                    syn::Error::new(lit.span(), "`id` must be an integer that fits in `u64`")
                })?;
                attrs.id = Some((value, lit.span()));
            } else if meta.path.is_ident("decode_versioned") {
                if attrs.hook.is_some() {
                    return Err(duplicate("decode_versioned", span));
                }
                let lit: LitStr = meta.value()?.parse()?;
                let path = lit.parse::<Path>().map_err(|_| {
                    syn::Error::new(
                        lit.span(),
                        "`decode_versioned` must be a path to a function, e.g. \"decode_wifi\"",
                    )
                })?;
                attrs.hook = Some(path);
            } else if meta.path.is_ident("name") {
                return Err(syn::Error::new(
                    span,
                    "`name` is not supported: the section ID is derived from the struct name, \
                     use `id = 0x...` to keep an ID across a rename",
                ));
            } else {
                return Err(syn::Error::new(
                    span,
                    "unknown `config` attribute, expected one of: `version`, `id`, `decode_versioned`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(attrs)
}

fn crate_path() -> TokenStream2 {
    match crate_name("rohi-config") {
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        _ => quote!(::rohi_config),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let attrs = parse_attrs(input)?;

    let version = match attrs.version {
        Some((version, _)) => version,
        None => {
            return Err(syn::Error::new(
                input.ident.span(),
                "missing `version`, add `#[config(version = 1)]`",
            ));
        }
    };

    // Only the struct identifier is hashed, never the module path: moving a type
    // between modules must not change its persistent identity. A raw
    // identifier (`r#Type`) names the same type as `Type`.
    let id = match attrs.id {
        Some((id, _)) => id,
        None => {
            if let Some(param) = input
                .generics
                .params
                .iter()
                .find(|param| !matches!(param, syn::GenericParam::Lifetime(_)))
            {
                return Err(syn::Error::new(
                    param.span(),
                    "generic section types need an explicit `id = 0x...`: the default ID \
                     hashes only the type name, so every instantiation would share it",
                ));
            }
            fnv1a64(input.ident.unraw().to_string().as_bytes())
        }
    };

    let krate = crate_path();
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let decode = attrs.hook.as_ref().map(|hook| {
        quote! {
            fn decode_versioned(
                version: u16,
                payload: &[u8],
            ) -> ::core::result::Result<Self, #krate::SectionError> {
                #hook(version, payload)
            }
        }
    });

    Ok(quote! {
        impl #impl_generics #krate::ConfigSection for #ident #ty_generics #where_clause {
            const ID: #krate::SectionId = #krate::SectionId(#id);
            const VERSION: u16 = #version;
            #decode
        }
    })
}

#[cfg(test)]
mod tests;
