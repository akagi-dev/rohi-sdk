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

//! The simulated Wi-Fi backend.
//!
//! IEEE 802.11 is not simulated. [`SimWifi`] implements the portable
//! [`WifiBackend`] contract on top of the simulated access points of the
//! [`World`], so the real [`WifiManager`] state machine
//! (desired-state reconciliation, backoff, link-loss handling) runs unchanged:
//!
//! ```text
//! wifi / network config -> WifiManager -> SimWifi -> World (access points, faults)
//! ```
//!
//! Connection outcomes flow through the portable error model:
//! `NetworkNotFound`, `AuthenticationFailed`, `AssociationFailed`, `DhcpFailed`.
//!
//! [`WifiManager`]: rohi_net::WifiManager

use core::net::Ipv4Addr;

use embassy_futures::select::{Either, select};
use embassy_time::{Instant, Timer};
use log::{info, warn};
use rohi_net::{
    AddressMode, LinkConfig, NetworkInfo, ScanCollector, WifiBackend, WifiError, WifiNetwork,
    WifiScanResults, WifiSecurity, WifiSsid,
};

use crate::command::WifiFault;
use crate::world::{AccessPoint, World, world};

/// What a scan takes.
const SCAN_MS: u64 = 150;

/// The MAC address of the simulated station.
const MAC: [u8; 6] = [0x02, 0x52, 0x4F, 0x48, 0x49, 0x01];

/// The simulated station interface.
pub struct SimWifi {
    world: &'static World,
    connected: Option<Connection>,
}

struct Connection {
    ssid: String,
    info: NetworkInfo,
}

impl SimWifi {
    pub fn new() -> Self {
        Self {
            world: world(),
            connected: None,
        }
    }
}

impl Default for SimWifi {
    fn default() -> Self {
        Self::new()
    }
}

fn ssid_text(ssid: &WifiSsid) -> String {
    String::from_utf8_lossy(ssid.as_bytes()).into_owned()
}

fn addressing(config: &LinkConfig) -> (Ipv4Addr, Option<Ipv4Addr>, Option<Ipv4Addr>) {
    let network = &config.network;
    match (network.address_mode, network.static_ip) {
        (AddressMode::Static, Some(ip)) => (
            Ipv4Addr::from(ip),
            network.gateway.map(Ipv4Addr::from),
            network.dns.map(Ipv4Addr::from),
        ),
        _ => (
            Ipv4Addr::new(10, 42, 0, 2),
            Some(Ipv4Addr::new(10, 42, 0, 1)),
            Some(Ipv4Addr::new(10, 42, 0, 1)),
        ),
    }
}

impl WifiBackend for SimWifi {
    async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
        Timer::after_millis(SCAN_MS).await;
        let state = self.world.lock();
        let mut found = ScanCollector::new();
        if state.wifi.network_up && Instant::now().as_millis() >= state.wifi.blocked_until_ms {
            for ap in state.wifi.aps.iter().filter(|ap| ap.available) {
                let Some(ssid) = WifiSsid::from_bytes(ap.ssid.as_bytes()) else {
                    continue;
                };
                found.insert(WifiNetwork {
                    ssid,
                    rssi_dbm: ap.rssi,
                    security: ap.security,
                    channel: ap.channel,
                });
            }
        }
        Ok(found.finish())
    }

    async fn connect(&mut self, config: &LinkConfig) -> Result<NetworkInfo, WifiError> {
        let ssid = config.wifi.ssid.as_ref().ok_or(WifiError::NotConfigured)?;
        let name = ssid_text(ssid);
        let delay = self.world.lock().wifi.connect_delay_ms;
        if delay > 0 {
            Timer::after_millis(u64::from(delay)).await;
        }
        let (ap, fault): (AccessPoint, Option<WifiFault>) = {
            let state = self.world.lock();
            let usable =
                state.wifi.network_up && Instant::now().as_millis() >= state.wifi.blocked_until_ms;
            let ap = state
                .wifi
                .aps
                .iter()
                .find(|ap| ap.ssid == name && ap.available && usable)
                .cloned();
            match ap {
                Some(ap) => (ap, state.wifi.fault),
                None => {
                    warn!("Wi-Fi: network `{name}` not found");
                    return Err(WifiError::NetworkNotFound);
                }
            }
        };
        if fault == Some(WifiFault::Association) {
            return Err(WifiError::AssociationFailed);
        }
        let password = config.wifi.password.as_ref().map(|p| p.expose_secret());
        let accepted = match (ap.security, &ap.password, password) {
            (WifiSecurity::Open, _, _) => true,
            (_, Some(expected), Some(given)) => expected.as_bytes() == given,
            (_, None, Some(given)) => !given.is_empty(),
            (_, _, None) => false,
        };
        if fault == Some(WifiFault::Authentication) || !accepted {
            warn!("Wi-Fi: authentication to `{name}` failed");
            return Err(WifiError::AuthenticationFailed);
        }
        if fault == Some(WifiFault::Dhcp) {
            return Err(WifiError::DhcpFailed);
        }
        let (ipv4, gateway, dns) = addressing(config);
        let info = NetworkInfo {
            ssid: ssid.clone(),
            rssi_dbm: ap.rssi,
            channel: Some(ap.channel),
            mac: Some(MAC),
            ipv4,
            gateway,
            dns,
        };
        {
            let mut state = self.world.lock();
            state.wifi.drop_requested = false;
            state.wifi.link_up = true;
        }
        info!("Wi-Fi: connected to `{name}` as {ipv4}");
        self.connected = Some(Connection {
            ssid: name,
            info: info.clone(),
        });
        Ok(info)
    }

    async fn disconnect(&mut self) -> Result<(), WifiError> {
        self.world.lock().wifi.link_up = false;
        if let Some(connection) = self.connected.take() {
            info!("Wi-Fi: disconnected from `{}`", connection.ssid);
        }
        Ok(())
    }

    async fn wait_link_lost(&mut self) -> WifiError {
        let Some(ssid) = self.connected.as_ref().map(|c| c.ssid.clone()) else {
            return core::future::pending().await;
        };
        loop {
            let flap = {
                let mut state = self.world.lock();
                let usable = state.wifi.network_up
                    && state
                        .wifi
                        .aps
                        .iter()
                        .any(|ap| ap.ssid == ssid && ap.available);
                if state.wifi.drop_requested || !usable {
                    state.wifi.drop_requested = false;
                    state.wifi.link_up = false;
                    drop(state);
                    warn!("Wi-Fi: link to `{ssid}` lost");
                    self.connected = None;
                    return WifiError::Disconnected;
                }
                state.wifi.flap
            };
            match flap {
                Some((up_ms, down_ms)) => {
                    match select(
                        self.world.wifi_changed.wait(),
                        Timer::after_millis(u64::from(up_ms)),
                    )
                    .await
                    {
                        Either::First(()) => {}
                        Either::Second(()) => {
                            {
                                let mut state = self.world.lock();
                                state.wifi.blocked_until_ms =
                                    Instant::now().as_millis() + u64::from(down_ms);
                                state.wifi.link_up = false;
                            }
                            warn!("Wi-Fi: link to `{ssid}` flapped");
                            self.connected = None;
                            return WifiError::Disconnected;
                        }
                    }
                }
                None => self.world.wifi_changed.wait().await,
            }
        }
    }

    fn network_info(&mut self) -> Option<NetworkInfo> {
        let connection = self.connected.as_mut()?;
        if let Some(ap) = self
            .world
            .lock()
            .wifi
            .aps
            .iter()
            .find(|ap| ap.ssid == connection.ssid)
        {
            connection.info.rssi_dbm = ap.rssi;
        }
        Some(connection.info.clone())
    }
}
