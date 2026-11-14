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

use esp_hal::peripherals::RTC_TIMER;
use esp_hal::rtc_cntl::{Rtc, RwdtStage, RwdtStageAction};
use esp_hal::time::Duration;

/// The longest wait the handler asks of the watchdog, in milliseconds. The
/// register holds hours at the lowest clock; one hour is far inside it.
const LONGEST_MS: u32 = 3_600_000;

/// Arms the RTC watchdog to reset the whole system in `timeout_ms`
/// milliseconds (at most an hour), replacing what is configured.
///
/// Called from the panic handler, so it takes the peripheral without owning
/// it: after a panic nothing else will touch the watchdog.
pub(super) fn arm(timeout_ms: u32) {
    // SAFETY: only the panic handler calls this, with interrupts off and
    // never returning to code that uses the RTC timer.
    let mut rtc = Rtc::new(unsafe { RTC_TIMER::steal() });
    rtc.rwdt.set_timeout(
        RwdtStage::Stage0,
        Duration::from_millis(u64::from(timeout_ms.min(LONGEST_MS))),
    );
    rtc.rwdt
        .set_stage_action(RwdtStage::Stage0, RwdtStageAction::ResetSystem);
    rtc.rwdt.enable();
    rtc.rwdt.feed();
}
