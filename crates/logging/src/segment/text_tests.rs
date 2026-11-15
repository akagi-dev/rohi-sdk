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

use std::string::String;

use rohi_time::UnixTime;

use super::text::*;
use crate::{LogLevel, LogRecord, MAX_MESSAGE_LEN, MAX_TARGET_LEN};

fn record(wall: Option<UnixTime>, target: &str, message: &str) -> LogRecord {
    let mut record = LogRecord::new(
        LogLevel::Warn,
        target,
        format_args!("{message}"),
        12_345,
        wall,
    );
    record.sequence = 42;
    record
}

fn line(record: &LogRecord) -> String {
    let mut out = String::new();
    write_line(&mut out, record).unwrap();
    out
}

#[test]
fn a_line_is_readable_text() {
    let wall = UnixTime::from_millis(1_767_225_600_000 + 3_723_004);
    assert_eq!(
        line(&record(Some(wall), "wifi", "connected to home")),
        "42 12345 2026-01-01T01:02:03.004Z WARN wifi: connected to home\n"
    );
    let mut cut = record(None, "storage", "x");
    cut.level = LogLevel::Error;
    cut.truncated = true;
    assert_eq!(line(&cut), "42 12345 - ERROR* storage: x\n");
}

#[test]
fn dates_follow_the_gregorian_calendar() {
    for (secs, text) in [
        (0, "1970-01-01T00:00:00.000Z"),
        (951_782_400, "2000-02-29T00:00:00.000Z"),
        (1_709_164_800, "2024-02-29T00:00:00.000Z"),
        (1_709_251_199, "2024-02-29T23:59:59.000Z"),
        (1_767_225_600, "2026-01-01T00:00:00.000Z"),
        (4_102_444_800, "2100-01-01T00:00:00.000Z"),
        (253_402_300_799, "9999-12-31T23:59:59.000Z"),
    ] {
        let wall = Some(UnixTime::from_secs(secs));
        let written = line(&record(wall, "t", "m"));
        assert!(written.contains(text), "{written}");
        assert_eq!(parse_line(written.trim_end()).unwrap().wall_time, wall);
    }
    // Beyond year 9999 there is no four-digit date: written as unknown.
    let far = Some(UnixTime::from_secs(253_402_300_800));
    assert!(line(&record(far, "t", "m")).contains(" - WARN "));
}

#[test]
fn every_record_reads_back_as_written() {
    let messages = [
        "plain",
        "",
        "with  spaces and: colons",
        "back\\slash",
        "two\nlines\r\nand\ttabs",
        "bell\u{7} and \u{1b}[31mred",
        "ünïcödé — 日本語",
        "trailing space ",
    ];
    for target in ["wifi", "a::b", "has space", "", "tab\there", "ünï"] {
        for message in messages {
            for wall in [None, Some(UnixTime::from_millis(1_800_000_123_456))] {
                let original = record(wall, target, message);
                let written = line(&original);
                assert_eq!(written.matches('\n').count(), 1, "{written:?}");
                assert!(written.ends_with('\n'));
                let parsed = parse_line(written.trim_end_matches('\n')).unwrap();
                assert_eq!(parsed, original, "{written:?}");
            }
        }
    }
}

#[test]
fn truncation_is_part_of_the_record() {
    let original = record(None, "t", &"é".repeat(MAX_MESSAGE_LEN));
    assert!(original.truncated);
    let written = line(&original);
    assert_eq!(parse_line(written.trim_end()).unwrap(), original);
}

#[test]
fn the_longest_line_fits_the_bound() {
    let nasty = "\u{1}".repeat(MAX_MESSAGE_LEN);
    let target = "\u{2}".repeat(MAX_TARGET_LEN);
    let mut worst = record(Some(UnixTime::from_secs(253_402_300_799)), &target, &nasty);
    worst.sequence = u64::MAX;
    worst.monotonic_ms = u64::MAX;
    worst.truncated = true;
    let written = line(&worst);
    assert!(
        written.len() <= MAX_LINE_LEN,
        "{} > {MAX_LINE_LEN}",
        written.len()
    );
    assert_eq!(parse_line(written.trim_end()).unwrap(), worst);
}

#[test]
fn damaged_or_foreign_lines_are_not_records() {
    for bad in [
        "",
        "hello",
        "x 1 - WARN t: m",
        "1 x - WARN t: m",
        "1 2 - LOUD t: m",
        "1 2 - WARN t m",
        "1 2 2026-13-01T00:00:00.000Z WARN t: m",
        "1 2 2026-01-01T00:00:00Z WARN t: m",
        "1 2 - WARN t: bad\\q",
        "1 2 - WARN t: bad\\x4",
        "1 2 - WARN t: trailing\\",
        "1 2 - OFF t: m",
    ] {
        assert_eq!(parse_line(bad), None, "{bad:?}");
    }
}

fn within(record: &LogRecord, max_len: usize) -> (String, usize) {
    let mut out = String::new();
    let len = write_line_within(&mut out, record, max_len).unwrap();
    (out, len)
}

#[test]
fn a_line_that_fits_is_the_ordinary_line() {
    let record = record(None, "wifi", "connected to home");
    let (text, len) = within(&record, 512);
    assert_eq!(text, line(&record));
    assert_eq!((len, line_len(&record)), (text.len(), text.len()));
    // Exactly the length of the line fits too.
    assert_eq!(within(&record, text.len()).0, text);
}

#[test]
fn a_longer_line_is_shortened_to_the_limit_and_still_parses() {
    let message = "é\u{1}\\\n€😀x".repeat(MAX_MESSAGE_LEN);
    let long = record(None, "ctl", &message);
    assert!(long.truncated);
    for max_len in [MIN_WITHIN_LEN, MIN_WITHIN_LEN + 1, 200, 256, 333, 512] {
        let (text, len) = within(&long, max_len);
        assert_eq!(len, text.len());
        assert!(text.len() <= max_len, "{max_len}: {}", text.len());
        assert!(text.ends_with('\n') && text.matches('\n').count() == 1);
        let parsed = parse_line(text.trim_end_matches('\n')).unwrap();
        assert!(parsed.truncated);
        assert_eq!((parsed.sequence, parsed.monotonic_ms), (42, 12_345));
        // A prefix of the message, cut at whole characters.
        assert!(long.message.starts_with(parsed.message.as_str()));
    }
}

#[test]
fn shortening_cuts_what_does_not_fit_and_nothing_else() {
    let mut message = String::new();
    while message.len() + 4 <= MAX_MESSAGE_LEN {
        message.push('\u{1}');
    }
    let mut target = String::new();
    while target.len() + 4 <= MAX_TARGET_LEN {
        target.push('\u{2}');
    }
    let long = record(
        Some(UnixTime::from_millis(1_700_000_000_123)),
        &target,
        &message,
    );
    let full = line_len(&long);
    assert!(full > 512, "{full}");
    let (text, len) = within(&long, 512);
    assert!(len <= 512);
    // One more character would not have fit: the line is as long as it can be.
    assert!(len + 4 > 512, "{len}");
    let parsed = parse_line(text.trim_end_matches('\n')).unwrap();
    assert!(parsed.message.len() < message.len());
    assert!(parsed.message.chars().all(|c| c == '\u{1}'));
}

#[test]
fn the_shortest_page_still_holds_the_longest_line_without_a_message() {
    let mut target = String::new();
    while target.len() + 4 <= MAX_TARGET_LEN {
        target.push('\u{2}');
    }
    let message = "x".repeat(MAX_MESSAGE_LEN);
    let mut long = record(
        Some(UnixTime::from_millis(1_700_000_000_123)),
        &target,
        &message,
    );
    long.sequence = u64::MAX;
    long.monotonic_ms = u64::MAX;
    let (text, _) = within(&long, MIN_WITHIN_LEN);
    assert!(text.len() <= MIN_WITHIN_LEN);
    assert!(parse_line(text.trim_end_matches('\n')).is_some());
    // A limit that cannot hold even the fields before the message is an error.
    let mut out = String::new();
    assert!(write_line_within(&mut out, &long, 10).is_err());
}
