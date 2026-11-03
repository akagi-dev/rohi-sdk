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

//! Derive macro for the service identity of `rohi-service`.
//!
//! # Overview
//!
//! `#[derive(Service)]` implements [`rohi_service::Service`] for a struct or
//! an enum of Robonomics Open Hardware Interface (ROHI) firmware: a stable
//! public `NAME` and a numeric `ID`. The macro only generates that identity;
//! status and actions are separate capability traits
//! ([`rohi_service::Status`], [`rohi_service::Action`]) that a service
//! implements by hand. This crate is a `proc-macro` crate; use the derive
//! through the re-export `rohi_service::Service`, next to the trait it
//! implements.
//!
//! # Quick example
//!
//! ```
//! use rohi_service::Service;
//!
//! #[derive(Service)]
//! pub struct WifiService;
//!
//! #[derive(Service)]
//! #[service(name = "wifi")]
//! pub struct WirelessService;
//!
//! assert_eq!(WifiService::NAME, "wifi");
//! assert_eq!(WifiService::ID, rohi_service::service_id("wifi"));
//! // The id follows the name only, never the Rust type.
//! assert_eq!(WifiService::ID, WirelessService::ID);
//! ```
//!
//! # Core concepts
//!
//! | Generated item | Value |
//! |----------------|-------|
//! | `Service::NAME` | the explicit `#[service(name = "...")]`, else the default name below |
//! | `Service::ID` | [`rohi_service::service_id`]`(NAME)`, the 32-bit FNV-1a hash of the UTF-8 bytes of the name |
//! | compile-time check | [`rohi_service::assert_service_name`]`(NAME)` in a `const` item |
//!
//! The default name is the type name without a trailing `Service`, in
//! `snake_case`: `WifiService` is `wifi`, `TimeSyncService` is `time_sync`,
//! `BLEService` is `ble`. A type that is called just `Service` keeps the name
//! `service`. Acronyms stay together: a new word starts at an upper-case
//! letter that follows a lower-case letter or digit, or that is followed by a
//! lower-case letter after an upper-case one. A raw identifier (`r#Type`) is
//! named like `Type`. `#[service(name = "...")]` replaces the rule; it is the
//! only attribute key and may appear once.
//!
//! The id depends on the name alone, never on the module path, the crate, the
//! type parameters or the platform. The algorithm and the names are wire ABI:
//! requests address a service by this id, so renaming a service changes its
//! id.
//!
//! # Architecture and integration
//!
//! ```text
//! #[derive(Service)] ──► impl Service (NAME, ID)
//!                              │
//!        impl Status / impl Action (by hand, in the feature crate)
//!                              │
//!        Statuses::new((&svc,)) / (&svc,) as ActionRegistry   (rohi-service)
//!                              │
//!        Device::new(..) ──► Controller::handle(..)           (rohi-control)
//! ```
//!
//! The expansion refers to the runtime crate as `::rohi_service` (or under its
//! renamed name in `Cargo.toml`), so a crate using the derive must depend on
//! `rohi-service`. The service guides and the registries are described in the
//! `rohi-service` crate documentation.
//!
//! # API navigation
//!
//! - [`Service`] - the derive macro of this crate.
//! - [`rohi_service::Service`] - the trait it implements, with the id rules.
//! - [`rohi_service::service_id`], [`rohi_service::service_name_is_valid`],
//!   [`rohi_service::MAX_NAME_LEN`]: the id and name rules as `const fn`s.
//!
//! # Guarantees and limitations
//!
//! - A name must match `[a-z][a-z0-9_]*`. The derive checks the characters
//!   while expanding and reports a compile error at the name; the length
//!   limit ([`rohi_service::MAX_NAME_LEN`], 24 bytes) is checked by a
//!   `const` assertion, so both are build errors, never runtime failures.
//! - Structs and enums are supported, unions are rejected. Generic types and
//!   lifetimes are supported: the generated impl keeps the generics and the
//!   identity does not depend on them.
//! - Unknown or duplicate `service` keys are compile errors.
//! - Two services of one firmware must not share an id; the registries of
//!   `rohi-service` and `rohi_control::Device::new` check it.
//!
//! An invalid name does not compile:
//!
//! ```compile_fail
//! use rohi_service::Service;
//!
//! #[derive(Service)]
//! #[service(name = "Wi-Fi")]
//! pub struct WifiService;
//! ```
//!
//! [`rohi_service::Service`]: ../rohi_service/trait.Service.html
//! [`rohi_service::Status`]: ../rohi_service/trait.Status.html
//! [`rohi_service::Action`]: ../rohi_service/trait.Action.html
//! [`rohi_service::service_id`]: ../rohi_service/fn.service_id.html
//! [`rohi_service::service_name_is_valid`]: ../rohi_service/fn.service_name_is_valid.html
//! [`rohi_service::assert_service_name`]: ../rohi_service/fn.assert_service_name.html
//! [`rohi_service::MAX_NAME_LEN`]: ../rohi_service/constant.MAX_NAME_LEN.html

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, LitStr, parse_macro_input};

/// Derives [`rohi_service::Service`](../rohi_service/trait.Service.html) for a struct or an enum.
///
/// `NAME` is the explicit `#[service(name = "...")]` or the type name without
/// a trailing `Service` in `snake_case`, `ID` is `service_id(NAME)`. See the
/// [crate documentation](crate) for the naming rules.
///
/// # Errors
///
/// Compile errors for a union, an invalid name (not `[a-z][a-z0-9_]*`, or
/// longer than `rohi_service::MAX_NAME_LEN`), an unknown `service` key and a
/// duplicate `name`.
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
