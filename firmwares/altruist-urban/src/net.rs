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

//! Wi-Fi station runtime.
//!
//! [`WifiManager`] is the single owner of the platform Wi-Fi backend. Other
//! tasks (the control plane) only use the [`WifiHandle`] returned by
//! [`handle`]; they never touch the radio driver. The persisted configuration
//! reaches the manager as desired state, published by the configuration owner
//! task, see [`crate::config`].
//!
//! The backend (`EspWifi` on the ESP, the simulated backend of the `std` platform) and
//! the IP stack are the platform's; this module holds what is common: the
//! shared state, the manager loop, the radio heap probe of the `wireless`
//! service and the wait for the first attempt.

use embassy_time::{Delay, Duration, Instant, with_timeout};
use log::{info, warn};
use rohi_sdk::net::{
    HeapStats, ManagerConfig, WifiBackend, WifiHandle, WifiManager, WifiShared, WifiState,
    WifiStatus, WirelessProbe,
};

use crate::tasks::RawMutex;

static WIFI: WifiShared<RawMutex> = WifiShared::new();

/// Milliseconds since boot: the clock of the link metrics.
pub fn uptime_ms() -> u64 {
    Instant::now().as_millis()
}

/// What the `wireless` service reads from the platform: the wireless heap,
/// when the platform has one.
#[derive(Clone, Copy)]
pub struct Probe {
    heap: Option<fn() -> HeapStats>,
}

impl Probe {
    /// `heap` reads the wireless heap, `None` on a platform without one.
    pub const fn new(heap: Option<fn() -> HeapStats>) -> Self {
        Self { heap }
    }
}

impl WirelessProbe for Probe {
    fn heap(&self) -> Option<HeapStats> {
        self.heap.map(|heap| heap())
    }
}

/// Handle to command the Wi-Fi manager and read its published state.
pub fn handle() -> WifiHandle<'static, RawMutex> {
    WIFI.handle()
}

/// The manager settings of the firmware: the defaults with the uptime clock.
pub fn manager_config() -> ManagerConfig {
    ManagerConfig::DEFAULT.with_clock(uptime_ms)
}

/// Runs the Wi-Fi connection manager over `backend`; never returns. The one
/// manager owns the backend, the handle of [`handle`] is the only way to
/// command it.
pub async fn run<B: WifiBackend>(backend: B, config: ManagerConfig) -> ! {
    let mut manager = WifiManager::new(backend, Delay, &WIFI, config);
    manager.run().await
}

/// Waits, at most `timeout`, until the first connection attempt has finished
/// (connected or failed).
pub async fn wait_first_attempt(timeout: Duration) {
    let wifi = handle();
    let Some(mut status) = wifi.status_receiver() else {
        return;
    };
    let settled = |status: &WifiStatus| {
        matches!(
            status.state,
            WifiState::Connected | WifiState::Disconnected | WifiState::Error | WifiState::Idle
        )
    };
    match with_timeout(timeout, status.get_and(settled)).await {
        Ok(status) => info!("Wi-Fi: first attempt finished: {:?}", status.state),
        Err(_) => warn!(
            "Wi-Fi: first attempt still running after {} s",
            timeout.as_secs()
        ),
    }
}
