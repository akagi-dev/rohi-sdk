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

//! The bounded record of one panic.

use core::fmt::{self, Display, Write};

/// Longest panic message kept, in bytes. Longer text is cut at a character
/// boundary and the record is flagged.
pub const MAX_MESSAGE: usize = 112;
/// Longest source file path kept, in bytes: the **end** of the path, which names
/// the crate and the file.
pub const MAX_FILE: usize = 40;
/// Most backtrace addresses kept.
pub const MAX_FRAMES: usize = 10;
/// Most log lines of one record, see [`Crash::line`].
pub const MAX_LINES: usize = 4;

/// Bytes of the encoded record. A stored ABI: see the crate documentation.
pub(crate) const PAYLOAD: usize = 224;

const FLAG_MESSAGE_CUT: u8 = 1;
const FLAG_FILE_CUT: u8 = 2;

/// One panic: what it said, where it happened and the addresses that led to it.
///
/// A `Crash` is plain data of a fixed size (about 250 bytes) that the panic
/// path builds on the stack, without allocating. The `serial`, `missed` and
/// `streak` are assigned by [`Slot::capture`](crate::Slot::capture).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Crash {
    pub(crate) serial: u32,
    pub(crate) missed: u32,
    pub(crate) uptime_ms: u64,
    pub(crate) streak: u8,
    flags: u8,
    line: u32,
    column: u32,
    frame_count: u8,
    frames: [u32; MAX_FRAMES],
    file_len: u8,
    file: [u8; MAX_FILE],
    message_len: u8,
    message: [u8; MAX_MESSAGE],
}

impl Crash {
    /// An empty record of a panic `uptime_ms` milliseconds after boot.
    pub const fn new(uptime_ms: u64) -> Self {
        Self {
            serial: 0,
            missed: 0,
            uptime_ms,
            streak: 0,
            flags: 0,
            line: 0,
            column: 0,
            frame_count: 0,
            frames: [0; MAX_FRAMES],
            file_len: 0,
            file: [0; MAX_FILE],
            message_len: 0,
            message: [0; MAX_MESSAGE],
        }
    }

    /// Formats the panic message, keeping at most [`MAX_MESSAGE`] bytes.
    ///
    /// Control characters become spaces, so a message is one line.
    pub fn set_message(&mut self, message: fmt::Arguments<'_>) {
        let mut cut = Cut {
            buf: &mut self.message,
            len: 0,
            cut: false,
        };
        // A failing `Display` leaves the part written so far.
        let _ = cut.write_fmt(message);
        let (len, was_cut) = (cut.len, cut.cut);
        self.message_len = len as u8;
        self.set_flag(FLAG_MESSAGE_CUT, was_cut);
    }

    /// Sets the source location, keeping the last [`MAX_FILE`] bytes of `file`.
    /// A `line` or `column` of zero means unknown.
    pub fn set_location(&mut self, file: &str, line: u32, column: u32) {
        let mut start = file.len().saturating_sub(MAX_FILE);
        while start < file.len() && !file.is_char_boundary(start) {
            start += 1;
        }
        let tail = file.get(start..).unwrap_or("").as_bytes();
        if let Some(out) = self.file.get_mut(..tail.len()) {
            out.copy_from_slice(tail);
            self.file_len = tail.len() as u8;
        }
        self.set_flag(FLAG_FILE_CUT, start > 0);
        self.line = line;
        self.column = column;
    }

    /// Appends a backtrace address; `false` when the record has no room.
    pub fn push_frame(&mut self, address: u32) -> bool {
        match self.frames.get_mut(usize::from(self.frame_count)) {
            Some(slot) => {
                *slot = address;
                self.frame_count += 1;
                true
            }
            None => false,
        }
    }

    fn set_flag(&mut self, flag: u8, on: bool) {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
    }

    /// Milliseconds since boot when the panic happened.
    pub const fn uptime_ms(&self) -> u64 {
        self.uptime_ms
    }

    /// The number of this panic since the retained memory was last cleared by a
    /// power-up, starting at 1.
    pub const fn serial(&self) -> u32 {
        self.serial
    }

    /// Earlier panics whose record was replaced before it reached the log.
    pub const fn missed(&self) -> u32 {
        self.missed
    }

    /// Consecutive panics shortly after boot, this one included.
    pub const fn streak(&self) -> u8 {
        self.streak
    }

    /// The panic message.
    pub fn message(&self) -> &str {
        text(&self.message, self.message_len)
    }

    /// Whether the message was cut to [`MAX_MESSAGE`] bytes.
    pub const fn message_cut(&self) -> bool {
        self.flags & FLAG_MESSAGE_CUT != 0
    }

    /// The end of the source file path, empty when unknown.
    pub fn file(&self) -> &str {
        text(&self.file, self.file_len)
    }

    /// Whether the start of the file path was cut.
    pub const fn file_cut(&self) -> bool {
        self.flags & FLAG_FILE_CUT != 0
    }

    /// The source line, 0 when unknown.
    pub const fn line_number(&self) -> u32 {
        self.line
    }

    /// The source column, 0 when unknown.
    pub const fn column(&self) -> u32 {
        self.column
    }

    /// The backtrace addresses, innermost first.
    pub fn frames(&self) -> &[u32] {
        self.frames
            .get(..usize::from(self.frame_count))
            .unwrap_or(&[])
    }

    /// How many log lines [`line`](Self::line) has: the message, the location
    /// when known, the backtrace when there is one, and the identification.
    pub fn line_count(&self) -> usize {
        2 + usize::from(self.file_len > 0) + usize::from(self.frame_count > 0)
    }

    /// The `index`-th line of the record as it is written to the log, or `None`
    /// past [`line_count`](Self::line_count). A line is at most 120 bytes.
    pub fn line(&self, index: usize) -> Option<Line<'_>> {
        let mut kinds = [
            Kind::Message,
            Kind::Location,
            Kind::Backtrace,
            Kind::Summary,
        ]
        .into_iter()
        .filter(|kind| match kind {
            Kind::Location => self.file_len > 0,
            Kind::Backtrace => self.frame_count > 0,
            _ => true,
        });
        kinds.nth(index).map(|kind| Line { crash: self, kind })
    }

    pub(crate) fn encode(&self, pending: bool, out: &mut [u8; PAYLOAD]) {
        let mut put = Put { buf: out, pos: 0 };
        put.u8(u8::from(pending));
        put.u8(self.streak);
        put.u8(self.frame_count);
        put.u8(self.flags);
        put.u32(self.serial);
        put.u32(self.missed);
        put.u64(self.uptime_ms);
        put.u32(self.line);
        put.u32(self.column);
        put.u8(self.file_len);
        put.u8(self.message_len);
        put.u16(0);
        for frame in &self.frames {
            put.u32(*frame);
        }
        put.bytes(&self.file);
        put.bytes(&self.message);
    }

    /// `None` when a field is out of range or text is not UTF-8: a record that
    /// passed its checksum but was written by something else.
    pub(crate) fn decode(bytes: &[u8; PAYLOAD]) -> Option<(bool, Self)> {
        let mut get = Get { buf: bytes, pos: 0 };
        let pending = get.u8()?;
        let mut crash = Self::new(0);
        crash.streak = get.u8()?;
        crash.frame_count = get.u8()?;
        crash.flags = get.u8()?;
        crash.serial = get.u32()?;
        crash.missed = get.u32()?;
        crash.uptime_ms = get.u64()?;
        crash.line = get.u32()?;
        crash.column = get.u32()?;
        crash.file_len = get.u8()?;
        crash.message_len = get.u8()?;
        get.u16()?;
        for frame in &mut crash.frames {
            *frame = get.u32()?;
        }
        get.bytes(&mut crash.file)?;
        get.bytes(&mut crash.message)?;
        let valid = pending <= 1
            && usize::from(crash.frame_count) <= MAX_FRAMES
            && usize::from(crash.file_len) <= MAX_FILE
            && usize::from(crash.message_len) <= MAX_MESSAGE
            && core::str::from_utf8(crash.file.get(..usize::from(crash.file_len))?).is_ok()
            && core::str::from_utf8(crash.message.get(..usize::from(crash.message_len))?).is_ok();
        valid.then_some((pending == 1, crash))
    }
}

impl fmt::Debug for Crash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Crash")
            .field("serial", &self.serial)
            .field("uptime_ms", &self.uptime_ms)
            .field("streak", &self.streak)
            .field("missed", &self.missed)
            .field("message", &self.message())
            .field("file", &self.file())
            .field("line", &self.line)
            .field("frames", &self.frames())
            .finish()
    }
}

fn text(buf: &[u8], len: u8) -> &str {
    buf.get(..usize::from(len))
        .and_then(|bytes| core::str::from_utf8(bytes).ok())
        .unwrap_or("")
}

#[derive(Clone, Copy)]
enum Kind {
    Message,
    Location,
    Backtrace,
    Summary,
}

/// One line of a [`Crash`] as it is written to the log; format it with
/// `Display`.
#[derive(Clone, Copy)]
pub struct Line<'a> {
    crash: &'a Crash,
    kind: Kind,
}

impl Display for Line<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let crash = self.crash;
        match self.kind {
            Kind::Message => write!(f, "panic: {}", crash.message()),
            Kind::Location => {
                let cut = if crash.file_cut() { ".." } else { "" };
                write!(f, "at {cut}{}", crash.file())?;
                if crash.line > 0 {
                    write!(f, ":{}", crash.line)?;
                    if crash.column > 0 {
                        write!(f, ":{}", crash.column)?;
                    }
                }
                Ok(())
            }
            Kind::Backtrace => {
                f.write_str("backtrace")?;
                for frame in crash.frames() {
                    write!(f, " {frame:08x}")?;
                }
                Ok(())
            }
            Kind::Summary => {
                write!(f, "crash #{}", crash.serial)?;
                if crash.streak > 1 {
                    write!(f, ", {} in a row", crash.streak)?;
                }
                if crash.missed > 0 {
                    write!(f, ", {} earlier not logged", crash.missed)?;
                }
                Ok(())
            }
        }
    }
}

/// Writes text into a fixed buffer: control characters become spaces and what
/// does not fit is dropped at a character boundary.
struct Cut<'a> {
    buf: &'a mut [u8],
    len: usize,
    cut: bool,
}

impl Write for Cut<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            if self.cut {
                break;
            }
            let ch = if ch.is_control() { ' ' } else { ch };
            let mut utf8 = [0u8; 4];
            let bytes = ch.encode_utf8(&mut utf8).as_bytes();
            match self.buf.get_mut(self.len..self.len + bytes.len()) {
                Some(room) => {
                    room.copy_from_slice(bytes);
                    self.len += bytes.len();
                }
                None => self.cut = true,
            }
        }
        Ok(())
    }
}

/// Little-endian writer over a fixed buffer. Writing past the end is ignored:
/// the buffer is sized for exactly what is written.
struct Put<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl Put<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        if let Some(out) = self.buf.get_mut(self.pos..self.pos + bytes.len()) {
            out.copy_from_slice(bytes);
        }
        self.pos += bytes.len();
    }

    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }
}

struct Get<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Get<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let bytes = self.buf.get(self.pos..self.pos + N)?;
        self.pos += N;
        bytes.try_into().ok()
    }

    fn bytes(&mut self, out: &mut [u8]) -> Option<()> {
        let bytes = self.buf.get(self.pos..self.pos + out.len())?;
        out.copy_from_slice(bytes);
        self.pos += out.len();
        Some(())
    }

    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[value]| value)
    }

    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }
}
