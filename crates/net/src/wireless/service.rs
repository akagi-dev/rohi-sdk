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

use core::cell::RefCell;
use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::{String, Vec};
use log::debug;
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_FIELD_VALUE_LEN, MAX_ISSUES,
    MAX_RECORDS, ResultFormat, Service, ServiceHealth, ServiceSummary, SnapshotInfo, Status,
    StatusWriter, Unit, Value, ValueKind, WriteError,
};
use rohi_time::{Clock, MonotonicTimestamp};

use crate::{
    BleError, BleProbe, BleState, WifiError, WifiHandle, WifiNetwork, WifiScanResults,
    WifiSecurity, WifiState,
};

/// How long an idle scan result is held for its client, in milliseconds.
pub const SCAN_HOLD_MS: u64 = 30_000;

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
/// | action `wifi_scan` (`Records`) | one record per network, strongest first, up to [`MAX_WIFI_SCAN_RESULTS`](crate::MAX_WIFI_SCAN_RESULTS): `ssid`, `rssi`, `channel`, `security` | `rohi action wireless wifi_scan` |
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
/// `delay`, and runs once: all its networks (those whose name is UTF-8) are one
/// `Records` result, so a client reads the pages of one scan and a later page
/// never scans again. Up to six networks are returned at once. A longer result
/// is kept in the service, in the compact form of the scan, for the one client
/// that read it first: it is released when that client closes it, after
/// [`SCAN_HOLD_MS`](crate::SCAN_HOLD_MS) without a read or when the control link is lost, and until
/// then another `wifi_scan` is `Busy`. `wifi_reconnect`
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
/// use rohi_time::{Clock, MonotonicTimestamp};
///
/// #[derive(Clone)]
/// struct Delay;
///
/// impl DelayNs for Delay {
///     async fn delay_ns(&mut self, _ns: u32) {}
/// }
///
/// struct Uptime;
///
/// impl Clock for Uptime {
///     fn now(&self) -> MonotonicTimestamp {
///         MonotonicTimestamp::from_millis(0)
///     }
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
/// let service = WirelessService::new(shared.handle(), Delay, Uptime, (), Ble);
///
/// // Wi-Fi is not configured, which is not a fault while BLE works.
/// let summary = block_on(service.summary());
/// assert_eq!(summary.health, ServiceHealth::Ok);
/// ```
///
/// [`WifiManager`]: crate::WifiManager
#[derive(Service)]
pub struct WirelessService<'a, M: RawMutex, D, C, P, B> {
    handle: WifiHandle<'a, M>,
    delay: D,
    clock: C,
    probe: P,
    ble: B,
    scan: Mutex<M, RefCell<ScanSlot>>,
}

/// The one scan result held for a client: a snapshot of the networks of one
/// scan, read page by page.
struct ScanSlot {
    // 0 when nothing is held.
    id: u32,
    next_id: u32,
    deadline_ms: u64,
    networks: WifiScanResults,
}

impl ScanSlot {
    const fn new() -> Self {
        Self {
            id: 0,
            next_id: 1,
            deadline_ms: 0,
            networks: WifiScanResults::new(),
        }
    }

    fn release(&mut self) {
        self.id = 0;
        self.networks.clear();
    }

    // Releases the held scan once its deadline passed.
    fn expire(&mut self, now_ms: u64) {
        if self.id != 0 && now_ms >= self.deadline_ms {
            report::scan_expired(self.id);
            self.release();
        }
    }
}

impl<'a, M: RawMutex, D: DelayNs + Clone, C: Clock, P: WirelessProbe, B: BleProbe>
    WirelessService<'a, M, D, C, P, B>
{
    /// The service of the manager behind `handle`, with the radio heap of
    /// `probe` and the state of the peripheral behind `ble`. `delay` bounds a
    /// scan; `clock` times out a scan result nobody reads.
    pub const fn new(handle: WifiHandle<'a, M>, delay: D, clock: C, probe: P, ble: B) -> Self {
        Self {
            handle,
            delay,
            clock,
            probe,
            ble,
            scan: Mutex::new(RefCell::new(ScanSlot::new())),
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock
            .now()
            .saturating_millis_since(MonotonicTimestamp::from_millis(0))
    }

    fn with_scan<R>(&self, f: impl FnOnce(&mut ScanSlot) -> R) -> R {
        self.scan.lock(|slot| f(&mut slot.borrow_mut()))
    }

    fn scan_timeout_ms(&self) -> u32 {
        self.handle.wifi_settings().scan_timeout_ms
    }
}

impl<M: RawMutex, D: DelayNs + Clone, C: Clock, P: WirelessProbe, B: BleProbe> Status
    for WirelessService<'_, M, D, C, P, B>
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

impl<M: RawMutex, D: DelayNs + Clone, C: Clock, P: WirelessProbe, B: BleProbe> Action
    for WirelessService<'_, M, D, C, P, B>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::WIFI_SCAN, "wifi_scan", ActionAccess::Read)
            .format(ResultFormat::Records)
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
                // The one held result belongs to a client that has not finished
                // reading it: no new scan until it is released or expires.
                if self.with_scan(|slot| {
                    slot.expire(self.now_ms());
                    slot.id != 0
                }) {
                    report::scan_busy();
                    return Err(ActionError::Busy);
                }
                let scan = self.handle.scan();
                let mut delay = self.delay.clone();
                let timeout = delay.delay_ms(self.scan_timeout_ms());
                let mut results = match select(scan, timeout).await {
                    Either::First(results) => results.map_err(action_error)?,
                    Either::Second(()) => {
                        report::scan_timed_out(self.scan_timeout_ms());
                        return Err(ActionError::Unavailable);
                    }
                };
                // A name that is not UTF-8 cannot be a result value.
                results.retain(|network| network.ssid.as_str().is_some());
                debug!(target: "wifi", "Scan action returns results (count={})", results.len());
                if results.len() <= MAX_RECORDS {
                    return write_networks(out, &results);
                }
                let total = u16::try_from(results.len()).map_err(|_| ActionError::Internal)?;
                let id = self.with_scan(|slot| {
                    slot.expire(self.now_ms());
                    if slot.id != 0 {
                        return None;
                    }
                    let id = slot.next_id;
                    slot.next_id = id.checked_add(1).unwrap_or(1);
                    slot.id = id;
                    slot.deadline_ms = self.now_ms() + SCAN_HOLD_MS;
                    slot.networks = results;
                    Some(id)
                });
                let Some(id) = id else {
                    report::scan_busy();
                    return Err(ActionError::Busy);
                };
                out.snapshot(SnapshotInfo { id, total })
                    .map_err(|_| ActionError::Internal)
            }
            action::WIFI_RECONNECT => self.handle.reconnect().map_err(action_error),
            action::WIFI_DISCONNECT => self.handle.disconnect().map_err(action_error),
            _ => {
                report::unknown_action(action);
                Err(ActionError::UnknownAction)
            }
        }
    }

    async fn read_records(
        &self,
        action: u32,
        snapshot: u32,
        from: u32,
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        if action != action::WIFI_SCAN {
            return Err(ActionError::UnknownSnapshot);
        }
        let now = self.now_ms();
        self.with_scan(|slot| {
            slot.expire(now);
            if slot.id == 0 || slot.id != snapshot {
                return Err(ActionError::UnknownSnapshot);
            }
            // A reader keeps the result alive.
            slot.deadline_ms = now + SCAN_HOLD_MS;
            let from = usize::try_from(from).map_err(|_| ActionError::InvalidArgument)?;
            let networks = slot.networks.get(from..).unwrap_or_default();
            write_networks(out, &networks[..networks.len().min(MAX_RECORDS)])
        })
    }

    async fn close_records(&self, snapshot: u32) -> Result<(), ActionError> {
        self.with_scan(|slot| {
            if slot.id == 0 || slot.id != snapshot {
                return Err(ActionError::UnknownSnapshot);
            }
            slot.release();
            Ok(())
        })
    }

    fn link_lost(&self) {
        self.with_scan(ScanSlot::release);
    }
}

// Writes `networks` as records: as many as `out` takes.
fn write_networks(
    out: &mut impl ActionOutput,
    networks: &[WifiNetwork],
) -> Result<(), ActionError> {
    for network in networks {
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
            .and_then(|()| out.variant(scan_field::SECURITY, security_value(network.security)));
        if written.is_err() {
            report::scan_output_failed();
            return Err(ActionError::Internal);
        }
    }
    Ok(())
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
    pub(super) fn scan_busy() {
        log::debug!(target: "wifi", "Scan refused: a scan result is held for a client");
    }

    #[cold]
    #[inline(never)]
    pub(super) fn scan_expired(id: u32) {
        log::debug!(target: "wifi", "Scan result released after its deadline (id={})", id);
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
    use rohi_service::{
        ActionRegistry, ActionResult, MAX_FIELDS, MAX_ISSUE_DESCRIPTORS, MAX_SERVICE_ENUMS,
    };

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

    #[derive(Clone, Copy)]
    struct Uptime;

    impl Clock for Uptime {
        fn now(&self) -> MonotonicTimestamp {
            MonotonicTimestamp::from_millis(0)
        }
    }

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
    type Service<'a, P> = WirelessService<'a, NoopRawMutex, NoDelay, Uptime, P, &'a Ble>;

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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, FULL, &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, FULL, &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, FULL, &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
        assert_eq!(health(&service), ServiceHealth::Error);
        assert_eq!(issue_codes(&service), [issue::AUTHENTICATION_FAILED]);
    }

    #[test]
    fn a_dhcp_failure_is_a_wireless_issue() {
        let ble = Ble::new(BleState::Advertising);
        let shared = Shared::new();
        settle(&shared, Some(WifiError::DhcpFailed));
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
        assert_eq!(issue_codes(&service), [issue::DHCP_FAILED]);
    }

    #[test]
    fn a_failed_ble_stack_is_an_issue_and_fails_the_service() {
        let ble = Ble::new(BleState::Error(BleError::BufferTooSmall));
        let shared = Shared::new();
        settle(&shared, None);
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
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
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
        assert_eq!(issue_codes(&service), [issue::DRIVER_ERROR]);

        // Different codes are both listed.
        let ble = Ble::new(BleState::Error(BleError::Busy));
        let service = WirelessService::new(shared.handle(), NoDelay, Uptime, (), &ble);
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
        let service = WirelessService::new(handle, NoDelay, Uptime, (), &ble);
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

    const WIRELESS: u32 = rohi_service::service_id("wireless");

    /// A radio whose scans find `networks`; it counts them.
    struct Scanner<'a> {
        networks: WifiScanResults,
        scans: &'a Cell<u32>,
    }

    impl WifiBackend for Scanner<'_> {
        async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
            self.scans.set(self.scans.get() + 1);
            Ok(self.networks.clone())
        }
        async fn connect(&mut self, _: &LinkConfig) -> Result<NetworkInfo, WifiError> {
            Err(WifiError::Timeout)
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

    /// A clock the test moves.
    struct Clk<'a>(&'a Cell<u64>);

    impl Clock for Clk<'_> {
        fn now(&self) -> MonotonicTimestamp {
            MonotonicTimestamp::from_millis(self.0.get())
        }
    }

    // `count` networks, strongest first: `net00` is the strongest.
    fn networks(count: usize) -> WifiScanResults {
        let mut collector = crate::ScanCollector::new();
        for index in 0..count {
            collector.insert(WifiNetwork {
                ssid: WifiSsid::from_bytes(std::format!("net{index:02}").as_bytes()).unwrap(),
                rssi_dbm: -40 - index as i8,
                security: WifiSecurity::Wpa2,
                channel: 1 + index as u8,
            });
        }
        collector.finish()
    }

    fn ssids(records: &[heapless::Vec<(u32, Value), 4>]) -> std::vec::Vec<std::string::String> {
        records
            .iter()
            .map(|record| match &record[0] {
                (id, Value::String(ssid)) if *id == scan_field::SSID => ssid.as_str().into(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    // Everything a scan test needs: one service over a radio that finds
    // `networks`, driven by `manager`.
    struct Rig<'a> {
        service: WirelessService<'a, NoopRawMutex, NoDelay, Clk<'a>, (), &'a Ble>,
        manager: WifiManager<'a, Scanner<'a>, NoDelay, NoopRawMutex>,
    }

    fn rig<'a>(
        shared: &'a Shared,
        ble: &'a Ble,
        scans: &'a Cell<u32>,
        clock: &'a Cell<u64>,
        networks: WifiScanResults,
    ) -> Rig<'a> {
        Rig {
            service: WirelessService::new(shared.handle(), NoDelay, Clk(clock), (), ble),
            manager: WifiManager::new(
                Scanner { networks, scans },
                NoDelay,
                shared,
                ManagerConfig::DEFAULT,
            ),
        }
    }

    impl Rig<'_> {
        // One `wifi_scan`: the first page of its result.
        fn scan(&mut self) -> Result<ActionResult, ActionError> {
            let registry = (&self.service,);
            let (result, ()) = block_on(embassy_futures::join::join(
                registry.invoke(WIRELESS, action::WIFI_SCAN, &[]),
                self.manager.step(),
            ));
            result
        }

        // A scan that is refused before the radio is asked: nothing drives the
        // manager, which would wait for a command that never comes.
        fn scan_refused(&self) -> Result<ActionResult, ActionError> {
            let registry = (&self.service,);
            block_on(registry.invoke(WIRELESS, action::WIFI_SCAN, &[]))
        }

        fn read(&self, snapshot: u32, from: u32) -> Result<rohi_service::Records, ActionError> {
            let registry = (&self.service,);
            block_on(registry.read_records(WIRELESS, action::WIFI_SCAN, snapshot, from))
        }

        fn close(&self, snapshot: u32) -> Result<(), ActionError> {
            let registry = (&self.service,);
            block_on(registry.close_records(WIRELESS, snapshot))
        }
    }

    #[test]
    fn a_scan_of_sixteen_networks_is_one_scan_read_in_three_pages() {
        let (ble, shared) = (Ble::new(BleState::Advertising), Shared::new());
        let (scans, clock) = (Cell::new(0), Cell::new(0));
        let mut rig = rig(&shared, &ble, &scans, &clock, networks(16));
        let Ok(ActionResult::Records(first)) = rig.scan() else {
            panic!("not records");
        };
        let snapshot = first.snapshot.expect("a result longer than a page is held");
        assert_eq!(snapshot.total, 16);
        let mut seen = ssids(&first.records);
        assert_eq!(seen.len(), MAX_RECORDS);
        // The later pages are the same scan: 6 + 6 + 4, nothing dropped.
        let second = rig.read(snapshot.id, 6).unwrap();
        let third = rig.read(snapshot.id, 12).unwrap();
        assert_eq!((second.len(), third.len()), (6, 4));
        seen.extend(ssids(&second));
        seen.extend(ssids(&third));
        let expected: std::vec::Vec<_> = (0..16).map(|i| std::format!("net{i:02}")).collect();
        assert_eq!(seen, expected);
        assert_eq!(scans.get(), 1);
        // A page is the same every time it is read.
        assert_eq!(rig.read(snapshot.id, 6).unwrap(), second);
        assert_eq!(scans.get(), 1);
        // The record carries every field.
        assert_eq!(first.records[1].len(), 4);
        assert_eq!(first.records[1][2], (scan_field::CHANNEL, Value::U64(2)));
        rig.close(snapshot.id).unwrap();
        assert_eq!(rig.read(snapshot.id, 6), Err(ActionError::UnknownSnapshot));
        assert_eq!(rig.close(snapshot.id), Err(ActionError::UnknownSnapshot));
    }

    #[test]
    fn a_short_scan_holds_nothing_and_no_networks_is_an_empty_collection() {
        let (ble, shared) = (Ble::new(BleState::Advertising), Shared::new());
        let (scans, clock) = (Cell::new(0), Cell::new(0));
        let mut rig = rig(&shared, &ble, &scans, &clock, networks(MAX_RECORDS));
        let Ok(ActionResult::Records(page)) = rig.scan() else {
            panic!("not records");
        };
        assert_eq!((page.records.len(), page.snapshot), (MAX_RECORDS, None));
        // Nothing is held: the next scan is free to run.
        assert!(rig.scan().is_ok());
        assert_eq!(scans.get(), 2);

        let mut rig = self::rig(&shared, &ble, &scans, &clock, networks(0));
        let Ok(ActionResult::Records(page)) = rig.scan() else {
            panic!("not records");
        };
        assert!(page.records.is_empty() && page.snapshot.is_none());
    }

    #[test]
    fn a_name_that_is_not_utf8_is_not_a_record_and_leaves_no_gap() {
        let (ble, shared) = (Ble::new(BleState::Advertising), Shared::new());
        let (scans, clock) = (Cell::new(0), Cell::new(0));
        let mut found = networks(8);
        found[3].ssid = WifiSsid::from_bytes(&[0xff, 0xfe]).unwrap();
        let mut rig = rig(&shared, &ble, &scans, &clock, found);
        let Ok(ActionResult::Records(first)) = rig.scan() else {
            panic!("not records");
        };
        let snapshot = first.snapshot.unwrap();
        // Seven networks are listed: the pages add up to the total.
        assert_eq!(snapshot.total, 7);
        let rest = rig.read(snapshot.id, 6).unwrap();
        assert_eq!(first.records.len() + rest.len(), 7);
        assert!(!ssids(&first.records).contains(&"net03".into()));
    }

    #[test]
    fn a_held_scan_is_one_at_a_time_and_expires_without_a_reader() {
        let (ble, shared) = (Ble::new(BleState::Advertising), Shared::new());
        let (scans, clock) = (Cell::new(0), Cell::new(0));
        let mut rig = rig(&shared, &ble, &scans, &clock, networks(10));
        let Ok(ActionResult::Records(first)) = rig.scan() else {
            panic!("not records");
        };
        let held = first.snapshot.unwrap().id;
        // Another scan is refused, and does not scan.
        assert_eq!(rig.scan_refused(), Err(ActionError::Busy));
        assert_eq!(scans.get(), 1);
        // A reader keeps it alive.
        clock.set(SCAN_HOLD_MS - 1);
        assert!(rig.read(held, 6).is_ok());
        clock.set(2 * SCAN_HOLD_MS - 2);
        assert!(rig.read(held, 6).is_ok());
        // Without one it is released after the hold, and its id is gone.
        clock.set(3 * SCAN_HOLD_MS);
        assert_eq!(rig.read(held, 6), Err(ActionError::UnknownSnapshot));
        let Ok(ActionResult::Records(next)) = rig.scan() else {
            panic!("not records");
        };
        assert_ne!(next.snapshot.unwrap().id, held);
        assert_eq!(rig.read(held, 6), Err(ActionError::UnknownSnapshot));
        assert_eq!(scans.get(), 2);
    }

    #[test]
    fn a_lost_link_releases_the_held_scan() {
        let (ble, shared) = (Ble::new(BleState::Advertising), Shared::new());
        let (scans, clock) = (Cell::new(0), Cell::new(0));
        let mut rig = rig(&shared, &ble, &scans, &clock, networks(10));
        let Ok(ActionResult::Records(first)) = rig.scan() else {
            panic!("not records");
        };
        (&rig.service,).link_lost();
        assert_eq!(
            rig.read(first.snapshot.unwrap().id, 6),
            Err(ActionError::UnknownSnapshot)
        );
        assert!(rig.scan().is_ok());
    }

    #[test]
    fn only_a_scan_has_a_result_and_a_snapshot_of_another_action_is_unknown() {
        let (ble, shared) = (Ble::new(BleState::Advertising), Shared::new());
        let (scans, clock) = (Cell::new(0), Cell::new(0));
        let rig = rig(&shared, &ble, &scans, &clock, networks(10));
        let registry = (&rig.service,);
        let id = WIRELESS;
        for action in [action::WIFI_RECONNECT, action::WIFI_DISCONNECT, 99] {
            assert_eq!(
                block_on(registry.read_records(id, action, 1, 0)),
                Err(ActionError::UnknownSnapshot)
            );
        }
        // The two commands have no result at all.
        for action in [action::WIFI_RECONNECT, action::WIFI_DISCONNECT] {
            assert_eq!(
                <Service<'static, ()> as Action>::ACTIONS
                    .iter()
                    .find(|descriptor| descriptor.id == action)
                    .unwrap()
                    .format,
                ResultFormat::Empty
            );
        }
        assert_eq!(
            <Service<'static, ()> as Action>::ACTIONS[0].format,
            ResultFormat::Records
        );
    }

    #[test]
    fn the_service_identity_is_part_of_the_wire_abi() {
        assert_eq!(
            <Service<'static, ()> as rohi_service::Service>::NAME,
            "wireless"
        );
    }
}
