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

//! `#[derive(Service)]` for `rohi-service`.
//!
//! ```ignore
//! #[derive(Service)]
//! pub struct WifiService { /* ... */ }
//!
//! #[derive(Service)]
//! #[service(name = "wifi")]
//! pub struct WirelessService { /* ... */ }
//! ```
//!
//! The derive implements `rohi_service::Service` for the type: `NAME` is the
//! public service name and `ID` is `rohi_service::service_id(NAME)`, a
//! deterministic hash of the name alone, never of the module path or the
//! platform.
//!
//! The default name is the type name without a trailing `Service`, in
//! `snake_case`: `WifiService` is `wifi`, `TimeSyncService` is `time_sync`,
//! `BLEService` is `ble`. A type that is called just `Service` keeps the name
//! `service`. Acronyms stay together: a new word starts at an upper-case letter
//! that follows a lower-case letter or digit, or that is followed by a
//! lower-case letter after an upper-case one. `#[service(name = "...")]`
//! replaces the rule.
//!
//! A name must match `[a-z][a-z0-9_]*` and fit `rohi_service::MAX_NAME_LEN`
//! bytes. The derive checks the characters while expanding and the type
//! system checks the length, so a bad name is a build error. Generic types are
//! supported; identity never depends on the type parameters.

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, LitStr, parse_macro_input};

/// Derives `rohi_service::Service`.
#[proc_macro_derive(Service, attributes(service))]
pub fn derive_service(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn crate_path() -> TokenStream2 {
    match crate_name("rohi-service") {
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        _ => quote!(::rohi_service),
    }
}

/// The default public name of a service type, see the crate documentation.
fn default_name(type_name: &str) -> String {
    let base = match type_name.strip_suffix("Service") {
        Some(base) if !base.is_empty() => base,
        _ => type_name,
    };
    let chars: Vec<char> = base.chars().collect();
    let mut name = String::new();
    for (index, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && index > 0 {
            let previous = chars[index - 1];
            let next_is_lower = chars.get(index + 1).is_some_and(|n| n.is_ascii_lowercase());
            if previous.is_ascii_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_ascii_uppercase() && next_is_lower)
            {
                name.push('_');
            }
        }
        name.push(c.to_ascii_lowercase());
    }
    name
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn explicit_name(input: &DeriveInput) -> syn::Result<Option<LitStr>> {
    let mut name = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("service")) {
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("name") {
                return Err(meta.error("unknown `service` key, expected `name`"));
            }
            if name.is_some() {
                return Err(meta.error("duplicate `name` attribute"));
            }
            name = Some(meta.value()?.parse::<LitStr>()?);
            Ok(())
        })?;
    }
    Ok(name)
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    if !matches!(input.data, Data::Struct(_) | Data::Enum(_)) {
        return Err(syn::Error::new(
            input.span(),
            "`Service` can only be derived for a struct or an enum",
        ));
    }
    let (name, span) = match explicit_name(input)? {
        Some(lit) => (lit.value(), lit.span()),
        None => (
            default_name(&input.ident.unraw().to_string()),
            input.ident.span(),
        ),
    };
    if !valid_name(&name) {
        return Err(syn::Error::new(
            span,
            format!("invalid service name `{name}`: it must match `[a-z][a-z0-9_]*`"),
        ));
    }

    let krate = crate_path();
    let ident = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #krate::Service for #ident #type_generics #where_clause {
            const NAME: &'static str = #name;
            const ID: u32 = #krate::service_id(#name);
        }

        const _: () = #krate::assert_service_name(#name);
    })
}

#[cfg(test)]
mod tests;
