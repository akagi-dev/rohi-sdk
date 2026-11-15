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
//! The RAM tail: the last few records, kept for `logging.tail`.

use core::fmt;

use crate::LogRecord;
use crate::segment::text::write_line;

/// Records the RAM tail of a [`LogSink`](crate::LogSink) keeps unless the
/// firmware chooses another depth.
pub const DEFAULT_TAIL_RECORDS: usize = 8;

/// A bounded ring of the most recent records, in publication order.
///
/// It is volatile and best effort: a record is overwritten when the ring is
/// full, nothing is written to storage and a reset empties it. Its sequence
/// numbers are its own: they start at 1 on every boot and are unrelated to
/// the `sequence` of the persistent history.
pub(crate) struct Tail<const T: usize> {
    // A slot is empty while its sequence is 0. Not `Option`: the empty value is
    // all zero bytes, so the ring lives in `.bss` and not in the initialised
    // `.data` of the chip.
    slots: [LogRecord; T],
    next: u64,
    session: u32,
}

/// Which records one `tail` request selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Page {
    /// The request carried a session or a cursor that is not valid for this
    /// boot: the page is the initial snapshot and the client must restart.
    pub reset: bool,
    /// Sequence of the first selected record; meaningless when `count` is 0.
    pub first: u64,
    /// Number of selected records; they are consecutive.
    pub count: u64,
}

/// The records of a page were overwritten before they were rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Overwritten;

/// What [`Tail::render`] produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rendered {
    /// Bytes written into the buffer.
    pub filled: usize,
    /// Size of the whole text of the page.
    pub total: u64,
}

impl<const T: usize> Tail<T> {
    pub(crate) const fn new() -> Self {
        const { assert!(T > 0, "the tail keeps at least one record") };
        Self {
            slots: [const { LogRecord::EMPTY }; T],
            next: 1,
            session: 1,
        }
    }

    /// Identifies this boot; never 0, which means "no session" in a request.
    pub(crate) fn session(&self) -> u32 {
        self.session
    }

    pub(crate) fn set_session(&mut self, session: u32) {
        self.session = session.max(1);
    }

    /// Copies `record` into the ring under the next sequence.
    pub(crate) fn publish(&mut self, record: &LogRecord) {
        let sequence = self.next;
        self.next = self.next.saturating_add(1);
        let slot = &mut self.slots[Self::slot(sequence)];
        slot.clone_from(record);
        slot.sequence = sequence;
    }

    fn slot(sequence: u64) -> usize {
        (sequence.wrapping_sub(1) % T as u64) as usize
    }

    /// Sequence of the newest record; 0 when there is none.
    pub(crate) fn newest(&self) -> u64 {
        self.next - 1
    }

    /// Sequence of the oldest record still held; 1 when there is none.
    pub(crate) fn oldest(&self) -> u64 {
        self.next.saturating_sub(T as u64).max(1)
    }

    fn held(&self) -> u64 {
        self.newest() - (self.oldest() - 1)
    }

    /// The record with `sequence`, if it is still held.
    pub(crate) fn get(&self, sequence: u64) -> Option<LogRecord> {
        if sequence < self.oldest() || sequence > self.newest() {
            return None;
        }
        let record = &self.slots[Self::slot(sequence)];
        (record.sequence == sequence).then(|| record.clone())
    }

    /// Selects the records for a request that carries `session` and `after`.
    ///
    /// - `session == 0` or `after == 0`: the latest `limit` records.
    /// - A session of another boot, or a cursor beyond the newest record:
    ///   the same snapshot, flagged as a reset.
    /// - Otherwise the records after `after`, oldest first, at most `limit`;
    ///   when `after + 1` was overwritten the page starts at the oldest
    ///   record held and the client sees the jump in the sequences.
    pub(crate) fn page(&self, session: u64, after: u64, limit: u64) -> Page {
        let limit = limit.clamp(1, T as u64);
        let newest = self.newest();
        let stale = session != 0 && session != u64::from(self.session);
        let ahead = session != 0 && after > newest;
        let reset = stale || ahead;
        if reset || session == 0 || after == 0 {
            let count = limit.min(self.held());
            return Page {
                reset,
                first: newest + 1 - count,
                count,
            };
        }
        let first = (after + 1).max(self.oldest());
        let count = if first > newest {
            0
        } else {
            limit.min(newest - first + 1)
        };
        Page {
            reset: false,
            first,
            count,
        }
    }
}

/// Writes the text of `count` records from `first` into `out`, skipping the
/// first `skip` bytes of it, and reports the size of the whole text.
///
/// The text is the lines of [`write_line`] with the tail sequence. `ring` copies
/// one record out at a time, so the ring is not held while it is formatted.
pub(crate) fn render(
    ring: impl Fn(u64) -> Option<LogRecord>,
    first: u64,
    count: u64,
    skip: u64,
    out: &mut [u8],
) -> Result<Rendered, Overwritten> {
    let mut window = Window {
        skip,
        out,
        filled: 0,
        total: 0,
    };
    for sequence in first..first.saturating_add(count) {
        let record = ring(sequence).ok_or(Overwritten)?;
        // A window never fails: it counts what does not fit.
        let _ = write_line(&mut window, &record);
    }
    Ok(Rendered {
        filled: window.filled,
        total: window.total,
    })
}

/// Keeps the bytes of a text that fall into `[skip, skip + out.len())` and
/// counts all of them.
struct Window<'a> {
    skip: u64,
    out: &'a mut [u8],
    filled: usize,
    total: u64,
}

impl fmt::Write for Window<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let bytes = text.as_bytes();
        let start = self.total;
        self.total += bytes.len() as u64;
        let from = self.skip.saturating_sub(start);
        if from >= bytes.len() as u64 {
            return Ok(());
        }
        let bytes = &bytes[from as usize..];
        let room = self.out.len() - self.filled;
        let len = bytes.len().min(room);
        self.out[self.filled..self.filled + len].copy_from_slice(&bytes[..len]);
        self.filled += len;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;

    use crate::LogLevel;

    fn record(text: &str) -> LogRecord {
        LogRecord::new(LogLevel::Info, "t", format_args!("{text}"), 5, None)
    }

    fn filled<const T: usize>(count: u64) -> Tail<T> {
        let mut tail = Tail::<T>::new();
        for n in 1..=count {
            tail.publish(&record(&std::format!("m{n}")));
        }
        tail
    }

    fn text<const T: usize>(tail: &Tail<T>, page: Page, skip: u64, room: usize) -> (String, u64) {
        let mut out = std::vec![0u8; room];
        let done = render(|n| tail.get(n), page.first, page.count, skip, &mut out).unwrap();
        out.truncate(done.filled);
        (String::from_utf8(out).unwrap(), done.total)
    }

    #[test]
    fn an_empty_tail_has_nothing_and_asks_for_nothing() {
        let tail = Tail::<4>::new();
        assert_eq!((tail.oldest(), tail.newest()), (1, 0));
        let page = tail.page(0, 0, 4);
        assert_eq!((page.reset, page.count), (false, 0));
        assert!(tail.get(1).is_none());
    }

    #[test]
    fn the_ring_overwrites_the_oldest_record_and_keeps_the_order() {
        let tail = filled::<4>(10);
        assert_eq!((tail.oldest(), tail.newest()), (7, 10));
        assert!(tail.get(6).is_none());
        assert!(tail.get(11).is_none());
        for n in 7..=10 {
            let record = tail.get(n).unwrap();
            assert_eq!(record.sequence, n);
            assert_eq!(record.message.as_str(), std::format!("m{n}"));
        }
    }

    #[test]
    fn the_first_request_gets_the_latest_records_oldest_first() {
        let tail = filled::<8>(20);
        let page = tail.page(0, 0, 3);
        assert_eq!((page.reset, page.first, page.count), (false, 18, 3));
        // `limit` is clamped to the depth of the ring and to 1 below.
        assert_eq!(tail.page(0, 0, 100).count, 8);
        assert_eq!(tail.page(0, 0, 0).count, 1);
        // Fewer records than asked are all of them.
        let few = filled::<8>(2);
        assert_eq!((few.page(0, 0, 8).first, few.page(0, 0, 8).count), (1, 2));
    }

    #[test]
    fn a_follow_request_gets_the_records_after_its_cursor_once() {
        let mut tail = filled::<8>(5);
        let session = u64::from(tail.session());
        let page = tail.page(session, 3, 8);
        assert_eq!((page.reset, page.first, page.count), (false, 4, 2));
        let none = tail.page(session, 5, 8);
        assert_eq!((none.reset, none.count), (false, 0));
        tail.publish(&record("next"));
        let next = tail.page(session, 5, 8);
        assert_eq!((next.first, next.count), (6, 1));
        // The limit bounds a page and the next one continues.
        let limited = tail.page(session, 0, 8);
        assert_eq!(limited.count, 6);
        let two = tail.page(session, 1, 2);
        assert_eq!((two.first, two.count), (2, 2));
    }

    #[test]
    fn overwritten_records_are_skipped_and_the_jump_is_visible() {
        let tail = filled::<4>(10);
        let session = u64::from(tail.session());
        let page = tail.page(session, 2, 4);
        assert_eq!((page.reset, page.first, page.count), (false, 7, 4));
        assert!(page.first > 2 + 1, "the client sees the gap");
    }

    #[test]
    fn another_session_or_a_cursor_from_the_future_is_a_reset() {
        let mut tail = filled::<8>(5);
        tail.set_session(9);
        let other = tail.page(8, 3, 8);
        assert!(other.reset);
        assert_eq!((other.first, other.count), (1, 5));
        let ahead = tail.page(9, 50, 2);
        assert!(ahead.reset);
        assert_eq!((ahead.first, ahead.count), (4, 2));
        // No session yet is not a reset.
        assert!(!tail.page(0, 50, 2).reset);
        // An empty tail still tells a client of another boot to start over.
        assert!(Tail::<4>::new().page(77, 3, 4).reset);
    }

    #[test]
    fn the_session_is_never_zero() {
        let mut tail = Tail::<2>::new();
        assert_eq!(tail.session(), 1);
        tail.set_session(0);
        assert_eq!(tail.session(), 1);
        tail.set_session(41);
        assert_eq!(tail.session(), 41);
    }

    #[test]
    fn the_text_is_the_same_whatever_the_chunking() {
        let tail = filled::<8>(6);
        let page = tail.page(0, 0, 6);
        let (whole, total) = text(&tail, page, 0, 4096);
        assert_eq!(total as usize, whole.len());
        assert_eq!(whole.lines().count(), 6);
        for chunk in [1usize, 7, 64] {
            let mut joined = String::new();
            let mut offset = 0;
            while offset < total {
                let (part, again) = text(&tail, page, offset, chunk);
                assert_eq!(again, total);
                assert!(!part.is_empty());
                offset += part.len() as u64;
                joined.push_str(&part);
            }
            assert_eq!(joined, whole);
        }
        // The lines carry the tail sequence and parse as archive lines.
        let first = crate::segment::text::parse_line(whole.lines().next().unwrap()).unwrap();
        assert_eq!((first.sequence, first.message.as_str()), (1, "m1"));
    }

    #[test]
    fn a_record_overwritten_before_it_is_rendered_is_reported() {
        let mut tail = filled::<4>(4);
        let page = tail.page(0, 0, 4);
        for _ in 0..3 {
            tail.publish(&record("later"));
        }
        let mut out = [0u8; 16];
        assert_eq!(
            render(|n| tail.get(n), page.first, page.count, 0, &mut out),
            Err(Overwritten)
        );
    }

    #[test]
    fn the_worst_case_line_is_rendered_whole_over_several_chunks() {
        let mut tail = Tail::<2>::new();
        let message = "\u{1}".repeat(crate::MAX_MESSAGE_LEN);
        let target = "\u{2}".repeat(crate::MAX_TARGET_LEN);
        tail.publish(&LogRecord::new(
            LogLevel::Error,
            &target,
            format_args!("{message}"),
            u64::MAX,
            None,
        ));
        let page = tail.page(0, 0, 1);
        let (whole, total) = text(&tail, page, 0, 4096);
        assert!(total > 512, "the line does not fit one chunk: {total}");
        assert!(total as usize <= crate::segment::text::MAX_LINE_LEN);
        let mut joined = String::new();
        while (joined.len() as u64) < total {
            let (part, _) = text(&tail, page, joined.len() as u64, 512);
            assert!(!part.is_empty(), "every chunk makes progress");
            joined.push_str(&part);
        }
        assert_eq!(joined, whole);
        let parsed = crate::segment::text::parse_line(whole.trim_end()).unwrap();
        assert_eq!(parsed.message.as_str(), message);
    }
}
