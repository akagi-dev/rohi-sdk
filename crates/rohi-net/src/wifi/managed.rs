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
//! The Wi-Fi configuration and status in the generic management plane.

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::{String, Vec};
use rohi_service::{
    ActionDescriptor, ActionError, ActionOutput, ConfigField, FieldDescriptor, FieldError, Issue,
    IssueSeverity, MAX_ISSUES, ManagedConfig, ManagedService, ServiceHealth, ServiceState,
    ServiceSummary, StatusWriter, Value, ValueKind, WriteError,
};

use super::{WifiConfig, WifiError, WifiHandle, WifiPassword, WifiSecurity, WifiSsid, WifiState};

/// Field ids of the `wifi` config section.
pub mod field {
    /// Network name; text only.
    pub const SSID: u32 = 1;
    /// Write-only passphrase; unset for an open network.
    pub const PASSWORD: u32 = 2;
}

impl ManagedConfig for WifiConfig {
    const NAME: &'static str = "wifi";
    const VERSION: u16 = 1;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::SSID, "ssid", ValueKind::String)
            .describe("Network name, 1 to 32 bytes"),
        ConfigField::new(field::PASSWORD, "password", ValueKind::String)
            .describe("Passphrase, 8 to 64 bytes; unset for an open network")
            .secret()
            .optional(),
    ];

    fn defaults() -> Self {
        Self::new(WifiSsid::empty(), None)
    }

    /// An SSID or password that is not UTF-8 cannot be expressed as text and
    /// is reported as invalid; it is not shown, but it keeps working.
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        match id {
            field::SSID => self
                .ssid
                .as_str()
                .and_then(Value::text)
                .map(Some)
                .ok_or(FieldError::Invalid),
            field::PASSWORD => match &self.password {
                None => Ok(None),
                Some(password) => core::str::from_utf8(password.expose_secret())
                    .ok()
                    .and_then(Value::text)
                    .map(Some)
                    .ok_or(FieldError::Invalid),
            },
            _ => Err(FieldError::UnknownField),
        }
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        match id {
            field::SSID => {
                self.ssid = WifiSsid::from_bytes(text.as_bytes()).ok_or(FieldError::Invalid)?;
            }
            field::PASSWORD => {
                self.password =
                    Some(WifiPassword::from_bytes(text.as_bytes()).ok_or(FieldError::Invalid)?);
            }
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::PASSWORD => {
                self.password = None;
                Ok(())
            }
            field::SSID => Err(FieldError::NotOptional),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        WifiConfig::validate(self).map_err(|_| FieldError::Invalid)
    }
}

/// Field ids of the `wifi` service status.
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

/// The Wi-Fi manager as a `wifi` service: status plus the `scan`, `reconnect`
/// and `disconnect` actions.
///
/// A scan is bounded by `scan_timeout_ms`, measured with `delay`.
pub struct WifiService<'a, M: RawMutex, D> {
    handle: WifiHandle<'a, M>,
    delay: D,
    scan_timeout_ms: u32,
}

impl<'a, M: RawMutex, D: DelayNs + Clone> WifiService<'a, M, D> {
    /// The service of the manager behind `handle`.
    pub const fn new(handle: WifiHandle<'a, M>, delay: D, scan_timeout_ms: u32) -> Self {
        Self {
            handle,
            delay,
            scan_timeout_ms,
        }
    }
}

impl<M: RawMutex, D: DelayNs + Clone> ManagedService for WifiService<'_, M, D> {
    const NAME: &'static str = "wifi";
    const VERSION: u16 = 1;
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
        let Some(info) = self.handle.network_info() else {
            return Ok(());
        };
        if let Some(ssid) = info.ssid.as_str() {
            out.text(status_field::SSID, ssid)?;
        }
        out.i64(status_field::RSSI, i64::from(info.rssi_dbm))?;
        let mut text = String::<16>::new();
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
                let timeout = delay.delay_ms(self.scan_timeout_ms);
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
    use rohi_service::StatusWriter;

    use super::*;
    use crate::WifiShared;

    #[derive(Clone)]
    struct NoDelay;

    impl embedded_hal_async::delay::DelayNs for NoDelay {
        async fn delay_ns(&mut self, _: u32) {
            core::future::pending::<()>().await;
        }
    }

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn fields_round_trip_and_the_password_is_optional() {
        let mut config = WifiConfig::defaults();
        assert!(config.validate().is_err());
        config.set_field(field::SSID, text("office")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        assert_eq!(config.read_field(field::PASSWORD), Ok(None));

        config
            .set_field(field::PASSWORD, text("hunter2hunter2"))
            .unwrap();
        assert_eq!(
            config.read_field(field::PASSWORD),
            Ok(Some(text("hunter2hunter2")))
        );
        assert_eq!(config.read_field(field::SSID), Ok(Some(text("office"))));
        config.unset_field(field::PASSWORD).unwrap();
        assert_eq!(config.password, None);
        assert_eq!(
            config.unset_field(field::SSID),
            Err(FieldError::NotOptional)
        );
    }

    #[test]
    fn the_complete_section_is_validated() {
        let mut config = WifiConfig::defaults();
        config.set_field(field::SSID, text("office")).unwrap();
        config.set_field(field::PASSWORD, text("short")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));
        let long = text(&"a".repeat(33));
        assert_eq!(
            config.set_field(field::SSID, long),
            Err(FieldError::Invalid)
        );
    }

    #[test]
    fn the_password_is_a_secret_field() {
        let secret: std::vec::Vec<_> = WifiConfig::FIELDS
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.field.name)
            .collect();
        assert_eq!(secret, ["password"]);
    }

    #[test]
    fn actions_are_declared_with_their_privileges() {
        type Service<'a> = WifiService<'a, NoopRawMutex, NoDelay>;
        let actions = <Service<'static> as ManagedService>::ACTIONS;
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
        use rohi_service::ActionOutput;

        struct Out;
        impl StatusWriter for Out {
            fn put(&mut self, _: u32, _: Value) -> Result<(), WriteError> {
                Ok(())
            }
        }
        impl ActionOutput for Out {
            fn next_record(&mut self) -> Result<(), WriteError> {
                Ok(())
            }
        }
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = WifiService::new(shared.handle(), NoDelay, 1_000);
        assert_eq!(block_on(service.invoke("reconnect", &[], &mut Out)), Ok(()));
        assert_eq!(
            block_on(service.invoke("explode", &[], &mut Out)),
            Err(ActionError::UnknownAction)
        );
    }

    #[test]
    fn a_new_manager_is_a_disabled_service_without_details() {
        let shared = WifiShared::<NoopRawMutex>::new();
        let service = WifiService::new(shared.handle(), NoDelay, 1_000);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Disabled);
        assert!(!summary.ready);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert!(issues.is_empty());

        struct Sink(usize);
        impl StatusWriter for Sink {
            fn put(&mut self, _: u32, _: Value) -> Result<(), WriteError> {
                self.0 += 1;
                Ok(())
            }
        }
        let mut sink = Sink(0);
        block_on(service.status(&mut sink)).unwrap();
        assert_eq!(sink.0, 0);
    }
}
