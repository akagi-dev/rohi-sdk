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

//! ESP32 Wi-Fi station backend.
//!
//! Everything vendor specific lives here: `esp-radio`, the `embassy-net`
//! stack, DHCP and the dynamically allocated radio heap. The backend
//! implements the portable [`WifiBackend`] and converts every vendor type
//! and error into the ROHI types before returning. Heap-backed vendor
//! collections (scan results) are converted and dropped inside this module.
//!
//! The driver is created through [`EspWireless::wifi`](crate::EspWireless::wifi),
//! which owns the shared radio heap and peripherals. Higher layers use
//! [`WifiHandle`](rohi_net::WifiHandle) only.

use core::net::Ipv4Addr;

use embassy_net::{Config as IpConfig, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer, with_timeout};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig,
    DisconnectReason, Interface, Password, Ssid, WifiController, WifiError as VendorError,
};
use log::{debug, warn};
use static_cell::StaticCell;

use rohi_net::{
    NetworkInfo, ScanCollector, WifiBackend, WifiConfig, WifiError, WifiNetwork, WifiScanResults,
    WifiSecurity, WifiSsid,
};

/// Sockets reserved in the IP stack: DHCP, DNS and headroom for later clients.
const NET_SOCKETS: usize = 6;
/// Upper bound of raw scan entries converted per scan (bounds the vendor `Vec`).
const MAX_RAW_SCAN_RESULTS: usize = 48;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DHCP_TIMEOUT: Duration = Duration::from_secs(20);
const STALE_CONFIG_POLL: Duration = Duration::from_millis(50);
const STALE_CONFIG_LIMIT: u32 = 40;

static RESOURCES: StaticCell<StackResources<NET_SOCKETS>> = StaticCell::new();

/// Background driver of the IP stack. Run it on its own task:
/// `runner.run().await`.
pub struct NetRunner(Runner<'static, Interface>);

impl NetRunner {
    pub async fn run(mut self) -> ! {
        self.0.run().await
    }
}

/// ESP32 implementation of [`WifiBackend`].
pub struct EspWifi {
    controller: WifiController<'static>,
    stack: Stack<'static>,
    ssid: Option<WifiSsid>,
}

impl EspWifi {
    /// Brings up the Wi-Fi driver in station mode and the IP stack (DHCPv4).
    ///
    /// Requires the wireless heap and the `esp-rtos` scheduler to be running
    /// and must be called once; see [`EspWireless`](crate::EspWireless).
    pub(crate) fn new(wifi: WIFI<'static>) -> Result<(Self, NetRunner), WifiError> {
        let controller = WifiController::new(wifi, ControllerConfig::default())
            .map_err(|error| map_driver_error(&error))?;
        let interface = Interface::station();

        let seed = (u64::from(Rng::new().random()) << 32) | u64::from(Rng::new().random());
        let (stack, runner) = embassy_net::new(
            interface,
            IpConfig::dhcpv4(Default::default()),
            RESOURCES.init(StackResources::new()),
            seed,
        );

        Ok((
            Self {
                controller,
                stack,
                ssid: None,
            },
            NetRunner(runner),
        ))
    }

    fn current_info(&mut self) -> Option<NetworkInfo> {
        let ssid = self.ssid?;
        let config = self.stack.config_v4()?;
        let rssi_dbm = self
            .controller
            .rssi()
            .ok()
            .and_then(|rssi| i8::try_from(rssi).ok())
            .unwrap_or(i8::MIN);
        Some(NetworkInfo {
            ssid,
            rssi_dbm,
            ipv4: Ipv4Addr::from(config.address.address().octets()),
            gateway: config
                .gateway
                .map(|gateway| Ipv4Addr::from(gateway.octets())),
            dns: config
                .dns_servers
                .first()
                .map(|dns| Ipv4Addr::from(dns.octets())),
        })
    }

    /// Waits until the previous connection's IP configuration is gone, so a
    /// stale lease is never mistaken for the new one.
    async fn wait_stale_config_cleared(&self) {
        for _ in 0..STALE_CONFIG_LIMIT {
            if !self.stack.is_config_up() {
                return;
            }
            Timer::after(STALE_CONFIG_POLL).await;
        }
    }

    async fn connect_inner(&mut self, config: &WifiConfig) -> Result<NetworkInfo, WifiError> {
        self.wait_stale_config_cleared().await;
        let station = station_config(config)?;
        self.controller
            .set_config(&Config::Station(station))
            .map_err(|error| map_driver_error(&error))?;

        self.controller
            .connect_async()
            .await
            .map_err(|error| match error {
                ConnectionError::Failed(info) => map_connect_reason(info.reason),
                ConnectionError::WifiError(error) => map_driver_error(&error),
                _ => WifiError::DriverError,
            })?;

        // Association alone is not network ready: wait for DHCP.
        with_timeout(DHCP_TIMEOUT, self.stack.wait_config_up())
            .await
            .map_err(|_| WifiError::DhcpFailed)?;

        self.ssid = Some(config.ssid);
        self.current_info().ok_or(WifiError::DhcpFailed)
    }
}

impl WifiBackend for EspWifi {
    async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
        let scan = ScanConfig::default().with_max(MAX_RAW_SCAN_RESULTS);
        // The vendor result is a heap-backed `Vec`; it never leaves this function.
        let access_points = self.controller.scan_async(&scan).await.map_err(|error| {
            warn!("Wi-Fi scan failed: {:?}", error);
            WifiError::ScanFailed
        })?;

        let mut collector = ScanCollector::new();
        for access_point in &access_points {
            let Some(ssid) = WifiSsid::from_bytes(access_point.ssid.as_str().as_bytes()) else {
                continue;
            };
            collector.insert(WifiNetwork {
                ssid,
                rssi_dbm: access_point.signal_strength,
                security: map_security(access_point.auth_method),
                channel: access_point.channel,
            });
        }
        debug!("Wi-Fi scan: {} raw result(s)", access_points.len());
        Ok(collector.finish())
    }

    async fn connect(&mut self, config: &WifiConfig) -> Result<NetworkInfo, WifiError> {
        match with_timeout(CONNECT_TIMEOUT, self.connect_inner(config)).await {
            Ok(Ok(info)) => Ok(info),
            Ok(Err(error)) => {
                self.ssid = None;
                let _ = self.disconnect().await;
                Err(error)
            }
            Err(_) => {
                self.ssid = None;
                let _ = self.disconnect().await;
                Err(WifiError::Timeout)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), WifiError> {
        self.ssid = None;
        match self.controller.disconnect_async().await {
            Ok(_) | Err(VendorError::NotConnected) => Ok(()),
            Err(error) => Err(map_driver_error(&error)),
        }
    }

    async fn wait_link_lost(&mut self) -> WifiError {
        match self.controller.wait_for_disconnect_async().await {
            Ok(info) => map_link_lost_reason(info.reason),
            // Not connected (any more): the link is already lost.
            Err(_) => WifiError::Disconnected,
        }
    }

    fn network_info(&mut self) -> Option<NetworkInfo> {
        self.current_info()
    }
}

fn station_config(config: &WifiConfig) -> Result<StationConfig, WifiError> {
    let ssid = Ssid::try_from(config.ssid.as_bytes()).map_err(|_| WifiError::InvalidConfig)?;
    let authentication = match &config.password {
        None => AuthenticationMethodConfig::Open,
        Some(password) => AuthenticationMethodConfig::WpaWpa2Personal(
            Password::try_from(password.expose_secret()).map_err(|_| WifiError::InvalidConfig)?,
        ),
    };
    Ok(StationConfig::default()
        .with_ssid(ssid)
        .with_authentication(authentication))
}

fn map_security(method: Option<AuthenticationMethod>) -> WifiSecurity {
    match method {
        Some(AuthenticationMethod::None) => WifiSecurity::Open,
        Some(AuthenticationMethod::Wep) => WifiSecurity::Wep,
        Some(AuthenticationMethod::Wpa) => WifiSecurity::Wpa,
        Some(AuthenticationMethod::Wpa2Personal) => WifiSecurity::Wpa2,
        Some(AuthenticationMethod::WpaWpa2Personal) => WifiSecurity::WpaWpa2,
        Some(AuthenticationMethod::Wpa3Personal) => WifiSecurity::Wpa3,
        Some(AuthenticationMethod::Wpa2Wpa3Personal) => WifiSecurity::Wpa2Wpa3,
        Some(
            AuthenticationMethod::Wpa2Enterprise
            | AuthenticationMethod::Wpa3Enterprise
            | AuthenticationMethod::Wpa2Wpa3Enterprise
            | AuthenticationMethod::WpaEnterprise,
        ) => WifiSecurity::Enterprise,
        _ => WifiSecurity::Unknown,
    }
}

fn map_connect_reason(reason: DisconnectReason) -> WifiError {
    match reason {
        DisconnectReason::NoAccessPointFound
        | DisconnectReason::NoAccessPointFoundWithCompatibleSecurity
        | DisconnectReason::NoAccessPointFoundInAuthmodeThreshold
        | DisconnectReason::NoAccessPointFoundInRssiThreshold => WifiError::NetworkNotFound,
        DisconnectReason::AuthenticationFailed
        | DisconnectReason::AuthenticationExpired
        | DisconnectReason::FourWayHandshakeTimeout
        | DisconnectReason::HandshakeTimeout
        | DisconnectReason::MicFailure
        | DisconnectReason::InvalidPmkid
        | DisconnectReason::_802_1xAuthenticationFailed => WifiError::AuthenticationFailed,
        DisconnectReason::Timeout | DisconnectReason::BeaconTimeout => WifiError::Timeout,
        _ => WifiError::AssociationFailed,
    }
}

fn map_link_lost_reason(reason: DisconnectReason) -> WifiError {
    match reason {
        DisconnectReason::AuthenticationFailed
        | DisconnectReason::FourWayHandshakeTimeout
        | DisconnectReason::HandshakeTimeout => WifiError::AuthenticationFailed,
        _ => WifiError::Disconnected,
    }
}

fn map_driver_error(error: &VendorError) -> WifiError {
    warn!("Wi-Fi driver error: {:?}", error);
    match error {
        VendorError::InvalidSsid | VendorError::InvalidPassword | VendorError::InvalidArguments => {
            WifiError::InvalidConfig
        }
        _ => WifiError::DriverError,
    }
}
