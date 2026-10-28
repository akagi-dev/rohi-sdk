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
//! The parsed declaration of a managed config: the container attributes, the
//! fields and their `#[config(...)]` keys. Everything `#[derive(Config)]`
//! generates, the field metadata, the field access and the persisted
//! overrides, is generated from this one model, so each field's id, name and
//! type are declared exactly once.

use std::collections::HashSet;

use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, GenericArgument, Ident, LitInt, LitStr, Path,
    PathArguments, Type,
};

use crate::duplicate;

pub(crate) struct Container {
    /// Public name of the section.
    pub(crate) name: LitStr,
    /// Name of the generated overrides type.
    pub(crate) overrides: Ident,
    pub(crate) version: u16,
    pub(crate) id: Option<u64>,
    /// Cross-field validation, `fn(&Config) -> Result<(), FieldError>`.
    pub(crate) validate: Option<Path>,
    /// The `&[EnumDescriptor]` the enum fields refer to.
    pub(crate) enums: Option<Expr>,
}

pub(crate) struct Field<'a> {
    pub(crate) ident: &'a Ident,
    /// The declared type of the struct field.
    pub(crate) ty: &'a Type,
    /// `T` of an `Option<T>` field, the type the codec converts.
    pub(crate) inner: Option<&'a Type>,
    pub(crate) id: u32,
    /// Public name.
    pub(crate) name: LitStr,
    /// The codec type; the standard codec when absent.
    pub(crate) with: Option<Type>,
    pub(crate) unit: Option<Ident>,
    pub(crate) min: Option<Expr>,
    pub(crate) max: Option<Expr>,
    pub(crate) enum_id: Option<Expr>,
    pub(crate) secret: bool,
    pub(crate) required: bool,
    pub(crate) read_only: bool,
    pub(crate) docs: Vec<&'a Attribute>,
}

impl Field<'_> {
    /// Whether the field may be set to "no value".
    pub(crate) fn optional(&self) -> bool {
        self.inner.is_some() && !self.required
    }

    /// The type the codec converts.
    pub(crate) fn value_type(&self) -> &Type {
        self.inner.unwrap_or(self.ty)
    }
}

pub(crate) struct Model<'a> {
    pub(crate) container: Container,
    pub(crate) fields: Vec<Field<'a>>,
}

/// `[a-z][a-z0-9_]*`: a name that is the same on every platform and in every
/// client.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn name_error(lit: &LitStr, what: &str) -> syn::Error {
    syn::Error::new(
        lit.span(),
        format!(
            "invalid {what} name `{}`: it must match `[a-z][a-z0-9_]*`",
            lit.value()
        ),
    )
}

fn parse_container(input: &DeriveInput) -> syn::Result<Container> {
    let mut name: Option<LitStr> = None;
    let mut overrides: Option<Ident> = None;
    let mut version: Option<u16> = None;
    let mut id: Option<u64> = None;
    let mut validate: Option<Path> = None;
    let mut enums: Option<Expr> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("config")) {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            if meta.path.is_ident("name") {
                if name.is_some() {
                    return Err(duplicate("name", span));
                }
                name = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("overrides") {
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
            } else if meta.path.is_ident("validate") {
                if validate.is_some() {
                    return Err(duplicate("validate", span));
                }
                let value = meta.value()?;
                validate = Some(if value.peek(LitStr) {
                    let lit: LitStr = value.parse()?;
                    lit.parse::<Path>().map_err(|_| {
                        syn::Error::new(lit.span(), "`validate` must be a path to a function")
                    })?
                } else {
                    value.parse()?
                });
            } else if meta.path.is_ident("enums") {
                if enums.is_some() {
                    return Err(duplicate("enums", span));
                }
                enums = Some(meta.value()?.parse()?);
            } else {
                return Err(syn::Error::new(
                    span,
                    "unknown `config` attribute, expected one of: `name`, `overrides`, \
                     `version`, `id`, `validate`, `enums`",
                ));
            }
            Ok(())
        })?;
    }
    let example = "#[config(name = \"wifi\", overrides = WifiOverrides, version = 1)]";
    let name = name.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            format!("missing `name`, add `{example}`"),
        )
    })?;
    if !valid_name(&name.value()) {
        return Err(name_error(&name, "section"));
    }
    let overrides = overrides.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            format!("missing `overrides`, add `{example}`"),
        )
    })?;
    let version = version.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            format!("missing `version`, add `{example}`"),
        )
    })?;
    Ok(Container {
        name,
        overrides,
        version,
        id,
        validate,
        enums,
    })
}

/// `T` of `Option<T>`.
fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let mut types = arguments.args.iter().filter_map(|argument| match argument {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    });
    match (types.next(), types.next()) {
        (Some(inner), None) => Some(inner),
        _ => None,
    }
}

fn flag(meta: &syn::meta::ParseNestedMeta<'_>, key: &str, slot: &mut bool) -> syn::Result<()> {
    if *slot {
        return Err(duplicate(key, meta.path.span()));
    }
    if meta.input.peek(syn::Token![=]) {
        return Err(syn::Error::new(
            meta.path.span(),
            format!("`{key}` is a flag and takes no value"),
        ));
    }
    *slot = true;
    Ok(())
}

fn parse_field(field: &syn::Field) -> syn::Result<Field<'_>> {
    let ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new(field.span(), "fields must be named"))?;
    let mut id: Option<u32> = None;
    let mut name: Option<LitStr> = None;
    let mut with: Option<Type> = None;
    let mut unit: Option<Ident> = None;
    let mut min: Option<Expr> = None;
    let mut max: Option<Expr> = None;
    let mut enum_id: Option<Expr> = None;
    let (mut secret, mut required, mut read_only) = (false, false, false);
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
            } else if meta.path.is_ident("name") {
                if name.is_some() {
                    return Err(duplicate("name", span));
                }
                name = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("with") {
                if with.is_some() {
                    return Err(duplicate("with", span));
                }
                with = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("unit") {
                if unit.is_some() {
                    return Err(duplicate("unit", span));
                }
                unit = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("min") {
                if min.is_some() {
                    return Err(duplicate("min", span));
                }
                min = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("max") {
                if max.is_some() {
                    return Err(duplicate("max", span));
                }
                max = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("enum_id") {
                if enum_id.is_some() {
                    return Err(duplicate("enum_id", span));
                }
                enum_id = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("secret") {
                flag(&meta, "secret", &mut secret)?;
            } else if meta.path.is_ident("required") {
                flag(&meta, "required", &mut required)?;
            } else if meta.path.is_ident("read_only") {
                flag(&meta, "read_only", &mut read_only)?;
            } else {
                return Err(syn::Error::new(
                    span,
                    "unknown field `config` attribute, expected one of: `id`, `name`, `with`, \
                     `unit`, `min`, `max`, `enum_id`, `secret`, `required`, `read_only`",
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
    let name = match name {
        Some(name) => name,
        None => LitStr::new(&syn::ext::IdentExt::unraw(ident).to_string(), ident.span()),
    };
    if !valid_name(&name.value()) {
        return Err(name_error(&name, "field"));
    }
    let inner = option_inner(&field.ty);

    if secret && inner.is_none() {
        return Err(syn::Error::new(
            ident.span(),
            format!(
                "the secret field `{ident}` must be an `Option`: only whether it is set is ever \
                 read, so it needs a state that means \"not set\""
            ),
        ));
    }
    if secret && read_only {
        return Err(syn::Error::new(
            ident.span(),
            format!("the field `{ident}` cannot be both `secret` and `read_only`"),
        ));
    }
    if required && inner.is_none() {
        return Err(syn::Error::new(
            ident.span(),
            format!("`required` only applies to an `Option` field, `{ident}` is always required"),
        ));
    }
    if required && read_only {
        return Err(syn::Error::new(
            ident.span(),
            format!("the field `{ident}` is `read_only`, `required` has no effect"),
        ));
    }
    if min.is_some() != max.is_some() {
        return Err(syn::Error::new(
            ident.span(),
            format!("the field `{ident}` needs both `min` and `max`"),
        ));
    }

    Ok(Field {
        ident,
        ty: &field.ty,
        inner,
        id,
        name,
        with,
        unit,
        min,
        max,
        enum_id,
        secret,
        required,
        read_only,
        docs: field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc"))
            .collect(),
    })
}

pub(crate) fn parse(input: &DeriveInput) -> syn::Result<Model<'_>> {
    let container = parse_container(input)?;

    if let Some(param) = input.generics.params.first() {
        return Err(syn::Error::new(
            param.span(),
            "a config cannot be generic: the overrides type and the section identity belong \
             to exactly one type",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "`Config` can only be derived for structs with named fields",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new(
            input.ident.span(),
            "`Config` needs named fields",
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
            "a config needs at least one field",
        ));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for field in &fields {
        if !ids.insert(field.id) {
            return Err(syn::Error::new(
                field.ident.span(),
                format!("duplicate field id {}", field.id),
            ));
        }
        if !names.insert(field.name.value()) {
            return Err(syn::Error::new(
                field.name.span(),
                format!("duplicate field name `{}`", field.name.value()),
            ));
        }
    }
    Ok(Model { container, fields })
}
