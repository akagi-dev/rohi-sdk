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
//! The bounded, owned form of one log event.

use core::fmt::{self, Write};

use heapless::String;
use rohi_time::UnixTime;

use crate::LogLevel;

/// Longest `target` kept, in bytes. Longer targets are cut.
pub const MAX_TARGET_LEN: usize = 24;
/// Longest `message` kept, in bytes. Longer messages are cut and flagged.
pub const MAX_MESSAGE_LEN: usize = 120;

/// One log event converted at the call site into owned, bounded data.
///
/// `sequence` is zero until the logging task assigns it when it takes the
/// record from the queue, so ordering across reboots is decided in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// Position in the persistent history; 0 before it is assigned.
    pub sequence: u64,
    /// Milliseconds since boot when the event happened.
    pub monotonic_ms: u64,
    /// Synchronized wall-clock time when the event happened, if known.
    pub wall_time: Option<UnixTime>,
    /// Severity, never [`LogLevel::Off`].
    pub level: LogLevel,
    /// The `log` target, cut to [`MAX_TARGET_LEN`] bytes.
    pub target: String<MAX_TARGET_LEN>,
    /// The formatted message, cut to [`MAX_MESSAGE_LEN`] bytes.
    pub message: String<MAX_MESSAGE_LEN>,
    /// Whether `target` or `message` was cut.
    pub truncated: bool,
}

impl LogRecord {
    /// Formats `args` into a record. Text that does not fit is cut at a
    /// character boundary and `truncated` is set; formatting never fails.
    pub fn new(
        level: LogLevel,
        target: &str,
        args: fmt::Arguments<'_>,
        monotonic_ms: u64,
        wall_time: Option<UnixTime>,
    ) -> Self {
        let mut truncated = false;
        let mut record = Self {
            sequence: 0,
            monotonic_ms,
            wall_time,
            level,
            target: String::new(),
            message: String::new(),
            truncated: false,
        };
        truncated |= push_cut(&mut record.target, target);
        let mut writer = Cutting {
            out: &mut record.message,
            cut: false,
        };
        // Cutting::write_str never fails; an error here is the user's Display
        // impl, which leaves the part written so far.
        let _ = writer.write_fmt(args);
        truncated |= writer.cut;
        record.truncated = truncated;
        record
    }
}

fn push_cut<const N: usize>(out: &mut String<N>, text: &str) -> bool {
    let room = N - out.len();
    let mut end = text.len().min(room);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let _ = out.push_str(&text[..end]);
    end < text.len()
}

struct Cutting<'a, const N: usize> {
    out: &'a mut String<N>,
    cut: bool,
}

impl<const N: usize> Write for Cutting<'_, N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if push_cut(self.out, text) {
            self.cut = true;
        }
        Ok(())
    }
}
