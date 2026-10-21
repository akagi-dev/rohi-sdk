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

use core::fmt;

/// Platform error of the ESP configuration flash backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EspStorageError {
    /// The access leaves the configuration region.
    OutOfBounds,
    /// Offset or length violates the flash write/erase granularity.
    InvalidAlignment,
    /// The region is not usable (unaligned, beyond flash capacity or empty).
    InvalidRegion,
    ReadFailed,
    WriteFailed,
    EraseFailed,
}

impl fmt::Display for EspStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfBounds => "access outside the configuration region",
            Self::InvalidAlignment => "unaligned flash access",
            Self::InvalidRegion => "invalid configuration flash region",
            Self::ReadFailed => "flash read failed",
            Self::WriteFailed => "flash write failed",
            Self::EraseFailed => "flash erase failed",
        })
    }
}

impl core::error::Error for EspStorageError {}
