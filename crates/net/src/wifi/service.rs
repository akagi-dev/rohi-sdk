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
//! The `wifi` service: the link, the radio heap and the actions `scan`,
//! `reconnect` and `disconnect`.

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::{String, Vec};
use rohi_service::{
    Action, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_FIELD_VALUE_LEN, MAX_ISSUES,
    Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter, Unit, Value,
    ValueKind, WriteError,
};

use crate::{WifiError, WifiHandle, WifiSecurity, WifiState};

// SSIDs and MAC addresses are status values and scan results, which are
// never cut.
const _: () = assert!(crate::MAX_SSID_LEN <= MAX_FIELD_VALUE_LEN);
const _: () = assert!("00:00:00:00:00:00".len() <= MAX_FIELD_VALUE_LEN);

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

/// What the [`WifiService`] reads from the platform, besides the Wi-Fi
/// manager. A value the platform cannot tell is `None` and not reported.
pub trait WifiProbe {
    /// The radio heap.
    fn heap(&self) -> Option<HeapStats>;

    /// The monotonic clock, in the time base of
    /// [`ManagerConfig::now_ms`](crate::ManagerConfig::now_ms).
    fn now_ms(&self) -> Option<u64>;
}

/// A platform without a heap or clock to report.
impl WifiProbe for () {
    fn heap(&self) -> Option<HeapStats> {
        None
    }

    fn now_ms(&self) -> Option<u64> {
        None
    }
}

/// Ids of the actions of the `wifi` service.
pub mod action {
    /// Scans for networks.
    pub const SCAN: u32 = 1;
    /// Drops the connection and connects again.
    pub const RECONNECT: u32 = 2;
    /// Disconnects and stays idle.
    pub const DISCONNECT: u32 = 3;
}

/// Issue codes of the `wifi` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// Wi-Fi is not configured.
    pub const NOT_CONFIGURED: u32 = BASE;
    /// The Wi-Fi manager is busy.
    pub const BUSY: u32 = BASE + 1;
    /// A Wi-Fi operation timed out.
    pub const TIMEOUT: u32 = BASE + 2;
    /// A scan failed.
    pub const SCAN_FAILED: u32 = BASE + 3;
    /// The configured network was not found.
    pub const NETWORK_NOT_FOUND: u32 = BASE + 4;
    /// The access point rejected the credentials.
    pub const AUTHENTICATION_FAILED: u32 = BASE + 5;
    /// Association with the access point failed.
    pub const ASSOCIATION_FAILED: u32 = BASE + 6;
    /// The link was lost.
    pub const DISCONNECTED: u32 = BASE + 7;
    /// The Wi-Fi configuration is invalid.
    pub const INVALID_CONFIG: u32 = BASE + 8;
    /// The Wi-Fi driver failed.
    pub const DRIVER_ERROR: u32 = BASE + 9;
}

/// Values of the `security` result of the `scan` action.
pub mod security {
    /// No encryption.
    pub const OPEN: u32 = 0;
    /// WEP.
    pub const WEP: u32 = 1;
    /// WPA.
    pub const WPA: u32 = 2;
    /// WPA2.
    pub const WPA2: u32 = 3;
    /// WPA and WPA2.
    pub const WPA_WPA2: u32 = 4;
    /// WPA3.
    pub const WPA3: u32 = 5;
    /// WPA2 and WPA3.
    pub const WPA2_WPA3: u32 = 6;
    /// Enterprise.
    pub const ENTERPRISE: u32 = 7;
    /// Not known.
    pub const UNKNOWN: u32 = 8;
}

/// Field ids of the `wifi` service status. Only ever added.
pub mod status_field {
    /// SSID of the connected network.
    pub const SSID: u32 = 1;
    /// Signal strength of the connection.
    pub const RSSI: u32 = 2;
    /// Channel of the access point.
    pub const CHANNEL: u32 = 3;
    /// Hardware address of the station.
    pub const MAC: u32 = 4;
    /// Connection attempts after a failure, a lost link or a `reconnect`.
    pub const RECONNECTS: u32 = 5;
    /// Time since the link came up.
    pub const LINK_UPTIME: u32 = 6;
    /// Size of the radio heap.
    pub const HEAP_SIZE: u32 = 7;
    /// Free radio heap.
    pub const HEAP_FREE: u32 = 8;
    /// Lowest free radio heap since boot.
    pub const HEAP_MIN_FREE: u32 = 9;
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

const SECURITY_ENUM: u32 = 1;

const ENUMS: &[EnumDescriptor] = &[EnumDescriptor::new(
    SECURITY_ENUM,
    &[
        EnumVariant::new(security::OPEN, "open"),
        EnumVariant::new(security::WEP, "wep"),
        EnumVariant::new(security::WPA, "wpa"),
        EnumVariant::new(security::WPA2, "wpa2"),
        EnumVariant::new(security::WPA_WPA2, "wpa_wpa2"),
        EnumVariant::new(security::WPA3, "wpa3"),
        EnumVariant::new(security::WPA2_WPA3, "wpa2_wpa3"),
        EnumVariant::new(security::ENTERPRISE, "enterprise"),
        EnumVariant::new(security::UNKNOWN, "unknown"),
    ],
)];

const ISSUES: &[IssueDescriptor] = &[
    IssueDescriptor::new(issue::NOT_CONFIGURED, "not_configured"),
    IssueDescriptor::new(issue::BUSY, "busy"),
    IssueDescriptor::new(issue::TIMEOUT, "timeout"),
    IssueDescriptor::new(issue::SCAN_FAILED, "scan_failed"),
    IssueDescriptor::new(issue::NETWORK_NOT_FOUND, "network_not_found"),
    IssueDescriptor::new(issue::AUTHENTICATION_FAILED, "authentication_failed"),
    IssueDescriptor::new(issue::ASSOCIATION_FAILED, "association_failed"),
    IssueDescriptor::new(issue::DISCONNECTED, "disconnected"),
    IssueDescriptor::new(issue::INVALID_CONFIG, "invalid_config"),
    IssueDescriptor::new(issue::DRIVER_ERROR, "driver_error"),
];

fn security_value(security: WifiSecurity) -> u32 {
    match security {
        WifiSecurity::Open => security::OPEN,
        WifiSecurity::Wep => security::WEP,
        WifiSecurity::Wpa => security::WPA,
        WifiSecurity::Wpa2 => security::WPA2,
        WifiSecurity::WpaWpa2 => security::WPA_WPA2,
        WifiSecurity::Wpa3 => security::WPA3,
        WifiSecurity::Wpa2Wpa3 => security::WPA2_WPA3,
        WifiSecurity::Enterprise => security::ENTERPRISE,
        WifiSecurity::Unknown => security::UNKNOWN,
    }
}

/// The issue behind a link error, `None` for the errors of the IP layer
/// (see the `network` service).
fn issue(error: WifiError) -> Option<Issue> {
    let (severity, code) = match error {
        WifiError::NotConfigured => (IssueSeverity::Info, issue::NOT_CONFIGURED),
        WifiError::Busy => (IssueSeverity::Warning, issue::BUSY),
        WifiError::Timeout => (IssueSeverity::Warning, issue::TIMEOUT),
        WifiError::ScanFailed => (IssueSeverity::Warning, issue::SCAN_FAILED),
        WifiError::NetworkNotFound => (IssueSeverity::Warning, issue::NETWORK_NOT_FOUND),
        WifiError::AuthenticationFailed => (IssueSeverity::Error, issue::AUTHENTICATION_FAILED),
        WifiError::AssociationFailed => (IssueSeverity::Warning, issue::ASSOCIATION_FAILED),
        WifiError::DhcpFailed => return None,
        WifiError::Disconnected => (IssueSeverity::Warning, issue::DISCONNECTED),
        WifiError::InvalidConfig => (IssueSeverity::Error, issue::INVALID_CONFIG),
        WifiError::DriverError => (IssueSeverity::Critical, issue::DRIVER_ERROR),
    };
    Some(Issue {
        severity,
        code,
        age_ms: None,
    })
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

/// How a link state reads as a service state; shared with the `network`
/// service, which runs on the same link.
pub(crate) fn summary_of(state: WifiState) -> ServiceSummary {
    use ServiceHealth as H;
    use ServiceState as S;
    let (state, health, ready) = match state {
        WifiState::NotConfigured => (S::Disabled, H::Unknown, false),
        WifiState::Idle => (S::Stopped, H::Unknown, false),
        WifiState::Scanning | WifiState::Connecting => (S::Starting, H::Unknown, false),
        WifiState::Connected => (S::Running, H::Ok, true),
        WifiState::Disconnected => (S::Running, H::Degraded, false),
        WifiState::Error => (S::Failed, H::Error, false),
    };
    ServiceSummary::new(state, health, ready)
}

const fn bytes(descriptor: FieldDescriptor) -> FieldDescriptor {
    descriptor.unit(Unit::Bytes)
}

/// The Wi-Fi link as a `wifi` service: the connection, the radio heap, and
/// the `scan`, `reconnect` and `disconnect` actions.
///
/// A scan is bounded by the `scan_timeout` of the configuration, measured
/// with `delay`.
#[derive(Service)]
pub struct WifiService<'a, M: RawMutex, D, P> {
    handle: WifiHandle<'a, M>,
    delay: D,
    probe: P,
}

impl<'a, M: RawMutex, D: DelayNs + Clone, P: WifiProbe> WifiService<'a, M, D, P> {
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
        self.handle.wifi_settings().scan_timeout_ms
    }
}

impl<M: RawMutex, D: DelayNs + Clone, P: WifiProbe> Status for WifiService<'_, M, D, P> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::SSID, "ssid", ValueKind::String),
        FieldDescriptor::new(status_field::RSSI, "rssi", ValueKind::I64).unit(Unit::Dbm),
        FieldDescriptor::new(status_field::CHANNEL, "channel", ValueKind::U64),
        FieldDescriptor::new(status_field::MAC, "mac", ValueKind::String),
        FieldDescriptor::new(status_field::RECONNECTS, "reconnects", ValueKind::U64),
        FieldDescriptor::new(status_field::LINK_UPTIME, "link_uptime", ValueKind::U64)
            .unit(Unit::Ms),
        bytes(FieldDescriptor::new(
            status_field::HEAP_SIZE,
            "heap_size",
            ValueKind::U64,
        )),
        bytes(FieldDescriptor::new(
            status_field::HEAP_FREE,
            "heap_free",
            ValueKind::U64,
        )),
        bytes(FieldDescriptor::new(
            status_field::HEAP_MIN_FREE,
            "heap_min_free",
            ValueKind::U64,
        )),
    ];
    const ENUMS: &'static [EnumDescriptor] = ENUMS;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        summary_of(self.handle.status().state)
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let status = self.handle.status();
        // A configured-but-idle or unconfigured radio is not an issue.
        if let Some(error) = status.error
            && matches!(status.state, WifiState::Error | WifiState::Disconnected)
            && let Some(found) = issue(error)
        {
            let _ = out.push(found);
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if let Some(info) = self.handle.network_info() {
            if let Some(ssid) = info.ssid.as_str() {
                out.text(status_field::SSID, ssid)?;
            }
            out.i64(status_field::RSSI, i64::from(info.rssi_dbm))?;
            if let Some(channel) = info.channel {
                out.u64(status_field::CHANNEL, u64::from(channel))?;
            }
            if let Some([a, b, c, d, e, f]) = info.mac {
                let mut text = String::<17>::new();
                let _ = write!(text, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}");
                out.text(status_field::MAC, &text)?;
            }
        }

        let metrics = self.handle.metrics();
        out.u64(status_field::RECONNECTS, u64::from(metrics.reconnects))?;
        if let (Some(since), Some(now)) = (metrics.connected_at_ms, self.probe.now_ms()) {
            out.u64(status_field::LINK_UPTIME, now.saturating_sub(since))?;
        }

        if let Some(heap) = self.probe.heap() {
            out.u64(status_field::HEAP_SIZE, heap.size as u64)?;
            out.u64(status_field::HEAP_FREE, heap.free as u64)?;
            if let Some(min_free) = heap.min_free {
                out.u64(status_field::HEAP_MIN_FREE, min_free as u64)?;
            }
        }
        Ok(())
    }
}

impl<M: RawMutex, D: DelayNs + Clone, P: WifiProbe> Action for WifiService<'_, M, D, P> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::SCAN, "scan")
            .results(&[
                FieldDescriptor::new(scan_field::SSID, "ssid", ValueKind::String),
                FieldDescriptor::new(scan_field::RSSI, "rssi", ValueKind::I64).unit(Unit::Dbm),
                FieldDescriptor::new(scan_field::CHANNEL, "channel", ValueKind::U64),
                FieldDescriptor::new(scan_field::SECURITY, "security", ValueKind::Enum)
                    .with_enum(SECURITY_ENUM),
            ])
            .enums(ENUMS),
        ActionDescriptor::new(action::RECONNECT, "reconnect").privileged(),
        ActionDescriptor::new(action::DISCONNECT, "disconnect").privileged(),
    ];

    async fn invoke(
        &self,
        action: u32,
        __args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::SCAN => {
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
                            out.variant(scan_field::SECURITY, security_value(network.security))
                        });
                    if written.is_err() {
                        return Err(ActionError::Internal);
                    }
                }
                Ok(())
            }
            action::RECONNECT => self.handle.reconnect().map_err(action_error),
            action::DISCONNECT => self.handle.disconnect().map_err(action_error),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::MAX_FIELDS;

    use super::*;
    use crate::WifiShared;
    use crate::{
        LinkConfig, ManagerConfig, NetworkInfo, WifiBackend, WifiConfig, WifiManager, WifiSsid,
    };

    #[derive(Clone)]
    struct NoDelay;

    impl DelayNs for NoDelay {
        async fn delay_ns(&mut self, _: u32) {
            core::future::pending::<()>().await;
        }
    }

    struct Probe {
        heap: Option<HeapStats>,
        now: Option<u64>,
    }

    impl WifiProbe for Probe {
        fn heap(&self) -> Option<HeapStats> {
            self.heap
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

    type Shared = WifiShared<NoopRawMutex>;
    type Service<'a, P> = WifiService<'a, NoopRawMutex, NoDelay, P>;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    fn config() -> WifiConfig {
        WifiConfig::new(WifiSsid::from_bytes(b"office").unwrap(), None)
    }

    fn info() -> NetworkInfo {
        NetworkInfo {
            ssid: WifiSsid::from_bytes(b"office").unwrap(),
            rssi_dbm: -61,
            channel: Some(11),
            mac: Some([0x02, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]),
            ipv4: core::net::Ipv4Addr::new(192, 168, 1, 41),
            gateway: None,
            dns: None,
        }
    }

    struct Radio {
        connects: usize,
        disconnects: usize,
    }

    impl WifiBackend for Radio {
        async fn scan(&mut self) -> Result<crate::WifiScanResults, WifiError> {
            Err(WifiError::ScanFailed)
        }
        async fn connect(&mut self, _: &LinkConfig) -> Result<NetworkInfo, WifiError> {
            self.connects += 1;
            Ok(info())
        }
        async fn disconnect(&mut self) -> Result<(), WifiError> {
            self.disconnects += 1;
            Ok(())
        }
        async fn wait_link_lost(&mut self) -> WifiError {
            core::future::pending().await
        }
        fn network_info(&mut self) -> Option<NetworkInfo> {
            Some(info())
        }
    }

    #[test]
    fn the_schema_is_well_formed() {
        let schema = <Service<'static, ()> as Status>::STATUS_SCHEMA;
        assert!(schema.len() <= MAX_FIELDS);
        let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        for field in schema {
            assert!(field.name.len() <= 24, "{}", field.name);
        }
    }

    #[test]
    fn actions_are_declared_with_their_privileges() {
        let actions = <Service<'static, ()> as Action>::ACTIONS;
        let flags: std::vec::Vec<_> = actions
            .iter()
            .map(|action| (action.name, action.privileged, action.secret_args))
            .collect();
        assert_eq!(
            flags,
            [
                ("scan", false, false),
                ("reconnect", true, false),
                ("disconnect", true, false)
            ]
        );
        assert_eq!(actions[0].results.len(), 4);
        assert!(actions[0].enums[0].contains(security::WPA2));
        assert_eq!(actions[0].results[3].enum_id, actions[0].enums[0].id);
        for action in actions {
            assert!(action.name.len() <= 24);
            for field in action.args.iter().chain(action.results) {
                assert!(field.name.len() <= 24);
            }
        }
    }

    #[test]
    fn reconnect_is_queued_and_unknown_actions_are_refused() {
        let shared = Shared::new();
        let service = WifiService::new(shared.handle(), NoDelay, ());
        let mut out = Collect::default();
        assert_eq!(
            block_on(service.invoke(action::RECONNECT, &[], &mut out)),
            Ok(())
        );
        assert_eq!(
            block_on(service.invoke(99, &[], &mut out)),
            Err(ActionError::UnknownAction)
        );
    }

    #[test]
    fn a_new_manager_is_a_disabled_service_that_still_reports_its_resources() {
        let shared = Shared::new();
        let service = WifiService::new(shared.handle(), NoDelay, FULL);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, rohi_service::ServiceState::Disabled);
        assert!(!summary.ready);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert!(issues.is_empty());

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(status_field::SSID).is_none());
        assert!(out.get(status_field::LINK_UPTIME).is_none());
        assert_eq!(out.get(status_field::RECONNECTS), Some(&Value::U64(0)));
        assert_eq!(out.get(status_field::HEAP_SIZE), Some(&Value::U64(131_072)));
        assert_eq!(out.get(status_field::HEAP_FREE), Some(&Value::U64(40_000)));
        assert_eq!(
            out.get(status_field::HEAP_MIN_FREE),
            Some(&Value::U64(30_000))
        );
    }

    #[test]
    fn what_the_platform_cannot_tell_is_not_reported() {
        let shared = Shared::new();
        let service = WifiService::new(shared.handle(), NoDelay, ());
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0, [(status_field::RECONNECTS, Value::U64(0))]);

        let probe = Probe {
            heap: Some(HeapStats {
                size: 10,
                free: 4,
                min_free: None,
            }),
            now: None,
        };
        let service = WifiService::new(shared.handle(), NoDelay, probe);
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(status_field::HEAP_FREE).is_some());
        assert!(out.get(status_field::HEAP_MIN_FREE).is_none());
    }

    #[test]
    fn a_connected_link_reports_details_and_uptime() {
        let shared = Shared::new();
        let handle = shared.handle();
        let mut manager = WifiManager::new(
            Radio {
                connects: 0,
                disconnects: 0,
            },
            NoDelay,
            &shared,
            ManagerConfig::DEFAULT.with_clock(|| 40_000),
        );
        handle.set_wifi_config(config());
        block_on(manager.step());
        block_on(manager.step());

        let service = WifiService::new(handle, NoDelay, FULL);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, rohi_service::ServiceState::Running);
        assert!(summary.ready);

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.get(status_field::SSID), Some(&text("office")));
        assert_eq!(out.get(status_field::RSSI), Some(&Value::I64(-61)));
        assert_eq!(out.get(status_field::CHANNEL), Some(&Value::U64(11)));
        assert_eq!(out.get(status_field::MAC), Some(&text("02:1a:2b:3c:4d:5e")));
        assert_eq!(
            out.get(status_field::LINK_UPTIME),
            Some(&Value::U64(60_000))
        );
    }

    #[test]
    fn the_scan_timeout_applies_without_a_configured_network() {
        let shared = Shared::new();
        let handle = shared.handle();
        let service = WifiService::new(handle, NoDelay, ());
        assert_eq!(
            service.scan_timeout_ms(),
            WifiConfig::default().scan_timeout_ms
        );
        handle.set_wifi_config(WifiConfig {
            scan_timeout_ms: 7_000,
            ..WifiConfig::default()
        });
        assert!(!handle.is_configured());
        assert_eq!(handle.wifi_config(), None);
        assert_eq!(service.scan_timeout_ms(), 7_000);
    }

    #[test]
    fn a_dhcp_failure_is_not_an_issue_of_the_link() {
        assert!(issue(WifiError::DhcpFailed).is_none());
        assert!(issue(WifiError::AuthenticationFailed).is_some());
    }

    #[test]
    fn the_service_identity_is_part_of_the_wire_abi() {
        assert_eq!(
            <Service<'static, ()> as rohi_service::Service>::NAME,
            "wifi"
        );
    }
}
