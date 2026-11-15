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

//! The RAM tail: the last few records, kept for the `TailLogs` request.

use core::fmt;

use rohi_service::{TailPage, TailRequest};

use crate::LogRecord;
use crate::segment::text::{line_len, write_line_within};

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

/// What a ring holds at one moment: the boot and the sequences of its ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub session: u32,
    /// Sequence of the oldest record held; 1 when there is none.
    pub oldest: u64,
    /// Sequence of the newest record held; 0 when there is none.
    pub newest: u64,
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

    pub(crate) fn view(&self) -> View {
        View {
            session: self.session,
            oldest: self.oldest(),
            newest: self.newest(),
        }
    }

    /// The record with `sequence`, if it is still held.
    pub(crate) fn get(&self, sequence: u64) -> Option<LogRecord> {
        if sequence < self.oldest() || sequence > self.newest() {
            return None;
        }
        let record = &self.slots[Self::slot(sequence)];
        (record.sequence == sequence).then(|| record.clone())
    }
}

/// Writes into a slice and fails, keeping what it has, when the text does not
/// fit.
struct Slice<'a> {
    out: &'a mut [u8],
    filled: usize,
}

impl fmt::Write for Slice<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.filled + text.len();
        self.out
            .get_mut(self.filled..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.filled = end;
        Ok(())
    }
}

/// Fills `out` with the page of the tail `request` asks for: whole lines, each
/// ending in a newline, in sequence order.
///
/// `view` is the state of the ring when the request arrived and `ring` copies
/// one record out at a time, so the ring is not held while a line is
/// formatted and a record that is overwritten meanwhile is counted in
/// [`TailPage::gap`], not read wrongly.
///
/// - A request with session 0 or cursor 0 starts at the oldest record held. A
///   session of another boot, or a cursor beyond the newest record, does the same
///   and sets `reset`.
/// - Otherwise the page starts after the cursor; when records after it were
///   overwritten, `gap` counts them and the page starts at the oldest held.
/// - A line that does not fit the room left ends the page and starts the next
///   one. A line that does not fit an empty page is shortened (see
///   [`write_line_within`]), so a page of at least
///   [`MIN_WITHIN_LEN`](crate::segment::text::MIN_WITHIN_LEN) bytes always
///   carries a line when there is one.
/// - No record after the cursor is an empty page whose cursor is the one of the
///   request.
pub(crate) fn read_page(
    view: View,
    request: TailRequest,
    ring: impl Fn(u64) -> Option<LogRecord>,
    out: &mut [u8],
) -> TailPage {
    let stale = request.session != 0 && request.session != view.session;
    let ahead = request.session != 0 && request.after > view.newest;
    let reset = stale || ahead;
    let from_start = reset || request.session == 0 || request.after == 0;
    let wanted = if from_start {
        view.oldest
    } else {
        request.after + 1
    };
    let mut sequence = wanted.max(view.oldest);
    let mut gap = if from_start { 0 } else { sequence - wanted };
    let mut cursor = if from_start { 0 } else { request.after };

    let mut page = Slice { out, filled: 0 };
    while sequence <= view.newest {
        let Some(record) = ring(sequence) else {
            if page.filled > 0 {
                break;
            }
            // Overwritten while the page was prepared: nothing to render.
            gap += 1;
            cursor = sequence;
            sequence += 1;
            continue;
        };
        let room = page.out.len() - page.filled;
        if page.filled > 0 && line_len(&record) > room {
            break;
        }
        let before = page.filled;
        let written = write_line_within(&mut page, &record, room);
        if written.is_err() {
            page.filled = before;
            break;
        }
        cursor = sequence;
        sequence += 1;
    }
    TailPage {
        session: view.session,
        len: page.filled,
        cursor,
        oldest_sequence: view.oldest,
        newest_sequence: view.newest,
        reset,
        gap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::{String, ToString};
    use std::vec::Vec;

    use crate::segment::text::{MAX_LINE_LEN, parse_line};
    use crate::{LogLevel, MAX_MESSAGE_LEN, MAX_TARGET_LEN};

    const PAGE: usize = rohi_service::MAX_TAIL_CHUNK;

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

    fn page<const T: usize>(
        tail: &Tail<T>,
        session: u32,
        after: u64,
        room: usize,
    ) -> (TailPage, String) {
        let mut out = std::vec![0u8; room];
        let page = read_page(
            tail.view(),
            TailRequest { session, after },
            |n| tail.get(n),
            &mut out,
        );
        out.truncate(page.len);
        (page, String::from_utf8(out).unwrap())
    }

    fn sequences(text: &str) -> Vec<u64> {
        text.lines()
            .map(|line| parse_line(line).unwrap().sequence)
            .collect()
    }

    #[test]
    fn an_empty_tail_has_nothing_to_return() {
        let tail = Tail::<4>::new();
        assert_eq!((tail.oldest(), tail.newest()), (1, 0));
        let (empty, text) = page(&tail, 0, 0, PAGE);
        assert_eq!(
            (empty.len, empty.cursor, empty.reset, empty.gap),
            (0, 0, false, 0)
        );
        assert_eq!((empty.oldest_sequence, empty.newest_sequence), (1, 0));
        assert!(text.is_empty());
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
    fn the_first_request_gets_the_oldest_records_held_in_order() {
        let tail = filled::<8>(20);
        let (first, text) = page(&tail, 0, 0, PAGE);
        assert_eq!(sequences(&text), (13..=20).collect::<Vec<_>>());
        assert_eq!((first.cursor, first.reset, first.gap), (20, false, 0));
        assert_eq!(first.session, tail.session());
        // A session without a cursor starts there too, and is no reset.
        let (again, text) = page(&tail, tail.session(), 0, PAGE);
        assert_eq!(sequences(&text).len(), 8);
        assert!(!again.reset);
    }

    #[test]
    fn a_cursor_returns_what_is_newer_and_an_empty_page_when_there_is_nothing() {
        let mut tail = filled::<8>(5);
        let session = tail.session();
        let (next, text) = page(&tail, session, 3, PAGE);
        assert_eq!(sequences(&text), [4, 5]);
        assert_eq!((next.cursor, next.reset, next.gap), (5, false, 0));
        let (none, text) = page(&tail, session, 5, PAGE);
        assert!(text.is_empty());
        assert_eq!(
            (none.len, none.cursor, none.reset, none.gap),
            (0, 5, false, 0)
        );
        tail.publish(&record("next"));
        let (more, text) = page(&tail, session, 5, PAGE);
        assert_eq!(sequences(&text), [6]);
        assert_eq!(more.cursor, 6);
    }

    #[test]
    fn a_small_page_is_continued_with_its_cursor_and_loses_nothing() {
        let tail = filled::<32>(30);
        let session = tail.session();
        let one_line = page(&tail, 0, 0, PAGE).1.lines().next().unwrap().len() + 1;
        // Room for exactly two lines: pages of two, in order, each whole.
        let room = one_line * 2 + one_line / 2;
        let mut seen = Vec::new();
        let mut cursor = 0;
        let mut pages = 0;
        loop {
            let (next, text) = page(&tail, session, cursor, room);
            if text.is_empty() {
                break;
            }
            assert!(text.ends_with('\n'), "{text:?}");
            assert!(text.len() <= room);
            assert!(next.len > 0 && next.cursor > cursor);
            assert_eq!(next.gap, 0);
            seen.extend(sequences(&text));
            cursor = next.cursor;
            pages += 1;
        }
        assert_eq!(seen, (1..=30).collect::<Vec<_>>());
        assert_eq!(pages, 15);
    }

    #[test]
    fn a_page_of_512_bytes_never_splits_a_record() {
        let mut tail = Tail::<32>::new();
        for n in 0..32 {
            tail.publish(&record(&"y".repeat(20 + n * 3)));
        }
        let mut cursor = 0;
        let mut total = 0;
        loop {
            let (next, text) = page(&tail, tail.session(), cursor, PAGE);
            if text.is_empty() {
                break;
            }
            assert!(text.len() <= PAGE);
            for line in text.lines() {
                assert!(parse_line(line).is_some(), "{line:?}");
            }
            total += text.lines().count();
            cursor = next.cursor;
        }
        assert_eq!(total, 32);
    }

    #[test]
    fn overwritten_records_are_reported_as_a_gap_and_the_page_continues_at_the_oldest() {
        let tail = filled::<4>(10);
        let session = tail.session();
        let (next, text) = page(&tail, session, 2, PAGE);
        assert_eq!(sequences(&text), [7, 8, 9, 10]);
        assert_eq!((next.reset, next.gap, next.oldest_sequence), (false, 4, 7));
        // Records 3..=6 are gone: the gap is explicit, not guessed from the text.
        let (caught_up, text) = page(&tail, session, 9, PAGE);
        assert_eq!(sequences(&text), [10]);
        assert_eq!(caught_up.gap, 0);
    }

    #[test]
    fn a_record_overwritten_while_the_page_is_prepared_is_a_gap() {
        let tail = filled::<4>(4);
        let view = tail.view();
        let mut out = [0u8; PAGE];
        // The ring has moved on after the view was taken: 1 and 2 are gone.
        let moved = filled::<4>(6);
        let page = read_page(
            view,
            TailRequest {
                session: view.session,
                after: 0,
            },
            |n| moved.get(n),
            &mut out,
        );
        assert_eq!(
            sequences(core::str::from_utf8(&out[..page.len]).unwrap()),
            [3, 4]
        );
        // Two records of the view were gone: they are counted.
        assert_eq!((page.gap, page.cursor, page.reset), (2, 4, false));
        // Nothing of the view is readable any more: an empty page that still
        // moves the cursor.
        let gone = filled::<4>(40);
        let page = read_page(
            view,
            TailRequest {
                session: view.session,
                after: 1,
            },
            |n| gone.get(n),
            &mut out,
        );
        assert_eq!((page.len, page.gap, page.cursor), (0, 3, 4));
    }

    #[test]
    fn another_session_or_a_cursor_from_the_future_is_a_reset() {
        let mut tail = filled::<8>(5);
        tail.set_session(9);
        let (other, text) = page(&tail, 8, 3, PAGE);
        assert!(other.reset);
        assert_eq!(sequences(&text), [1, 2, 3, 4, 5]);
        assert_eq!((other.session, other.gap), (9, 0));
        let (ahead, text) = page(&tail, 9, 50, PAGE);
        assert!(ahead.reset);
        assert_eq!(sequences(&text).len(), 5);
        // No session yet is not a reset.
        assert!(!page(&tail, 0, 50, PAGE).0.reset);
        // An empty tail still tells a client of another boot to start over.
        let (restarted, text) = page(&Tail::<4>::new(), 77, 3, PAGE);
        assert!(restarted.reset && text.is_empty());
        assert_eq!(restarted.cursor, 0);
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
    fn the_lines_carry_the_tail_sequence_and_parse_as_archive_lines() {
        let tail = filled::<8>(3);
        let (_, text) = page(&tail, 0, 0, PAGE);
        let first = parse_line(text.lines().next().unwrap()).unwrap();
        assert_eq!((first.sequence, first.message.as_str()), (1, "m1"));
    }

    fn worst_case(fill: char) -> LogRecord {
        let message = fill.to_string().repeat(MAX_MESSAGE_LEN);
        let target = "\u{2}".repeat(MAX_TARGET_LEN);
        LogRecord::new(
            LogLevel::Error,
            &target,
            format_args!("{message}"),
            u64::MAX,
            None,
        )
    }

    #[test]
    fn the_worst_case_line_is_shortened_to_the_page_and_still_parses() {
        // Every character of the message is a control character: four bytes
        // each when escaped.
        let mut tail = Tail::<2>::new();
        tail.publish(&worst_case('\u{1}'));
        tail.publish(&record("after"));
        let full = line_len(&tail.get(1).unwrap());
        assert!(full > PAGE && full <= MAX_LINE_LEN, "{full}");
        let (first, text) = page(&tail, 0, 0, PAGE);
        assert!(text.len() <= PAGE && text.ends_with('\n'), "{}", text.len());
        assert_eq!(
            text.lines().count(),
            1,
            "the short line waits for the next page"
        );
        let parsed = parse_line(text.trim_end()).unwrap();
        assert!(parsed.truncated);
        assert!(parsed.message.len() < MAX_MESSAGE_LEN);
        assert!(parsed.message.chars().all(|c| c == '\u{1}'));
        // Forward progress: the next page has the following record.
        assert_eq!(first.cursor, 1);
        let (second, text) = page(&tail, first.session, first.cursor, PAGE);
        assert_eq!(sequences(&text), [2]);
        assert_eq!(second.cursor, 2);
    }

    #[test]
    fn a_shortened_line_is_cut_on_a_character_boundary_without_splitting_an_escape() {
        // Multi-byte characters: cut whole, the text stays UTF-8.
        for fill in ['é', '€', '😀', '\u{1}', '\\'] {
            let mut tail = Tail::<1>::new();
            tail.publish(&worst_case(fill));
            for room in [crate::segment::text::MIN_WITHIN_LEN, 200, 333, 512] {
                let (_, text) = page(&tail, 0, 0, room);
                assert!(text.len() <= room, "{fill:?} {room}");
                let parsed = parse_line(text.trim_end()).unwrap();
                assert!(parsed.message.chars().all(|c| c == fill), "{fill:?} {room}");
            }
        }
    }

    #[test]
    fn a_page_too_small_for_any_line_makes_no_progress_and_loses_nothing() {
        let tail = filled::<4>(3);
        let (empty, text) = page(&tail, 0, 0, 10);
        assert!(text.is_empty());
        assert_eq!((empty.len, empty.cursor, empty.gap), (0, 0, 0));
    }

    #[test]
    fn the_largest_line_fits_a_tail_page_once_shortened() {
        const { assert!(crate::segment::text::MIN_WITHIN_LEN < PAGE) };
        const { assert!(crate::segment::text::MIN_WITHIN_LEN + 4 * MAX_MESSAGE_LEN == MAX_LINE_LEN) };
    }
}
