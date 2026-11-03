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
//! The line-oriented UTF-8 text of a log segment.
//!
//! One record per line, readable without the firmware:
//!
//! ```text
//! 42 12345 2026-11-03T05:24:43.221Z WARN wifi: connected to home
//! 43 12400 - ERROR* storage: cut message
//! ```
//!
//! The fields are the sequence, the milliseconds since boot, the wall-clock
//! time in UTC (`-` while unknown), the level (a `*` follows it when the
//! record was truncated), the target followed by `:` and the message. Spaces,
//! backslashes and control characters in the target, and backslashes and
//! control characters in the message, are written as `\\`, `\n`, `\r`, `\t`
//! or `\xNN`, so a record never spans lines.

use core::fmt::{self, Write};

use heapless::String;
use rohi_time::UnixTime;

use crate::{LogLevel, LogRecord, MAX_MESSAGE_LEN, MAX_TARGET_LEN};

/// Longest line this module writes, with the newline, in bytes.
pub const MAX_LINE_LEN: usize =
    20 + 1 + 20 + 1 + 24 + 1 + 6 + 1 + MAX_TARGET_LEN * 4 + 2 + MAX_MESSAGE_LEN * 4 + 1;

const MILLIS_PER_DAY: u64 = 86_400_000;
const MAX_YEAR: i64 = 9_999;

fn escape(out: &mut impl Write, text: &str, in_target: bool) -> fmt::Result {
    for character in text.chars() {
        match character {
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            ' ' if in_target => out.write_str("\\x20")?,
            control if control.is_control() => write!(out, "\\x{:02x}", control as u32)?,
            other => out.write_char(other)?,
        }
    }
    Ok(())
}

fn unescape<const N: usize>(text: &str) -> Option<String<N>> {
    let mut out = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character).ok()?;
            continue;
        }
        let decoded = match characters.next()? {
            '\\' => '\\',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'x' => {
                let high = characters.next()?.to_digit(16)?;
                let low = characters.next()?.to_digit(16)?;
                char::from_u32(high * 16 + low)?
            }
            _ => return None,
        };
        out.push(decoded).ok()?;
    }
    Some(out)
}

// Days since 1970-01-01 to a civil date (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_part + 2) / 5 + 1) as u32;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_part = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_part + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn write_wall(out: &mut impl Write, wall: Option<UnixTime>) -> fmt::Result {
    let Some(wall) = wall else {
        return out.write_char('-');
    };
    let millis = wall.as_millis();
    let (year, month, day) = civil_from_days((millis / MILLIS_PER_DAY) as i64);
    if year > MAX_YEAR {
        return out.write_char('-');
    }
    let in_day = millis % MILLIS_PER_DAY;
    write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1_000 % 60,
        in_day % 1_000
    )
}

fn parse_wall(text: &str) -> Option<Option<UnixTime>> {
    if text == "-" {
        return Some(None);
    }
    let bytes = text.as_bytes();
    if bytes.len() != 24 || !text.is_ascii() {
        return None;
    }
    let number = |from: usize, to: usize| text[from..to].parse::<u32>().ok();
    let separators = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (23, b'Z'),
    ];
    if separators.iter().any(|(at, byte)| bytes[*at] != *byte) {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second, milli) = (
        number(11, 13)?,
        number(14, 16)?,
        number(17, 19)?,
        number(20, 23)?,
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(i64::from(year), month, day);
    let millis = u64::try_from(days).ok()? * MILLIS_PER_DAY
        + u64::from(hour) * 3_600_000
        + u64::from(minute) * 60_000
        + u64::from(second) * 1_000
        + u64::from(milli);
    Some(Some(UnixTime::from_millis(millis)))
}

fn level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Off => "OFF",
        LogLevel::Error => "ERROR",
        LogLevel::Warn => "WARN",
        LogLevel::Info => "INFO",
        LogLevel::Debug => "DEBUG",
    }
}

fn parse_level(text: &str) -> Option<LogLevel> {
    Some(match text {
        "ERROR" => LogLevel::Error,
        "WARN" => LogLevel::Warn,
        "INFO" => LogLevel::Info,
        "DEBUG" => LogLevel::Debug,
        _ => return None,
    })
}

/// Writes `record` as one line, newline included.
///
/// # Errors
///
/// Only what `out` returns; a line is at most [`MAX_LINE_LEN`] bytes.
pub fn write_line(out: &mut impl Write, record: &LogRecord) -> fmt::Result {
    write!(out, "{} {} ", record.sequence, record.monotonic_ms)?;
    write_wall(out, record.wall_time)?;
    write!(
        out,
        " {}{} ",
        level_name(record.level),
        if record.truncated { "*" } else { "" }
    )?;
    escape(out, &record.target, true)?;
    out.write_str(": ")?;
    escape(out, &record.message, false)?;
    out.write_char('\n')
}

/// Reads one line written by [`write_line`], without its newline.
///
/// Returns `None` for anything else, so damaged or foreign lines are skipped
/// instead of trusted.
pub fn parse_line(line: &str) -> Option<LogRecord> {
    let mut parts = line.splitn(5, ' ');
    let sequence = parts.next()?.parse().ok()?;
    let monotonic_ms = parts.next()?.parse().ok()?;
    let wall_time = parse_wall(parts.next()?)?;
    let level = parts.next()?;
    let (level, truncated) = match level.strip_suffix('*') {
        Some(level) => (level, true),
        None => (level, false),
    };
    let level = parse_level(level)?;
    let rest = parts.next()?;
    let (target, message) = rest.split_once(' ').unwrap_or((rest, ""));
    let target = target.strip_suffix(':')?;
    Some(LogRecord {
        sequence,
        monotonic_ms,
        wall_time,
        level,
        target: unescape(target)?,
        message: unescape(message)?,
        truncated,
    })
}
