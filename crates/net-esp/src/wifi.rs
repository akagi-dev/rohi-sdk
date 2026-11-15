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
//!
//! # Behaviour
//!
//! * **Security.** A configured password selects WPA/WPA2 personal, no
//!   password selects an open network. WPA3-only networks are not supported
//!   (scans still report WPA3 networks, see
//!   [`WifiSecurity`](rohi_net::WifiSecurity)).
//! * **Timeouts.** Association is bounded by the `connect_timeout` of the
//!   [`WifiConfig`] (`WifiError::Timeout`); obtaining the address is bounded
//!   separately by `dhcp_timeout` of the [`NetworkConfig`]: a missing DHCP
//!   lease is `WifiError::DhcpFailed`, a static address that does not see the
//!   link in time is `WifiError::Timeout`. A scan has no timeout of its own;
//!   the `wifi` service bounds it with `scan_timeout`.
//! * **Scans.** At most 48 raw access points are converted, hidden networks
//!   and duplicate SSIDs are removed and the strongest
//!   [`MAX_WIFI_SCAN_RESULTS`](rohi_net::MAX_WIFI_SCAN_RESULTS) are kept
//!   ([`ScanCollector`]).
//! * **Error mapping.** No access point found maps to
//!   `NetworkNotFound`; failed authentication or a 4-way handshake timeout to
//!   `AuthenticationFailed`; a beacon or association timeout to `Timeout`;
//!   invalid SSID or password to `InvalidConfig`; any other vendor failure to
//!   `AssociationFailed` (while connecting) or `DriverError`.
//! * **Stale leases.** Before a DHCP connect the backend waits (at most 2 s)
//!   until the IP configuration of the previous connection is gone, so a
//!   stale lease is never taken for the new one.

use core::net::Ipv4Addr;

use embassy_net::{
    Config as IpConfig, ConfigV4, DhcpConfig, HardwareAddress, Ipv4Address, Ipv4Cidr, Runner,
    Stack, StackResources, StaticConfigV4,
};
use embassy_time::{Duration, Timer, with_timeout};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig,
    DisconnectReason, Interface, Password, Ssid, WifiController, WifiError as VendorError,
};
use log::{debug, info, trace, warn};

use rohi_net::{
    LinkConfig, NetworkConfig, NetworkInfo, ScanCollector, WifiBackend, WifiConfig, WifiError,
    WifiNetwork, WifiScanResults, WifiSecurity, WifiSsid,
};

/// Upper bound of raw scan entries converted per scan (bounds the vendor `Vec`).
const MAX_RAW_SCAN_RESULTS: usize = 48;

const STALE_CONFIG_POLL: Duration = Duration::from_millis(50);
const STALE_CONFIG_LIMIT: u32 = 40;

/// Background driver of the IP stack. Run it on its own task:
/// `runner.run().await`.
pub struct NetRunner(Runner<'static, Interface>);

impl NetRunner {
    /// Runs the IP stack forever. Spawn it on its own task; the IP data plane
    /// does not make progress without it.
    pub async fn run(mut self) -> ! {
        info!(target: "wifi", "IP stack runner started");
        self.0.run().await
    }
}

/// ESP32 implementation of [`WifiBackend`].
///
/// Created by [`EspWireless::wifi`](crate::EspWireless::wifi) and owned by the
/// single [`WifiManager`](rohi_net::WifiManager). When the manager drops a
/// pending `connect` because a command or a new configuration arrived, it
/// calls `disconnect` afterwards; a failed `connect` disconnects by itself.
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
    pub(crate) fn new<const SOCKETS: usize>(
        wifi: WIFI<'static>,
        resources: &'static mut StackResources<SOCKETS>,
    ) -> Result<(Self, NetRunner, Stack<'static>), WifiError> {
        debug!(target: "wifi", "Starting the driver in station mode");
        let controller = WifiController::new(wifi, ControllerConfig::default())
            .map_err(|error| map_driver_error(&error))?;
        info!(target: "wifi", "Driver started");
        let interface = Interface::station();

        let seed = (u64::from(Rng::new().random()) << 32) | u64::from(Rng::new().random());
        let (stack, runner) = embassy_net::new(
            interface,
            IpConfig::dhcpv4(Default::default()),
            resources,
            seed,
        );

        Ok((
            Self {
                controller,
                stack,
                ssid: None,
            },
            NetRunner(runner),
            stack,
        ))
    }

    fn current_info(&mut self) -> Option<NetworkInfo> {
        let ssid = self.ssid.clone()?;
        let config = self.stack.config_v4()?;
        let rssi_dbm = self
            .controller
            .rssi()
            .ok()
            .and_then(|rssi| i8::try_from(rssi).ok())
            .unwrap_or(i8::MIN);
        let channel = self
            .controller
            .ap_info()
            .ok()
            .map(|access_point| access_point.channel);
        let HardwareAddress::Ethernet(mac) = self.stack.hardware_address();
        Some(NetworkInfo {
            ssid,
            rssi_dbm,
            channel,
            mac: Some(mac.0),
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
        for waited in 0..STALE_CONFIG_LIMIT {
            if !self.stack.is_config_up() {
                if waited > 0 {
                    debug!(
                        target: "wifi",
                        "Stale IP configuration cleared (polls={})",
                        waited
                    );
                }
                return;
            }
            Timer::after(STALE_CONFIG_POLL).await;
        }
        warn!(
            target: "wifi",
            "Stale IP configuration still up (timeout_ms={})",
            STALE_CONFIG_POLL.as_millis() * u64::from(STALE_CONFIG_LIMIT)
        );
    }

    /// Hands the address settings of `config` to the IP stack: the static
    /// address, or a DHCP client that announces the host name.
    fn apply_address(&self, config: &NetworkConfig) -> Result<(), WifiError> {
        let ipv4 = match config.static_settings() {
            Some(settings) => {
                debug!(
                    target: "wifi",
                    "Using static address (ip={} prefix_len={} gateway={})",
                    settings.address, settings.prefix_len, settings.gateway
                );
                let address = Ipv4Cidr::new(
                    Ipv4Address::from(settings.address.octets()),
                    settings.prefix_len,
                );
                let mut dns_servers = heapless::Vec::new();
                if let Some(dns) = settings.dns {
                    let _ = dns_servers.push(Ipv4Address::from(dns.octets()));
                }
                ConfigV4::Static(StaticConfigV4 {
                    address,
                    gateway: Some(Ipv4Address::from(settings.gateway.octets())),
                    dns_servers,
                })
            }
            None => {
                debug!(target: "wifi", "Using DHCP address (hostname={:?})", config.hostname);
                let mut dhcp = DhcpConfig::default();
                if let Some(hostname) = &config.hostname {
                    dhcp.hostname =
                        Some(heapless::String::try_from(hostname.as_str()).map_err(|_| {
                            warn!(target: "wifi", "Host name does not fit the DHCP client");
                            WifiError::InvalidConfig
                        })?);
                }
                ConfigV4::Dhcp(dhcp)
            }
        };
        self.stack.set_config_v4(ipv4);
        Ok(())
    }

    async fn connect_inner(&mut self, config: &LinkConfig) -> Result<NetworkInfo, WifiError> {
        self.apply_address(&config.network)?;
        // A static address is up at once; only a DHCP lease can be stale.
        if config.network.static_settings().is_none() {
            self.wait_stale_config_cleared().await;
        }
        let station = station_config(&config.wifi)?;
        debug!(target: "wifi", "Associating with access point (ssid={:?})", config.wifi.ssid);
        self.controller
            .set_config(&Config::Station(station))
            .map_err(|error| map_driver_error(&error))?;

        // Only association is bounded by `connect_timeout_ms`; the wait below
        // has its own `dhcp_timeout_ms`.
        let connect_timeout = Duration::from_millis(config.wifi.connect_timeout_ms.into());
        with_timeout(connect_timeout, self.controller.connect_async())
            .await
            .map_err(|_| {
                debug!(
                    target: "wifi",
                    "Association timed out (timeout_ms={})",
                    config.wifi.connect_timeout_ms
                );
                WifiError::Timeout
            })?
            .map_err(|error| match error {
                ConnectionError::Failed(info) => {
                    warn!(target: "wifi", "Association failed (reason={:?})", info.reason);
                    map_connect_reason(info.reason)
                }
                ConnectionError::WifiError(error) => map_driver_error(&error),
                _ => {
                    warn!(target: "wifi", "Association failed for an unknown reason");
                    WifiError::DriverError
                }
            })?;
        debug!(target: "wifi", "Associated, waiting for the IPv4 configuration");

        // Association alone is not network ready: wait for the DHCP lease, or
        // for the link when the address is static.
        let wait = Duration::from_millis(config.network.dhcp_timeout_ms.into());
        if config.network.static_settings().is_some() {
            with_timeout(wait, self.stack.wait_link_up())
                .await
                .map_err(|_| {
                    debug!(
                        target: "wifi",
                        "Link not up in time (timeout_ms={})",
                        config.network.dhcp_timeout_ms
                    );
                    WifiError::Timeout
                })?;
        } else {
            with_timeout(wait, self.stack.wait_config_up())
                .await
                .map_err(|_| {
                    debug!(
                        target: "wifi",
                        "No DHCP lease in time (timeout_ms={})",
                        config.network.dhcp_timeout_ms
                    );
                    WifiError::DhcpFailed
                })?;
        }

        self.ssid = config.wifi.ssid.clone();
        self.current_info().ok_or_else(|| {
            warn!(target: "wifi", "IPv4 configuration disappeared right after it was obtained");
            WifiError::DhcpFailed
        })
    }
}

impl WifiBackend for EspWifi {
    async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
        let scan = ScanConfig::default().with_max(MAX_RAW_SCAN_RESULTS);
        // The vendor result is a heap-backed `Vec`; it never leaves this function.
        let access_points = self.controller.scan_async(&scan).await.map_err(|error| {
            warn!(target: "wifi", "Scan failed (err={:?})", error);
            WifiError::ScanFailed
        })?;

        trace!(target: "wifi", "Scan returned raw results (count={})", access_points.len());
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
        let results = collector.finish();
        debug!(
            target: "wifi",
            "Scan finished (raw_count={} kept_count={})",
            access_points.len(),
            results.len()
        );
        Ok(results)
    }

    async fn connect(&mut self, config: &LinkConfig) -> Result<NetworkInfo, WifiError> {
        match self.connect_inner(config).await {
            Ok(info) => Ok(info),
            Err(error) => {
                debug!(target: "wifi", "Connect failed, leaving the network (err={:?})", error);
                self.ssid = None;
                let _ = self.disconnect().await;
                Err(error)
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), WifiError> {
        self.ssid = None;
        match self.controller.disconnect_async().await {
            Ok(_) => {
                debug!(target: "wifi", "Disconnected");
                Ok(())
            }
            Err(VendorError::NotConnected) => {
                trace!(target: "wifi", "Disconnect while not connected");
                Ok(())
            }
            Err(error) => Err(map_driver_error(&error)),
        }
    }

    async fn wait_link_lost(&mut self) -> WifiError {
        match self.controller.wait_for_disconnect_async().await {
            Ok(info) => {
                debug!(target: "wifi", "Link lost (reason={:?})", info.reason);
                map_link_lost_reason(info.reason)
            }
            // Not connected (any more): the link is already lost.
            Err(error) => {
                debug!(
                    target: "wifi",
                    "Wait for disconnect failed, link is down (err={:?})",
                    error
                );
                WifiError::Disconnected
            }
        }
    }

    fn network_info(&mut self) -> Option<NetworkInfo> {
        self.current_info()
    }
}

fn station_config(config: &WifiConfig) -> Result<StationConfig, WifiError> {
    let ssid = config.ssid.as_ref().ok_or_else(|| {
        warn!(target: "wifi", "No SSID to connect to");
        WifiError::InvalidConfig
    })?;
    let ssid = Ssid::try_from(ssid.as_bytes()).map_err(|_| {
        warn!(target: "wifi", "Driver rejected the SSID");
        WifiError::InvalidConfig
    })?;
    let authentication = match &config.password {
        None => AuthenticationMethodConfig::Open,
        Some(password) => AuthenticationMethodConfig::WpaWpa2Personal(
            Password::try_from(password.expose_secret()).map_err(|_| {
                warn!(target: "wifi", "Driver rejected the password");
                WifiError::InvalidConfig
            })?,
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
    warn!(target: "wifi", "Driver error (err={:?})", error);
    match error {
        VendorError::InvalidSsid | VendorError::InvalidPassword | VendorError::InvalidArguments => {
            WifiError::InvalidConfig
        }
        _ => WifiError::DriverError,
    }
}
