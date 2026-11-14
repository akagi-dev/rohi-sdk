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

//! Persistent logging of the Altruist Urban.
//!
//! ```text
//! info!(..) ─► log facade ─► Fanout ─┬─► console (esp-println), ESP_LOG level
//!                                    └─► LogSink (level from the `logging` section)
//!                                            │ bounded queue, never blocks
//!                                            ▼
//!                                       logging_task ─► LogStorage ─► `rohi-log` flash ring
//! ```
//!
//! The persistent level is the `logging` config section (the production level
//! `warn`, `info` in a development build) and changes at runtime; the console
//! keeps its own level from `ESP_LOG`. The task is the only code that writes
//! the log partition. Flash programming stalls the CPU, so records are written
//! in batches (see the flush interval of the section), never from the logging
//! call itself.
//!
//! The only backend this firmware has is the internal flash: a board without
//! an SD card reports `storage_unavailable` when `sd` is selected, and never
//! falls back to the flash.

use core::cell::Cell;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Delay;
use log::{LevelFilter, Log, Metadata, Record, warn};
use rohi_sdk::esp::config::FlashRegion;
use rohi_sdk::esp::config::esp::PartitionError;
use rohi_sdk::logging::storage::{FlashLogStorage, FlashMedium, RingStorage, SECTOR_SIZE};
use rohi_sdk::logging::{Backends, LogStorageKind, Logging, LoggingConfig};

use super::flash::Flash;
use crate::logging::{self as shared, INITIAL};
use crate::tasks::RawMutex;
use crate::time;

/// The flash and the `rohi-log` region the log task owns, set by [`start`].
type LogPartition = (Flash, Result<FlashRegion, PartitionError>);

/// What [`start`] hands to the backend.
static PARTITION: Mutex<RawMutex, Cell<Option<LogPartition>>> = Mutex::new(Cell::new(None));

/// The log backends of this board.
pub struct FlashBackends;

impl Backends for FlashBackends {
    type Storage = FlashLogStorage<Flash>;

    async fn open(&mut self, kind: LogStorageKind) -> Option<Self::Storage> {
        match kind {
            LogStorageKind::Internal => {
                let (flash, region) = PARTITION.lock(Cell::get)?;
                let region = match region {
                    Ok(region) => region,
                    Err(error) => {
                        warn_unusable("no usable rohi-log partition", &error);
                        return None;
                    }
                };
                let sectors = region.length / SECTOR_SIZE;
                match FlashMedium::new(flash, region.offset as u32, sectors as u32) {
                    Ok(medium) => Some(RingStorage::new(medium)),
                    Err(error) => {
                        warn_unusable("rohi-log is not a valid region", &error);
                        None
                    }
                }
            }
            // No SD card on this board.
            LogStorageKind::Sd => None,
        }
    }
}

// Out of line, so that the report does not enlarge the future of the task.
#[cold]
#[inline(never)]
fn warn_unusable(what: &str, error: &dyn core::fmt::Debug) {
    warn!("Log storage: {what}: {error:?}");
}

/// The logging runtime of the firmware.
pub type Runtime = shared::Runtime<FlashBackends>;

/// The `logging` service of the control plane.
pub type Service = shared::Service<FlashBackends>;

static LOGGING: Runtime = Logging::new(&time::TIME, FlashBackends, INITIAL);

/// The runtime, for the control plane.
pub fn runtime() -> &'static Runtime {
    &LOGGING
}

/// Hands the effective `logging` section to the runtime; called by the
/// configuration owner after every commit. Synchronous: the level applies to
/// the next log call.
pub fn apply(config: LoggingConfig) {
    LOGGING.apply(config);
}

/// The level of the console, from `ESP_LOG` at build time (`INFO` if unset or
/// not a plain level).
const CONSOLE_LEVEL: LevelFilter = match option_env!("ESP_LOG") {
    Some(value) => parse_level(value),
    None => LevelFilter::Info,
};

const fn parse_level(value: &str) -> LevelFilter {
    const fn is(value: &[u8], name: &[u8]) -> bool {
        if value.len() != name.len() {
            return false;
        }
        let mut index = 0;
        while index < name.len() {
            if value[index].to_ascii_lowercase() != name[index] {
                return false;
            }
            index += 1;
        }
        true
    }
    let value = value.as_bytes();
    if is(value, b"off") {
        LevelFilter::Off
    } else if is(value, b"error") {
        LevelFilter::Error
    } else if is(value, b"warn") {
        LevelFilter::Warn
    } else if is(value, b"debug") {
        LevelFilter::Debug
    } else if is(value, b"trace") {
        LevelFilter::Trace
    } else {
        LevelFilter::Info
    }
}

/// The `log` backend: the console and the persistent sink, each with its own
/// level.
struct Fanout;

impl Log for Fanout {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= CONSOLE_LEVEL || LOGGING.sink().enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if record.level() <= CONSOLE_LEVEL {
            esp_println::println!("{} - {}", record.level(), record.args());
        }
        LOGGING.sink().log(record);
    }

    fn flush(&self) {}
}

/// Installs the `log` backend. The ceiling is `debug` so that the persistent
/// level can be raised to `debug` at runtime without a new firmware; the
/// console level stays what `ESP_LOG` says. Call once, first thing at boot.
pub fn init_logger() {
    // SAFETY: called once, before any other task runs and before interrupts
    // that log are enabled; the same sequence `esp-println` uses.
    unsafe {
        // Only fails when a logger is already installed: keep it.
        let _ = log::set_logger_racy(&Fanout);
        log::set_max_level_racy(LevelFilter::Debug);
    }
}

#[embassy_executor::task]
async fn logging_task() {
    LOGGING.run(Delay).await
}

/// Starts the logging task on the `rohi-log` partition. A missing partition
/// does not stop the firmware: the service reports `storage_unavailable`.
pub fn start(spawner: Spawner, flash: Flash, region: Result<FlashRegion, PartitionError>) {
    PARTITION.lock(|cell| cell.set(Some((flash, region))));
    crate::tasks::spawn(spawner, "logging_task", logging_task());
}
