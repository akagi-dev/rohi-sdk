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

//! The `system` status: why the device started and what its last crash was.

use core::fmt::{self, Write};

use heapless::Vec;
use rohi_crash::{Policy, Report, Reset};
use rohi_service::{
    FIRST_SERVICE_ISSUE, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity,
    MAX_FIELD_VALUE_LEN, MAX_ISSUES, ServiceHealth, ServiceState, ServiceSummary, Status,
    StatusWriter, Unit, ValueKind, WriteError,
};

use crate::{SystemPlatform, SystemService};

/// Ids of the status fields of the `system` service.
pub mod status_field {
    /// `reset_reason`: why the chip started this time, see [`Reset::name`](rohi_crash::Reset::name).
    pub const RESET_REASON: u32 = 1;
    /// `crashes`: panics since the retained memory was last cleared by a power-up.
    pub const CRASHES: u32 = 2;
    /// `crash_streak`: consecutive panics shortly after boot at the last panic.
    pub const CRASH_STREAK: u32 = 3;
    /// `last_crash`: the start of the message of the last panic.
    pub const LAST_CRASH: u32 = 4;
    /// `last_crash_at`: the end of the source file and the line of the last panic.
    pub const LAST_CRASH_AT: u32 = 5;
    /// `last_crash_uptime`: time since boot of the last panic.
    pub const LAST_CRASH_UPTIME: u32 = 6;
    /// `last_crash_logged`: whether the last panic is in the persistent log.
    pub const LAST_CRASH_LOGGED: u32 = 7;
}

/// Issue codes of the `system` service.
pub mod issue {
    use super::FIRST_SERVICE_ISSUE;

    /// The device started because of a panic: see `rohi logs`.
    pub const CRASHED: u32 = FIRST_SERVICE_ISSUE;
    /// The device keeps panicking shortly after boot and waits between starts.
    pub const CRASH_LOOP: u32 = FIRST_SERVICE_ISSUE + 1;
    /// The last panic is not in the persistent log yet (or never will be).
    pub const CRASH_NOT_LOGGED: u32 = FIRST_SERVICE_ISSUE + 2;
    /// The chip reset by a watchdog or because of a brown-out, without a panic record.
    pub const UNEXPECTED_RESET: u32 = FIRST_SERVICE_ISSUE + 3;
}

/// A fixed buffer of at most [`MAX_FIELD_VALUE_LEN`] bytes of text.
struct Short {
    buf: [u8; MAX_FIELD_VALUE_LEN],
    len: usize,
}

impl Short {
    const fn new() -> Self {
        Self {
            buf: [0; MAX_FIELD_VALUE_LEN],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        self.buf
            .get(..self.len)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }
}

/// Keeps the start of what is written; what does not fit is dropped at a
/// character boundary.
impl Write for Short {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut end = text.len().min(MAX_FIELD_VALUE_LEN - self.len);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let (Some(room), Some(part)) = (
            self.buf.get_mut(self.len..self.len + end),
            text.as_bytes().get(..end),
        ) {
            room.copy_from_slice(part);
            self.len += end;
        }
        Ok(())
    }
}

fn location(report: &Report) -> Short {
    let mut out = Short::new();
    let Some(stored) = &report.last else {
        return out;
    };
    let crash = &stored.crash;
    let mut tail = Short::new();
    let _ = write!(tail, ":{}", crash.line_number());
    let suffix = if crash.line_number() > 0 {
        tail.as_str()
    } else {
        ""
    };
    let room = MAX_FIELD_VALUE_LEN - suffix.len();
    let file = crash.file();
    let mut start = file.len().saturating_sub(room);
    while !file.is_char_boundary(start) {
        start += 1;
    }
    let _ = write!(out, "{}{suffix}", file.get(start..).unwrap_or(""));
    out
}

impl<P: SystemPlatform> Status for SystemService<P> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(
            status_field::RESET_REASON,
            "reset_reason",
            ValueKind::String,
        ),
        FieldDescriptor::new(status_field::CRASHES, "crashes", ValueKind::U64),
        FieldDescriptor::new(status_field::CRASH_STREAK, "crash_streak", ValueKind::U64),
        FieldDescriptor::new(status_field::LAST_CRASH, "last_crash", ValueKind::String),
        FieldDescriptor::new(
            status_field::LAST_CRASH_AT,
            "last_crash_at",
            ValueKind::String,
        ),
        FieldDescriptor::new(
            status_field::LAST_CRASH_UPTIME,
            "last_crash_uptime",
            ValueKind::U64,
        )
        .unit(Unit::Ms),
        FieldDescriptor::new(
            status_field::LAST_CRASH_LOGGED,
            "last_crash_logged",
            ValueKind::Bool,
        ),
    ];
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::CRASHED, "crashed"),
        IssueDescriptor::new(issue::CRASH_LOOP, "crash_loop"),
        IssueDescriptor::new(issue::CRASH_NOT_LOGGED, "crash_not_logged"),
        IssueDescriptor::new(issue::UNEXPECTED_RESET, "unexpected_reset"),
    ];
    // Diagnostics: the device works without them.
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        let health = match self.0.boot_report() {
            Some(report) if in_loop(&report) => ServiceHealth::Degraded,
            _ => ServiceHealth::Ok,
        };
        ServiceSummary::new(ServiceState::Running, health, true)
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let Some(report) = self.0.boot_report() else {
            return;
        };
        let mut add = |code, severity| {
            let _ = out.push(Issue {
                severity,
                code,
                age_ms: None,
            });
        };
        if in_loop(&report) {
            add(issue::CRASH_LOOP, IssueSeverity::Error);
        } else if report.reset == Reset::Panic {
            add(issue::CRASHED, IssueSeverity::Warning);
        } else if report.reset.is_fault() {
            add(issue::UNEXPECTED_RESET, IssueSeverity::Warning);
        }
        if report.last.is_some_and(|stored| stored.pending) {
            add(issue::CRASH_NOT_LOGGED, IssueSeverity::Warning);
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let Some(report) = self.0.boot_report() else {
            return Ok(());
        };
        out.text(status_field::RESET_REASON, report.reset.name())?;
        let Some(stored) = report.last else {
            return out.u64(status_field::CRASHES, 0);
        };
        let crash = &stored.crash;
        out.u64(status_field::CRASHES, u64::from(crash.serial()))?;
        out.u64(status_field::CRASH_STREAK, u64::from(crash.streak()))?;
        let mut message = Short::new();
        let _ = message.write_str(crash.message());
        out.text(status_field::LAST_CRASH, message.as_str())?;
        let at = location(&report);
        if !at.as_str().is_empty() {
            out.text(status_field::LAST_CRASH_AT, at.as_str())?;
        }
        out.u64(status_field::LAST_CRASH_UPTIME, crash.uptime_ms())?;
        out.bool(status_field::LAST_CRASH_LOGGED, !stored.pending)
    }
}

/// Panics that follow one another shortly after boot, as many as the default
/// [`Policy`] calls a crash loop, and the latest start was one of them.
fn in_loop(report: &Report) -> bool {
    report.reset == Reset::Panic && report.rapid() >= Policy::DEFAULT.max_rapid
}
