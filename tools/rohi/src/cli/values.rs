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
//! Parsing of values given on the command line or in files, driven by the
//! field descriptions of the device.

use crate::cli::Ctx;
use crate::cli::diagnostic::CliError;
use crate::model::{FieldDescriptor, Value, ValueKind};

fn usage(message: String) -> CliError {
    CliError::Usage(message)
}

/// Splits `NAME=VALUE`.
pub fn assignment(text: &str) -> Result<(&str, &str), CliError> {
    text.split_once('=')
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(|| usage(format!("`{text}` is not NAME=VALUE")))
}

/// The field called `name`, or an error that lists the known names.
pub fn lookup<'a>(
    fields: impl Iterator<Item = &'a FieldDescriptor> + Clone,
    what: &str,
    name: &str,
) -> Result<&'a FieldDescriptor, CliError> {
    fields
        .clone()
        .find(|field| field.name == name)
        .ok_or_else(|| {
            let known: Vec<&str> = fields.map(|field| field.name.as_str()).collect();
            usage(format!(
                "{what} has no field `{name}`; its fields are: {}",
                known.join(", ")
            ))
        })
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let text = text.strip_prefix("0x").unwrap_or(text);
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}

/// Parses `text` as the kind of `field`.
pub fn parse(field: &FieldDescriptor, text: &str) -> Result<Value, CliError> {
    parse_inner(field, text, true)
}

/// Parses a secret `text`; errors never include it.
fn parse_secret(field: &FieldDescriptor, text: &str) -> Result<Value, CliError> {
    parse_inner(field, text, false)
}

fn parse_inner(field: &FieldDescriptor, text: &str, echo: bool) -> Result<Value, CliError> {
    let bad = |expected: &str| {
        if echo {
            usage(format!("{}: expected {expected}, got `{text}`", field.name))
        } else {
            usage(format!("{}: expected {expected}", field.name))
        }
    };
    match field.kind {
        ValueKind::Bool => match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "off" | "0" => Ok(Value::Bool(false)),
            _ => Err(bad("true or false")),
        },
        ValueKind::I64 => text.parse().map(Value::I64).map_err(|_| bad("an integer")),
        ValueKind::U64 => text
            .parse()
            .map(Value::U64)
            .map_err(|_| bad("a non-negative integer")),
        ValueKind::F32 => text.parse().map(Value::F32).map_err(|_| bad("a number")),
        ValueKind::String => Ok(Value::String(text.to_owned())),
        ValueKind::Bytes => hex_bytes(text)
            .map(Value::Bytes)
            .ok_or_else(|| bad("hex bytes")),
        ValueKind::Enum => {
            if field.variants.iter().any(|variant| variant == text) {
                Ok(Value::Enum(text.to_owned()))
            } else {
                Err(bad(&format!("one of {}", field.variants.join(", "))))
            }
        }
        ValueKind::Unrecognized => Err(usage(format!(
            "{}: this version of rohi does not know the type of the field",
            field.name
        ))),
    }
}

/// Converts a JSON value of a configuration file to the kind of `field`.
pub fn from_json(field: &FieldDescriptor, json: &serde_json::Value) -> Result<Value, CliError> {
    use serde_json::Value as Json;
    let bad = |expected: &str| {
        CliError::File(format!("{}: expected {expected}, found {json}", field.name))
    };
    match (field.kind, json) {
        (ValueKind::Bool, Json::Bool(value)) => Ok(Value::Bool(*value)),
        (ValueKind::I64, Json::Number(number)) => number
            .as_i64()
            .map(Value::I64)
            .ok_or_else(|| bad("an integer")),
        (ValueKind::U64, Json::Number(number)) => number
            .as_u64()
            .map(Value::U64)
            .ok_or_else(|| bad("a non-negative integer")),
        (ValueKind::F32, Json::Number(number)) => number
            .as_f64()
            .map(|number| Value::F32(number as f32))
            .ok_or_else(|| bad("a number")),
        (ValueKind::String, Json::String(text)) => Ok(Value::String(text.clone())),
        (ValueKind::Bytes, Json::String(text)) => hex_bytes(text)
            .map(Value::Bytes)
            .ok_or_else(|| bad("hex bytes")),
        (ValueKind::Enum, Json::String(text)) if field.variants.contains(text) => {
            Ok(Value::Enum(text.clone()))
        }
        (ValueKind::Enum, _) => Err(bad(&format!("one of {}", field.variants.join(", ")))),
        (ValueKind::Bool, _) => Err(bad("true or false")),
        (ValueKind::I64, _) => Err(bad("an integer")),
        (ValueKind::U64, _) => Err(bad("a non-negative integer")),
        (ValueKind::F32, _) => Err(bad("a number")),
        (ValueKind::String, _) => Err(bad("a string")),
        (ValueKind::Bytes, _) => Err(bad("hex bytes in a string")),
        (ValueKind::Unrecognized, _) => Err(bad("a value of a type this rohi does not know")),
    }
}

/// Reads the value of `field` from stdin, or asks for it on a terminal, so
/// that it stays out of the shell history.
pub async fn secret(ctx: &mut Ctx, field: &FieldDescriptor) -> Result<Value, CliError> {
    let text = if ctx.io.interactive() {
        ctx.io.hidden(&format!("{}: ", field.name)).await?
    } else {
        ctx.io.secret_from_stdin(&field.name).await?
    };
    parse_secret(field, &text)
}

/// Either `-` (a secret, read separately) or a plain value.
pub async fn argument(
    ctx: &mut Ctx,
    field: &FieldDescriptor,
    text: &str,
) -> Result<Value, CliError> {
    if text == "-" {
        secret(ctx, field).await
    } else {
        parse(field, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(kind: ValueKind) -> FieldDescriptor {
        FieldDescriptor {
            id: 1,
            name: "f".into(),
            kind,
            unit: String::new(),
            variants: vec!["a".into(), "b".into()],
            variant_values: vec![0, 1],
        }
    }

    #[test]
    fn values_are_parsed_by_the_kind_of_the_field() {
        assert_eq!(
            parse(&field(ValueKind::Bool), "yes").unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            parse(&field(ValueKind::U64), "443").unwrap(),
            Value::U64(443)
        );
        assert_eq!(parse(&field(ValueKind::I64), "-5").unwrap(), Value::I64(-5));
        assert_eq!(
            parse(&field(ValueKind::F32), "1.5").unwrap(),
            Value::F32(1.5)
        );
        assert_eq!(
            parse(&field(ValueKind::Bytes), "0aff").unwrap(),
            Value::Bytes(vec![0x0a, 0xff])
        );
        assert_eq!(
            parse(&field(ValueKind::Enum), "b").unwrap(),
            Value::Enum("b".into())
        );
        // An empty string is a value, not "unset".
        assert_eq!(
            parse(&field(ValueKind::String), "").unwrap(),
            Value::String(String::new())
        );
    }

    #[test]
    fn bad_values_name_the_field_and_what_was_expected() {
        let error = parse(&field(ValueKind::U64), "-1").unwrap_err().to_string();
        assert!(
            error.contains("f: expected a non-negative integer"),
            "{error}"
        );
        assert!(parse(&field(ValueKind::Enum), "c").is_err());
        assert!(parse(&field(ValueKind::Bytes), "abc").is_err());
        assert!(parse(&field(ValueKind::Bool), "maybe").is_err());
    }

    #[test]
    fn secret_parse_errors_do_not_echo_the_input() {
        let error = parse_secret(&field(ValueKind::Bytes), "zz-secret")
            .unwrap_err()
            .to_string();
        assert!(error.contains("f: expected hex bytes"), "{error}");
        assert!(!error.contains("zz-secret"), "{error}");
        let error = parse_secret(&field(ValueKind::Enum), "zz-secret")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("zz-secret"), "{error}");
    }

    #[test]
    fn json_values_are_checked_against_the_kind() {
        assert_eq!(
            from_json(&field(ValueKind::U64), &serde_json::json!(5)).unwrap(),
            Value::U64(5)
        );
        assert!(from_json(&field(ValueKind::U64), &serde_json::json!("5")).is_err());
        assert!(from_json(&field(ValueKind::U64), &serde_json::json!(-1)).is_err());
        assert!(from_json(&field(ValueKind::String), &serde_json::json!(true)).is_err());
    }

    #[test]
    fn assignments_split_at_the_first_equals_sign() {
        assert_eq!(assignment("host=a=b").unwrap(), ("host", "a=b"));
        assert!(assignment("host").is_err());
        assert!(assignment("=x").is_err());
    }
}
