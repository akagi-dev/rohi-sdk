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

//! Persistent logging on the host: the log ring in a file.
//!
//! ```text
//! info!(..) ─► log facade ─► Fanout ─┬─► stderr (env_logger), `--log-level` / `RUST_LOG`
//!                                    └─► LogSink (level from the `logging` section)
//!                                            │ bounded queue, never blocks
//!                                            ▼
//!                                       logging task ─► RingStorage ─► FileLogFlash
//! ```
//!
//! It is the logging runtime of the firmware over a different medium: the
//! `internal` backend is a [`FileLogFlash`] with the geometry of the `rohi-log`
//! partition (a file next to the configuration flash, or memory with
//! `--ephemeral`), so the history survives a restart of the process like it
//! survives a reset of the device. There is no SD card on the host: `sd` reports
//! `storage_unavailable` and never falls back to the file.

use std::sync::OnceLock;

use embassy_executor::Spawner;
use embassy_time::Delay;
use log::{LevelFilter, Log, Metadata, Record};
use rohi_sdk::logging::storage::RingStorage;
use rohi_sdk::logging::{Backends, LogStorageKind, Logging, LoggingConfig};
use rohi_sdk::sim::log_storage::FileLogFlash;

use crate::logging::{self as shared, INITIAL};
use crate::time;

/// The flash the `internal` backend opens, set by [`start`].
static FLASH: OnceLock<FileLogFlash> = OnceLock::new();

/// The log backends of the host.
pub struct HostBackends;

impl Backends for HostBackends {
    type Storage = RingStorage<FileLogFlash>;

    async fn open(&mut self, kind: LogStorageKind) -> Option<Self::Storage> {
        match kind {
            LogStorageKind::Internal => Some(RingStorage::new(FLASH.get()?.clone())),
            // No SD card on the host.
            LogStorageKind::Sd => None,
        }
    }
}

/// The logging runtime of the host application.
pub type Runtime = shared::Runtime<HostBackends>;

/// The `logging` service of the control plane.
pub type Service = shared::Service<HostBackends>;

static LOGGING: Runtime = Logging::new(&time::TIME, HostBackends, INITIAL);

/// The runtime, for the control plane.
pub fn runtime() -> &'static Runtime {
    &LOGGING
}

/// Hands the effective `logging` section to the runtime; called by the
/// configuration owner after every commit.
pub fn apply(config: LoggingConfig) {
    LOGGING.apply(config);
}

/// The `log` backend: the console and the persistent sink, each with its own
/// level.
struct Fanout {
    console: env_logger::Logger,
}

impl Log for Fanout {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.console.enabled(metadata) || LOGGING.sink().enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        self.console.log(record);
        LOGGING.sink().log(record);
    }

    fn flush(&self) {
        self.console.flush();
    }
}

static FANOUT: OnceLock<Fanout> = OnceLock::new();

/// Installs the `log` backend: readable timestamps on stderr at `level` (or what
/// `RUST_LOG` says), and the persistent sink at the level of the `logging`
/// section. The ceiling is `debug` at least, so that the persistent level can be
/// raised at runtime. A second call does nothing.
pub fn init_logger(level: LevelFilter) {
    let console = env_logger::Builder::new()
        .filter_level(level)
        .parse_default_env()
        .format_timestamp_millis()
        .build();
    let ceiling = console.filter().max(LevelFilter::Debug);
    let fanout = FANOUT.get_or_init(|| Fanout { console });
    if log::set_logger(fanout).is_ok() {
        log::set_max_level(ceiling);
    }
}

#[embassy_executor::task]
async fn logging_task() {
    LOGGING.run(Delay).await
}

/// Starts the logging task on `flash`.
pub fn start(spawner: Spawner, flash: FileLogFlash) {
    let _ = FLASH.set(flash);
    crate::tasks::spawn(spawner, "logging_task", logging_task());
}
