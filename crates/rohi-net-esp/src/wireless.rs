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
//! the `WIFI` and `BT` peripherals and the single wireless heap and hands out
//! each driver once. Nothing else in the firmware may initialize radio state or
//! add heap regions for the radio.
//!
//! The firmware decides how much RAM the wireless stack may use: it sizes the
//! heap with [`wireless_heap!`](crate::wireless_heap!) and the static IP and BLE
//! buffers with [`NetResources`] and [`BleResources`].

use esp_hal::peripherals::{BT, WIFI};
use log::info;
use rohi_net::{BleConfig, WifiError};

use crate::ble::{BleResources, BleRunner, EspBle};
use crate::ip::{EspIp, NetResources};
use crate::wifi::{EspWifi, NetRunner};

/// Proof that the wireless heap was added to the allocator; created only by
/// [`wireless_heap!`](crate::wireless_heap!).
///
/// The heap is a fixed region: only the radio drivers allocate from it, ROHI
/// application code stays heapless, and Wi-Fi and BLE draw from the same
/// heap. Verify [`WirelessHeap::min_free`] on hardware with both radios
/// active before changing its size.
pub struct WirelessHeapRegion {
    size: usize,
}

impl WirelessHeapRegion {
    #[doc(hidden)]
    pub const fn __new(size: usize) -> Self {
        Self { size }
    }
}

/// Adds a wireless heap of `` bytes (a constant expression) to the
/// global allocator and returns the [`WirelessHeapRegion`] that
/// [`EspWireless::new`] needs. Call it once, before `esp_rtos::start`.
///
/// ```ignore
/// let heap = rohi_net_esp::wireless_heap!(WIRELESS_HEAP_SIZE);
/// let mut wireless = EspWireless::new(peripherals.WIFI, peripherals.BT, heap);
/// ```
#[macro_export]
macro_rules! wireless_heap {
    ($size:expr) => {{
        $crate::__private::esp_alloc::heap_allocator!(size: $size);
        $crate::WirelessHeapRegion::__new($size)
    }};
}

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
    /// Creates the shared wireless runtime on the heap created by
    /// [`wireless_heap!`](crate::wireless_heap!).
    ///
    /// Call exactly once, before `esp_rtos::start` and before any radio use;
    /// the peripherals and the heap region are moved in, so a second call
    /// cannot compile.
    pub fn new(wifi: WIFI<'static>, bt: BT<'static>, heap: WirelessHeapRegion) -> Self {
        info!("Wireless heap: {} B", heap.size);
        Self {
            wifi: Some(wifi),
            bt: Some(bt),
        }
    }

    /// Brings up Wi-Fi in station mode and the IP stack (DHCPv4).
    ///
    /// `resources` is the static storage of the IP stack, sized by the
    /// firmware. Returns the Wi-Fi driver for the `WifiManager`, the runner
    /// of the IP stack and the IP backend behind
    /// [`NetworkHandle`](rohi_net::NetworkHandle).
    ///
    /// The `esp-rtos` scheduler must be running. May be called once.
    pub fn wifi<
        const SOCKETS: usize,
        const TCP: usize,
        const RX: usize,
        const TX: usize,
        const UDP: usize,
        const UDP_BUF: usize,
    >(
        &mut self,
        resources: &'static mut NetResources<SOCKETS, TCP, RX, TX, UDP, UDP_BUF>,
    ) -> Result<(EspWifi, NetRunner, EspIp<TCP, RX, TX, UDP, UDP_BUF>), WifiError> {
        let wifi = self.wifi.take().ok_or(WifiError::DriverError)?;
        let (stack_resources, tcp, udp) = resources.split();
        let (wifi, runner, stack) = EspWifi::new(wifi, stack_resources)?;
        Ok((wifi, runner, EspIp::new(stack, tcp, udp)))
    }

    /// Brings up the BLE controller and host stack as a GATT peripheral
    /// exposing the byte pipe described by `config`, with the static storage
    /// and bounds of `resources`.
    ///
    /// The `esp-rtos` scheduler must be running. May be called once. The
    /// returned runner must be polled for the transport to work.
    #[allow(clippy::type_complexity)]
    pub fn ble<
        const RX_DEPTH: usize,
        const TX_DEPTH: usize,
        const CONNECTIONS: usize,
        const L2CAP_CHANNELS: usize,
        const ATTRIBUTES: usize,
        const CONTROLLER_SLOTS: usize,
    >(
        &mut self,
        config: BleConfig,
        resources: &'static mut BleResources<
            RX_DEPTH,
            TX_DEPTH,
            CONNECTIONS,
            L2CAP_CHANNELS,
            ATTRIBUTES,
            CONTROLLER_SLOTS,
        >,
    ) -> Result<
        (
            EspBle<RX_DEPTH, TX_DEPTH>,
            BleRunner<
                RX_DEPTH,
                TX_DEPTH,
                CONNECTIONS,
                L2CAP_CHANNELS,
                ATTRIBUTES,
                CONTROLLER_SLOTS,
            >,
        ),
        WirelessError,
    > {
        let bt = self.bt.take().ok_or(WirelessError::AlreadyTaken)?;
        crate::ble::new(bt, config, resources)
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
