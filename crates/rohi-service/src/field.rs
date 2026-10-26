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
use crate::config::FieldError;
use crate::value::{Value, ValueKind};

/// Static description of one field of a config section or a service.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldDescriptor {
    /// Stable within the section or service: only ever added, never reused.
    pub id: u32,
    /// Stable name; nested fields use dotted paths (`tls.enabled`).
    pub name: &'static str,
    /// Type of the value.
    pub kind: ValueKind,
    /// One line of presentation text.
    pub description: &'static str,
    /// Unit of the value, empty when unitless.
    pub unit: &'static str,
    /// Allowed values of an [`ValueKind::Enum`] field.
    pub variants: &'static [&'static str],
}

impl FieldDescriptor {
    /// A field with no description, unit or variants.
    pub const fn new(id: u32, name: &'static str, kind: ValueKind) -> Self {
        Self {
            id,
            name,
            kind,
            description: "",
            unit: "",
            variants: &[],
        }
    }

    /// Sets the description.
    pub const fn describe(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    /// Sets the unit.
    pub const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    /// Sets the allowed variants of an enum field.
    pub const fn variants(mut self, variants: &'static [&'static str]) -> Self {
        self.variants = variants;
        self
    }
}

/// A [`FieldDescriptor`] extended with the config-only attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfigField {
    /// Common description.
    pub field: FieldDescriptor,
    /// Whether clients may change the field.
    pub writable: bool,
    /// Write-only: never returned, only reported as configured.
    pub secret: bool,
    /// May be unset; unset is distinct from an empty string or zero.
    pub optional: bool,
    /// Inclusive lower bound of a numeric field.
    pub min: Option<f64>,
    /// Inclusive upper bound of a numeric field.
    pub max: Option<f64>,
}

impl ConfigField {
    /// A required, writable, non-secret field.
    pub const fn new(id: u32, name: &'static str, kind: ValueKind) -> Self {
        Self::from_descriptor(FieldDescriptor::new(id, name, kind))
    }

    /// A required, writable, non-secret field with the given description.
    pub const fn from_descriptor(field: FieldDescriptor) -> Self {
        Self {
            field,
            writable: true,
            secret: false,
            optional: false,
            min: None,
            max: None,
        }
    }

    /// Sets the description.
    pub const fn describe(mut self, description: &'static str) -> Self {
        self.field = self.field.describe(description);
        self
    }

    /// Sets the unit.
    pub const fn unit(mut self, unit: &'static str) -> Self {
        self.field = self.field.unit(unit);
        self
    }

    /// Sets the allowed variants of an enum field.
    pub const fn variants(mut self, variants: &'static [&'static str]) -> Self {
        self.field = self.field.variants(variants);
        self
    }

    /// Bounds a numeric field, inclusive.
    pub const fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    /// Marks the field as optional.
    pub const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Marks the field as a write-only secret.
    pub const fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Marks the field as not changeable by clients.
    pub const fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }

    /// Checks what the schema can express: the kind, the enum variants and
    /// the numeric range. Cross-field and domain rules belong to the typed
    /// section.
    pub fn check(&self, value: &Value) -> Result<(), FieldError> {
        if value.kind() != self.field.kind {
            return Err(FieldError::WrongKind);
        }
        if let Value::Enum(variant) = value
            && !self.field.variants.contains(&variant.as_str())
        {
            return Err(FieldError::OutOfRange);
        }
        if let Some(number) = value.as_number() {
            if number.is_nan() {
                return Err(FieldError::OutOfRange);
            }
            if self.min.is_some_and(|min| number < min) || self.max.is_some_and(|max| number > max)
            {
                return Err(FieldError::OutOfRange);
            }
        }
        Ok(())
    }
}
