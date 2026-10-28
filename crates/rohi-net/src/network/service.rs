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
//! The `network` service: the address and the socket usage of the IP layer.

use core::fmt::Write as _;

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::{String, Vec};
use rohi_service::{
    FIRST_SERVICE_ISSUE, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES,
    ManagedService, ServiceSummary, StatusWriter, ValueKind, WriteError,
};

use crate::wifi::service_summary;
use crate::{IpUsage, WifiError, WifiHandle, WifiState};

/// What the [`NetworkService`] reads from the platform, besides the Wi-Fi
/// manager. A value the platform cannot tell is `None` and not reported.
pub trait NetworkProbe {
    /// Socket slots of the IP stack, `None` while there is no IP stack.
    fn sockets(&self) -> Option<IpUsage>;
}

/// A platform without an IP stack to report.
impl NetworkProbe for () {
    fn sockets(&self) -> Option<IpUsage> {
        None
    }
}

/// Id of the `network` service.
pub const SERVICE_ID: u32 = 2;

/// Issue codes of the `network` service.
pub mod issue {
    /// No IPv4 configuration was obtained.
    pub const DHCP_FAILED: u32 = super::FIRST_SERVICE_ISSUE;
}

/// Field ids of the `network` service status. Only ever added.
pub mod status_field {
    /// IPv4 address.
    pub const IP: u32 = 1;
    /// IPv4 gateway.
    pub const GATEWAY: u32 = 2;
    /// IPv4 DNS server.
    pub const DNS: u32 = 3;
    /// TCP connections in use.
    pub const TCP_IN_USE: u32 = 5;
    /// TCP connections available in total.
    pub const TCP_CAPACITY: u32 = 6;
    /// UDP sockets in use.
    pub const UDP_IN_USE: u32 = 7;
    /// UDP sockets available in total.
    pub const UDP_CAPACITY: u32 = 8;
}

/// The IP layer of the Wi-Fi link as a `network` service: address and
/// socket usage. It has no actions; the link is operated through `wifi`.
pub struct NetworkService<'a, M: RawMutex, P> {
    handle: WifiHandle<'a, M>,
    probe: P,
}

impl<'a, M: RawMutex, P: NetworkProbe> NetworkService<'a, M, P> {
    /// The service of the manager behind `handle`, with the platform
    /// readings of `probe`.
    pub const fn new(handle: WifiHandle<'a, M>, probe: P) -> Self {
        Self { handle, probe }
    }
}

impl<M: RawMutex, P: NetworkProbe> ManagedService for NetworkService<'_, M, P> {
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "network";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::IP, "ip", ValueKind::String),
        FieldDescriptor::new(status_field::GATEWAY, "gateway", ValueKind::String),
        FieldDescriptor::new(status_field::DNS, "dns", ValueKind::String),
        FieldDescriptor::new(status_field::TCP_IN_USE, "tcp_in_use", ValueKind::U64),
        FieldDescriptor::new(status_field::TCP_CAPACITY, "tcp_capacity", ValueKind::U64),
        FieldDescriptor::new(status_field::UDP_IN_USE, "udp_in_use", ValueKind::U64),
        FieldDescriptor::new(status_field::UDP_CAPACITY, "udp_capacity", ValueKind::U64),
    ];
    const ISSUES: &'static [IssueDescriptor] =
        &[IssueDescriptor::new(issue::DHCP_FAILED, "dhcp_failed")];

    async fn summary(&self) -> ServiceSummary {
        service_summary(self.handle.status().state)
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let status = self.handle.status();
        if status.error == Some(WifiError::DhcpFailed)
            && matches!(status.state, WifiState::Error | WifiState::Disconnected)
        {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code: issue::DHCP_FAILED,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if let Some(info) = self.handle.network_info() {
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
        }

        if let Some(sockets) = self.probe.sockets() {
            out.u64(status_field::TCP_IN_USE, sockets.tcp_in_use as u64)?;
            out.u64(status_field::TCP_CAPACITY, sockets.tcp_capacity as u64)?;
            out.u64(status_field::UDP_IN_USE, sockets.udp_in_use as u64)?;
            out.u64(status_field::UDP_CAPACITY, sockets.udp_capacity as u64)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{MAX_FIELDS, ServiceState, Value};

    use super::*;
    use crate::{
        ManagerConfig, NetworkInfo, WifiBackend, WifiConfig, WifiManager, WifiScanResults,
        WifiShared, WifiSsid,
    };

    #[derive(Clone)]
    struct NoDelay;

    impl embedded_hal_async::delay::DelayNs for NoDelay {
        async fn delay_ns(&mut self, _: u32) {
            core::future::pending::<()>().await;
        }
    }

    struct Sockets(Option<IpUsage>);

    impl NetworkProbe for Sockets {
        fn sockets(&self) -> Option<IpUsage> {
            self.0
        }
    }

    const USAGE: IpUsage = IpUsage {
        tcp_in_use: 1,
        tcp_capacity: 2,
        udp_in_use: 0,
        udp_capacity: 1,
    };

    #[derive(Default)]
    struct Collect(std::vec::Vec<(u32, Value)>);

    impl StatusWriter for Collect {
        fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
            self.0.push((id, value));
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

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    fn wifi() -> WifiConfig {
        WifiConfig::new(WifiSsid::from_bytes(b"office").unwrap(), None)
    }

    fn info() -> NetworkInfo {
        NetworkInfo {
            ssid: WifiSsid::from_bytes(b"office").unwrap(),
            rssi_dbm: -61,
            channel: Some(11),
            mac: None,
            ipv4: core::net::Ipv4Addr::new(192, 168, 1, 41),
            gateway: Some(core::net::Ipv4Addr::new(192, 168, 1, 1)),
            dns: None,
        }
    }

    #[test]
    fn the_schema_is_well_formed() {
        let schema = <NetworkService<'static, NoopRawMutex, ()> as ManagedService>::STATUS_SCHEMA;
        assert!(schema.len() <= MAX_FIELDS);
        let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        for field in schema {
            assert!(field.name.len() <= 24, "{}", field.name);
        }
        assert!(<NetworkService<'static, NoopRawMutex, ()> as ManagedService>::ACTIONS.is_empty());
    }

    #[test]
    fn without_a_link_only_the_sockets_are_reported() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = NetworkService::new(shared.handle(), Sockets(Some(USAGE)));
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Disabled);
        assert!(!summary.ready);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert!(issues.is_empty());

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(status_field::IP).is_none());
        assert_eq!(out.get(status_field::TCP_IN_USE), Some(&Value::U64(1)));
        assert_eq!(out.get(status_field::TCP_CAPACITY), Some(&Value::U64(2)));
        assert_eq!(out.get(status_field::UDP_IN_USE), Some(&Value::U64(0)));
        assert_eq!(out.get(status_field::UDP_CAPACITY), Some(&Value::U64(1)));
    }

    #[test]
    fn what_the_platform_cannot_tell_is_not_reported() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = NetworkService::new(shared.handle(), ());
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.0.is_empty());
        assert!(out.get(status_field::TCP_IN_USE).is_none());
    }

    #[test]
    fn a_connected_link_reports_the_address() {
        struct Radio;
        impl WifiBackend for Radio {
            async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
                Err(WifiError::ScanFailed)
            }
            async fn connect(&mut self, _: &crate::LinkConfig) -> Result<NetworkInfo, WifiError> {
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
        let mut manager = WifiManager::new(Radio, NoDelay, &shared, ManagerConfig::DEFAULT);
        handle.set_wifi_config(wifi());
        block_on(manager.step());
        block_on(manager.step());

        let service = NetworkService::new(handle, ());
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Running);
        assert!(summary.ready);

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.get(status_field::IP), Some(&text("192.168.1.41")));
        assert_eq!(out.get(status_field::GATEWAY), Some(&text("192.168.1.1")));
        assert!(out.get(status_field::DNS).is_none());
    }

    #[test]
    fn only_a_dhcp_failure_is_an_issue_of_the_network() {
        struct Failing(WifiError);
        impl WifiBackend for Failing {
            async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
                Err(WifiError::ScanFailed)
            }
            async fn connect(&mut self, _: &crate::LinkConfig) -> Result<NetworkInfo, WifiError> {
                Err(self.0)
            }
            async fn disconnect(&mut self) -> Result<(), WifiError> {
                Ok(())
            }
            async fn wait_link_lost(&mut self) -> WifiError {
                core::future::pending().await
            }
            fn network_info(&mut self) -> Option<NetworkInfo> {
                None
            }
        }

        for (error, expected) in [
            (WifiError::DhcpFailed, 1),
            (WifiError::AuthenticationFailed, 0),
        ] {
            let shared = WifiShared::<NoopRawMutex>::new();
            let handle = shared.handle();
            let mut manager =
                WifiManager::new(Failing(error), NoDelay, &shared, ManagerConfig::DEFAULT);
            handle.set_wifi_config(wifi());
            block_on(manager.step());
            block_on(manager.step());

            let service = NetworkService::new(handle, ());
            let mut issues = Vec::new();
            block_on(service.issues(&mut issues));
            assert_eq!(issues.len(), expected, "{error:?} {:?}", handle.status());
        }
    }
}
