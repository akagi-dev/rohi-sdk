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
//! The `wifi` service: the link, the radio heap and the actions `scan`,
//! `connect`, `reconnect` and `disconnect`.

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::{String, Vec};
use rohi_config::Override;
use rohi_service::{
    ActionDescriptor, ActionError, ActionOutput, ConfigError, ConfigUpdate, EnumDescriptor,
    EnumVariant, FieldDescriptor, FieldError, Issue, IssueDescriptor, IssueSeverity,
    MAX_FIELD_VALUE_LEN, MAX_ISSUES, ManagedService, ServiceHealth, ServiceState, ServiceSummary,
    StatusWriter, Unit, Value, ValueKind, WriteError,
};

use crate::{WifiConfig, WifiError, WifiHandle, WifiPassword, WifiSecurity, WifiSsid, WifiState};

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

/// Id of the `wifi` service.
pub const SERVICE_ID: u32 = 1;

/// Ids of the actions of the `wifi` service.
pub mod action {
    /// Scans for networks.
    pub const SCAN: u32 = 1;
    /// Stores a network and connects to it.
    pub const CONNECT: u32 = 2;
    /// Drops the connection and connects again.
    pub const RECONNECT: u32 = 3;
    /// Disconnects and stays idle.
    pub const DISCONNECT: u32 = 4;
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

/// Field ids of the arguments of the `connect` action.
pub mod connect_field {
    /// Network name.
    pub const SSID: u32 = 1;
    /// Passphrase; empty for an open network.
    pub const PASSWORD: u32 = 2;
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

fn config_error(error: ConfigError) -> ActionError {
    match error {
        ConfigError::Field(
            FieldError::Invalid | FieldError::OutOfRange | FieldError::WrongKind,
        ) => ActionError::InvalidArgument,
        ConfigError::Unavailable => ActionError::Unavailable,
        _ => ActionError::Internal,
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
/// the `scan`, `connect`, `reconnect` and `disconnect` actions.
///
/// A scan is bounded by the `scan_timeout` of the configuration, measured
/// with `delay`. `connect` changes the `wifi` section through `config`, the
/// same transaction as a client's config patch, so it keeps the other
/// overrides and the revision of the section follows.
pub struct WifiService<'a, M: RawMutex, D, P, U> {
    handle: WifiHandle<'a, M>,
    delay: D,
    probe: P,
    config: U,
}

impl<'a, M: RawMutex, D: DelayNs + Clone, P: WifiProbe, U: ConfigUpdate<WifiConfig>>
    WifiService<'a, M, D, P, U>
{
    /// The service of the manager behind `handle`, with the platform
    /// readings of `probe` and the transactions on the `wifi` section through
    /// `config`.
    pub const fn new(handle: WifiHandle<'a, M>, delay: D, probe: P, config: U) -> Self {
        Self {
            handle,
            delay,
            probe,
            config,
        }
    }

    fn scan_timeout_ms(&self) -> u32 {
        self.handle.wifi_settings().scan_timeout_ms
    }

    async fn connect(&self, args: &[(u32, Value)]) -> Result<(), ActionError> {
        let text = |id| {
            args.iter()
                .find(|(field, _)| *field == id)
                .and_then(|(_, value)| value.as_str())
                .ok_or(ActionError::InvalidArgument)
        };
        let ssid = WifiSsid::from_bytes(text(connect_field::SSID)?.as_bytes())
            .ok_or(ActionError::InvalidArgument)?;
        // An empty password is an open network. That is an explicit choice, so
        // it is persisted as "no password", not as following the default.
        let password = match text(connect_field::PASSWORD)? {
            "" => Override::Value(None),
            password => Override::Value(Some(
                WifiPassword::from_bytes(password.as_bytes())
                    .ok_or(ActionError::InvalidArgument)?,
            )),
        };

        // The other overrides (the timing) are kept; only the network changes.
        // The effective result is validated and published by the section in the
        // commit step; a changed configuration makes the manager connect by itself.
        let before = self.handle.wifi_config();
        self.config
            .update(|overrides| {
                overrides.ssid = Override::Value(Some(ssid));
                overrides.password = password;
            })
            .await
            .map_err(config_error)?;
        // The same configuration is ignored by the manager and needs the
        // explicit new attempt.
        if self.handle.wifi_config() == before {
            self.handle.reconnect().map_err(action_error)?;
        }
        Ok(())
    }
}

impl<M: RawMutex, D: DelayNs + Clone, P: WifiProbe, U: ConfigUpdate<WifiConfig>> ManagedService
    for WifiService<'_, M, D, P, U>
{
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "wifi";
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
        ActionDescriptor::new(action::CONNECT, "connect")
            .args(&[
                FieldDescriptor::new(connect_field::SSID, "ssid", ValueKind::String),
                FieldDescriptor::new(connect_field::PASSWORD, "password", ValueKind::String),
            ])
            .secret(),
        ActionDescriptor::new(action::RECONNECT, "reconnect").privileged(),
        ActionDescriptor::new(action::DISCONNECT, "disconnect").privileged(),
    ];

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

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
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
            action::CONNECT => self.connect(args).await,
            action::RECONNECT => self.handle.reconnect().map_err(action_error),
            action::DISCONNECT => self.handle.disconnect().map_err(action_error),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::RefCell;

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{MAX_FIELDS, ManagedSection, SectionStore, StoreError};

    use super::*;
    use crate::WifiOverrides;
    use crate::WifiShared;
    use crate::{LinkConfig, ManagerConfig, NetworkInfo, WifiBackend, WifiManager};

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

    /// Like the firmware: storing hands the value to the manager.
    struct Store<'a> {
        handle: WifiHandle<'a, NoopRawMutex>,
        value: RefCell<Option<WifiOverrides>>,
        firmware: RefCell<WifiOverrides>,
        writes: RefCell<usize>,
        fail: bool,
    }

    impl<'a> Store<'a> {
        fn new(handle: WifiHandle<'a, NoopRawMutex>) -> Self {
            Self {
                handle,
                value: RefCell::new(None),
                firmware: RefCell::new(WifiOverrides::default()),
                writes: RefCell::new(0),
                fail: false,
            }
        }
    }

    impl SectionStore<WifiConfig> for &Store<'_> {
        fn firmware_overrides(&self) -> WifiOverrides {
            self.firmware.borrow().clone()
        }

        async fn load(&self) -> Result<Option<WifiOverrides>, StoreError> {
            if self.fail {
                return Err(StoreError::Unavailable);
            }
            Ok(self.value.borrow().clone())
        }

        async fn store(
            &self,
            overrides: &WifiOverrides,
            effective: &WifiConfig,
        ) -> Result<(), StoreError> {
            *self.writes.borrow_mut() += 1;
            *self.value.borrow_mut() = Some(overrides.clone());
            self.handle.set_wifi_config(effective.clone());
            Ok(())
        }

        async fn remove(&self, effective: &WifiConfig) -> Result<(), StoreError> {
            *self.value.borrow_mut() = None;
            self.handle.set_wifi_config(effective.clone());
            Ok(())
        }
    }

    fn section<'a>(
        store: &'a Store<'a>,
    ) -> ManagedSection<WifiConfig, &'a Store<'a>, NoopRawMutex> {
        ManagedSection::new(store)
    }

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
    type Service<'a, P> = WifiService<
        'a,
        NoopRawMutex,
        NoDelay,
        P,
        ManagedSection<WifiConfig, &'a Store<'a>, NoopRawMutex>,
    >;

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

    fn connect_args(ssid: &str, password: &str) -> [(u32, Value); 2] {
        [
            (connect_field::SSID, text(ssid)),
            (connect_field::PASSWORD, text(password)),
        ]
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
        let schema = <Service<'static, ()> as ManagedService>::STATUS_SCHEMA;
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
        let actions = <Service<'static, ()> as ManagedService>::ACTIONS;
        let flags: std::vec::Vec<_> = actions
            .iter()
            .map(|action| (action.name, action.privileged, action.secret_args))
            .collect();
        assert_eq!(
            flags,
            [
                ("scan", false, false),
                ("connect", true, true),
                ("reconnect", true, false),
                ("disconnect", true, false)
            ]
        );
        assert_eq!(actions[0].results.len(), 4);
        assert!(actions[0].enums[0].contains(security::WPA2));
        assert_eq!(actions[0].results[3].enum_id, actions[0].enums[0].id);
        let args: std::vec::Vec<_> = actions[1].args.iter().map(|arg| arg.name).collect();
        assert_eq!(args, ["ssid", "password"]);
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
        let store = Store::new(shared.handle());
        let service = WifiService::new(shared.handle(), NoDelay, (), section(&store));
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
        let store = Store::new(shared.handle());
        let service = WifiService::new(shared.handle(), NoDelay, FULL, section(&store));
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
        let store = Store::new(shared.handle());
        let service = WifiService::new(shared.handle(), NoDelay, (), section(&store));
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
        let service = WifiService::new(shared.handle(), NoDelay, probe, section(&store));
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(status_field::HEAP_FREE).is_some());
        assert!(out.get(status_field::HEAP_MIN_FREE).is_none());
    }

    #[test]
    fn a_connected_link_reports_details_and_uptime() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
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

        let service = WifiService::new(handle, NoDelay, FULL, section(&store));
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
    fn connect_stores_the_network_and_the_manager_joins_it() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        let mut manager = WifiManager::new(
            Radio {
                connects: 0,
                disconnects: 0,
            },
            NoDelay,
            &shared,
            ManagerConfig::DEFAULT,
        );
        let service = WifiService::new(handle, NoDelay, (), section(&store));
        let mut out = Collect::default();

        let args = connect_args("office", "hunter2hunter2");
        assert_eq!(
            block_on(service.invoke(action::CONNECT, &args, &mut out)),
            Ok(())
        );
        let stored = rohi_config::materialize::<WifiConfig>(
            &Default::default(),
            &store.value.borrow().clone().unwrap(),
        );
        assert_eq!(
            stored.ssid.as_ref().and_then(WifiSsid::as_str),
            Some("office")
        );
        assert_eq!(
            stored.password.as_ref().map(WifiPassword::expose_secret),
            Some(&b"hunter2hunter2"[..])
        );

        block_on(manager.step());
        block_on(manager.step());
        assert_eq!(handle.status().state, WifiState::Connected);
    }

    #[test]
    fn connect_keeps_the_timing_and_an_empty_password_means_open() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        *store.value.borrow_mut() = Some(WifiOverrides {
            ssid: Override::Value(Some(WifiSsid::from_bytes(b"old").unwrap())),
            password: Override::Value(Some(WifiPassword::from_bytes(b"hunter2hunter2").unwrap())),
            scan_timeout_ms: Override::Value(5_000),
            ..WifiOverrides::default()
        });
        let service = WifiService::new(handle, NoDelay, (), section(&store));

        let args = connect_args("cafe", "");
        let mut out = Collect::default();
        assert_eq!(
            block_on(service.invoke(action::CONNECT, &args, &mut out)),
            Ok(())
        );
        let stored = store.value.borrow().clone().unwrap();
        // An open network is explicit: it is not the same as following the default.
        assert_eq!(stored.password, Override::Value(None));
        assert_eq!(stored.scan_timeout_ms, Override::Value(5_000));
        let stored = rohi_config::materialize::<WifiConfig>(&Default::default(), &stored);
        assert_eq!(
            stored.ssid.as_ref().and_then(WifiSsid::as_str),
            Some("cafe")
        );
        assert_eq!(stored.password, None);
        assert_eq!(stored.scan_timeout_ms, 5_000);
    }

    #[test]
    fn an_open_network_stays_open_when_the_firmware_default_has_a_password() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        *store.firmware.borrow_mut() = WifiOverrides {
            ssid: Override::Value(Some(WifiSsid::from_bytes(b"factory").unwrap())),
            password: Override::Value(Some(WifiPassword::from_bytes(b"factory-password").unwrap())),
            ..WifiOverrides::default()
        };
        let service = WifiService::new(handle, NoDelay, (), section(&store));
        let mut out = Collect::default();
        assert_eq!(
            block_on(service.invoke(action::CONNECT, &connect_args("cafe", ""), &mut out)),
            Ok(())
        );
        let published = handle.wifi_config().unwrap();
        assert_eq!(
            published.ssid.as_ref().and_then(WifiSsid::as_str),
            Some("cafe")
        );
        assert_eq!(published.password, None);
    }

    #[test]
    fn connect_applies_the_user_overrides_on_the_firmware_overrides() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        *store.firmware.borrow_mut() = WifiOverrides {
            scan_timeout_ms: Override::Value(7_000),
            ..WifiOverrides::default()
        };
        let service = WifiService::new(handle, NoDelay, (), section(&store));
        let mut out = Collect::default();
        assert_eq!(
            block_on(service.invoke(
                action::CONNECT,
                &connect_args("office", "hunter2hunter2"),
                &mut out
            )),
            Ok(())
        );
        assert_eq!(handle.wifi_config().unwrap().scan_timeout_ms, 7_000);
        let stored = store.value.borrow().clone().unwrap();
        assert_eq!(stored.scan_timeout_ms, Override::Default);
    }

    #[test]
    fn the_scan_timeout_applies_without_a_configured_network() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        let service = WifiService::new(handle, NoDelay, (), section(&store));
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
    fn connect_shares_the_revision_and_does_not_lose_a_concurrent_patch() {
        use rohi_service::{ConfigSectionOps, Mutation, MutationOp};

        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        let section = section(&store);
        let service = WifiService::new(handle, NoDelay, (), &section);
        let mut out = Collect::default();
        let args = connect_args("office", "hunter2hunter2");
        let timing = Mutation {
            field_id: crate::wifi::managed::field::SCAN_TIMEOUT,
            op: MutationOp::Set(Value::U64(9_000)),
        };
        let (connected, patched) = block_on(embassy_futures::join::join(
            service.invoke(action::CONNECT, &args, &mut out),
            section.patch(&[timing], None, false),
        ));
        assert_eq!(connected, Ok(()));
        assert!(patched.is_ok());
        // Both writes went through the section: both overrides survive and
        // the revision counted both.
        let stored = store.value.borrow().clone().unwrap();
        assert_eq!(
            stored.ssid,
            Override::Value(Some(WifiSsid::from_bytes(b"office").unwrap()))
        );
        assert_eq!(stored.scan_timeout_ms, Override::Value(9_000));
        assert_eq!(section.info().revision, 2);
    }

    #[test]
    fn connect_to_the_same_network_connects_again() {
        let shared = Shared::new();
        let handle = shared.handle();
        let store = Store::new(handle);
        let mut manager = WifiManager::new(
            Radio {
                connects: 0,
                disconnects: 0,
            },
            NoDelay,
            &shared,
            ManagerConfig::DEFAULT,
        );
        let service = WifiService::new(handle, NoDelay, (), section(&store));
        let mut out = Collect::default();
        let args = connect_args("office", "hunter2hunter2");

        block_on(service.invoke(action::CONNECT, &args, &mut out)).unwrap();
        block_on(manager.step());
        block_on(manager.step());
        assert_eq!(handle.status().state, WifiState::Connected);

        // Nothing changed, so the store is not what restarts the link.
        block_on(service.invoke(action::CONNECT, &args, &mut out)).unwrap();
        assert_eq!(*store.writes.borrow(), 2);
        block_on(manager.step());
        block_on(manager.step());
        assert_eq!(handle.metrics().reconnects, 1);
    }

    #[test]
    fn invalid_credentials_are_refused_and_nothing_is_stored() {
        let shared = Shared::new();
        let store = Store::new(shared.handle());
        let service = WifiService::new(shared.handle(), NoDelay, (), section(&store));
        let mut out = Collect::default();

        for args in [
            connect_args("", "hunter2hunter2"),
            connect_args("office", "short"),
            connect_args(&"a".repeat(33), ""),
        ] {
            assert_eq!(
                block_on(service.invoke(action::CONNECT, &args, &mut out)),
                Err(ActionError::InvalidArgument)
            );
        }
        assert_eq!(
            block_on(service.invoke(action::CONNECT, &[], &mut out)),
            Err(ActionError::InvalidArgument)
        );
        assert_eq!(
            block_on(service.invoke(
                action::CONNECT,
                &[(connect_field::SSID, Value::U64(1)), (2, text(""))],
                &mut out
            )),
            Err(ActionError::InvalidArgument)
        );
        assert_eq!(*store.writes.borrow(), 0);
        assert!(store.value.borrow().is_none());
    }

    #[test]
    fn an_unavailable_store_is_reported() {
        let shared = Shared::new();
        let mut store = Store::new(shared.handle());
        store.fail = true;
        let service = WifiService::new(shared.handle(), NoDelay, (), section(&store));
        let mut out = Collect::default();
        assert_eq!(
            block_on(service.invoke(action::CONNECT, &connect_args("office", ""), &mut out)),
            Err(ActionError::Unavailable)
        );
    }

    #[test]
    fn a_dhcp_failure_is_not_an_issue_of_the_link() {
        assert!(issue(WifiError::DhcpFailed).is_none());
        assert!(issue(WifiError::AuthenticationFailed).is_some());
    }
}
