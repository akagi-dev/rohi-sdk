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

//! Conversion between the typed fields of a [`Config`](crate::Config) and the
//! generic [`Value`].
//!
//! `#[derive(Config)]` generates the field access of a section. A field type
//! it knows how to convert is handled by [`Standard`]; any other type names
//! its own codec with `#[config(with = MyCodec)]`. Domain types are never
//! guessed: an SSID is not just a string and a hostname is not just text, so
//! the codec of the type decides which values are valid.
//!
//! A codec only converts and checks what the type can express (the kind, a
//! range that fits the type, a domain rule). The kind of the schema field, its
//! enum variants and its numeric bounds are checked by the
//! [`Section`](crate::Section) before the codec is asked, and rules across
//! fields belong to the validation of the config.
//!
//! ```
//! use rohi_service::{Codec, FieldError, Standard, Value, ValueKind};
//!
//! assert_eq!(<Standard as Codec<u32>>::KIND, ValueKind::U64);
//! assert_eq!(<Standard as Codec<u32>>::encode(&7), Ok(Some(Value::U64(7))));
//! assert_eq!(
//!     <Standard as Codec<u8>>::decode(&Value::U64(300)),
//!     Err(FieldError::OutOfRange)
//! );
//! ```

use heapless::String;

use crate::config::FieldError;
use crate::value::{Value, ValueKind};

/// How a field of type `T` appears as a [`Value`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no conversion between `{T}` and a config value",
    label = "this type needs its own codec",
    note = "implement `rohi_service::Codec<{T}>` for a codec type and name it on the field: `#[config(with = MyCodec)]`"
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
}
