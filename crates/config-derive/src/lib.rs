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
///////////////////////////////////////////////////////////////////////////////
//! Derive macros for `rohi-config` configuration sections: `ConfigSection` for
//! persistent sections and `Config` (see below) for managed configs.
//!
//! ```ignore
//! #[derive(minicbor::Encode, minicbor::Decode, minicbor::CborLen, ConfigSection)]
//! #[cbor(array)]
//! #[config(version = 1)]
//! pub struct WifiConfig {
//!     #[n(0)]
//!     pub ssid: WifiSsid,
//! }
//! ```
//!
//! Every production section must declare its CBOR representation explicitly
//! (`#[cbor(array)]` or `#[cbor(map)]`) instead of relying on the `minicbor`
//! derive default, because the encoding is persistent ABI. Pin it with a
//! golden byte test.
//!
//! Supported `#[config(...)]` keys:
//!
//! - `version = N` (required, `u16`): section schema version;
//! - `id = 0x...` (`u64`): explicit section ID, used only to keep an existing ID
//!   across a type rename, to resolve a hash collision or to import a legacy ID;
//! - `decode_versioned = "path::to::fn"`: migration hook with the signature
//!   `fn(version: u16, payload: &[u8]) -> Result<Self, SectionError>`.
//!
//! Unless `id` is given, the section ID is the FNV-1a 64-bit hash of the UTF-8
//! bytes of the Rust struct name (no module or crate path, no `r#` prefix).
//! The algorithm and the struct name are part of the persistent storage ABI:
//! renaming a persisted `ConfigSection` type changes its ID. Types with type
//! or const parameters must give an explicit `id`, since all instantiations
//! would otherwise share the name-derived one. The derive only generates
//! metadata; it never inspects fields, encoding belongs to `minicbor`.

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

/// Derives `rohi_config::ConfigSection`.
///
/// The type must also implement `minicbor::Encode`, `minicbor::Decode` and
/// `minicbor::CborLen`.
#[proc_macro_derive(ConfigSection, attributes(config))]
pub fn derive_config_section(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derives the `rohi_service::Config` implementation and the persisted
/// overrides of a typed config section from one declaration.
///
/// ```ignore
/// #[derive(Clone, Config)]
/// #[config(name = "wifi", overrides = WifiOverrides, version = 1, validate = validate_wifi)]
/// pub struct WifiConfig {
///     #[config(id = 1, with = SsidCodec)]
///     pub ssid: Option<WifiSsid>,
///     #[config(id = 2, secret, with = PasswordCodec)]
///     pub password: Option<WifiPassword>,
///     #[config(id = 3, name = "scan_timeout", unit = Ms, min = 1_000, max = 120_000)]
///     pub scan_timeout_ms: u32,
/// }
/// ```
///
/// On the struct: `name` (the public name of the section) and `overrides` (the
/// name of the generated overrides type) and `version` are required; `id`
/// (explicit section ID, as for `ConfigSection`), `validate = path` (a
/// `fn(&Self) -> Result<(), FieldError>` for the rules across fields; without
/// it every section is valid) and `enums = CONST` (the `&[EnumDescriptor]` the
/// enum fields refer to) are optional.
///
/// Every field needs `#[config(id = N)]`, a stable `u32` that is the CBOR map
/// key of its override and only ever added. Optional keys: `name` (the public
/// name, the identifier by default), `with = Codec` (the `Codec<T>` that
/// converts the type, the standard codec by default), `unit = Ms` (a variant
/// of `Unit`), `min`/`max` (inclusive bounds of a numeric field), `enum_id`,
/// and the flags `secret`, `required` and `read_only`.
///
/// An `Option<T>` field is optional: it can be set to "no value" and the codec
/// converts `T`; `required` keeps it required. A `secret` field must be an
/// `Option` and is only ever asked whether it is set. A `read_only` field is
/// shown to clients but not persisted. The type of a field needs `Clone` and
/// the `minicbor` traits.
///
/// Generated next to the config, with the same visibility: the struct
/// `WifiOverrides`, which holds an `Override<T>` per persisted field with the
/// type taken from the config, its CBOR map codec keyed by the field ids,
/// `Default` (nothing overridden), a `Debug` that redacts secrets,
/// `ConfigSection` (section ID: the FNV-1a hash of the name of the *config*
/// type, never of the overrides), `ConfigOverrides<WifiConfig>` and
/// `HasOverrides for WifiConfig`, and `Config for WifiConfig`. Generic types
/// and structs without named fields are rejected.
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
