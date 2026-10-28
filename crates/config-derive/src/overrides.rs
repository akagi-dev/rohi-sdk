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
//! The persisted per-field overrides of a typed config section, generated
//! from the config declaration: the overrides struct with its CBOR map codec
//! keyed by the field ids, and its `ConfigOverrides`, `ConfigSection` and
//! `HasOverrides` relationships.
//!
//! A `read_only` field is exposed to clients but never persisted: it has no
//! override and takes its value from `Default`.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::ext::IdentExt;
use syn::{DeriveInput, LitStr, Visibility};

use crate::model::Model;
use crate::{crate_path, fnv1a64};

pub(crate) fn expand(input: &DeriveInput, model: &Model<'_>) -> TokenStream {
    let krate = crate_path();
    let minicbor = LitStr::new(&quote!(#krate::minicbor).to_string(), Span::call_site());
    let config = &input.ident;
    let container = &model.container;
    let name = &container.overrides;
    let version = container.version;
    // As for `ConfigSection`: the hash of the name of the effective type, so
    // that the generated overrides name never enters the persistent ABI.
    let section_id = container
        .id
        .unwrap_or_else(|| fnv1a64(config.unraw().to_string().as_bytes()));
    let vis: &Visibility = &input.vis;

    let stored: Vec<_> = model.fields.iter().filter(|f| !f.read_only).collect();
    let idents: Vec<_> = stored.iter().map(|f| f.ident).collect();
    let types: Vec<_> = stored.iter().map(|f| f.ty).collect();
    let ids: Vec<_> = stored.iter().map(|f| f.id).collect();
    let docs: Vec<_> = stored
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

    // Not derived: a secret must never be shown, whatever its type prints.
    let debug_name = name.unraw().to_string();
    let debug_fields = stored.iter().map(|f| {
        let ident = f.ident;
        let label = ident.unraw().to_string();
        if f.secret {
            quote!(.field(#label, &#krate::RedactedOverride(&self.#ident)))
        } else {
            quote!(.field(#label, &self.#ident))
        }
    });

    quote! {
        #[doc = #overrides_doc]
        #[derive(
            ::core::clone::Clone,
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

        impl ::core::fmt::Debug for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(#debug_name)
                    #(#debug_fields)*
                    .finish()
            }
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
    }
}
