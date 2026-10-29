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

//! The schema, the defaults and the persisted overrides of every managed
//! config section of the device, pinned.
//!
//! These are the persistent and wire ABI of the sections: field ids and
//! names, kinds, units, bounds, optionality, the enums, the id and version of
//! the section and the CBOR encoding of its overrides. However a config is
//! declared, none of it may change by accident. Review the diff and accept an
//! intended change with `UPDATE_GOLDEN=1 cargo test -p rohi --test
//! config_schemas`.

use std::fmt::Write as _;

use rohi_config::{ConfigOverrides, ConfigSection, HasOverrides};
use rohi_connectivity::ConnectivityConfig;
use rohi_location::LocationConfig;
use rohi_net::{BleSettings, NetworkConfig, WifiConfig};
use rohi_service::{Config, FieldError, Value};
use rohi_time_ntp::NtpServersConfig;

// `Value`'s `Debug` shows only the kind, because a value can be a secret.
fn show(value: &Result<Option<Value>, FieldError>) -> String {
    match value {
        Ok(None) => "none".into(),
        Ok(Some(Value::Bool(v))) => format!("bool {v}"),
        Ok(Some(Value::I64(v))) => format!("i64 {v}"),
        Ok(Some(Value::U64(v))) => format!("u64 {v}"),
        Ok(Some(Value::F32(v))) => format!("f32 {v}"),
        Ok(Some(Value::String(v))) => format!("string {:?}", v.as_str()),
        Ok(Some(Value::Bytes(v))) => format!("bytes {v:02x?}"),
        Ok(Some(Value::Enum(v))) => format!("enum {v}"),
        Err(error) => format!("error {error:?}"),
    }
}

fn describe<C: Config>(out: &mut String) {
    let id = <C::Overrides as ConfigSection>::ID.0;
    let version = <C::Overrides as ConfigSection>::VERSION;
    writeln!(out, "[{}] id={id:#018x} version={version}", C::NAME).unwrap();
    for field in C::FIELDS {
        let d = &field.field;
        writeln!(
            out,
            "  field {} {:?} {:?} unit={:?} enum={} writable={} secret={} optional={} bounds={:?}",
            d.id,
            d.name,
            d.kind,
            d.unit,
            d.enum_id,
            field.writable,
            field.secret,
            field.optional,
            field.bounds,
        )
        .unwrap();
    }
    for descriptor in C::ENUMS {
        write!(out, "  enum {}:", descriptor.id).unwrap();
        for variant in descriptor.variants {
            write!(out, " {}={:?}", variant.value, variant.name).unwrap();
        }
        writeln!(out).unwrap();
    }
    writeln!(
        out,
        "  overridable {:?}",
        <C::Overrides as ConfigOverrides<C>>::FIELD_IDS
    )
    .unwrap();

    let config = C::default();
    for field in C::FIELDS {
        let id = field.field.id;
        if field.secret {
            writeln!(out, "  default {id}: set={:?}", config.field_is_set(id)).unwrap();
        } else {
            writeln!(out, "  default {id}: {}", show(&config.read_field(id))).unwrap();
        }
    }
    writeln!(out, "  default valid={:?}", config.validate()).unwrap();

    // Every field overridden with its default value, as persisted.
    let mut overrides = C::Overrides::default();
    for id in <C::Overrides as ConfigOverrides<C>>::FIELD_IDS {
        assert!(overrides.capture(&config, *id));
    }
    let mut buffer = [0u8; 256];
    let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
    let hex: String = buffer[..used].iter().map(|b| format!("{b:02x}")).collect();
    writeln!(out, "  cbor {hex}").unwrap();
}

fn golden(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {name}; run with UPDATE_GOLDEN=1 to create it"));
    assert_eq!(
        actual, expected,
        "{name} changed; review the diff and run with UPDATE_GOLDEN=1 to accept it"
    );
}

#[test]
fn the_managed_sections_of_the_device_are_pinned() {
    let mut out = String::new();
    describe::<WifiConfig>(&mut out);
    describe::<NetworkConfig>(&mut out);
    describe::<BleSettings>(&mut out);
    describe::<ConnectivityConfig>(&mut out);
    describe::<LocationConfig>(&mut out);
    describe::<NtpServersConfig>(&mut out);
    golden("config-sections.txt", &out);
}

// Keeps the section types linked to their overrides in the dump above.
const _: fn() = || {
    fn has_overrides<C: HasOverrides>() {}
    has_overrides::<WifiConfig>();
};
