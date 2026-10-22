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

//! Derive macro for `rohi-config` configuration sections.
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
