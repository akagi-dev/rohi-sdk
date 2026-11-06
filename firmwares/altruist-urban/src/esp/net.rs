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

//! ESP Wi-Fi and IP stack: the platform side of [`crate::net`].
//!
//! [`rohi_sdk::net::WifiManager`] is the single owner of the ESP Wi-Fi
//! backend, see [`crate::net`].

use embassy_executor::Spawner;
use rohi_sdk::esp::net::{EspWifi, EspWireless, NetRunner, wireless_heap};
use rohi_sdk::net::{HeapStats, NetworkHandle, WifiError};
use static_cell::StaticCell;

use super::resources::{Ip, NetResources};
use crate::net;

static NET_RESOURCES: StaticCell<NetResources> = StaticCell::new();
static IP: StaticCell<Ip> = StaticCell::new();

/// Reads the wireless heap, for the probe of the `wifi` and `network`
/// services.
pub const fn heap_probe() -> Option<fn() -> HeapStats> {
    Some(heap)
}

fn heap() -> HeapStats {
    wireless_heap().stats()
}

/// Brings up Wi-Fi and the IP stack with the sizes of [`super::resources`]
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

/// Runs the IP stack.
#[embassy_executor::task]
pub async fn net_runner_task(runner: NetRunner) {
    runner.run().await
}

/// Runs the Wi-Fi connection manager.
#[embassy_executor::task]
pub async fn wifi_task(backend: EspWifi) {
    net::run(backend, net::manager_config()).await
}
