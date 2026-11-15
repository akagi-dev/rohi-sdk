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
#![allow(
    clippy::panic,
    reason = "a test hook: it panics on purpose, and only in a build that asks for it"
)]

//! Deliberate panics for the hardware checklist of the crash records
//! (`docs/validation/hardware-testing.md`). Built only with the `crash-inject`
//! feature, never in a production image.
//!
//! The point is chosen at build time: `ROHI_CRASH_AT=<point> cargo build-esp32c3
//! --features crash-inject` (the variable is read with `option_env!`, so
//! changing it rebuilds). Without the variable nothing panics.
//!
//! | Point | Where it panics |
//! |-------|-----------------|
//! | `early` | at the top of `main`, before `esp_hal::init`: every boot, so it is also the crash loop |
//! | `flash` | right after the flash driver is shared, **inside** its lock |
//! | `logging` | in the log task, while it opens the storage, **holding** the log lock |
//! | `running` | 30 s after everything started: normal operation, radios and sensors running |

use embassy_time::{Duration, Timer};

const POINT: Option<&str> = option_env!("ROHI_CRASH_AT");

/// How long `running` waits after the start.
const RUNNING_DELAY: Duration = Duration::from_secs(30);

fn armed(point: &str) -> bool {
    POINT == Some(point)
}

/// Panics if `point` is the chosen one.
pub fn at(point: &str) {
    if armed(point) {
        panic!("crash-inject: {point}");
    }
}

/// Waits and then panics if `running` is the chosen point; otherwise returns at once.
pub async fn running() {
    if armed("running") {
        Timer::after(RUNNING_DELAY).await;
        panic!("crash-inject: running");
    }
}
