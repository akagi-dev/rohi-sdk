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

use esp_hal::rtc_cntl::SocResetReason as Soc;
use rohi_crash::Reset;

/// The reset reason of the chip as a portable [`Reset`].
///
/// The chip reports a brown-out and the first power-up with the same code as
/// a power-up and the reset pin; all three are [`Reset::PowerOn`] here.
pub fn reset_reason() -> Reset {
    match esp_hal::system::reset_reason() {
        Some(Soc::ChipPowerOn) => Reset::PowerOn,
        Some(Soc::CoreSw | Soc::Cpu0Sw) => Reset::Software,
        Some(Soc::CoreDeepSleep) => Reset::DeepSleep,
        Some(
            Soc::CoreMwdt0
            | Soc::CoreMwdt1
            | Soc::CoreRtcWdt
            | Soc::Cpu0Mwdt0
            | Soc::Cpu0Mwdt1
            | Soc::Cpu0RtcWdt
            | Soc::SysRtcWdt
            | Soc::SysSuperWdt,
        ) => Reset::Watchdog,
        Some(Soc::SysBrownOut) => Reset::Brownout,
        Some(_) | None => Reset::Other,
    }
}

pub(super) const fn code(reset: Reset) -> u8 {
    match reset {
        Reset::PowerOn => 1,
        Reset::Software => 2,
        Reset::Watchdog => 3,
        Reset::Brownout => 4,
        Reset::DeepSleep => 5,
        Reset::Other => 6,
        Reset::Panic => 7,
    }
}

pub(super) const fn from_code(code: u8) -> Reset {
    match code {
        1 => Reset::PowerOn,
        2 => Reset::Software,
        3 => Reset::Watchdog,
        4 => Reset::Brownout,
        5 => Reset::DeepSleep,
        7 => Reset::Panic,
        _ => Reset::Other,
    }
}
