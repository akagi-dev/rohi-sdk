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

//! Persistent logging of the Altruist Urban: the portable part.
//!
//! ```text
//! info!(..) ─► log facade ─► platform logger ─┬─► console (esp-println, stderr)
//!                                             └─► LogSink (level from the `logging` section)
//!                                                     │ bounded queue, never blocks
//!                                                     ▼
//!                                                logging task ─► LogStorage (platform)
//! ```
//!
//! The persistent level, the storage, the codec and the sizes are the `logging`
//! config section: the firmware policy is `warn` in a release build and `info`
//! in a development build, and a commit applies without a restart (see
//! [`crate::bindings::LoggingBinding`]). The console keeps a level of its own.
//!
//! The platform owns what is physical: the log storage (the `rohi-log` flash
//! partition on the ESP, a file on the host), the `log` backend that fans out to
//! the console and the sink, and the Embassy task that drives the runtime (a task
//! cannot be generic). This module has the parts every platform shares: the
//! queue and read sizes, the aliases of the runtime and of the service for the
//! [`Backends`](rohi_sdk::logging::Backends) of a platform, and the initial configuration.

use rohi_sdk::logging::{Logging, LoggingConfig, LoggingService};

use crate::tasks::RawMutex;
use crate::time::Time;

/// Records the persistent log queue holds between the logging calls and the
/// logging task.
///
/// - Consumer: `rohi_sdk::logging::LogSink`, one `LogRecord` of about 200 bytes
///   per entry, so this is about 5 KiB of static RAM.
/// - Why: the task takes the queue empty every time it runs; the depth covers
///   a burst of boot messages before the task and the storage are up, and the
///   stall while a flash sector is erased (tens of milliseconds).
/// - Exhausted: the new record is dropped and counted in the `dropped_records`
///   status field of the `logging` service; the caller never waits.
/// - Measured: not measured.
pub const QUEUE_DEPTH: usize = 24;

/// Bytes of text one `logging.read` returns.
///
/// - Consumer: each open `read` stream of the `logging` service holds its
///   answer in RAM, two streams at most (`rohi_sdk::logging` keeps two).
/// - Why: about twenty records of text per call keeps a read to a few
///   round trips on BLE and the memory small.
/// - Exhausted: the answer ends at the last record that fits and the client
///   asks again after the last sequence it got.
/// - Measured: not measured.
pub const READ_BYTES: usize = 2048;

/// The logging runtime over the backends `K` of a platform.
pub type Runtime<K> = Logging<RawMutex, &'static Time, K, QUEUE_DEPTH, READ_BYTES>;

/// The `logging` service of the control plane over the backends `K` of a
/// platform.
pub type Service<K> = LoggingService<'static, RawMutex, &'static Time, K, QUEUE_DEPTH, READ_BYTES>;

/// What a build persists before the stored configuration is applied: the
/// default section with the persistent level of the build profile.
pub const INITIAL: LoggingConfig = LoggingConfig {
    level: if cfg!(debug_assertions) {
        rohi_sdk::logging::DEVELOPMENT_LEVEL
    } else {
        rohi_sdk::logging::PRODUCTION_LEVEL
    },
    ..LoggingConfig::DEFAULT
};
