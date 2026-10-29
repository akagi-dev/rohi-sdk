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
use rohi_config::FieldError;
use rohi_config::{Value, ValueKind};

/// Unit of a numeric field, a compact code instead of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Unitless.
    None,
    /// Milliseconds.
    Ms,
    /// Seconds.
    S,
    /// Hertz.
    Hz,
    /// Decibel-milliwatts.
    Dbm,
    /// Bytes.
    Bytes,
    /// Percent.
    Percent,
    /// Degrees of an angle.
    Degrees,
    /// Meters.
    Meters,
    /// Degrees Celsius.
    Celsius,
    /// Pascals.
    Pascal,
    /// Parts per million.
    Ppm,
    /// Parts per billion.
    Ppb,
    /// Decibels.
    Decibels,
    /// Micrograms per cubic meter.
    MicrogramsPerCubicMeter,
    /// Microsieverts per hour.
    MicrosievertsPerHour,
}

/// One named value of an enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumVariant {
    /// The number carried by [`Value::Enum`]. Stable: only ever added.
    pub value: u32,
    /// Stable machine-readable name.
    pub name: &'static str,
}

impl EnumVariant {
    /// A variant.
    pub const fn new(value: u32, name: &'static str) -> Self {
        Self { value, name }
    }
}

/// The variants of an enum, referenced by [`FieldDescriptor::enum_id`].
///
/// Variants are not part of the field: a section, service or action lists
/// its enums once and fields that are no enums carry nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumDescriptor {
    /// Non-zero, unique within the section, service or action.
    pub id: u32,
    /// The named values.
    pub variants: &'static [EnumVariant],
}

impl EnumDescriptor {
    /// An enum with the given variants.
    pub const fn new(id: u32, variants: &'static [EnumVariant]) -> Self {
        Self { id, variants }
    }

    /// Whether `value` is a variant.
    pub fn contains(&self, value: u32) -> bool {
        self.variants.iter().any(|variant| variant.value == value)
    }

    /// The name of `value`.
    pub fn name_of(&self, value: u32) -> Option<&'static str> {
        self.variants
            .iter()
            .find(|variant| variant.value == value)
            .map(|variant| variant.name)
    }

    /// The value named `name`.
    pub fn value_of(&self, name: &str) -> Option<u32> {
        self.variants
            .iter()
            .find(|variant| variant.name == name)
            .map(|variant| variant.value)
    }
}

/// Whether `value` is a variant of the enum `id` in `enums`.
pub(crate) fn enum_allows(enums: &[EnumDescriptor], id: u32, value: u32) -> bool {
    enums
        .iter()
        .find(|descriptor| descriptor.id == id)
        .is_some_and(|descriptor| descriptor.contains(value))
}

/// Static description of one field of a config section, a service or an
/// action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldDescriptor {
    /// Stable within the section or service: only ever added, never reused.
    pub id: u32,
    /// Stable name; nested fields use dotted paths (`tls.enabled`).
    pub name: &'static str,
    /// Type of the value.
    pub kind: ValueKind,
    /// Unit of the value.
    pub unit: Unit,
    /// The [`EnumDescriptor`] of an [`ValueKind::Enum`] field; 0 otherwise.
    pub enum_id: u32,
}

impl FieldDescriptor {
    /// A unitless field that is no enum.
    pub const fn new(id: u32, name: &'static str, kind: ValueKind) -> Self {
        Self {
            id,
            name,
            kind,
            unit: Unit::None,
            enum_id: 0,
        }
    }

    /// Sets the unit.
    pub const fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    /// Sets the enum of an [`ValueKind::Enum`] field.
    pub const fn with_enum(mut self, enum_id: u32) -> Self {
        self.enum_id = enum_id;
        self
    }
}

/// Inclusive bounds of a numeric field, in the type of the field. They are
/// checked by the device and never sent to clients; integers are never
/// converted to floating point to be compared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bounds {
    /// For [`ValueKind::I64`].
    I64(i64, i64),
    /// For [`ValueKind::U64`].
    U64(u64, u64),
    /// For [`ValueKind::F32`]; `NaN` is never within the bounds.
    F32(f32, f32),
}

impl Bounds {
    /// Whether `value` is of the bounded kind and within the bounds.
    pub fn contains(&self, value: &Value) -> bool {
        match (*self, value) {
            (Self::I64(min, max), Value::I64(value)) => (min..=max).contains(value),
            (Self::U64(min, max), Value::U64(value)) => (min..=max).contains(value),
            (Self::F32(min, max), Value::F32(value)) => (min..=max).contains(value),
            _ => false,
        }
    }
}

/// A [`FieldDescriptor`] extended with the config-only attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfigField {
    /// Common description.
    pub field: FieldDescriptor,
    /// Whether clients may change the field.
    pub writable: bool,
    /// Write-only: never returned, only reported as set.
    pub secret: bool,
    /// May be unset; unset is distinct from an empty string or zero.
    pub optional: bool,
    /// Inclusive bounds of a numeric field.
    pub bounds: Option<Bounds>,
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
            bounds: None,
        }
    }

    /// Sets the unit.
    pub const fn unit(mut self, unit: Unit) -> Self {
        self.field = self.field.unit(unit);
        self
    }

    /// Sets the enum of an enum field.
    pub const fn with_enum(mut self, enum_id: u32) -> Self {
        self.field = self.field.with_enum(enum_id);
        self
    }

    /// Bounds a numeric field, inclusive.
    pub const fn bounds(mut self, bounds: Bounds) -> Self {
        self.bounds = Some(bounds);
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

    /// Checks what the schema can express: the kind, the enum variants in
    /// `enums` and the bounds. Cross-field and domain rules belong to the
    /// typed section.
    pub fn check(&self, value: &Value, enums: &[EnumDescriptor]) -> Result<(), FieldError> {
        if value.kind() != self.field.kind {
            return Err(FieldError::WrongKind);
        }
        if let Value::Enum(variant) = value
            && !enum_allows(enums, self.field.enum_id, *variant)
        {
            return Err(FieldError::OutOfRange);
        }
        if let Some(bounds) = &self.bounds
            && !bounds.contains(value)
        {
            return Err(FieldError::OutOfRange);
        }
        Ok(())
    }
}
