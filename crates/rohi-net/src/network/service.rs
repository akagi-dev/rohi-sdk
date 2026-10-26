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

//! The `network` service: the link, the address, the radio heap and the
//! socket usage of the network.

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::{String, Vec};
use rohi_service::{
    ActionDescriptor, ActionError, ActionOutput, FieldDescriptor, Issue, IssueSeverity, MAX_ISSUES,
    ManagedService, ServiceHealth, ServiceState, ServiceSummary, StatusWriter, Value, ValueKind,
    WriteError,
};

use super::config::{AddressMode, DEFAULT_SCAN_TIMEOUT_MS};
use super::managed::ADDRESS_MODES;
use crate::{IpUsage, WifiError, WifiHandle, WifiSecurity, WifiState};

/// Usage of the memory the radio drivers allocate from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    /// Total size in bytes.
    pub size: usize,
    /// Free bytes right now.
    pub free: usize,
    /// Lowest number of free bytes seen since boot, if the platform tracks it.
    pub min_free: Option<usize>,
}

/// What the [`NetworkService`] reads from the platform, besides the Wi-Fi
/// manager. A value the platform cannot tell is `None` and not reported.
pub trait NetworkProbe {
    /// The radio heap.
    fn heap(&self) -> Option<HeapStats>;

    /// Socket slots of the IP stack, `None` while there is no IP stack.
    fn sockets(&self) -> Option<IpUsage>;

    /// The monotonic clock, in the time base of
    /// [`ManagerConfig::now_ms`](crate::ManagerConfig::now_ms).
    fn now_ms(&self) -> Option<u64>;
}

/// A platform without a heap, IP stack or clock to report.
impl NetworkProbe for () {
    fn heap(&self) -> Option<HeapStats> {
        None
    }

    fn sockets(&self) -> Option<IpUsage> {
        None
    }

    fn now_ms(&self) -> Option<u64> {
        None
    }
}

/// Field ids of the `network` service status. Only ever added; 1 to 5 are the
/// fields of the former `wifi` service.
pub mod status_field {
    /// SSID of the connected network.
    pub const SSID: u32 = 1;
    /// Signal strength of the connection.
    pub const RSSI: u32 = 2;
    /// IPv4 address.
    pub const IP: u32 = 3;
    /// IPv4 gateway.
    pub const GATEWAY: u32 = 4;
    /// IPv4 DNS server.
    pub const DNS: u32 = 5;
    /// Channel of the access point.
    pub const CHANNEL: u32 = 6;
    /// Hardware address of the station.
    pub const MAC: u32 = 7;
    /// Connection attempts after a failure, a lost link or a `reconnect`.
    pub const RECONNECTS: u32 = 8;
    /// Time since the link came up.
    pub const LINK_UPTIME: u32 = 9;
    /// How the address is obtained, `dhcp` or `static`.
    pub const ADDRESS_MODE: u32 = 10;
    /// Size of the radio heap.
    pub const HEAP_SIZE: u32 = 11;
    /// Free radio heap.
    pub const HEAP_FREE: u32 = 12;
    /// Lowest free radio heap since boot.
    pub const HEAP_MIN_FREE: u32 = 13;
    /// TCP connections in use.
    pub const TCP_IN_USE: u32 = 14;
    /// TCP connections available in total.
    pub const TCP_CAPACITY: u32 = 15;
    /// UDP sockets in use.
    pub const UDP_IN_USE: u32 = 16;
    /// UDP sockets available in total.
    pub const UDP_CAPACITY: u32 = 17;
}

fn issue(error: WifiError) -> Issue {
    let (severity, code, message) = match error {
        WifiError::NotConfigured => (
            IssueSeverity::Info,
            "not_configured",
            "Wi-Fi is not configured",
        ),
        WifiError::Busy => (IssueSeverity::Warning, "busy", "Wi-Fi manager is busy"),
        WifiError::Timeout => (
            IssueSeverity::Warning,
            "timeout",
            "Wi-Fi operation timed out",
        ),
        WifiError::ScanFailed => (IssueSeverity::Warning, "scan_failed", "Wi-Fi scan failed"),
        WifiError::NetworkNotFound => (
            IssueSeverity::Warning,
            "network_not_found",
            "The configured network was not found",
        ),
        WifiError::AuthenticationFailed => (
            IssueSeverity::Error,
            "authentication_failed",
            "The access point rejected the credentials",
        ),
        WifiError::AssociationFailed => (
            IssueSeverity::Warning,
            "association_failed",
            "Association with the access point failed",
        ),
        WifiError::DhcpFailed => (
            IssueSeverity::Warning,
            "dhcp_failed",
            "No IPv4 configuration was obtained",
        ),
        WifiError::Disconnected => (IssueSeverity::Warning, "disconnected", "The link was lost"),
        WifiError::InvalidConfig => (
            IssueSeverity::Error,
            "invalid_config",
            "The Wi-Fi configuration is invalid",
        ),
        WifiError::DriverError => (
            IssueSeverity::Critical,
            "driver_error",
            "The Wi-Fi driver failed",
        ),
    };
    Issue {
        severity,
        code,
        message,
        age_ms: None,
    }
}

/// Field ids of the results of the `scan` action.
pub mod scan_field {
    /// Network name; networks whose name is not UTF-8 are not listed.
    pub const SSID: u32 = 1;
    /// Signal strength.
    pub const RSSI: u32 = 2;
    /// Channel number.
    pub const CHANNEL: u32 = 3;
    /// Security of the network.
    pub const SECURITY: u32 = 4;
}

const SECURITY_VARIANTS: &[&str] = &[
    "open",
    "wep",
    "wpa",
    "wpa2",
    "wpa_wpa2",
    "wpa3",
    "wpa2_wpa3",
    "enterprise",
    "unknown",
];

fn security_name(security: WifiSecurity) -> &'static str {
    match security {
        WifiSecurity::Open => "open",
        WifiSecurity::Wep => "wep",
        WifiSecurity::Wpa => "wpa",
        WifiSecurity::Wpa2 => "wpa2",
        WifiSecurity::WpaWpa2 => "wpa_wpa2",
        WifiSecurity::Wpa3 => "wpa3",
        WifiSecurity::Wpa2Wpa3 => "wpa2_wpa3",
        WifiSecurity::Enterprise => "enterprise",
        WifiSecurity::Unknown => "unknown",
    }
}

fn action_error(error: WifiError) -> ActionError {
    match error {
        WifiError::NotConfigured => ActionError::Rejected("not_configured"),
        WifiError::Busy => ActionError::Busy,
        WifiError::Timeout | WifiError::ScanFailed => ActionError::Unavailable,
        WifiError::InvalidConfig => ActionError::InvalidArgument,
        _ => ActionError::Internal,
    }
}

/// The network as a `network` service: link, address, heap and socket
/// metrics, plus the `scan`, `reconnect` and `disconnect` actions.
///
/// A scan is bounded by the `scan_timeout` of the configuration, measured
/// with `delay`.
pub struct NetworkService<'a, M: RawMutex, D, P> {
    handle: WifiHandle<'a, M>,
    delay: D,
    probe: P,
}

impl<'a, M: RawMutex, D: DelayNs + Clone, P: NetworkProbe> NetworkService<'a, M, D, P> {
    /// The service of the manager behind `handle`, with the platform
    /// readings of `probe`.
    pub const fn new(handle: WifiHandle<'a, M>, delay: D, probe: P) -> Self {
        Self {
            handle,
            delay,
            probe,
        }
    }

    fn scan_timeout_ms(&self) -> u32 {
        self.handle
            .desired_config()
            .map_or(DEFAULT_SCAN_TIMEOUT_MS, |config| config.scan_timeout_ms)
    }
}

const fn bytes(descriptor: FieldDescriptor) -> FieldDescriptor {
    descriptor.unit("B")
}

impl<M: RawMutex, D: DelayNs + Clone, P: NetworkProbe> ManagedService
    for NetworkService<'_, M, D, P>
{
    const NAME: &'static str = "network";
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::SSID, "ssid", ValueKind::String)
            .describe("Connected network"),
        FieldDescriptor::new(status_field::RSSI, "rssi", ValueKind::I64)
            .describe("Signal strength")
            .unit("dBm"),
        FieldDescriptor::new(status_field::IP, "ip", ValueKind::String).describe("IPv4 address"),
        FieldDescriptor::new(status_field::GATEWAY, "gateway", ValueKind::String)
            .describe("IPv4 gateway"),
        FieldDescriptor::new(status_field::DNS, "dns", ValueKind::String)
            .describe("IPv4 DNS server"),
        FieldDescriptor::new(status_field::CHANNEL, "channel", ValueKind::U64)
            .describe("Channel of the access point"),
        FieldDescriptor::new(status_field::MAC, "mac", ValueKind::String)
            .describe("Hardware address of the station"),
        FieldDescriptor::new(status_field::RECONNECTS, "reconnects", ValueKind::U64)
            .describe("Reconnect attempts since boot"),
        FieldDescriptor::new(status_field::LINK_UPTIME, "link_uptime", ValueKind::U64)
            .describe("Time since the link came up")
            .unit("ms"),
        FieldDescriptor::new(status_field::ADDRESS_MODE, "address_mode", ValueKind::Enum)
            .describe("How the IPv4 address is obtained")
            .variants(ADDRESS_MODES),
        bytes(
            FieldDescriptor::new(status_field::HEAP_SIZE, "heap_size", ValueKind::U64)
                .describe("Size of the radio heap"),
        ),
        bytes(
            FieldDescriptor::new(status_field::HEAP_FREE, "heap_free", ValueKind::U64)
                .describe("Free radio heap"),
        ),
        bytes(
            FieldDescriptor::new(status_field::HEAP_MIN_FREE, "heap_min_free", ValueKind::U64)
                .describe("Lowest free radio heap since boot"),
        ),
        FieldDescriptor::new(status_field::TCP_IN_USE, "tcp_in_use", ValueKind::U64)
            .describe("TCP connections in use"),
        FieldDescriptor::new(status_field::TCP_CAPACITY, "tcp_capacity", ValueKind::U64)
            .describe("TCP connections available"),
        FieldDescriptor::new(status_field::UDP_IN_USE, "udp_in_use", ValueKind::U64)
            .describe("UDP sockets in use"),
        FieldDescriptor::new(status_field::UDP_CAPACITY, "udp_capacity", ValueKind::U64)
            .describe("UDP sockets available"),
    ];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new("scan")
            .describe("Scan for networks, strongest first")
            .results(&[
                FieldDescriptor::new(scan_field::SSID, "ssid", ValueKind::String),
                FieldDescriptor::new(scan_field::RSSI, "rssi", ValueKind::I64).unit("dBm"),
                FieldDescriptor::new(scan_field::CHANNEL, "channel", ValueKind::U64),
                FieldDescriptor::new(scan_field::SECURITY, "security", ValueKind::Enum)
                    .variants(SECURITY_VARIANTS),
            ]),
        ActionDescriptor::new("reconnect")
            .describe("Drop the connection and connect again")
            .privileged(),
        ActionDescriptor::new("disconnect")
            .describe("Disconnect and stay idle")
            .privileged(),
    ];

    async fn summary(&self) -> ServiceSummary {
        use ServiceHealth as H;
        use ServiceState as S;
        let status = self.handle.status();
        let (state, health, ready) = match status.state {
            WifiState::NotConfigured => (S::Disabled, H::Unknown, false),
            WifiState::Idle => (S::Stopped, H::Unknown, false),
            WifiState::Scanning | WifiState::Connecting => (S::Starting, H::Unknown, false),
            WifiState::Connected => (S::Running, H::Ok, true),
            WifiState::Disconnected => (S::Running, H::Degraded, false),
            WifiState::Error => (S::Failed, H::Error, false),
        };
        ServiceSummary::new(state, health, ready)
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let status = self.handle.status();
        // A configured-but-idle or unconfigured radio is not an issue.
        if let Some(error) = status.error
            && matches!(status.state, WifiState::Error | WifiState::Disconnected)
        {
            let _ = out.push(issue(error));
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if let Some(info) = self.handle.network_info() {
            if let Some(ssid) = info.ssid.as_str() {
                out.text(status_field::SSID, ssid)?;
            }
            out.i64(status_field::RSSI, i64::from(info.rssi_dbm))?;
            let mut text = String::<17>::new();
            let _ = write!(text, "{}", info.ipv4);
            out.text(status_field::IP, &text)?;
            for (id, address) in [
                (status_field::GATEWAY, info.gateway),
                (status_field::DNS, info.dns),
            ] {
                if let Some(address) = address {
                    text.clear();
                    let _ = write!(text, "{address}");
                    out.text(id, &text)?;
                }
            }
            if let Some(channel) = info.channel {
                out.u64(status_field::CHANNEL, u64::from(channel))?;
            }
            if let Some([a, b, c, d, e, f]) = info.mac {
                text.clear();
                let _ = write!(text, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}");
                out.text(status_field::MAC, &text)?;
            }
        }

        let metrics = self.handle.metrics();
        out.u64(status_field::RECONNECTS, u64::from(metrics.reconnects))?;
        if let (Some(since), Some(now)) = (metrics.connected_at_ms, self.probe.now_ms()) {
            out.u64(status_field::LINK_UPTIME, now.saturating_sub(since))?;
        }
        if let Some(config) = self.handle.desired_config() {
            let mode = match config.address_mode {
                AddressMode::Dhcp => "dhcp",
                AddressMode::Static => "static",
            };
            out.variant(status_field::ADDRESS_MODE, mode)?;
        }

        if let Some(heap) = self.probe.heap() {
            out.u64(status_field::HEAP_SIZE, heap.size as u64)?;
            out.u64(status_field::HEAP_FREE, heap.free as u64)?;
            if let Some(min_free) = heap.min_free {
                out.u64(status_field::HEAP_MIN_FREE, min_free as u64)?;
            }
        }
        if let Some(sockets) = self.probe.sockets() {
            out.u64(status_field::TCP_IN_USE, sockets.tcp_in_use as u64)?;
            out.u64(status_field::TCP_CAPACITY, sockets.tcp_capacity as u64)?;
            out.u64(status_field::UDP_IN_USE, sockets.udp_in_use as u64)?;
            out.u64(status_field::UDP_CAPACITY, sockets.udp_capacity as u64)?;
        }
        Ok(())
    }

    async fn invoke(
        &self,
        action: &str,
        _args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            "scan" => {
                let scan = self.handle.scan();
                let mut delay = self.delay.clone();
                let timeout = delay.delay_ms(self.scan_timeout_ms());
                let results = match select(scan, timeout).await {
                    Either::First(results) => results.map_err(action_error)?,
                    Either::Second(()) => return Err(ActionError::Unavailable),
                };
                for network in results.iter() {
                    let Some(ssid) = network.ssid.as_str() else {
                        continue;
                    };
                    if out.next_record().is_err() {
                        break;
                    }
                    let written = out
                        .text(scan_field::SSID, ssid)
                        .and_then(|()| out.i64(scan_field::RSSI, i64::from(network.rssi_dbm)))
                        .and_then(|()| out.u64(scan_field::CHANNEL, u64::from(network.channel)))
                        .and_then(|()| {
                            out.variant(scan_field::SECURITY, security_name(network.security))
                        });
                    if written.is_err() {
                        return Err(ActionError::Internal);
                    }
                }
                Ok(())
            }
            "reconnect" => self.handle.reconnect().map_err(action_error),
            "disconnect" => self.handle.disconnect().map_err(action_error),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{ActionOutput, MAX_FIELDS};

    use super::*;
    use crate::{NetworkConfig, NetworkInfo, WifiShared, WifiSsid};

    #[derive(Clone)]
    struct NoDelay;

    impl DelayNs for NoDelay {
        async fn delay_ns(&mut self, _: u32) {
            core::future::pending::<()>().await;
        }
    }

    struct Probe {
        heap: Option<HeapStats>,
        sockets: Option<IpUsage>,
        now: Option<u64>,
    }

    impl NetworkProbe for Probe {
        fn heap(&self) -> Option<HeapStats> {
            self.heap
        }

        fn sockets(&self) -> Option<IpUsage> {
            self.sockets
        }

        fn now_ms(&self) -> Option<u64> {
            self.now
        }
    }

    const FULL: Probe = Probe {
        heap: Some(HeapStats {
            size: 131_072,
            free: 40_000,
            min_free: Some(30_000),
        }),
        sockets: Some(IpUsage {
            tcp_in_use: 1,
            tcp_capacity: 2,
            udp_in_use: 0,
            udp_capacity: 1,
        }),
        now: Some(100_000),
    };

    #[derive(Default)]
    struct Collect(std::vec::Vec<(u32, Value)>);

    impl StatusWriter for Collect {
        fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
            self.0.push((id, value));
            Ok(())
        }
    }

    impl ActionOutput for Collect {
        fn next_record(&mut self) -> Result<(), WriteError> {
            Ok(())
        }
    }

    impl Collect {
        fn get(&self, id: u32) -> Option<&Value> {
            self.0
                .iter()
                .find(|(field, _)| *field == id)
                .map(|(_, v)| v)
        }
    }

    type Service<'a, P> = NetworkService<'a, NoopRawMutex, NoDelay, P>;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    fn config() -> NetworkConfig {
        NetworkConfig::new(WifiSsid::from_bytes(b"office").unwrap(), None)
    }

    fn info() -> NetworkInfo {
        NetworkInfo {
            ssid: WifiSsid::from_bytes(b"office").unwrap(),
            rssi_dbm: -61,
            channel: Some(11),
            mac: Some([0x02, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]),
            ipv4: core::net::Ipv4Addr::new(192, 168, 1, 41),
            gateway: Some(core::net::Ipv4Addr::new(192, 168, 1, 1)),
            dns: None,
        }
    }

    #[test]
    fn the_schema_is_well_formed() {
        let schema = <Service<'static, ()> as ManagedService>::STATUS_SCHEMA;
        assert!(schema.len() <= MAX_FIELDS);
        let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        for field in schema {
            assert!(field.name.len() <= 24, "{}", field.name);
            assert!(field.description.len() <= 40, "{}", field.name);
        }
        // The fields of the former `wifi` service keep their ids.
        let wifi: [(u32, &str); 5] = [
            (1, "ssid"),
            (2, "rssi"),
            (3, "ip"),
            (4, "gateway"),
            (5, "dns"),
        ];
        for (id, name) in wifi {
            assert!(schema.iter().any(|f| f.id == id && f.name == name));
        }
    }

    #[test]
    fn actions_are_declared_with_their_privileges() {
        let actions = <Service<'static, ()> as ManagedService>::ACTIONS;
        let privileged: std::vec::Vec<_> = actions
            .iter()
            .map(|action| (action.name, action.privileged))
            .collect();
        assert_eq!(
            privileged,
            [("scan", false), ("reconnect", true), ("disconnect", true)]
        );
        assert_eq!(actions[0].results.len(), 4);
        assert!(actions[0].results[3].variants.contains(&"wpa2"));
    }

    #[test]
    fn reconnect_is_queued_and_unknown_actions_are_refused() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = NetworkService::new(shared.handle(), NoDelay, ());
        let mut out = Collect::default();
        assert_eq!(block_on(service.invoke("reconnect", &[], &mut out)), Ok(()));
        assert_eq!(
            block_on(service.invoke("explode", &[], &mut out)),
            Err(ActionError::UnknownAction)
        );
    }

    #[test]
    fn a_new_manager_is_a_disabled_service_that_still_reports_its_resources() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = NetworkService::new(shared.handle(), NoDelay, FULL);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Disabled);
        assert!(!summary.ready);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert!(issues.is_empty());

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        // No link details and no address mode without a configuration.
        assert!(out.get(status_field::SSID).is_none());
        assert!(out.get(status_field::IP).is_none());
        assert!(out.get(status_field::ADDRESS_MODE).is_none());
        assert!(out.get(status_field::LINK_UPTIME).is_none());
        assert_eq!(out.get(status_field::RECONNECTS), Some(&Value::U64(0)));
        assert_eq!(out.get(status_field::HEAP_SIZE), Some(&Value::U64(131_072)));
        assert_eq!(out.get(status_field::HEAP_FREE), Some(&Value::U64(40_000)));
        assert_eq!(
            out.get(status_field::HEAP_MIN_FREE),
            Some(&Value::U64(30_000))
        );
        assert_eq!(out.get(status_field::TCP_IN_USE), Some(&Value::U64(1)));
        assert_eq!(out.get(status_field::TCP_CAPACITY), Some(&Value::U64(2)));
        assert_eq!(out.get(status_field::UDP_IN_USE), Some(&Value::U64(0)));
        assert_eq!(out.get(status_field::UDP_CAPACITY), Some(&Value::U64(1)));
    }

    #[test]
    fn what_the_platform_cannot_tell_is_not_reported() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = NetworkService::new(shared.handle(), NoDelay, ());
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0, [(status_field::RECONNECTS, Value::U64(0))]);

        let probe = Probe {
            heap: Some(HeapStats {
                size: 10,
                free: 4,
                min_free: None,
            }),
            sockets: None,
            now: None,
        };
        let service = NetworkService::new(shared.handle(), NoDelay, probe);
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(status_field::HEAP_FREE).is_some());
        assert!(out.get(status_field::HEAP_MIN_FREE).is_none());
        assert!(out.get(status_field::TCP_IN_USE).is_none());
    }

    #[test]
    fn a_configured_network_reports_its_address_mode() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let handle = shared.handle();
        let service = NetworkService::new(handle, NoDelay, ());
        handle.set_desired_config(Some(config()));
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(status_field::ADDRESS_MODE),
            Value::variant("dhcp").as_ref()
        );

        let mut fixed = config();
        fixed.address_mode = crate::AddressMode::Static;
        handle.set_desired_config(Some(fixed));
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(status_field::ADDRESS_MODE),
            Value::variant("static").as_ref()
        );
    }

    #[test]
    fn a_connected_link_reports_details_and_uptime() {
        use crate::{ManagerConfig, WifiBackend, WifiManager};
        use crate::{WifiError, WifiScanResults};

        struct Radio;
        impl WifiBackend for Radio {
            async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
                Err(WifiError::ScanFailed)
            }
            async fn connect(&mut self, _: &NetworkConfig) -> Result<NetworkInfo, WifiError> {
                Ok(info())
            }
            async fn disconnect(&mut self) -> Result<(), WifiError> {
                Ok(())
            }
            async fn wait_link_lost(&mut self) -> WifiError {
                core::future::pending().await
            }
            fn network_info(&mut self) -> Option<NetworkInfo> {
                Some(info())
            }
        }

        let shared = WifiShared::<NoopRawMutex>::new();
        let handle = shared.handle();
        let mut manager = WifiManager::new(
            Radio,
            NoDelay,
            &shared,
            ManagerConfig::DEFAULT.with_clock(|| 40_000),
        );
        handle.set_desired_config(Some(config()));
        block_on(manager.step());
        block_on(manager.step());

        let service = NetworkService::new(handle, NoDelay, FULL);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Running);
        assert!(summary.ready);

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.get(status_field::SSID), Some(&text("office")));
        assert_eq!(out.get(status_field::RSSI), Some(&Value::I64(-61)));
        assert_eq!(out.get(status_field::IP), Some(&text("192.168.1.41")));
        assert_eq!(out.get(status_field::GATEWAY), Some(&text("192.168.1.1")));
        assert!(out.get(status_field::DNS).is_none());
        assert_eq!(out.get(status_field::CHANNEL), Some(&Value::U64(11)));
        assert_eq!(out.get(status_field::MAC), Some(&text("02:1a:2b:3c:4d:5e")));
        assert_eq!(
            out.get(status_field::LINK_UPTIME),
            Some(&Value::U64(60_000))
        );
    }
}
