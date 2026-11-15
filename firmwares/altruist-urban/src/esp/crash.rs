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

//! Crash records of the Altruist Urban: the panic of the previous run in the
//! persistent log.
//!
//! ```text
//! panic ─► rohi_sdk::esp::crash::on_panic ─► retained RTC RAM ─► reset
//!                                                                  │
//!   boot:  chip::init() ─► reset reason (the `system` status)      │
//!   log task, right after the storage opens:                       ▼
//!     Backends::boot_record(i) ─► rohi-log (ahead of this run's records)
//!     Backends::boot_records_written ─► chip::acknowledge(serial)
//! ```
//!
//! The lines of the record are handed to the log task through the
//! `Backends` hooks of [`FlashBackends`](super::logging::FlashBackends), so
//! they are written outside the queue (a full queue or a level of `off` cannot
//! lose them) and released only when the storage has them. Until then the
//! record stays in the retained memory and the `system` status reports
//! `crash_not_logged`. The panic path itself is in `rohi_sdk::esp::crash` and
//! uses none of this.

use log::{info, warn};
use rohi_sdk::crash::{Report, Reset};
use rohi_sdk::esp::crash as chip;
use rohi_sdk::logging::{LogLevel, LogRecord};

/// The log target of the lines of a crash.
const TARGET: &str = "panic";

/// Logs why the chip started: a fault (a panic, a watchdog, a brown-out) is a
/// warning, so that it reaches the persistent log at the production level.
pub fn log_reset(reset: Reset) {
    if reset.is_fault() {
        warn!(target: "system", "Reset (reason={})", reset.name());
    } else {
        info!(target: "system", "Reset (reason={})", reset.name());
    }
}

/// The `system` status of this boot.
pub fn boot_report() -> Option<Report> {
    Some(chip::report())
}

/// The `index`-th line of the panic the previous run left in the retained
/// memory, as a log record stamped with the time of the panic; `None` past the
/// last line or when there is no record the log lacks.
pub fn boot_record(index: usize) -> Option<LogRecord> {
    let stored = chip::pending()?;
    let line = stored.crash.line(index)?;
    Some(LogRecord::new(
        LogLevel::Error,
        TARGET,
        format_args!("{line}"),
        stored.crash.uptime_ms(),
        None,
    ))
}

/// The log has the lines: the retained record is released.
pub fn boot_records_written() {
    if let Some(stored) = chip::pending() {
        chip::acknowledge(stored.crash.serial());
    }
}
