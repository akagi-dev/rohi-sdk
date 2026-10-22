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

//! Wi-Fi station runtime.
//!
//! [`WifiManager`] is the single owner of the ESP Wi-Fi backend. Other tasks
//! (the control plane) only use the [`WifiHandle`] returned by [`handle`]; they
//! never touch the radio driver.

use embassy_time::{Delay, Duration, with_timeout};
use log::{error, info, warn};
use rohi_net::{
    ManagerConfig, WifiConfig, WifiHandle, WifiManager, WifiShared, WifiState, WifiStatus,
};
use rohi_net_esp::{EspWifi, NetRunner};

use crate::config::ConfigHandle;
use crate::tasks::RawMutex;

static WIFI: WifiShared<RawMutex> = WifiShared::new();

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
    let mut manager = WifiManager::new(backend, Delay, &WIFI, ManagerConfig::DEFAULT);
    manager.run().await
}

/// Reads the persisted Wi-Fi configuration, if any.
pub async fn load_config(config: ConfigHandle) -> Option<WifiConfig> {
    match config.get::<WifiConfig>().await {
        Ok(config) => config,
        Err(error) => {
            warn!("Wi-Fi configuration unreadable: {}", error);
            None
        }
    }
}

/// Applies the persisted configuration at boot. Without one the manager
/// stays `NotConfigured` and does not attempt anything. Returns whether a
/// configuration was applied.
pub async fn apply_stored_config(config: ConfigHandle) -> bool {
    let Some(stored) = load_config(config).await else {
        info!("Wi-Fi: not configured");
        return false;
    };
    if let Err(err) = handle().apply_config(Some(stored)).await {
        error!("Wi-Fi: cannot apply stored configuration: {}", err);
        return false;
    }
    true
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
