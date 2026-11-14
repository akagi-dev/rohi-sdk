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

use rohi_sdk::config::Override;
use rohi_sdk::logging::{Logging, LoggingConfig, LoggingOverrides, LoggingService};

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

/// Retention budget of the chip: the whole `rohi-log` partition, 240 KiB, on the
/// 4 MB ESP32-C3. The ring keeps two of its 4 KiB sectors for the format
/// header, so with [`SEGMENT_BYTES`] it retains 14 full segments (224 KiB).
#[cfg(feature = "esp32c3")]
pub const RETENTION_BYTES: u32 = 240 * 1024;

/// Segment size of the chip: 16 KiB, four erase sectors, on the ESP32-C3.
#[cfg(feature = "esp32c3")]
pub const SEGMENT_BYTES: u32 = 16 * 1024;

/// Retention budget of the chip: the 1 MiB `rohi-log` partition of the
/// ESP32-C6 and the host image.
#[cfg(not(feature = "esp32c3"))]
pub const RETENTION_BYTES: u32 = rohi_sdk::logging::DEFAULT_RETENTION_BYTES;

/// Segment size of the chip: 64 KiB, sixteen erase sectors.
#[cfg(not(feature = "esp32c3"))]
pub const SEGMENT_BYTES: u32 = rohi_sdk::logging::DEFAULT_SEGMENT_BYTES;

const fn persistent_level(development: bool) -> rohi_sdk::logging::LogLevel {
    if development {
        rohi_sdk::logging::DEVELOPMENT_LEVEL
    } else {
        rohi_sdk::logging::PRODUCTION_LEVEL
    }
}

/// What a build persists before the stored configuration is applied: the
/// default section with the persistent level of the build profile and the
/// retention and segment size of the chip.
pub const INITIAL: LoggingConfig = LoggingConfig {
    level: persistent_level(cfg!(debug_assertions)),
    retention_bytes: RETENTION_BYTES,
    segment_bytes: SEGMENT_BYTES,
    ..LoggingConfig::DEFAULT
};

/// The firmware policy of the `logging` section: the level of the build
/// profile and the retention and segment size of the chip. It sits between the
/// defaults and the user overrides, so a user value stays authoritative.
pub fn firmware_overrides(development: bool) -> LoggingOverrides {
    LoggingOverrides {
        retention_bytes: Override::Value(RETENTION_BYTES),
        segment_bytes: Override::Value(SEGMENT_BYTES),
        ..LoggingConfig::firmware_overrides(development)
    }
}

#[cfg(test)]
mod tests {
    use rohi_sdk::config::materialize;

    use super::*;

    #[test]
    fn the_chip_policy_is_what_the_section_starts_from() {
        for development in [false, true] {
            let effective =
                materialize::<LoggingConfig>(&firmware_overrides(development), &Default::default());
            assert_eq!(effective.retention_bytes, RETENTION_BYTES);
            assert_eq!(effective.segment_bytes, SEGMENT_BYTES);
            assert_eq!(effective.validate(), Ok(()));
        }
        assert_eq!(INITIAL.retention_bytes, RETENTION_BYTES);
        assert_eq!(INITIAL.segment_bytes, SEGMENT_BYTES);
        assert_eq!(INITIAL.validate(), Ok(()));
    }

    #[test]
    fn the_user_overrides_beat_the_chip_policy() {
        let user = LoggingOverrides {
            retention_bytes: Override::Value(128 * 1024),
            segment_bytes: Override::Value(8 * 1024),
            ..Default::default()
        };
        let effective = materialize::<LoggingConfig>(&firmware_overrides(false), &user);
        assert_eq!(effective.retention_bytes, 128 * 1024);
        assert_eq!(effective.segment_bytes, 8 * 1024);
    }

    #[test]
    fn both_chip_policies_are_valid_for_their_partition() {
        // ESP32-C3: 240 KiB partition, 16 KiB segments; ESP32-C6 and the host:
        // 1 MiB, 64 KiB segments. Checked on every build, whatever the chip.
        for (retention_bytes, segment_bytes) in [(240 * 1024, 16 * 1024), (1024 * 1024, 64 * 1024)]
        {
            let config = LoggingConfig {
                retention_bytes,
                segment_bytes,
                ..LoggingConfig::DEFAULT
            };
            assert_eq!(config.validate(), Ok(()));
        }
    }

    #[cfg(not(feature = "esp32c3"))]
    #[test]
    fn the_default_policy_is_the_section_default() {
        assert_eq!(RETENTION_BYTES, LoggingConfig::DEFAULT.retention_bytes);
        assert_eq!(SEGMENT_BYTES, LoggingConfig::DEFAULT.segment_bytes);
    }
}
