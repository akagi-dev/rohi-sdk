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
//! The `ble` config section in the generic management plane.

use rohi_config::ConfigSection;
use rohi_service::{ConfigField, FieldError, ManagedConfig, Value, ValueKind};

use super::config::{BleName, BleSettings};

/// Field ids of the `ble` config section. Only ever added.
pub mod field {
    /// Advertised name; unset keeps the firmware default.
    pub const NAME: u32 = 1;
}

impl ManagedConfig for BleSettings {
    const SECTION_ID: u64 = <Self as ConfigSection>::ID.0;
    const NAME: &'static str = "ble";
    const VERSION: u16 = <Self as ConfigSection>::VERSION;
    const FIELDS: &'static [ConfigField] =
        &[ConfigField::new(field::NAME, "name", ValueKind::String).optional()];

    fn defaults() -> Self {
        Self::default()
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        match id {
            field::NAME => match &self.name {
                None => Ok(None),
                Some(name) => Value::text(name.as_str())
                    .map(Some)
                    .ok_or(FieldError::Invalid),
            },
            _ => Err(FieldError::UnknownField),
        }
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            field::NAME => {
                let text = value.as_str().ok_or(FieldError::WrongKind)?;
                self.name = Some(BleName::new(text).ok_or(FieldError::Invalid)?);
                Ok(())
            }
            _ => Err(FieldError::UnknownField),
        }
    }

    fn field_is_set(&self, id: u32) -> Result<bool, FieldError> {
        Ok(self.read_field(id)?.is_some())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::NAME => {
                self.name = None;
                Ok(())
            }
            _ => Err(FieldError::UnknownField),
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rohi_service::MAX_CONFIG_FIELDS;

    use super::*;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn the_schema_is_well_formed() {
        assert_eq!(<BleSettings as ManagedConfig>::VERSION, 1);
        assert!(BleSettings::FIELDS.len() <= MAX_CONFIG_FIELDS);
        for field in BleSettings::FIELDS {
            assert!(field.field.name.len() <= 24);
            assert!(!field.secret);
        }
    }

    #[test]
    fn the_name_is_optional_and_validated() {
        let mut settings = BleSettings::defaults();
        assert_eq!(settings.read_field(field::NAME), Ok(None));
        settings.set_field(field::NAME, text("urban")).unwrap();
        assert_eq!(settings.read_field(field::NAME), Ok(Some(text("urban"))));
        assert_eq!(ManagedConfig::validate(&settings), Ok(()));
        settings.unset_field(field::NAME).unwrap();
        assert_eq!(settings.name, None);

        assert_eq!(
            settings.set_field(field::NAME, text("")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            settings.set_field(field::NAME, text(&"a".repeat(30))),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            settings.set_field(field::NAME, Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            settings.set_field(9, text("x")),
            Err(FieldError::UnknownField)
        );
        assert_eq!(settings.unset_field(9), Err(FieldError::UnknownField));
        assert_eq!(settings.read_field(9), Err(FieldError::UnknownField));
    }
}
