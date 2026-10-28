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
use heapless::{String, Vec};
use zeroize::Zeroize;

/// Maximum length of a string or bytes [`Value`], in bytes.
pub const MAX_VALUE_LEN: usize = 64;

/// Primitive type of a [`Value`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// `true` or `false`.
    Bool,
    /// Signed 64-bit integer.
    I64,
    /// Unsigned 64-bit integer.
    U64,
    /// 32-bit float.
    F32,
    /// UTF-8 text.
    String,
    /// Arbitrary bytes.
    Bytes,
    /// One of the variants of the enum the field descriptor refers to.
    Enum,
}

/// A generic typed value, the common currency of config and status.
///
/// `Debug` shows only the kind: a value can be a secret.
#[derive(Clone, PartialEq)]
pub enum Value {
    /// See [`ValueKind::Bool`].
    Bool(bool),
    /// See [`ValueKind::I64`].
    I64(i64),
    /// See [`ValueKind::U64`].
    U64(u64),
    /// See [`ValueKind::F32`].
    F32(f32),
    /// See [`ValueKind::String`].
    String(String<MAX_VALUE_LEN>),
    /// See [`ValueKind::Bytes`].
    Bytes(Vec<u8, MAX_VALUE_LEN>),
    /// See [`ValueKind::Enum`].
    Enum(u32),
}

impl Value {
    /// The kind of the value.
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::I64(_) => ValueKind::I64,
            Self::U64(_) => ValueKind::U64,
            Self::F32(_) => ValueKind::F32,
            Self::String(_) => ValueKind::String,
            Self::Bytes(_) => ValueKind::Bytes,
            Self::Enum(_) => ValueKind::Enum,
        }
    }

    /// A text value; `None` if `text` exceeds [`MAX_VALUE_LEN`].
    pub fn text(text: &str) -> Option<Self> {
        String::try_from(text).ok().map(Self::String)
    }

    /// A bytes value; `None` if `bytes` exceed [`MAX_VALUE_LEN`].
    pub fn bytes(bytes: &[u8]) -> Option<Self> {
        Vec::from_slice(bytes).ok().map(Self::Bytes)
    }

    /// The number of an enum value, if this is a [`Value::Enum`].
    pub const fn as_enum(&self) -> Option<u32> {
        match self {
            Self::Enum(value) => Some(*value),
            _ => None,
        }
    }

    /// The boolean, if this is a [`Value::Bool`].
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The integer, if this is a [`Value::U64`].
    pub const fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(value) => Some(*value),
            _ => None,
        }
    }

    /// The integer, if this is a [`Value::I64`].
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(*value),
            _ => None,
        }
    }

    /// The float, if this is a [`Value::F32`].
    pub const fn as_f32(&self) -> Option<f32> {
        match self {
            Self::F32(value) => Some(*value),
            _ => None,
        }
    }

    /// The text of a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// The bytes of a [`Value::Bytes`].
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value),
            _ => None,
        }
    }
}

impl core::fmt::Debug for Value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Value({:?})", self.kind())
    }
}

impl Value {
    /// Overwrites the content of a text or bytes value, for values that held
    /// a secret.
    pub fn wipe(&mut self) {
        match self {
            Self::String(text) => {
                // SAFETY: only zero bytes are written, which are valid UTF-8.
                unsafe { text.as_mut_vec() }.as_mut_slice().zeroize();
                text.clear();
            }
            Self::Bytes(bytes) => {
                bytes.as_mut_slice().zeroize();
                bytes.clear();
            }
            Self::Enum(_) | Self::Bool(_) | Self::I64(_) | Self::U64(_) | Self::F32(_) => {}
        }
    }
}

/// Why a field access or a whole-section validation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// The section has no field with this id.
    UnknownField,
    /// The field cannot be changed by clients.
    ReadOnly,
    /// The value has the wrong kind.
    WrongKind,
    /// The value is outside the range or variants of the field.
    OutOfRange,
    /// The value or the resulting section is rejected by the typed
    /// validation.
    Invalid,
    /// The field is required: it cannot be set to "no value".
    NotOptional,
    /// The field is a write-only secret: its value is never read, only
    /// whether it is set.
    Secret,
}
