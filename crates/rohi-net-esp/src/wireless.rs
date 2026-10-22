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
//! Shared ESP wireless runtime.
//!
//! Wi-Fi and BLE share one radio, one PHY, the coexistence arbiter and one
//! allocator. [`EspWireless`] is the only way to create the drivers: it takes
//! the `WIFI` and `BT` peripherals, adds the single wireless heap to the
//! global allocator and hands out each driver once. Nothing else in the
//! firmware may initialize radio state or add heap regions for the radio.

use esp_hal::peripherals::{BT, WIFI};
use log::info;
use rohi_net::{BleConfig, WifiError};

use crate::ble::{BleRunner, EspBle};
use crate::wifi::{EspWifi, NetRunner};

/// Size of the dedicated heap used by the wireless stack, in bytes.
///
/// Only the radio drivers allocate from it; ROHI application code stays
/// heapless. Wi-Fi and BLE draw from the same heap. Verify
/// [`WirelessHeap::min_free`] on hardware with both radios active before
/// changing it.
pub const WIRELESS_HEAP_SIZE: usize = 128 * 1024;

/// Usage of the wireless heap, for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WirelessHeap {
    /// Total size in bytes.
    pub size: usize,
    /// Free bytes right now.
    pub free: usize,
    /// Lowest number of free bytes seen since boot. Only available with the
    /// `heap-stats` feature.
    pub min_free: Option<usize>,
}

/// A driver was requested twice, or BLE could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WirelessError {
    /// The driver was already created.
    AlreadyTaken,
    /// The BLE controller failed to initialize.
    BleInit,
}

impl core::fmt::Display for WirelessError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::AlreadyTaken => "wireless driver already created",
            Self::BleInit => "BLE controller initialization failed",
        })
    }
}

/// Owner of the shared ESP wireless resources.
///
/// ```text
/// EspWireless::new(WIFI, BT)      allocator, radio peripherals
///   ├── wifi()  -> EspWifi + NetRunner
///   └── ble()   -> EspBle + BleRunner
/// ```
pub struct EspWireless {
    wifi: Option<WIFI<'static>>,
    bt: Option<BT<'static>>,
}

impl EspWireless {
    /// Creates the shared wireless runtime.
    ///
    /// Adds the wireless heap to the global allocator. Call exactly once,
    /// before `esp_rtos::start` and before any radio use; the peripherals are
    /// moved in, so a second call cannot compile.
    pub fn new(wifi: WIFI<'static>, bt: BT<'static>) -> Self {
        esp_alloc::heap_allocator!(size: WIRELESS_HEAP_SIZE);
        info!("Wireless heap: {} B", WIRELESS_HEAP_SIZE);
        Self {
            wifi: Some(wifi),
            bt: Some(bt),
        }
    }

    /// Brings up Wi-Fi in station mode and the IP stack (DHCPv4).
    ///
    /// The `esp-rtos` scheduler must be running. May be called once.
    pub fn wifi(&mut self) -> Result<(EspWifi, NetRunner), WifiError> {
        let wifi = self.wifi.take().ok_or(WifiError::DriverError)?;
        EspWifi::new(wifi)
    }

    /// Brings up the BLE controller and host stack as a GATT peripheral
    /// exposing the byte pipe described by `config`.
    ///
    /// The `esp-rtos` scheduler must be running. May be called once. The
    /// returned runner must be polled for the transport to work.
    pub fn ble(&mut self, config: BleConfig) -> Result<(EspBle, BleRunner), WirelessError> {
        let bt = self.bt.take().ok_or(WirelessError::AlreadyTaken)?;
        crate::ble::new(bt, config)
    }

    /// Current usage of the shared wireless heap.
    pub fn heap(&self) -> WirelessHeap {
        wireless_heap()
    }
}

/// Current usage of the shared wireless heap; usable from any task.
pub fn wireless_heap() -> WirelessHeap {
    let stats = esp_alloc::HEAP.stats();
    WirelessHeap {
        size: stats.size,
        free: esp_alloc::HEAP.free(),
        #[cfg(feature = "heap-stats")]
        min_free: Some(stats.size.saturating_sub(stats.max_usage)),
        #[cfg(not(feature = "heap-stats"))]
        min_free: None,
    }
}
