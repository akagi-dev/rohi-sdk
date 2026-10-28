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

//! `#[derive(ConfigOverrides)]`: the persisted per-field overrides of a typed
//! config section, generated from the config struct itself.

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Ident, LitInt, LitStr, Visibility};

use crate::{crate_path, duplicate, fnv1a64};

struct Container {
    overrides: Ident,
    version: u16,
    id: Option<u64>,
}

fn parse_container(input: &DeriveInput) -> syn::Result<Container> {
    let mut overrides: Option<Ident> = None;
    let mut version: Option<u16> = None;
    let mut id: Option<u64> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("config")) {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            if meta.path.is_ident("overrides") {
                if overrides.is_some() {
                    return Err(duplicate("overrides", span));
                }
                overrides = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("version") {
                if version.is_some() {
                    return Err(duplicate("version", span));
                }
                let lit: LitInt = meta.value()?.parse()?;
                version = Some(lit.base10_parse::<u16>().map_err(|_| {
                    syn::Error::new(lit.span(), "`version` must be an integer in 0..=65535")
                })?);
            } else if meta.path.is_ident("id") {
                if id.is_some() {
                    return Err(duplicate("id", span));
                }
                let lit: LitInt = meta.value()?.parse()?;
                id = Some(lit.base10_parse::<u64>().map_err(|_| {
                    syn::Error::new(lit.span(), "`id` must be an integer that fits in `u64`")
                })?);
            } else {
                return Err(syn::Error::new(
                    span,
                    "unknown `config` attribute, expected one of: `overrides`, `version`, `id`",
                ));
            }
            Ok(())
        })?;
    }
    let overrides = overrides.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "missing `overrides`, add `#[config(overrides = MyOverrides, version = 1)]`",
        )
    })?;
    let version = version.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "missing `version`, add `#[config(overrides = MyOverrides, version = 1)]`",
        )
    })?;
    Ok(Container {
        overrides,
        version,
        id,
    })
}

struct Field<'a> {
    ident: &'a Ident,
    ty: &'a syn::Type,
    id: u32,
    docs: Vec<&'a syn::Attribute>,
}

fn parse_field(field: &syn::Field) -> syn::Result<Field<'_>> {
    let ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new(field.span(), "fields must be named"))?;
    let mut id: Option<u32> = None;
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("config")) {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            if meta.path.is_ident("id") {
                if id.is_some() {
                    return Err(duplicate("id", span));
                }
                let lit: LitInt = meta.value()?.parse()?;
                id = Some(lit.base10_parse::<u32>().map_err(|_| {
                    syn::Error::new(
                        lit.span(),
                        "field `id` must be an integer that fits in `u32`",
                    )
                })?);
            } else {
                return Err(syn::Error::new(
                    span,
                    "unknown field `config` attribute, expected `id`",
                ));
            }
            Ok(())
        })?;
    }
    let id = id.ok_or_else(|| {
        syn::Error::new(
            ident.span(),
            format!("field `{ident}` needs a stable id, add `#[config(id = N)]`"),
        )
    })?;
    Ok(Field {
        ident,
        ty: &field.ty,
        id,
        docs: field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc"))
            .collect(),
    })
}

pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let container = parse_container(input)?;

    if let Some(param) = input.generics.params.first() {
        return Err(syn::Error::new(
            param.span(),
            "a config with overrides cannot be generic: the overrides type and the section \
             identity belong to exactly one type",
        ));
    }

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "`ConfigOverrides` can only be derived for structs with named fields",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new(
            input.ident.span(),
            "`ConfigOverrides` needs named fields",
        ));
    };
    let fields = named
        .named
        .iter()
        .map(parse_field)
        .collect::<syn::Result<Vec<_>>>()?;
    if fields.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "a config with overrides needs at least one field",
        ));
    }
    let mut seen = HashSet::new();
    for field in &fields {
        if !seen.insert(field.id) {
            return Err(syn::Error::new(
                field.ident.span(),
                format!("duplicate field id {}", field.id),
            ));
        }
    }

    let krate = crate_path();
    let minicbor = LitStr::new(&quote!(#krate::minicbor).to_string(), Span::call_site());
    let config = &input.ident;
    let name = &container.overrides;
    let version = container.version;
    // As for `ConfigSection`: the hash of the name of the effective type, so
    // that the generated overrides name never enters the persistent ABI.
    let section_id = container
        .id
        .unwrap_or_else(|| fnv1a64(config.unraw().to_string().as_bytes()));
    let vis: &Visibility = &input.vis;

    let idents: Vec<_> = fields.iter().map(|f| f.ident).collect();
    let types: Vec<_> = fields.iter().map(|f| f.ty).collect();
    let ids: Vec<_> = fields.iter().map(|f| f.id).collect();
    let docs: Vec<_> = fields
        .iter()
        .map(|f| {
            let docs = &f.docs;
            quote!(#(#docs)*)
        })
        .collect();
    let overrides_doc = format!(
        "Persisted overrides of [`{config}`] (section `{config}`): a CBOR map of the fields that \
         were set explicitly, keyed by their field ids; an absent field is not overridden."
    );

    Ok(quote! {
        #[doc = #overrides_doc]
        #[derive(
            ::core::clone::Clone,
            ::core::fmt::Debug,
            ::core::cmp::PartialEq,
            #krate::minicbor::Encode,
            #krate::minicbor::Decode,
            #krate::minicbor::CborLen,
        )]
        #[cbor(map, crate = #minicbor)]
        #vis struct #name {
            #(
                #docs
                #[n(#ids)]
                pub #idents: #krate::Override<#types>,
            )*
        }

        impl ::core::default::Default for #name {
            fn default() -> Self {
                Self { #(#idents: #krate::Override::Default),* }
            }
        }

        impl #krate::ConfigSection for #name {
            const ID: #krate::SectionId = #krate::SectionId(#section_id);
            const VERSION: u16 = #version;
        }

        impl #krate::ConfigOverrides<#config> for #name {
            const FIELD_IDS: &'static [u32] = &[#(#ids),*];

            fn apply_to(&self, config: &mut #config) {
                #(
                    if let #krate::Override::Value(value) = &self.#idents {
                        config.#idents = ::core::clone::Clone::clone(value);
                    }
                )*
            }

            fn is_overridden(&self, id: u32) -> ::core::option::Option<bool> {
                match id {
                    #(#ids => ::core::option::Option::Some(self.#idents.is_value()),)*
                    _ => ::core::option::Option::None,
                }
            }

            fn capture(&mut self, config: &#config, id: u32) -> bool {
                match id {
                    #(#ids => {
                        self.#idents =
                            #krate::Override::Value(::core::clone::Clone::clone(&config.#idents));
                        true
                    })*
                    _ => false,
                }
            }

            fn clear(&mut self, id: u32) -> bool {
                match id {
                    #(#ids => {
                        self.#idents = #krate::Override::Default;
                        true
                    })*
                    _ => false,
                }
            }

            fn is_empty(&self) -> bool {
                true #(&& !self.#idents.is_value())*
            }
        }

        impl #krate::HasOverrides for #config {
            type Overrides = #name;
        }
    })
}
