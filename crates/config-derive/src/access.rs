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
//! The `rohi_service::Config` implementation of a managed config: the field
//! metadata and the generic field access, generated from the config
//! declaration.

use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::DeriveInput;
use syn::spanned::Spanned;

use crate::model::{Field, Model};

fn service_path() -> TokenStream {
    match crate_name("rohi-service") {
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        _ => quote!(::rohi_service),
    }
}

/// The codec of a field: the one it names, or the standard one. The span is
/// that of the field type, so that a type without a codec is reported there.
fn codec(field: &Field<'_>) -> TokenStream {
    let cfg = crate::crate_path();
    let value_type = field.value_type();
    match &field.with {
        Some(with) => quote_spanned!(with.span()=> <#with as #cfg::Codec<#value_type>>),
        None => quote_spanned!(value_type.span()=> <#cfg::Standard as #cfg::Codec<#value_type>>),
    }
}

fn descriptor(field: &Field<'_>, svc: &TokenStream) -> TokenStream {
    let codec = codec(field);
    let (id, name) = (field.id, &field.name);
    let mut out = quote!(#svc::ConfigField::new(#id, #name, #codec::KIND));
    if let Some(unit) = &field.unit {
        out = quote!(#out.unit(#svc::Unit::#unit));
    }
    if let Some(enum_id) = &field.enum_id {
        out = quote!(#out.with_enum(#enum_id));
    }
    if let (Some(min), Some(max)) = (&field.min, &field.max) {
        out = quote! {
            #out.bounds(match #codec::KIND {
                #svc::ValueKind::U64 => #svc::Bounds::U64((#min) as u64, (#max) as u64),
                #svc::ValueKind::I64 => #svc::Bounds::I64((#min) as i64, (#max) as i64),
                #svc::ValueKind::F32 => #svc::Bounds::F32((#min) as f32, (#max) as f32),
                _ => panic!("`min` and `max` need a numeric field"),
            })
        };
    }
    if field.optional() {
        out = quote!(#out.optional());
    }
    if field.secret {
        out = quote!(#out.secret());
    }
    if field.read_only {
        out = quote!(#out.read_only());
    }
    out
}

pub(crate) fn expand(input: &DeriveInput, model: &Model<'_>) -> TokenStream {
    let svc = service_path();
    let config = &input.ident;
    let container = &model.container;
    let name = &container.name;
    let fields = &model.fields;

    let descriptors = fields.iter().map(|f| descriptor(f, &svc));
    let enums = match &container.enums {
        Some(enums) => quote!(#enums),
        None => quote!(&[]),
    };

    let reads = fields.iter().map(|f| {
        let (id, ident) = (f.id, f.ident);
        let codec = codec(f);
        if f.secret {
            quote!(#id => ::core::result::Result::Err(#svc::FieldError::Secret),)
        } else if f.inner.is_some() {
            quote! {
                #id => match &self.#ident {
                    ::core::option::Option::None => ::core::result::Result::Ok(::core::option::Option::None),
                    ::core::option::Option::Some(value) => #codec::encode(value),
                },
            }
        } else {
            quote!(#id => #codec::encode(&self.#ident),)
        }
    });

    let writes = fields.iter().map(|f| {
        let (id, ident) = (f.id, f.ident);
        let codec = codec(f);
        if f.read_only {
            return quote!(#id => ::core::result::Result::Err(#svc::FieldError::ReadOnly),);
        }
        let decoded = if f.inner.is_some() {
            quote!(::core::option::Option::Some(decoded))
        } else {
            quote!(decoded)
        };
        if f.secret {
            // The copy that was handed over is wiped, whether or not it fits.
            quote! {
                #id => {
                    let mut value = value;
                    let decoded = #codec::decode(&value);
                    value.wipe();
                    let decoded = decoded?;
                    self.#ident = #decoded;
                    ::core::result::Result::Ok(())
                }
            }
        } else {
            quote! {
                #id => {
                    let decoded = #codec::decode(&value)?;
                    self.#ident = #decoded;
                    ::core::result::Result::Ok(())
                }
            }
        }
    });

    let clears = fields.iter().map(|f| {
        let (id, ident) = (f.id, f.ident);
        if f.read_only {
            quote!(#id => ::core::result::Result::Err(#svc::FieldError::ReadOnly),)
        } else if f.optional() {
            quote! {
                #id => {
                    self.#ident = ::core::option::Option::None;
                    ::core::result::Result::Ok(())
                }
            }
        } else {
            quote!(#id => ::core::result::Result::Err(#svc::FieldError::NotOptional),)
        }
    });

    // A secret is only asked whether it exists, never read.
    let secret_presence = fields.iter().filter(|f| f.secret).map(|f| {
        let (id, ident) = (f.id, f.ident);
        quote!(#id => ::core::result::Result::Ok(self.#ident.is_some()),)
    });

    let validate = match &container.validate {
        Some(path) => quote!(#path(self)),
        None => quote!(::core::result::Result::Ok(())),
    };

    let names = fields.iter().map(|f| &f.name).chain(Some(name));

    quote! {
        impl #svc::Config for #config {
            const NAME: &'static str = #name;
            const FIELDS: &'static [#svc::ConfigField] = &[#(#descriptors),*];
            const ENUMS: &'static [#svc::EnumDescriptor] = #enums;

            fn read_field(
                &self,
                id: u32,
            ) -> ::core::result::Result<::core::option::Option<#svc::Value>, #svc::FieldError> {
                match id {
                    #(#reads)*
                    _ => ::core::result::Result::Err(#svc::FieldError::UnknownField),
                }
            }

            fn field_is_set(&self, id: u32) -> ::core::result::Result<bool, #svc::FieldError> {
                match id {
                    #(#secret_presence)*
                    _ => ::core::result::Result::Ok(
                        ::core::option::Option::is_some(&#svc::Config::read_field(self, id)?),
                    ),
                }
            }

            fn set_field(
                &mut self,
                id: u32,
                value: #svc::Value,
            ) -> ::core::result::Result<(), #svc::FieldError> {
                match id {
                    #(#writes)*
                    _ => ::core::result::Result::Err(#svc::FieldError::UnknownField),
                }
            }

            fn set_none(&mut self, id: u32) -> ::core::result::Result<(), #svc::FieldError> {
                match id {
                    #(#clears)*
                    _ => ::core::result::Result::Err(#svc::FieldError::UnknownField),
                }
            }

            fn validate(&self) -> ::core::result::Result<(), #svc::FieldError> {
                #validate
            }
        }

        // The schema is evaluated with the type, not only when a section is
        // built: a bad declaration fails here.
        const _: &[#svc::ConfigField] = <#config as #svc::Config>::FIELDS;
        const _: () = {
            #(assert!(#names.len() <= #svc::MAX_NAME_LEN, "a section or field name is too long");)*
        };
    }
}
