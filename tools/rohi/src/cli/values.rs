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
use crate::model::{CatalogEntry, FieldDescriptor, Value, ValueKind};

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

/// Length of an address, in bytes.
const ADDRESS_LEN: usize = 32;

/// The 32 bytes of an address given as hexadecimal, with or without `0x`, or
/// as an SS58 address.
fn address(text: &str) -> Option<Vec<u8>> {
    use sp_core::crypto::Ss58Codec as _;
    let hex = text.strip_prefix("0x").unwrap_or(text);
    if hex.len() == 2 * ADDRESS_LEN {
        return hex_bytes(hex);
    }
    sp_core::ed25519::Public::from_ss58check(text)
        .ok()
        .map(|key| key.0.to_vec())
}

/// The bytes of a bitset of `size` bytes from `text`: `all`, `none`, or
/// comma-separated catalog names. A name that is the prefix of dotted names
/// selects all of them (`bmx280` is every `bmx280.*`). The bytes in hex are
/// accepted too.
pub fn selection(
    field: &FieldDescriptor,
    text: &str,
    size: usize,
    catalog: &[CatalogEntry],
) -> Result<Value, CliError> {
    let bad = |why: String| usage(format!("{}: {why}", field.name));
    let text = text.trim();
    match text {
        "all" => return Ok(Value::Bytes(vec![0xFF; size])),
        "none" => return Ok(Value::Bytes(vec![0; size])),
        _ => {}
    }
    if text.len() == 2 * size
        && !catalog.iter().any(|entry| entry.name == text)
        && let Some(bytes) = hex_bytes(text)
    {
        return Ok(Value::Bytes(bytes));
    }
    let mut bytes = vec![0u8; size];
    for name in text.split(',').map(str::trim) {
        let prefix = format!("{name}.");
        let mut found = false;
        for entry in catalog
            .iter()
            .filter(|entry| entry.name == name || entry.name.starts_with(&prefix))
        {
            let index = usize::try_from(entry.bit)
                .ok()
                .filter(|bit| *bit < 8 * size);
            let Some(index) = index else {
                return Err(bad(format!("`{}` does not fit the bitset", entry.name)));
            };
            bytes[index / 8] |= 1 << (index % 8);
            found = true;
        }
        if !found {
            let names: Vec<&str> = catalog.iter().map(|entry| entry.name.as_str()).collect();
            return Err(bad(format!(
                "unknown `{name}`; expected all, none or some of: {}",
                names.join(", ")
            )));
        }
    }
    Ok(Value::Bytes(bytes))
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
        ValueKind::Address => address(text)
            .map(Value::Bytes)
            .ok_or_else(|| bad("an address: 32 bytes of hex or an SS58 address")),
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
        (ValueKind::Address, Json::String(text)) => address(text)
            .map(Value::Bytes)
            .ok_or_else(|| bad("an address: 32 bytes of hex or an SS58 address")),
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
        (ValueKind::Address, _) => Err(bad("an address in a string")),
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
            selection: None,
        }
    }

    #[test]
    fn an_address_is_hex_with_or_without_the_prefix_or_ss58() {
        let key = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
        let expected = Value::Bytes(hex_bytes(key).unwrap());
        assert_eq!(parse(&field(ValueKind::Address), key).unwrap(), expected);
        assert_eq!(
            parse(&field(ValueKind::Address), &format!("0x{key}")).unwrap(),
            expected
        );
        let ss58 =
            crate::ss58(&rohi_identity::PublicKey::from_slice(&hex_bytes(key).unwrap()).unwrap());
        assert_eq!(parse(&field(ValueKind::Address), &ss58).unwrap(), expected);
        assert!(parse(&field(ValueKind::Address), "0xabcd").is_err());
        assert!(parse(&field(ValueKind::Address), "not an address").is_err());
    }

    fn catalog() -> Vec<CatalogEntry> {
        [
            (0, "location"),
            (1, "bmx280.temperature"),
            (2, "bmx280.humidity"),
            (7, "sds011.pm2_5"),
            (9, "microphone.average"),
        ]
        .into_iter()
        .map(|(bit, name)| CatalogEntry {
            bit,
            name: name.into(),
        })
        .collect()
    }

    #[test]
    fn a_selection_is_names_a_sensor_prefix_all_or_none() {
        let f = field(ValueKind::Bytes);
        let bits = |text: &str| selection(&f, text, 32, &catalog()).unwrap();
        let mask = |bits: &[usize]| {
            let mut bytes = vec![0u8; 32];
            for bit in bits {
                bytes[bit / 8] |= 1 << (bit % 8);
            }
            Value::Bytes(bytes)
        };
        assert_eq!(bits("none"), Value::Bytes(vec![0; 32]));
        assert_eq!(bits("all"), Value::Bytes(vec![0xFF; 32]));
        assert_eq!(bits("location"), mask(&[0]));
        assert_eq!(bits("bmx280,sds011.pm2_5"), mask(&[1, 2, 7]));
        assert_eq!(bits("bmx280.humidity, microphone.average"), mask(&[2, 9]));
        assert_eq!(bits(&"ff".repeat(32)), Value::Bytes(vec![0xFF; 32]));
        assert!(selection(&f, "bmx", 32, &catalog()).is_err());
        assert!(selection(&f, "location,,bmx280", 32, &catalog()).is_err());
        assert!(selection(&f, "microphone", 1, &catalog()).is_err());
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
