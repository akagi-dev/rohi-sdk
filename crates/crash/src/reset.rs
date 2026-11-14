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

//! Why the chip started.

/// Why the chip started this time.
///
/// A platform maps the reset reason of its chip to the first six variants;
/// [`Reset::observed`] turns a software or watchdog reset that followed a
/// recorded panic into [`Reset::Panic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    /// The chip lost power, or the reset pin was pulled. Retained memory is
    /// not kept.
    PowerOn,
    /// The firmware asked for it (`system.reboot`) or the cause is unknown
    /// but not a fault.
    Software,
    /// A watchdog fired.
    Watchdog,
    /// The supply voltage dropped below the brown-out threshold.
    Brownout,
    /// The chip woke from deep sleep.
    DeepSleep,
    /// Any other reason of the chip.
    Other,
    /// The firmware panicked and the panic handler reset the chip (or a
    /// watchdog ended its cool-down).
    Panic,
}

impl Reset {
    /// The name shown in the `system` status.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PowerOn => "power_on",
            Self::Software => "software",
            Self::Watchdog => "watchdog",
            Self::Brownout => "brownout",
            Self::DeepSleep => "deep_sleep",
            Self::Other => "other",
            Self::Panic => "panic",
        }
    }

    /// The reset that really happened, given the one the chip reports and
    /// whether the retained memory held an unlogged panic at boot.
    ///
    /// Only a software or watchdog reset can follow a panic: the panic handler
    /// resets by software, and a watchdog ends a hung handler or the cool-down
    /// of a crash loop. A power-up empties the retained memory.
    pub const fn observed(reported: Self, crash_recorded: bool) -> Self {
        match reported {
            Self::Software | Self::Watchdog if crash_recorded => Self::Panic,
            other => other,
        }
    }

    /// Whether the reset is not a normal start: a fault the operator should look at.
    pub const fn is_fault(self) -> bool {
        matches!(self, Self::Panic | Self::Watchdog | Self::Brownout)
    }
}
