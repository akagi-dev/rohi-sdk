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

//! Conversion between the typed fields of a managed config (`rohi_service::Config`)
//! and the generic [`Value`].
//!
//! `#[derive(Config)]` generates the field access of a section. A field type
//! it knows how to convert is handled by [`Standard`]; a domain type that is
//! text or an enum number implements [`TextValue`] or [`VariantValue`] and is
//! named with `#[config(with = Text)]` or `#[config(with = Variant)]`; any
//! other type names its own codec with `#[config(with = MyCodec)]`. Domain types are never
//! guessed: an SSID is not just a string and a hostname is not just text, so
//! the codec of the type decides which values are valid.
//!
//! A codec only converts and checks what the type can express (the kind, a
//! range that fits the type, a domain rule). The kind of the schema field, its
//! enum variants and its numeric bounds are checked by the
//! `Section` before the codec is asked, and rules across
//! fields belong to the validation of the config.
//!
//! ```
//! use rohi_config::{Codec, FieldError, Standard, Value, ValueKind};
//!
//! assert_eq!(<Standard as Codec<u32>>::KIND, ValueKind::U64);
//! assert_eq!(<Standard as Codec<u32>>::encode(&7), Ok(Some(Value::U64(7))));
//! assert_eq!(
//!     <Standard as Codec<u8>>::decode(&Value::U64(300)),
//!     Err(FieldError::OutOfRange)
//! );
//! ```

use heapless::String;

use crate::value::{FieldError, Value, ValueKind};

/// How a field of type `T` appears as a [`Value`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no conversion between `{T}` and a config value",
    label = "this type needs its own codec",
    note = "implement `rohi_config::Codec<{T}>` for a codec type and name it on the field: `#[config(with = MyCodec)]`"
)]
pub trait Codec<T> {
    /// The kind of the generic value of the field.
    const KIND: ValueKind;

    /// The value of the field. `Ok(None)` is a field that has no value (the
    /// type has a state that means "not given"); a value that cannot be
    /// expressed is an error and is never cut. A secret field is never
    /// encoded: the generic interface only asks whether it is set.
    fn encode(value: &T) -> Result<Option<Value>, FieldError>;

    /// The field from a value, which already has the [`KIND`](Self::KIND) of
    /// the field. Any other kind is [`FieldError::WrongKind`], a value the type
    /// cannot hold is [`FieldError::OutOfRange`] or [`FieldError::Invalid`].
    fn decode(value: &Value) -> Result<T, FieldError>;
}

/// The codec of the types that have an obvious generic value: `bool`, the
/// unsigned integers (as [`ValueKind::U64`]), the signed integers (as
/// [`ValueKind::I64`]), `f32` and `heapless::String<N>`.
///
/// Integers are converted with a range check, never by truncation. A text that
/// does not fit the string, or the generic value, is rejected.
pub struct Standard;

impl Codec<bool> for Standard {
    const KIND: ValueKind = ValueKind::Bool;

    fn encode(value: &bool) -> Result<Option<Value>, FieldError> {
        Ok(Some(Value::Bool(*value)))
    }

    fn decode(value: &Value) -> Result<bool, FieldError> {
        value.as_bool().ok_or(FieldError::WrongKind)
    }
}

macro_rules! unsigned {
    ($($ty:ty),+) => {$(
        impl Codec<$ty> for Standard {
            const KIND: ValueKind = ValueKind::U64;

            fn encode(value: &$ty) -> Result<Option<Value>, FieldError> {
                Ok(Some(Value::U64(u64::from(*value))))
            }

            fn decode(value: &Value) -> Result<$ty, FieldError> {
                let number = value.as_u64().ok_or(FieldError::WrongKind)?;
                <$ty>::try_from(number).map_err(|_| FieldError::OutOfRange)
            }
        }
    )+};
}

macro_rules! signed {
    ($($ty:ty),+) => {$(
        impl Codec<$ty> for Standard {
            const KIND: ValueKind = ValueKind::I64;

            fn encode(value: &$ty) -> Result<Option<Value>, FieldError> {
                Ok(Some(Value::I64(i64::from(*value))))
            }

            fn decode(value: &Value) -> Result<$ty, FieldError> {
                let number = value.as_i64().ok_or(FieldError::WrongKind)?;
                <$ty>::try_from(number).map_err(|_| FieldError::OutOfRange)
            }
        }
    )+};
}

unsigned!(u8, u16, u32, u64);
signed!(i8, i16, i32, i64);

impl Codec<f32> for Standard {
    const KIND: ValueKind = ValueKind::F32;

    fn encode(value: &f32) -> Result<Option<Value>, FieldError> {
        Ok(Some(Value::F32(*value)))
    }

    fn decode(value: &Value) -> Result<f32, FieldError> {
        value.as_f32().ok_or(FieldError::WrongKind)
    }
}

impl<const N: usize> Codec<String<N>> for Standard {
    const KIND: ValueKind = ValueKind::String;

    fn encode(value: &String<N>) -> Result<Option<Value>, FieldError> {
        Value::text(value).map(Some).ok_or(FieldError::Invalid)
    }

    fn decode(value: &Value) -> Result<String<N>, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        String::try_from(text).map_err(|_| FieldError::OutOfRange)
    }
}

/// A type that is configured as text: a name, a host, a credential.
///
/// The type decides what a valid text is, so a domain type is never silently
/// treated as any string: it opts in by implementing this trait and is named
/// on a field with `#[config(with = Text)]`.
pub trait TextValue: Sized {
    /// The type from text; `None` when the text is not valid for it. The
    /// conversion must neither truncate nor repair.
    fn from_text(text: &str) -> Option<Self>;

    /// The text of the value; `None` when it has none to show: it is not UTF-8,
    /// or it is a secret, which is never converted to a value.
    fn as_text(&self) -> Option<&str>;
}

/// The codec of a [`TextValue`], shown as [`ValueKind::String`].
pub struct Text;

impl<T: TextValue> Codec<T> for Text {
    const KIND: ValueKind = ValueKind::String;

    fn encode(value: &T) -> Result<Option<Value>, FieldError> {
        value
            .as_text()
            .and_then(Value::text)
            .map(Some)
            .ok_or(FieldError::Invalid)
    }

    fn decode(value: &Value) -> Result<T, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        T::from_text(text).ok_or(FieldError::Invalid)
    }
}

/// Text of any length the generic value holds, for tests of crates with `std`.
#[cfg(any(test, feature = "std"))]
impl TextValue for std::string::String {
    fn from_text(text: &str) -> Option<Self> {
        Some(text.into())
    }

    fn as_text(&self) -> Option<&str> {
        Some(self)
    }
}

/// A type that is one of the numbered variants of an enum field.
///
/// The numbers are those of the enum table of the section; the `Section`
/// has already checked that a written number is one of them, so
/// [`from_variant`](Self::from_variant) only maps the number to the type.
pub trait VariantValue: Sized {
    /// The number of the variant.
    fn variant(&self) -> u32;

    /// The type of a variant number; `None` for a number it has no value for.
    fn from_variant(variant: u32) -> Option<Self>;
}

/// A plain variant number.
impl VariantValue for u32 {
    fn variant(&self) -> u32 {
        *self
    }

    fn from_variant(variant: u32) -> Option<Self> {
        Some(variant)
    }
}

/// The codec of a [`VariantValue`], shown as [`ValueKind::Enum`].
pub struct Variant;

impl<T: VariantValue> Codec<T> for Variant {
    const KIND: ValueKind = ValueKind::Enum;

    fn encode(value: &T) -> Result<Option<Value>, FieldError> {
        Ok(Some(Value::Enum(value.variant())))
    }

    fn decode(value: &Value) -> Result<T, FieldError> {
        let variant = value.as_enum().ok_or(FieldError::WrongKind)?;
        T::from_variant(variant).ok_or(FieldError::OutOfRange)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_VALUE_LEN;

    fn round_trip<T: PartialEq + core::fmt::Debug>(value: T, expected: Value)
    where
        Standard: Codec<T>,
    {
        assert_eq!(Standard::encode(&value), Ok(Some(expected.clone())));
        assert_eq!(<Standard as Codec<T>>::decode(&expected), Ok(value));
    }

    #[test]
    fn primitives_round_trip_as_their_generic_kind() {
        round_trip(true, Value::Bool(true));
        round_trip(200u8, Value::U64(200));
        round_trip(60_000u16, Value::U64(60_000));
        round_trip(4_000_000_000u32, Value::U64(4_000_000_000));
        round_trip(u64::MAX, Value::U64(u64::MAX));
        round_trip(-100i8, Value::I64(-100));
        round_trip(i16::MIN, Value::I64(-32_768));
        round_trip(i32::MAX, Value::I64(2_147_483_647));
        round_trip(i64::MIN, Value::I64(i64::MIN));
        round_trip(1.5f32, Value::F32(1.5));
        round_trip(
            String::<8>::try_from("abc").unwrap(),
            Value::text("abc").unwrap(),
        );
    }

    #[test]
    fn the_kind_follows_the_type() {
        assert_eq!(<Standard as Codec<bool>>::KIND, ValueKind::Bool);
        assert_eq!(<Standard as Codec<u8>>::KIND, ValueKind::U64);
        assert_eq!(<Standard as Codec<u64>>::KIND, ValueKind::U64);
        assert_eq!(<Standard as Codec<i8>>::KIND, ValueKind::I64);
        assert_eq!(<Standard as Codec<f32>>::KIND, ValueKind::F32);
        assert_eq!(<Standard as Codec<String<4>>>::KIND, ValueKind::String);
    }

    #[test]
    fn a_value_that_does_not_fit_the_type_is_never_truncated() {
        assert_eq!(
            <Standard as Codec<u8>>::decode(&Value::U64(256)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            <Standard as Codec<u32>>::decode(&Value::U64(u64::from(u32::MAX) + 1)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            <Standard as Codec<i8>>::decode(&Value::I64(-129)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            <Standard as Codec<String<3>>>::decode(&Value::text("abcd").unwrap()),
            Err(FieldError::OutOfRange)
        );
        // A string wider than the generic value cannot be shown.
        let wide =
            String::<{ MAX_VALUE_LEN + 1 }>::try_from(&"x".repeat(MAX_VALUE_LEN + 1)[..]).unwrap();
        assert_eq!(Standard::encode(&wide), Err(FieldError::Invalid));
    }

    #[test]
    fn another_kind_is_a_wrong_kind() {
        assert_eq!(
            <Standard as Codec<bool>>::decode(&Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            <Standard as Codec<u32>>::decode(&Value::I64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            <Standard as Codec<i32>>::decode(&Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            <Standard as Codec<f32>>::decode(&Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            <Standard as Codec<String<4>>>::decode(&Value::Bool(true)),
            Err(FieldError::WrongKind)
        );
    }

    /// A name of at most four bytes.
    #[derive(Debug, PartialEq)]
    struct Name(String<4>);

    impl TextValue for Name {
        fn from_text(text: &str) -> Option<Self> {
            (!text.is_empty()).then(|| String::try_from(text).ok().map(Self))?
        }

        fn as_text(&self) -> Option<&str> {
            Some(&self.0)
        }
    }

    /// A secret has no text to show.
    struct Pin;

    impl TextValue for Pin {
        fn from_text(text: &str) -> Option<Self> {
            (text.len() == 4).then_some(Self)
        }

        fn as_text(&self) -> Option<&str> {
            None
        }
    }

    #[test]
    fn a_text_value_decides_what_text_is_valid() {
        assert_eq!(<Text as Codec<Name>>::KIND, ValueKind::String);
        let name = Name(String::try_from("abc").unwrap());
        assert_eq!(
            <Text as Codec<Name>>::encode(&name),
            Ok(Some(Value::text("abc").unwrap()))
        );
        assert_eq!(
            <Text as Codec<Name>>::decode(&Value::text("abc").unwrap()),
            Ok(name)
        );
        // The type refuses what it cannot hold; nothing is cut or repaired.
        for refused in ["", "abcde"] {
            assert_eq!(
                <Text as Codec<Name>>::decode(&Value::text(refused).unwrap()),
                Err(FieldError::Invalid)
            );
        }
        assert_eq!(
            <Text as Codec<Name>>::decode(&Value::U64(1)),
            Err(FieldError::WrongKind)
        );
    }

    #[test]
    fn a_value_without_text_is_not_converted() {
        assert!(<Text as Codec<Pin>>::decode(&Value::text("1234").unwrap()).is_ok());
        assert_eq!(<Text as Codec<Pin>>::encode(&Pin), Err(FieldError::Invalid));
    }

    #[test]
    fn std_strings_are_text() {
        let text = std::string::String::from("hello");
        assert_eq!(
            <Text as Codec<std::string::String>>::encode(&text),
            Ok(Some(Value::text("hello").unwrap()))
        );
        assert_eq!(
            <Text as Codec<std::string::String>>::decode(&Value::text("x").unwrap()),
            Ok("x".into())
        );
    }

    #[derive(Debug, PartialEq)]
    enum Mode {
        Fast,
        Slow,
    }

    impl VariantValue for Mode {
        fn variant(&self) -> u32 {
            match self {
                Self::Fast => 0,
                Self::Slow => 5,
            }
        }

        fn from_variant(variant: u32) -> Option<Self> {
            match variant {
                0 => Some(Self::Fast),
                5 => Some(Self::Slow),
                _ => None,
            }
        }
    }

    #[test]
    fn a_variant_value_is_an_enum_number() {
        assert_eq!(<Variant as Codec<Mode>>::KIND, ValueKind::Enum);
        assert_eq!(
            <Variant as Codec<Mode>>::encode(&Mode::Slow),
            Ok(Some(Value::Enum(5)))
        );
        assert_eq!(
            <Variant as Codec<Mode>>::decode(&Value::Enum(0)),
            Ok(Mode::Fast)
        );
        assert_eq!(
            <Variant as Codec<Mode>>::decode(&Value::Enum(3)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            <Variant as Codec<Mode>>::decode(&Value::U64(0)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(<Variant as Codec<u32>>::decode(&Value::Enum(7)), Ok(7));
        assert_eq!(
            <Variant as Codec<u32>>::encode(&7),
            Ok(Some(Value::Enum(7)))
        );
    }
}
