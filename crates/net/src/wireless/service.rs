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
//! The `wireless` service: the Wi-Fi link, the radio heap, the BLE peripheral
//! health and the actions `wifi_scan`, `wifi_reconnect` and `wifi_disconnect`.

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::{String, Vec};
use log::debug;
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_FIELD_VALUE_LEN, MAX_ISSUES,
    Service, ServiceHealth, ServiceSummary, Status, StatusWriter, Unit, Value, ValueKind,
    WriteError,
};

use crate::{BleError, BleProbe, BleState, WifiError, WifiHandle, WifiSecurity, WifiState};

// SSIDs are scan results, which are never cut.
const _: () = assert!(crate::MAX_SSID_LEN <= MAX_FIELD_VALUE_LEN);
// The dotted form of an IPv4 address is a status value.
const _: () = assert!("255.255.255.255".len() <= MAX_FIELD_VALUE_LEN);

/// Usage of the memory the radio drivers allocate from. The `wireless`
/// service reports the free part as `heap_free`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    /// Total size in bytes.
    pub size: usize,
    /// Free bytes right now.
    pub free: usize,
    /// Lowest number of free bytes seen since boot, if the platform tracks it.
    pub min_free: Option<usize>,
}

/// What the [`WirelessService`] reads from the platform, besides the Wi-Fi
/// manager and the BLE peripheral. A value the platform cannot tell is `None`
/// and not reported.
pub trait WirelessProbe {
    /// The radio heap.
    fn heap(&self) -> Option<HeapStats>;
}

/// A platform without a radio heap to report.
impl WirelessProbe for () {
    fn heap(&self) -> Option<HeapStats> {
        None
    }
}

/// Ids of the actions of the `wireless` service.
pub mod action {
    /// Scans for Wi-Fi networks.
    pub const WIFI_SCAN: u32 = 1;
    /// Queues a forced Wi-Fi reconnection.
    pub const WIFI_RECONNECT: u32 = 2;
    /// Disconnects Wi-Fi and stays idle.
    pub const WIFI_DISCONNECT: u32 = 3;
}

/// Issue codes of the `wireless` service. A code shared by two parts is one
/// code: `busy`, `disconnected` and `driver_error` can come from Wi-Fi or BLE.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// Wi-Fi is not configured.
    pub const NOT_CONFIGURED: u32 = BASE;
    /// The Wi-Fi manager or the BLE transport is busy.
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
    /// The Wi-Fi link was lost or the BLE central disconnected.
    pub const DISCONNECTED: u32 = BASE + 7;
    /// The wireless configuration is invalid.
    pub const INVALID_CONFIG: u32 = BASE + 8;
    /// The Wi-Fi or BLE driver failed.
    pub const DRIVER_ERROR: u32 = BASE + 9;
    /// The link is up but no IPv4 configuration was obtained.
    pub const DHCP_FAILED: u32 = BASE + 10;
    /// A BLE chunk did not fit.
    pub const BUFFER_TOO_SMALL: u32 = BASE + 11;
}

/// Values of the `security` result of the `wifi_scan` action.
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

/// Values of the `wifi_state` status field.
pub mod wifi_state {
    /// No network is configured.
    pub const NOT_CONFIGURED: u32 = 0;
    /// Configured but deliberately not connected.
    pub const IDLE: u32 = 1;
    /// A scan is running.
    pub const SCANNING: u32 = 2;
    /// Associating and acquiring an IPv4 configuration.
    pub const CONNECTING: u32 = 3;
    /// Associated and the IPv4 configuration is usable.
    pub const CONNECTED: u32 = 4;
    /// The link is down; a reconnect is scheduled.
    pub const DISCONNECTED: u32 = 5;
    /// The last connection attempt failed; a retry is scheduled.
    pub const ERROR: u32 = 6;
}

/// Field ids of the `wireless` service status. Only ever added.
pub mod status_field {
    /// State of the Wi-Fi connection, see [`wifi_state`](super::wifi_state).
    pub const WIFI_STATE: u32 = 1;
    /// IPv4 address of the Wi-Fi interface.
    pub const WIFI_IP: u32 = 2;
    /// Signal strength of the Wi-Fi connection.
    pub const WIFI_RSSI: u32 = 3;
    /// Connection attempts after a failure, a lost link or a reconnect.
    pub const WIFI_RECONNECTS: u32 = 4;
    /// Free radio heap.
    pub const HEAP_FREE: u32 = 5;
}

/// Field ids of the results of the `wifi_scan` action.
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
const WIFI_STATE_ENUM: u32 = 1;

const SECURITY_ENUMS: &[EnumDescriptor] = &[EnumDescriptor::new(
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

const STATUS_ENUMS: &[EnumDescriptor] = &[EnumDescriptor::new(
    WIFI_STATE_ENUM,
    &[
        EnumVariant::new(wifi_state::NOT_CONFIGURED, "not_configured"),
        EnumVariant::new(wifi_state::IDLE, "idle"),
        EnumVariant::new(wifi_state::SCANNING, "scanning"),
        EnumVariant::new(wifi_state::CONNECTING, "connecting"),
        EnumVariant::new(wifi_state::CONNECTED, "connected"),
        EnumVariant::new(wifi_state::DISCONNECTED, "disconnected"),
        EnumVariant::new(wifi_state::ERROR, "error"),
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
    IssueDescriptor::new(issue::DHCP_FAILED, "dhcp_failed"),
    IssueDescriptor::new(issue::BUFFER_TOO_SMALL, "buffer_too_small"),
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

fn wifi_state_value(state: WifiState) -> u32 {
    match state {
        WifiState::NotConfigured => wifi_state::NOT_CONFIGURED,
        WifiState::Idle => wifi_state::IDLE,
        WifiState::Scanning => wifi_state::SCANNING,
        WifiState::Connecting => wifi_state::CONNECTING,
        WifiState::Connected => wifi_state::CONNECTED,
        WifiState::Disconnected => wifi_state::DISCONNECTED,
        WifiState::Error => wifi_state::ERROR,
    }
}

fn wifi_issue(error: WifiError) -> Issue {
    let (severity, code) = match error {
        WifiError::NotConfigured => (IssueSeverity::Info, issue::NOT_CONFIGURED),
        WifiError::Busy => (IssueSeverity::Warning, issue::BUSY),
        WifiError::Timeout => (IssueSeverity::Warning, issue::TIMEOUT),
        WifiError::ScanFailed => (IssueSeverity::Warning, issue::SCAN_FAILED),
        WifiError::NetworkNotFound => (IssueSeverity::Warning, issue::NETWORK_NOT_FOUND),
        WifiError::AuthenticationFailed => (IssueSeverity::Error, issue::AUTHENTICATION_FAILED),
        WifiError::AssociationFailed => (IssueSeverity::Warning, issue::ASSOCIATION_FAILED),
        WifiError::DhcpFailed => (IssueSeverity::Warning, issue::DHCP_FAILED),
        WifiError::Disconnected => (IssueSeverity::Warning, issue::DISCONNECTED),
        WifiError::InvalidConfig => (IssueSeverity::Error, issue::INVALID_CONFIG),
        WifiError::DriverError => (IssueSeverity::Critical, issue::DRIVER_ERROR),
    };
    Issue {
        severity,
        code,
        age_ms: None,
    }
}

fn ble_issue(error: BleError) -> Issue {
    let code = match error {
        BleError::Disconnected => issue::DISCONNECTED,
        BleError::BufferTooSmall => issue::BUFFER_TOO_SMALL,
        BleError::Busy => issue::BUSY,
        BleError::DriverError => issue::DRIVER_ERROR,
    };
    Issue {
        severity: IssueSeverity::Error,
        code,
        age_ms: None,
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

/// How the Wi-Fi link reads as a health, `None` while it says nothing: an
/// unconfigured, idle or still connecting radio is not a fault.
fn wifi_health(state: WifiState) -> Option<ServiceHealth> {
    match state {
        WifiState::NotConfigured
        | WifiState::Idle
        | WifiState::Scanning
        | WifiState::Connecting => None,
        WifiState::Connected => Some(ServiceHealth::Ok),
        WifiState::Disconnected => Some(ServiceHealth::Degraded),
        WifiState::Error => Some(ServiceHealth::Error),
    }
}

/// How the BLE peripheral reads as a health, `None` while it is off.
fn ble_health(state: BleState) -> Option<ServiceHealth> {
    match state {
        BleState::Disabled => None,
        BleState::Advertising | BleState::Connected(_) => Some(ServiceHealth::Ok),
        BleState::Error(_) => Some(ServiceHealth::Error),
    }
}

/// The worst of the parts that say something, `Unknown` when none does.
fn worst(parts: [Option<ServiceHealth>; 2]) -> ServiceHealth {
    let rank = |health| match health {
        ServiceHealth::Unknown => 0,
        ServiceHealth::Ok => 1,
        ServiceHealth::Degraded => 2,
        ServiceHealth::Error => 3,
    };
    parts
        .into_iter()
        .flatten()
        .max_by_key(|health| rank(*health))
        .unwrap_or(ServiceHealth::Unknown)
}

/// The wireless side of the device as one `wireless` service: the Wi-Fi
/// connection, the radio heap, the health of the BLE peripheral, and the
/// `wifi_scan`, `wifi_reconnect` and `wifi_disconnect` actions.
///
/// Wi-Fi, the IP layer and BLE stay separate internally ([`WifiManager`],
/// [`NetworkConfig`](crate::NetworkConfig), [`BleTransport`](crate::BleTransport)).
/// Clients see them through the generic service pipeline ([`rohi_service`])
/// as one service and one config section ([`WirelessConfig`](crate::WirelessConfig)):
///
/// | Plane | What | `rohi` |
/// |-------|------|--------|
/// | config section `wireless` | 15 fields: `wifi_*`, `ip_*` and `ble_name` | `rohi config wireless wifi_ssid=office wifi_password=-` |
/// | service `wireless` | `wifi_state`, `wifi_ip`, `wifi_rssi` (dBm), `wifi_reconnects`, `heap_free` (bytes). `wifi_ip` and `wifi_rssi` only while connected, `heap_free` only when the [`WirelessProbe`] reports it | `rohi status wireless` |
/// | action `wifi_scan` | one record per network, strongest first: `ssid`, `rssi`, `channel`, `security` | `rohi action wireless wifi_scan` |
/// | actions `wifi_reconnect`, `wifi_disconnect` (mutate access) | queue a forced connection attempt / disconnect and stay idle | `rohi action wireless wifi_reconnect` |
///
/// There is no SSID, BLE name or BLE state field: the BLE peripheral is always
/// on, and a failed one is reported through the health and the issues.
///
/// **Health** is the worst of the Wi-Fi link and the BLE peripheral. A
/// `NotConfigured`, `Idle`, `Scanning` or `Connecting` link and a `Disabled`
/// peripheral say nothing, so an unconfigured Wi-Fi with a working BLE
/// peripheral is `ok`; `Connected` is `ok`, `Disconnected` is `degraded` and
/// `Error` is `error`; an advertising or connected peripheral is `ok` and a
/// failed one is `error`. With nothing to say the health is `unknown`.
///
/// **Issues** list the last Wi-Fi error while the state is `Error` or
/// `Disconnected` (`authentication_failed`, `network_not_found`,
/// `dhcp_failed`, ...) and the error of a failed BLE peripheral. A code that
/// both parts can raise (`busy`, `disconnected`, `driver_error`) is listed
/// once. See [`issue`](mod@issue).
///
/// A scan is bounded by `wifi_scan_timeout` (20 s by default), measured with
/// `delay`, and lists at most six networks (the record limit of an action
/// result); networks whose name is not UTF-8 are not listed. `wifi_reconnect`
/// only queues the attempt: success means it was accepted, not that the link
/// is up, so watch `wifi_state`. `wifi_disconnect` keeps the radio idle until a
/// `wifi_reconnect` or a configuration change, and can drop an active TCP
/// control connection.
///
/// ```
/// use embassy_futures::block_on;
/// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
/// use embedded_hal_async::delay::DelayNs;
/// use rohi_net::{BleProbe, BleState, WifiShared, WirelessService};
/// use rohi_service::{ServiceHealth, Status};
///
/// #[derive(Clone)]
/// struct Delay;
///
/// impl DelayNs for Delay {
///     async fn delay_ns(&mut self, _ns: u32) {}
/// }
///
/// struct Ble;
///
/// impl BleProbe for Ble {
///     fn state(&self) -> BleState {
///         BleState::Advertising
///     }
/// }
///
/// let shared = WifiShared::<NoopRawMutex>::new();
/// // `()` is a probe that reports no radio heap.
/// let service = WirelessService::new(shared.handle(), Delay, (), Ble);
///
/// // Wi-Fi is not configured, which is not a fault while BLE works.
/// let summary = block_on(service.summary());
/// assert_eq!(summary.health, ServiceHealth::Ok);
/// ```
///
/// [`WifiManager`]: crate::WifiManager
#[derive(Service)]
pub struct WirelessService<'a, M: RawMutex, D, P, B> {
    handle: WifiHandle<'a, M>,
    delay: D,
    probe: P,
    ble: B,
}

impl<'a, M: RawMutex, D: DelayNs + Clone, P: WirelessProbe, B: BleProbe>
    WirelessService<'a, M, D, P, B>
{
    /// The service of the manager behind `handle`, with the radio heap of
    /// `probe` and the state of the peripheral behind `ble`.
    pub const fn new(handle: WifiHandle<'a, M>, delay: D, probe: P, ble: B) -> Self {
        Self {
            handle,
            delay,
            probe,
            ble,
        }
    }

    fn scan_timeout_ms(&self) -> u32 {
        self.handle.wifi_settings().scan_timeout_ms
    }
}

impl<M: RawMutex, D: DelayNs + Clone, P: WirelessProbe, B: BleProbe> Status
    for WirelessService<'_, M, D, P, B>
{
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::WIFI_STATE, "wifi_state", ValueKind::Enum)
            .with_enum(WIFI_STATE_ENUM),
        FieldDescriptor::new(status_field::WIFI_IP, "wifi_ip", ValueKind::String),
        FieldDescriptor::new(status_field::WIFI_RSSI, "wifi_rssi", ValueKind::I64).unit(Unit::Dbm),
        FieldDescriptor::new(
            status_field::WIFI_RECONNECTS,
            "wifi_reconnects",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::HEAP_FREE, "heap_free", ValueKind::U64)
            .unit(Unit::Bytes),
    ];
    const ENUMS: &'static [EnumDescriptor] = STATUS_ENUMS;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(worst([
            wifi_health(self.handle.status().state),
            ble_health(self.ble.state()),
        ]))
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let status = self.handle.status();
        // A configured-but-idle or unconfigured radio is not an issue.
        if let Some(error) = status.error
            && matches!(status.state, WifiState::Error | WifiState::Disconnected)
        {
            let _ = out.push(wifi_issue(error));
        }
        if let BleState::Error(error) = self.ble.state() {
            let found = ble_issue(error);
            if !out.iter().any(|issue| issue.code == found.code) {
                let _ = out.push(found);
            }
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.variant(
            status_field::WIFI_STATE,
            wifi_state_value(self.handle.status().state),
        )?;
        if let Some(info) = self.handle.network_info() {
            let mut text = String::<15>::new();
            let _ = write!(text, "{}", info.ipv4);
            out.text(status_field::WIFI_IP, &text)?;
            out.i64(status_field::WIFI_RSSI, i64::from(info.rssi_dbm))?;
        }
        out.u64(
            status_field::WIFI_RECONNECTS,
            u64::from(self.handle.metrics().reconnects),
        )?;
        if let Some(heap) = self.probe.heap() {
            out.u64(status_field::HEAP_FREE, heap.free as u64)?;
        }
        Ok(())
    }
}

impl<M: RawMutex, D: DelayNs + Clone, P: WirelessProbe, B: BleProbe> Action
    for WirelessService<'_, M, D, P, B>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::WIFI_SCAN, "wifi_scan", ActionAccess::Read)
            .results(&[
                FieldDescriptor::new(scan_field::SSID, "ssid", ValueKind::String),
                FieldDescriptor::new(scan_field::RSSI, "rssi", ValueKind::I64).unit(Unit::Dbm),
                FieldDescriptor::new(scan_field::CHANNEL, "channel", ValueKind::U64),
                FieldDescriptor::new(scan_field::SECURITY, "security", ValueKind::Enum)
                    .with_enum(SECURITY_ENUM),
            ])
            .enums(SECURITY_ENUMS),
        ActionDescriptor::new(
            action::WIFI_RECONNECT,
            "wifi_reconnect",
            ActionAccess::Mutate,
        ),
        ActionDescriptor::new(
            action::WIFI_DISCONNECT,
            "wifi_disconnect",
            ActionAccess::Mutate,
        ),
    ];

    async fn invoke(
        &self,
        action: u32,
        __args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::WIFI_SCAN => {
                let scan = self.handle.scan();
                let mut delay = self.delay.clone();
                let timeout = delay.delay_ms(self.scan_timeout_ms());
                let results = match select(scan, timeout).await {
                    Either::First(results) => results.map_err(action_error)?,
                    Either::Second(()) => {
                        report::scan_timed_out(self.scan_timeout_ms());
                        return Err(ActionError::Unavailable);
                    }
                };
                debug!(target: "wifi", "Scan action returns results (count={})", results.len());
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
                        report::scan_output_failed();
                        return Err(ActionError::Internal);
                    }
                }
                Ok(())
            }
            action::WIFI_RECONNECT => self.handle.reconnect().map_err(action_error),
            action::WIFI_DISCONNECT => self.handle.disconnect().map_err(action_error),
            _ => {
                report::unknown_action(action);
                Err(ActionError::UnknownAction)
            }
        }
    }
}

// Out of line, so that the reports do not enlarge the frame of the action.
mod report {
    use log::warn;

    #[cold]
    #[inline(never)]
    pub(super) fn scan_timed_out(timeout_ms: u32) {
        warn!(target: "wifi", "Scan action timed out (timeout_ms={})", timeout_ms);
    }

    #[cold]
    #[inline(never)]
    pub(super) fn scan_output_failed() {
        warn!(target: "wifi", "Scan result could not be written");
    }

    #[cold]
    #[inline(never)]
    pub(super) fn unknown_action(action: u32) {
        warn!(target: "wifi", "Unknown action requested (id={})", action);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::Cell;

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{MAX_FIELDS, MAX_ISSUE_DESCRIPTORS, MAX_SERVICE_ENUMS};

    use super::*;
    use crate::{
        BleConnectionInfo, LinkConfig, ManagerConfig, NetworkInfo, WifiBackend, WifiConfig,
        WifiManager, WifiScanResults, WifiShared, WifiSsid,
    };

    #[derive(Clone)]
    struct NoDelay;

    impl DelayNs for NoDelay {
        async fn delay_ns(&mut self, _: u32) {
            core::future::pending::<()>().await;
        }
    }

    struct Heap(Option<HeapStats>);

    impl WirelessProbe for Heap {
        fn heap(&self) -> Option<HeapStats> {
            self.0
        }
    }

    const FULL: Heap = Heap(Some(HeapStats {
        size: 131_072,
        free: 40_000,
        min_free: Some(30_000),
    }));

    struct Ble(Cell<BleState>);

    impl Ble {
        fn new(state: BleState) -> Self {
            Self(Cell::new(state))
        }
    }

    impl BleProbe for &Ble {
        fn state(&self) -> BleState {
            self.0.get()
        }
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
    type Service<'a, P> = WirelessService<'a, NoopRawMutex, NoDelay, P, &'a Ble>;

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

    /// A radio that connects or fails with `error`.
    struct Radio(Option<WifiError>);

    impl WifiBackend for Radio {
        async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
            Err(WifiError::ScanFailed)
        }
        async fn connect(&mut self, _: &LinkConfig) -> Result<NetworkInfo, WifiError> {
            self.0.map_or_else(|| Ok(info()), Err)
        }
        async fn disconnect(&mut self) -> Result<(), WifiError> {
            Ok(())
        }
        async fn wait_link_lost(&mut self) -> WifiError {
            core::future::pending().await
        }
        fn network_info(&mut self) -> Option<NetworkInfo> {
            self.0.is_none().then(info)
        }
    }

    /// Drives the manager to its first settled state.
    fn settle(shared: &Shared, error: Option<WifiError>) {
        let mut manager = WifiManager::new(Radio(error), NoDelay, shared, ManagerConfig::DEFAULT);
        shared.handle().set_wifi_config(config());
        block_on(manager.step());
        block_on(manager.step());
    }

    fn health<P: WirelessProbe>(service: &Service<'_, P>) -> ServiceHealth {
        block_on(service.summary()).health
    }

    fn issue_codes<P: WirelessProbe>(service: &Service<'_, P>) -> std::vec::Vec<u32> {
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        issues.iter().map(|issue| issue.code).collect()
    }

    #[test]
    fn the_schema_fits_the_limits() {
        let schema = <Service<'static, ()> as Status>::STATUS_SCHEMA;
        assert!(schema.len() <= MAX_FIELDS);
        let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        for field in schema {
            assert!(field.name.len() <= 24, "{}", field.name);
        }
        assert!(<Service<'static, ()> as Status>::ENUMS.len() <= MAX_SERVICE_ENUMS);
        assert!(<Service<'static, ()> as Status>::ISSUES.len() <= MAX_ISSUE_DESCRIPTORS);
    }

    #[test]
    fn the_status_has_exactly_the_five_diagnostic_fields() {
        let names: std::vec::Vec<_> = <Service<'static, ()> as Status>::STATUS_SCHEMA
            .iter()
            .map(|field| field.name)
            .collect();
        assert_eq!(
            names,
            [
                "wifi_state",
                "wifi_ip",
                "wifi_rssi",
                "wifi_reconnects",
                "heap_free"
            ]
        );
    }

    #[test]
    fn the_issue_codes_are_unique_and_named() {
        let issues = <Service<'static, ()> as Status>::ISSUES;
        assert_eq!(issues.len(), 12);
        for (index, issue) in issues.iter().enumerate() {
            for other in &issues[..index] {
                assert_ne!(issue.code, other.code);
                assert_ne!(issue.name, other.name);
            }
        }
    }

    #[test]
    fn exactly_three_actions_are_declared_with_their_privileges() {
        let actions = <Service<'static, ()> as Action>::ACTIONS;
        let flags: std::vec::Vec<_> = actions
            .iter()
            .map(|action| (action.id, action.name, action.access, action.secret_args))
            .collect();
        assert_eq!(
            flags,
            [
                (1, "wifi_scan", ActionAccess::Read, false),
                (2, "wifi_reconnect", ActionAccess::Mutate, false),
                (3, "wifi_disconnect", ActionAccess::Mutate, false)
            ]
        );
        assert_eq!(actions[0].results.len(), 4);
        assert!(actions[0].args.is_empty());
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
    fn reconnect_and_disconnect_are_queued_and_unknown_actions_are_refused() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        let mut out = Collect::default();
        for id in [action::WIFI_RECONNECT, action::WIFI_DISCONNECT] {
            assert_eq!(block_on(service.invoke(id, &[], &mut out)), Ok(()));
        }
        assert_eq!(
            block_on(service.invoke(99, &[], &mut out)),
            Err(ActionError::UnknownAction)
        );
    }

    #[test]
    fn a_full_command_queue_is_busy() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        let mut out = Collect::default();
        for _ in 0..crate::WIFI_COMMAND_QUEUE_DEPTH {
            assert_eq!(
                block_on(service.invoke(action::WIFI_RECONNECT, &[], &mut out)),
                Ok(())
            );
        }
        assert_eq!(
            block_on(service.invoke(action::WIFI_RECONNECT, &[], &mut out)),
            Err(ActionError::Busy)
        );
    }

    #[test]
    fn an_unconfigured_wifi_with_a_working_ble_is_healthy() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        let service = WirelessService::new(shared.handle(), NoDelay, FULL, &ble);
        assert_eq!(health(&service), ServiceHealth::Ok);
        assert!(issue_codes(&service).is_empty());

        // Without BLE the service has nothing to say.
        ble.0.set(BleState::Disabled);
        assert_eq!(health(&service), ServiceHealth::Unknown);
    }

    #[test]
    fn a_new_manager_reports_its_state_counters_and_heap_but_no_link_details() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        let service = WirelessService::new(shared.handle(), NoDelay, FULL, &ble);
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(status_field::WIFI_STATE),
            Some(&Value::Enum(wifi_state::NOT_CONFIGURED))
        );
        assert!(out.get(status_field::WIFI_IP).is_none());
        assert!(out.get(status_field::WIFI_RSSI).is_none());
        assert_eq!(out.get(status_field::WIFI_RECONNECTS), Some(&Value::U64(0)));
        assert_eq!(out.get(status_field::HEAP_FREE), Some(&Value::U64(40_000)));
        assert_eq!(out.0.len(), 3);
    }

    #[test]
    fn what_the_platform_cannot_tell_is_not_reported() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(status_field::HEAP_FREE).is_none());
        assert_eq!(out.0.len(), 2);
    }

    #[test]
    fn a_connected_link_reports_the_address_and_the_signal_only() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        settle(&shared, None);
        let service = WirelessService::new(shared.handle(), NoDelay, FULL, &ble);
        assert_eq!(health(&service), ServiceHealth::Ok);

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(status_field::WIFI_STATE),
            Some(&Value::Enum(wifi_state::CONNECTED))
        );
        assert_eq!(out.get(status_field::WIFI_IP), Some(&text("192.168.1.41")));
        assert_eq!(out.get(status_field::WIFI_RSSI), Some(&Value::I64(-61)));
        // Neither the SSID nor anything of BLE is a status field.
        assert_eq!(out.0.len(), 5);
    }

    #[test]
    fn a_wifi_failure_is_an_issue_even_with_a_working_ble() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        settle(&shared, Some(WifiError::AuthenticationFailed));
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        assert_eq!(health(&service), ServiceHealth::Error);
        assert_eq!(issue_codes(&service), [issue::AUTHENTICATION_FAILED]);
    }

    #[test]
    fn a_dhcp_failure_is_a_wireless_issue() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        settle(&shared, Some(WifiError::DhcpFailed));
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        assert_eq!(issue_codes(&service), [issue::DHCP_FAILED]);
    }

    #[test]
    fn a_failed_ble_stack_is_an_issue_and_fails_the_service() {
        let ble = Ble::new(BleState::Error(BleError::BufferTooSmall));
        let shared = Shared::new();
        settle(&shared, None);
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        assert_eq!(health(&service), ServiceHealth::Error);
        assert_eq!(issue_codes(&service), [issue::BUFFER_TOO_SMALL]);

        ble.0.set(BleState::Connected(BleConnectionInfo {
            encrypted: false,
            authenticated: false,
        }));
        assert_eq!(health(&service), ServiceHealth::Ok);
        assert!(issue_codes(&service).is_empty());
    }

    #[test]
    fn a_code_both_parts_can_raise_is_listed_once() {
        let ble = Ble::new(BleState::Error(BleError::DriverError));
        let shared = Shared::new();
        settle(&shared, Some(WifiError::DriverError));
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        assert_eq!(issue_codes(&service), [issue::DRIVER_ERROR]);

        // Different codes are both listed.
        let ble = Ble::new(BleState::Error(BleError::Busy));
        let service = WirelessService::new(shared.handle(), NoDelay, (), &ble);
        assert_eq!(issue_codes(&service), [issue::DRIVER_ERROR, issue::BUSY]);
    }

    #[test]
    fn the_health_is_the_worst_part() {
        use ServiceHealth::{Degraded, Error, Ok, Unknown};
        assert_eq!(worst([None, None]), Unknown);
        assert_eq!(worst([Some(Ok), None]), Ok);
        assert_eq!(worst([Some(Ok), Some(Degraded)]), Degraded);
        assert_eq!(worst([Some(Degraded), Some(Error)]), Error);
        assert_eq!(worst([None, Some(Error)]), Error);
    }

    #[test]
    fn the_scan_timeout_applies_without_a_configured_network() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        let handle = shared.handle();
        let service = WirelessService::new(handle, NoDelay, (), &ble);
        assert_eq!(
            service.scan_timeout_ms(),
            WifiConfig::default().scan_timeout_ms
        );
        handle.set_wifi_config(WifiConfig {
            scan_timeout_ms: 7_000,
            ..WifiConfig::default()
        });
        assert!(!handle.is_configured());
        assert_eq!(service.scan_timeout_ms(), 7_000);
    }

    #[test]
    fn the_service_identity_is_part_of_the_wire_abi() {
        assert_eq!(
            <Service<'static, ()> as rohi_service::Service>::NAME,
            "wireless"
        );
    }
}
