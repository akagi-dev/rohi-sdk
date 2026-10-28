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
//! [`WifiManager`] is the single owner of the ESP Wi-Fi backend. Other tasks
//! (the control plane) only use the [`WifiHandle`] returned by [`handle`]; they
//! never touch the radio driver. The persisted configuration reaches the
//! manager as desired state, published by the configuration owner task, see
//! [`crate::config`].

use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Instant, with_timeout};
use log::{info, warn};
use rohi_net::{
    HeapStats, IpUsage, ManagerConfig, NetworkHandle, NetworkProbe, WifiError, WifiHandle,
    WifiManager, WifiProbe, WifiShared, WifiState, WifiStatus,
};
use rohi_net_esp::{EspWifi, EspWireless, NetRunner, wireless_heap};
use static_cell::StaticCell;

use crate::resources::{Ip, NetResources};
use crate::tasks::RawMutex;

static WIFI: WifiShared<RawMutex> = WifiShared::new();
static NET_RESOURCES: StaticCell<NetResources> = StaticCell::new();
static IP: StaticCell<Ip> = StaticCell::new();

/// Brings up Wi-Fi and the IP stack with the sizes of [`crate::resources`]
/// and starts their tasks ([`wifi_task`] and [`net_runner_task`]).
///
/// Returns the [`NetworkHandle`] of the IP data plane (DNS and TCP). Call once.
pub fn start(
    spawner: Spawner,
    wireless: &mut EspWireless,
) -> Result<NetworkHandle<'static, Ip>, WifiError> {
    let (backend, runner, ip) = wireless.wifi(NET_RESOURCES.init(NetResources::new()))?;
    let ip: &'static Ip = IP.init(ip);
    spawner.spawn(net_runner_task(runner).unwrap());
    spawner.spawn(wifi_task(backend).unwrap());
    Ok(NetworkHandle::new(ip))
}

/// Milliseconds since boot: the clock of the link uptime.
fn uptime_ms() -> u64 {
    Instant::now().as_millis()
}

/// What the `wifi` and `network` services read from the platform: the wireless
/// heap and the clock, and the socket usage of the IP stack (when it started).
#[derive(Clone, Copy)]
pub struct Probe {
    ip: Option<NetworkHandle<'static, Ip>>,
}

impl Probe {
    /// `ip` is the IP data plane returned by [`start`], `None` if it did not start.
    pub const fn new(ip: Option<NetworkHandle<'static, Ip>>) -> Self {
        Self { ip }
    }
}

impl WifiProbe for Probe {
    fn heap(&self) -> Option<HeapStats> {
        Some(wireless_heap().stats())
    }

    fn now_ms(&self) -> Option<u64> {
        Some(uptime_ms())
    }
}

impl NetworkProbe for Probe {
    fn sockets(&self) -> Option<IpUsage> {
        self.ip.map(|ip| ip.usage())
    }
}

/// Handle to command the Wi-Fi manager and read its published state.
pub fn handle() -> WifiHandle<'static, RawMutex> {
    WIFI.handle()
}

/// Runs the IP stack.
#[embassy_executor::task]
pub async fn net_runner_task(runner: NetRunner) {
    runner.run().await
}

/// Runs the Wi-Fi connection manager.
#[embassy_executor::task]
pub async fn wifi_task(backend: EspWifi) {
    let mut manager = WifiManager::new(
        backend,
        Delay,
        &WIFI,
        ManagerConfig::DEFAULT.with_clock(uptime_ms),
    );
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
