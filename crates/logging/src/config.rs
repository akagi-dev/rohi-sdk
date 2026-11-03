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
//! The `logging` config section.

use minicbor::{CborLen, Decode, Encode};
use rohi_config::{Text, TextValue, Variant, VariantValue};
use rohi_service::{Config, EnumDescriptor, EnumVariant, FieldError};

use crate::LogLevel;

/// Smallest and largest segment, in bytes. A segment is the unit of rotation,
/// eviction and download, and a multiple of the flash erase size.
pub const SEGMENT_BYTES_RANGE: (u32, u32) = (4 * 1024, 64 * 1024);
/// Granularity of `segment_bytes`: the erase sector of the internal flash.
pub const SEGMENT_GRANULE: u32 = 4 * 1024;
/// Smallest and largest `retention_bytes`.
pub const RETENTION_BYTES_RANGE: (u32, u32) = (8 * 1024, 64 * 1024 * 1024);
/// Smallest and largest `flush_interval`, in milliseconds.
pub const FLUSH_INTERVAL_RANGE_MS: (u32, u32) = (1_000, 600_000);

/// Default retention budget: 128 KiB.
pub const DEFAULT_RETENTION_BYTES: u32 = 128 * 1024;
/// Default segment size: 8 KiB.
pub const DEFAULT_SEGMENT_BYTES: u32 = 8 * 1024;
/// Default flush interval: 10 s.
pub const DEFAULT_FLUSH_INTERVAL_MS: u32 = 10_000;

/// Persistent level of a production build.
pub const PRODUCTION_LEVEL: LogLevel = LogLevel::Warn;
/// Persistent level of a development or test build.
pub const DEVELOPMENT_LEVEL: LogLevel = LogLevel::Info;

/// Id of the enum of the `level` field.
pub const LEVEL_ENUM: u32 = 1;

/// The enum of the `level` config field.
pub const LEVELS: &[EnumDescriptor] = &[EnumDescriptor::new(
    LEVEL_ENUM,
    &[
        EnumVariant::new(0, "off"),
        EnumVariant::new(1, "error"),
        EnumVariant::new(2, "warn"),
        EnumVariant::new(3, "info"),
        EnumVariant::new(4, "debug"),
    ],
)];

impl VariantValue for LogLevel {
    fn variant(&self) -> u32 {
        *self as u32
    }

    fn from_variant(variant: u32) -> Option<Self> {
        u8::try_from(variant).ok().and_then(Self::from_u8)
    }
}

/// Where the log history is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum LogStorageKind {
    /// The dedicated internal flash region.
    #[default]
    #[n(0)]
    Internal,
    /// An SD card.
    #[n(1)]
    Sd,
}

impl LogStorageKind {
    /// The stable name: `internal` or `sd`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Sd => "sd",
        }
    }
}

impl TextValue for LogStorageKind {
    fn from_text(text: &str) -> Option<Self> {
        match text {
            "internal" => Some(Self::Internal),
            "sd" => Some(Self::Sd),
            _ => None,
        }
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.name())
    }
}

/// How new segments are compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum Compression {
    /// Plain text segments (`.log`).
    #[n(0)]
    None,
    /// LZ4 frames (`.log.lz4`).
    #[default]
    #[n(1)]
    Lz4,
}

impl Compression {
    /// The stable name: `none` or `lz4`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Lz4 => "lz4",
        }
    }
}

impl TextValue for Compression {
    fn from_text(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::None),
            "lz4" => Some(Self::Lz4),
            _ => None,
        }
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.name())
    }
}

/// The effective `logging` config section.
///
/// | Field | Id | Type | Meaning |
/// |-------|----|------|---------|
/// | `level` | 1 | enum `off`/`error`/`warn`/`info`/`debug` | what the persistent log keeps |
/// | `storage` | 2 | text `internal`/`sd` | the backend that holds the history |
/// | `compression` | 3 | text `none`/`lz4` | codec of **new** segments |
/// | `retention_bytes` | 4 | bytes | budget of the active backend |
/// | `segment_bytes` | 5 | bytes | rotation granularity |
/// | `flush_interval` | 6 | ms | longest a record waits in RAM |
///
/// `storage` and `compression` are text, not enums, because a section can
/// describe one enum ([`MAX_ENUMS`](rohi_service::MAX_ENUMS)) and `level` is
/// the one with the most use for discovery.
///
/// # Validation
///
/// The whole effective section is checked before anything is stored:
/// every field is in its range, `segment_bytes` is a multiple of
/// [`SEGMENT_GRANULE`] and `retention_bytes` holds at least two segments, so
/// rotation always has a sealed segment to evict.
///
/// ```
/// use rohi_logging::{Compression, LogLevel, LoggingConfig};
///
/// let config = LoggingConfig::default();
/// assert_eq!(config.level, LogLevel::Warn);
/// assert_eq!(config.compression, Compression::Lz4);
/// assert!(config.validate().is_ok());
///
/// let one_segment = LoggingConfig { retention_bytes: config.segment_bytes, ..config };
/// assert!(one_segment.validate().is_err());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Config)]
#[config(
    name = "logging",
    overrides = LoggingOverrides,
    version = 1,
    validate = validate_section,
    enums = LEVELS
)]
pub struct LoggingConfig {
    /// What the persistent log keeps.
    #[config(id = 1, with = Variant, enum_id = LEVEL_ENUM)]
    pub level: LogLevel,
    /// The backend that holds the history.
    #[config(id = 2, with = Text)]
    pub storage: LogStorageKind,
    /// Codec of newly created segments.
    #[config(id = 3, with = Text)]
    pub compression: Compression,
    /// Budget of the active backend, in bytes.
    #[config(
        id = 4,
        unit = Bytes,
        min = RETENTION_BYTES_RANGE.0,
        max = RETENTION_BYTES_RANGE.1
    )]
    pub retention_bytes: u32,
    /// Size at which a segment is sealed, in bytes.
    #[config(
        id = 5,
        unit = Bytes,
        min = SEGMENT_BYTES_RANGE.0,
        max = SEGMENT_BYTES_RANGE.1
    )]
    pub segment_bytes: u32,
    /// Longest a record waits in RAM before it is written.
    #[config(
        id = 6,
        name = "flush_interval",
        unit = Ms,
        min = FLUSH_INTERVAL_RANGE_MS.0,
        max = FLUSH_INTERVAL_RANGE_MS.1
    )]
    pub flush_interval_ms: u32,
}

impl LoggingConfig {
    /// The defaults: [`PRODUCTION_LEVEL`], internal flash, LZ4, 128 KiB in
    /// 8 KiB segments, flushed every 10 s.
    pub const DEFAULT: Self = Self {
        level: PRODUCTION_LEVEL,
        storage: LogStorageKind::Internal,
        compression: Compression::Lz4,
        retention_bytes: DEFAULT_RETENTION_BYTES,
        segment_bytes: DEFAULT_SEGMENT_BYTES,
        flush_interval_ms: DEFAULT_FLUSH_INTERVAL_MS,
    };
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn within(value: u32, (min, max): (u32, u32)) -> bool {
    (min..=max).contains(&value)
}

impl LoggingConfig {
    /// Checks the section as a whole.
    ///
    /// # Errors
    ///
    /// [`FieldError::Invalid`] when a value is out of its range,
    /// `segment_bytes` is not a multiple of [`SEGMENT_GRANULE`] or
    /// `retention_bytes` holds fewer than two segments.
    pub fn validate(&self) -> Result<(), FieldError> {
        let valid = within(self.segment_bytes, SEGMENT_BYTES_RANGE)
            && self.segment_bytes.is_multiple_of(SEGMENT_GRANULE)
            && within(self.retention_bytes, RETENTION_BYTES_RANGE)
            && self.retention_bytes / 2 >= self.segment_bytes
            && within(self.flush_interval_ms, FLUSH_INTERVAL_RANGE_MS);
        if valid {
            Ok(())
        } else {
            Err(FieldError::Invalid)
        }
    }

    /// The firmware policy of a build profile: the level a development or
    /// test build persists, [`PRODUCTION_LEVEL`] otherwise. Everything else
    /// keeps its default. The user can still override it at runtime.
    pub fn firmware_overrides(development: bool) -> LoggingOverrides {
        LoggingOverrides {
            level: rohi_config::Override::Value(if development {
                DEVELOPMENT_LEVEL
            } else {
                PRODUCTION_LEVEL
            }),
            ..Default::default()
        }
    }
}

fn validate_section(config: &LoggingConfig) -> Result<(), FieldError> {
    config.validate()
}
