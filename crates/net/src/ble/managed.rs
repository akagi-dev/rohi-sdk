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
//! The `ble` config section in the generic management plane.

use rohi_config::TextValue;

use super::config::BleName;

/// The advertised name as text. A name is 1 to 29 bytes of UTF-8 without
/// control characters; the section's fields and ids are declared on
/// [`BleSettings`](super::BleSettings).
impl TextValue for BleName {
    fn from_text(text: &str) -> Option<Self> {
        Self::new(text)
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use rohi_config::ConfigOverrides;
    use rohi_service::{Config, FieldError, MAX_CONFIG_FIELDS, Value};

    use crate::{BleOverrides, BleSettings};

    const NAME: u32 = 1;
    use rohi_config::ConfigSection;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn the_schema_is_well_formed() {
        assert_eq!(<BleOverrides as ConfigSection>::VERSION, 1);
        assert_eq!(BleSettings::FIELDS.len(), BleOverrides::FIELD_IDS.len());
        assert!(BleSettings::FIELDS.len() <= MAX_CONFIG_FIELDS);
        for field in BleSettings::FIELDS {
            assert!(field.field.name.len() <= 24);
            assert!(!field.secret);
        }
    }

    #[test]
    fn the_name_is_optional_and_validated() {
        let mut settings = BleSettings::default();
        assert_eq!(settings.read_field(NAME), Ok(None));
        settings.set_field(NAME, text("urban")).unwrap();
        assert_eq!(settings.read_field(NAME), Ok(Some(text("urban"))));
        assert_eq!(Config::validate(&settings), Ok(()));
        let mut overrides = BleOverrides::default();
        assert!(overrides.capture(&settings, NAME));
        assert_eq!(
            rohi_config::materialize::<BleSettings>(&Default::default(), &overrides).name,
            settings.name
        );
        assert!(overrides.clear(NAME));
        assert_eq!(
            rohi_config::materialize::<BleSettings>(&Default::default(), &overrides).name,
            None
        );

        assert_eq!(settings.set_field(NAME, text("")), Err(FieldError::Invalid));
        assert_eq!(
            settings.set_field(NAME, text(&"a".repeat(30))),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            settings.set_field(NAME, Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            settings.set_field(9, text("x")),
            Err(FieldError::UnknownField)
        );
        assert_eq!(BleOverrides::default().is_overridden(9), None);
        assert_eq!(settings.read_field(9), Err(FieldError::UnknownField));
    }
}
