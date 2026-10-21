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
//! #[derive(ConfigSection, Serialize, Deserialize, MaxSize)]
//! #[config(name = "wifi", version = 1)]
//! pub struct WifiConfig { /* bounded fields */ }
//! ```
//!
//! Supported `#[config(...)]` keys:
//!
//! - `version = N` (required, `u16`): section schema version;
//! - `name = "..."`: stable logical name hashed into the section ID;
//! - `id = 0x...` (`u64`): explicit section ID, mutually exclusive with `name`;
//! - `decode_versioned = "path::to::fn"`: migration hook with the signature
//!   `fn(version: u16, payload: &[u8]) -> Result<Self, SectionError>`.
//!
//! The section ID is the FNV-1a 64-bit hash of the UTF-8 bytes of the explicit
//! name, or of the Rust type name when no name is given. The algorithm is part
//! of the persistent storage ABI and must never change.

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
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
/// The type must also implement serde `Serialize` + `Deserialize` and postcard
/// `MaxSize`.
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
    name: Option<(String, Span)>,
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
            } else if meta.path.is_ident("name") {
                if attrs.name.is_some() {
                    return Err(duplicate("name", span));
                }
                let lit: LitStr = meta.value()?.parse()?;
                if lit.value().is_empty() {
                    return Err(syn::Error::new(lit.span(), "`name` must not be empty"));
                }
                attrs.name = Some((lit.value(), lit.span()));
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
            } else {
                return Err(syn::Error::new(
                    span,
                    "unknown `config` attribute, expected one of: `version`, `name`, `id`, \
                     `decode_versioned`",
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

    if let (Some(_), Some((_, id_span))) = (&attrs.name, &attrs.id) {
        return Err(syn::Error::new(
            *id_span,
            "`id` and `name` are mutually exclusive: the explicit `id` already fixes the section ID",
        ));
    }

    let type_name = input.ident.to_string();
    let name = attrs
        .name
        .as_ref()
        .map_or_else(|| type_name.clone(), |(name, _)| name.clone());
    let id = match (&attrs.id, &attrs.name) {
        (Some((id, _)), _) => *id,
        (None, Some((name, _))) => fnv1a64(name.as_bytes()),
        (None, None) => fnv1a64(type_name.as_bytes()),
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
            const NAME: &'static str = #name;
            const ID: #krate::SectionId = #krate::SectionId(#id);
            const VERSION: u16 = #version;
            #decode
        }
    })
}

#[cfg(test)]
mod tests;
