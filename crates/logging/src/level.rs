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
//! Severity of a persistent log record.

/// The levels the persistent log can keep, from nothing to `debug`.
///
/// The numeric values are ordered by verbosity: a record is kept when its
/// level is at most the configured one. `trace` is not offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum LogLevel {
    /// Nothing is persisted.
    Off = 0,
    /// Errors only.
    Error = 1,
    /// Warnings and errors.
    #[default]
    Warn = 2,
    /// Informational records and above.
    Info = 3,
    /// Everything but `trace`.
    Debug = 4,
}

impl LogLevel {
    /// The level with this number, `None` outside `0..=4`.
    pub const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Off,
            1 => Self::Error,
            2 => Self::Warn,
            3 => Self::Info,
            4 => Self::Debug,
            _ => return None,
        })
    }

    /// The stable name used in text lines and the CLI.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// The record level of a `log` level; `trace` has none.
    pub const fn from_log(level: log::Level) -> Option<Self> {
        match level {
            log::Level::Error => Some(Self::Error),
            log::Level::Warn => Some(Self::Warn),
            log::Level::Info => Some(Self::Info),
            log::Level::Debug => Some(Self::Debug),
            log::Level::Trace => None,
        }
    }

    /// Whether a record of `level` is kept when this is the configured level.
    pub const fn keeps(self, level: log::Level) -> bool {
        match Self::from_log(level) {
            Some(record) => (record as u8) <= (self as u8),
            None => false,
        }
    }
}
