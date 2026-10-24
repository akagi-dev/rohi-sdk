use core::cell::RefCell;
use core::net::Ipv4Addr;
use std::vec::Vec;

use embassy_futures::block_on;
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_hal::units::{Celsius, Pascal, Ppm, RelativeHumidity};
use rohi_hal::{
    Bmx280Update, MeasurementStore, MeasurementUpdate, MeasurementsSnapshot, MonotonicTimestamp,
    Scd4xUpdate, SensorErrorCode, SensorEvent, SensorKind, SensorState,
};
use rohi_identity::{Identity, PublicKey};
use rohi_net::{
    NetworkInfo, WifiConfig, WifiNetwork, WifiPassword, WifiScanResults, WifiSecurity, WifiSsid,
    WifiState, WifiStatus,
};

use crate::{
    Capabilities, ControlServices, Controller, DeviceInfo, DeviceTime, LinkSecurity, OpenPolicy,
    RequireAuthenticated, RequireEncryption, ServiceError,
};

fn ts(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn ssid(value: &str) -> WifiSsid {
    WifiSsid::from_bytes(value.as_bytes()).unwrap()
}

fn password(value: &str) -> WifiPassword {
    WifiPassword::from_bytes(value.as_bytes()).unwrap()
}

/// In-memory services recording the order of mutations.
struct Fake {
    capabilities: Capabilities,
    snapshot: MeasurementsSnapshot,
    config: RefCell<Option<WifiConfig>>,
    log: RefCell<Vec<&'static str>>,
    status: WifiStatus,
    info: Option<NetworkInfo>,
    scan: WifiScanResults,
    set_fails: bool,
    identity: RefCell<Option<Identity>>,
    time: DeviceTime,
    store_identity_fails: bool,
}

impl Fake {
    fn new() -> Self {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::StateChanged {
            sensor: SensorKind::Bmx280,
            state: SensorState::Ready,
            timestamp: ts(900),
        });
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
            Bmx280Update::bme280(
                Celsius(22.5),
                RelativeHumidity(41.0),
                Pascal(101_325.0),
                ts(1_000),
            ),
        )));
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Scd4x(
            Scd4xUpdate {
                co2: Ppm(640.0),
                temperature: Celsius(23.0),
                humidity: RelativeHumidity(40.0),
                timestamp: ts(1_500),
            },
        )));
        store.apply(SensorEvent::Error {
            sensor: SensorKind::Scd4x,
            error: SensorErrorCode::Crc,
            timestamp: ts(1_600),
        });
        Self {
            capabilities: Capabilities::ALL,
            snapshot: store.snapshot(ts(4_000)),
            config: RefCell::new(None),
            log: RefCell::new(Vec::new()),
            status: WifiStatus::new(WifiState::NotConfigured),
            info: None,
            scan: WifiScanResults::new(),
            set_fails: false,
            identity: RefCell::new(None),
            time: DeviceTime::UNSYNCHRONIZED,
            store_identity_fails: false,
        }
    }
}

impl ControlServices for &Fake {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            firmware: "fake-1.2.3",
            board: "bench-esp32c3",
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn measurements(&self) -> Result<MeasurementsSnapshot, ServiceError> {
        Ok(self.snapshot)
    }

    async fn wifi_config(&self) -> Result<Option<WifiConfig>, ServiceError> {
        Ok(self.config.borrow().clone())
    }

    async fn set_wifi_config(&self, config: Option<&WifiConfig>) -> Result<(), ServiceError> {
        self.log.borrow_mut().push("set");
        if self.set_fails {
            return Err(ServiceError::Internal);
        }
        *self.config.borrow_mut() = config.cloned();
        Ok(())
    }

    async fn wifi_scan(&self) -> Result<WifiScanResults, ServiceError> {
        Ok(self.scan.clone())
    }

    fn wifi_status(&self) -> WifiStatus {
        self.status
    }

    fn wifi_network_info(&self) -> Option<NetworkInfo> {
        self.info.clone()
    }

    fn wifi_reconnect(&self) -> Result<(), ServiceError> {
        self.log.borrow_mut().push("reconnect");
        Ok(())
    }

    fn wifi_disconnect(&self) -> Result<(), ServiceError> {
        self.log.borrow_mut().push("disconnect");
        Err(ServiceError::Busy)
    }

    fn time_status(&self) -> DeviceTime {
        self.time
    }

    async fn identity(&self) -> Result<Option<PublicKey>, ServiceError> {
        Ok(self.identity.borrow().as_ref().map(Identity::public_key))
    }

    async fn store_identity(&self, identity: &Identity) -> Result<(), ServiceError> {
        self.log.borrow_mut().push("store_identity");
        if self.store_identity_fails {
            return Err(ServiceError::Unavailable);
        }
        *self.identity.borrow_mut() = Some(identity.clone());
        Ok(())
    }

    async fn erase_identity(&self) -> Result<(), ServiceError> {
        self.log.borrow_mut().push("erase_identity");
        *self.identity.borrow_mut() = None;
        Ok(())
    }
}

const SECURE: LinkSecurity = LinkSecurity {
    encrypted: true,
    authenticated: true,
};
const PLAIN: LinkSecurity = LinkSecurity {
    encrypted: false,
    authenticated: false,
};

fn request(id: u32, call: Call) -> pb::Request {
    pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: id,
        request: Some(call),
    }
}

fn send<P: crate::AuthorizationPolicy>(
    controller: &Controller<&Fake, P>,
    link: &LinkSecurity,
    request: &pb::Request,
) -> pb::Response {
    let mut wire = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(request, &mut wire).unwrap();
    call_raw(controller, link, &wire[..len])
}

fn call_raw<P: crate::AuthorizationPolicy>(
    controller: &Controller<&Fake, P>,
    link: &LinkSecurity,
    bytes: &[u8],
) -> pb::Response {
    let mut out = [0u8; limits::MAX_RESPONSE_LEN];
    let len = block_on(controller.handle(link, bytes, &mut out));
    assert!(len > 0);
    decode(&out[..len]).unwrap()
}

fn error_code(response: &pb::Response) -> pb::ErrorCode {
    match &response.response {
        Some(Reply::Error(error)) => error.code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn update(ssid_value: Option<&str>, pw: Option<Option<&str>>) -> pb::Request {
    let mut update = pb::WifiConfigUpdate::default();
    if let Some(value) = ssid_value {
        update.set_ssid(heapless::Vec::from_slice(value.as_bytes()).unwrap());
    }
    match pw {
        Some(Some(value)) => {
            update.password_update = Some(pb::WifiConfigUpdate_::PasswordUpdate::Password(
                heapless::Vec::from_slice(value.as_bytes()).unwrap(),
            ));
        }
        Some(None) => {
            update.password_update = Some(pb::WifiConfigUpdate_::PasswordUpdate::ClearPassword(
                pb::Empty {},
            ));
        }
        None => {}
    }
    request(
        9,
        Call::UpdateConfig(pb::UpdateConfigRequest {
            wifi_update: Some(pb::UpdateConfigRequest_::WifiUpdate::UpdateWifi(update)),
        }),
    )
}

#[test]
fn get_info_echoes_request_id_and_version() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(42, Call::GetInfo(pb::GetInfoRequest {})),
    );
    assert_eq!(response.request_id, 42);
    assert_eq!(response.protocol_version, 1);
    let Some(Reply::GetInfo(info)) = response.response else {
        panic!("not info");
    };
    assert_eq!(info.firmware.as_str(), "fake-1.2.3");
    assert_eq!(info.board.as_str(), "bench-esp32c3");
}

#[test]
fn capabilities_are_listed() {
    let mut fake = Fake::new();
    fake.capabilities = Capabilities::SENSORS | Capabilities::WIFI_STATUS;
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetCapabilities(pb::GetCapabilitiesRequest {})),
    );
    let Some(Reply::GetCapabilities(caps)) = response.response else {
        panic!("not capabilities");
    };
    assert_eq!(
        caps.capabilities.as_slice(),
        &[pb::Capability::Sensors, pb::Capability::WifiStatus]
    );
}

#[test]
fn missing_capability_is_not_supported() {
    let mut fake = Fake::new();
    fake.capabilities = Capabilities::SENSORS;
    let controller = Controller::new(&fake, OpenPolicy);
    for call in [
        Call::GetConfig(pb::GetConfigRequest {}),
        Call::ScanWifi(pb::ScanWifiRequest {}),
        Call::ReconnectWifi(pb::ReconnectWifiRequest {}),
    ] {
        let response = send(&controller, &SECURE, &request(5, call));
        assert_eq!(error_code(&response), pb::ErrorCode::NotSupported);
        assert_eq!(response.request_id, 5);
    }
    assert!(fake.log.borrow().is_empty());
}

#[test]
fn unknown_request_is_a_typed_error() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    // Request type 99 from a future protocol revision.
    let response = call_raw(
        &controller,
        &PLAIN,
        &[0x08, 0x01, 0x10, 0x09, 0x9a, 0x06, 0x00],
    );
    assert_eq!(error_code(&response), pb::ErrorCode::NotSupported);
    assert_eq!(response.request_id, 9);
}

#[test]
fn malformed_request_is_invalid_argument() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = call_raw(&controller, &PLAIN, &[0x08, 0x01, 0x10]);
    assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);
    assert_eq!(response.request_id, 0);

    let response = call_raw(&controller, &PLAIN, &[0xff; limits::MAX_REQUEST_LEN + 1]);
    assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);
}

#[test]
fn wrong_protocol_version_is_not_supported() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let mut message = request(3, Call::GetInfo(pb::GetInfoRequest {}));
    message.protocol_version = 2;
    let response = send(&controller, &PLAIN, &message);
    assert_eq!(error_code(&response), pb::ErrorCode::NotSupported);
    assert_eq!(response.request_id, 3);
    assert_eq!(response.protocol_version, 1);
}

#[test]
fn sensors_use_canonical_units_and_request_time_age() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetSensors(pb::GetSensorsRequest {})),
    );
    let Some(Reply::GetSensors(sensors)) = response.response else {
        panic!("not sensors");
    };
    assert_eq!(sensors.uptime_ms, 4_000);
    assert_eq!(sensors.sensors.len(), 2);

    let bmx = &sensors.sensors[0];
    assert_eq!(bmx.name.as_str(), "bmx280");
    assert_eq!(bmx.state, pb::SensorState::Ready);
    assert_eq!(bmx.error(), None);
    let kinds: Vec<_> = bmx.measurements.iter().map(|m| m.r#type).collect();
    assert_eq!(
        kinds,
        [
            pb::MeasurementType::Temperature,
            pb::MeasurementType::Humidity,
            pb::MeasurementType::Pressure
        ]
    );
    let temperature = &bmx.measurements[0];
    assert_eq!(temperature.value(), Some(&22.5));
    assert_eq!(temperature.age_ms(), Some(&3_000));
    assert_eq!(bmx.measurements[2].value(), Some(&101_325.0));

    let scd = &sensors.sensors[1];
    assert_eq!(scd.name.as_str(), "scd4x");
    assert_eq!(scd.error(), Some(&pb::SensorError::Crc));
    assert_eq!(scd.measurements[0].r#type, pb::MeasurementType::Co2);
    assert_eq!(scd.measurements[0].value(), Some(&640.0));
}

#[test]
fn never_measured_values_are_absent() {
    let mut store = MeasurementStore::new();
    store.apply(SensorEvent::StateChanged {
        sensor: SensorKind::Scd4x,
        state: SensorState::WarmingUp,
        timestamp: ts(1),
    });
    let snapshot = store.snapshot(ts(10));
    let sensors = crate::mapping::sensors(&snapshot);
    // A detected sensor without data keeps its status but has no values.
    assert_eq!(sensors.sensors.len(), 1);
    for sensor in &sensors.sensors {
        for measurement in &sensor.measurements {
            assert_eq!(measurement.value(), None);
            assert_eq!(measurement.age_ms(), None);
        }
    }
}

#[test]
fn config_never_returns_the_password() {
    let fake = Fake::new();
    *fake.config.borrow_mut() = Some(WifiConfig::new(
        ssid("office"),
        Some(password("s3cretpass")),
    ));
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetConfig(pb::GetConfigRequest {})),
    );

    let mut out = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut out).unwrap();
    assert!(!out[..len].windows(10).any(|w| w == b"s3cretpass"));

    let Some(Reply::GetConfig(config)) = response.response else {
        panic!("not config");
    };
    let wifi = config.wifi().unwrap();
    assert_eq!(wifi.ssid.as_slice(), b"office");
    assert!(wifi.password_configured);
}

#[test]
fn config_without_wifi_has_no_wifi_section() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetConfig(pb::GetConfigRequest {})),
    );
    let Some(Reply::GetConfig(config)) = response.response else {
        panic!("not config");
    };
    assert!(config.wifi().is_none());
}

#[test]
fn first_update_sets_the_config() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &SECURE,
        &update(Some("office"), Some(Some("hunter2hunter2"))),
    );
    assert!(matches!(response.response, Some(Reply::UpdateConfig(_))));
    assert_eq!(response.request_id, 9);
    assert_eq!(fake.log.borrow().as_slice(), ["set"]);
    let stored = fake.config.borrow().clone().unwrap();
    assert_eq!(stored.ssid.as_bytes(), b"office");
    assert!(stored.password.is_some());
}

#[test]
fn omitted_fields_are_preserved() {
    let fake = Fake::new();
    *fake.config.borrow_mut() = Some(WifiConfig::new(ssid("old"), Some(password("old-password"))));
    let controller = Controller::new(&fake, OpenPolicy);

    // Only the SSID changes: the password stays.
    let response = send(&controller, &SECURE, &update(Some("new"), None));
    assert!(matches!(response.response, Some(Reply::UpdateConfig(_))));
    let stored = fake.config.borrow().clone().unwrap();
    assert_eq!(stored.ssid.as_bytes(), b"new");
    assert_eq!(stored.password, Some(password("old-password")));

    // Only the password changes: the SSID stays.
    send(
        &controller,
        &SECURE,
        &update(None, Some(Some("another-password"))),
    );
    let stored = fake.config.borrow().clone().unwrap();
    assert_eq!(stored.ssid.as_bytes(), b"new");
    assert_eq!(stored.password, Some(password("another-password")));
}

#[test]
fn explicit_clear_removes_only_the_password() {
    let fake = Fake::new();
    *fake.config.borrow_mut() = Some(WifiConfig::new(
        ssid("cafe"),
        Some(password("old-password")),
    ));
    let controller = Controller::new(&fake, OpenPolicy);
    send(&controller, &SECURE, &update(None, Some(None)));
    let stored = fake.config.borrow().clone().unwrap();
    assert_eq!(stored.ssid.as_bytes(), b"cafe");
    assert_eq!(stored.password, None);
}

#[test]
fn clear_wifi_removes_the_section_and_stops_the_runtime() {
    let fake = Fake::new();
    *fake.config.borrow_mut() = Some(WifiConfig::new(ssid("cafe"), None));
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &SECURE,
        &request(
            2,
            Call::UpdateConfig(pb::UpdateConfigRequest {
                wifi_update: Some(pb::UpdateConfigRequest_::WifiUpdate::ClearWifi(
                    pb::Empty {},
                )),
            }),
        ),
    );
    assert!(matches!(response.response, Some(Reply::UpdateConfig(_))));
    assert_eq!(*fake.config.borrow(), None);
}

#[test]
fn invalid_updates_store_nothing() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);

    // No stored configuration and no SSID.
    let response = send(
        &controller,
        &SECURE,
        &update(None, Some(Some("hunter2hunter2"))),
    );
    assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);
    // Too short for WPA.
    let response = send(
        &controller,
        &SECURE,
        &update(Some("office"), Some(Some("short"))),
    );
    assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);
    // Empty SSID.
    let response = send(&controller, &SECURE, &update(Some(""), Some(None)));
    assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);

    assert!(fake.log.borrow().is_empty());
    assert_eq!(*fake.config.borrow(), None);
}

#[test]
fn failed_set_is_reported() {
    let mut fake = Fake::new();
    fake.set_fails = true;
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &SECURE, &update(Some("office"), Some(None)));
    assert_eq!(error_code(&response), pb::ErrorCode::Internal);
    assert_eq!(fake.log.borrow().as_slice(), ["set"]);
}

#[test]
fn wifi_status_reports_connection_details() {
    let mut fake = Fake::new();
    fake.status = WifiStatus::new(WifiState::Connected);
    fake.info = Some(NetworkInfo {
        ssid: ssid("office"),
        rssi_dbm: -61,
        ipv4: Ipv4Addr::new(192, 168, 1, 20),
        gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
        dns: None,
    });
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetWifiStatus(pb::GetWifiStatusRequest {})),
    );
    let Some(Reply::GetWifiStatus(status)) = response.response else {
        panic!("not status");
    };
    assert_eq!(status.state, pb::WifiState::Connected);
    assert_eq!(status.failure(), None);
    let connection = status.connection().unwrap();
    assert_eq!(connection.rssi_dbm, -61);
    assert_eq!(connection.ipv4, 0xc0a8_0114);
    assert_eq!(connection.gateway(), Some(&0xc0a8_0101));
    assert_eq!(connection.dns(), None);
}

#[test]
fn wifi_status_reports_the_failure() {
    let mut fake = Fake::new();
    fake.status = WifiStatus::failed(WifiState::Error, rohi_net::WifiError::AuthenticationFailed);
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetWifiStatus(pb::GetWifiStatusRequest {})),
    );
    let Some(Reply::GetWifiStatus(status)) = response.response else {
        panic!("not status");
    };
    assert_eq!(status.state, pb::WifiState::Error);
    assert_eq!(
        status.failure(),
        Some(&pb::WifiFailure::AuthenticationFailed)
    );
    assert!(status.connection().is_none());
}

#[test]
fn full_scan_fits_the_response_limit() {
    let mut fake = Fake::new();
    for i in 0..rohi_net::MAX_WIFI_SCAN_RESULTS {
        let name = std::format!("network-with-a-long-name-{i:02}-xxxx");
        fake.scan
            .push(WifiNetwork {
                ssid: WifiSsid::from_bytes(&name.as_bytes()[..32]).unwrap(),
                rssi_dbm: -90 + i as i8,
                security: WifiSecurity::Wpa2Wpa3,
                channel: 11,
            })
            .unwrap();
    }
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::ScanWifi(pb::ScanWifiRequest {})),
    );
    let Some(Reply::ScanWifi(scan)) = response.response else {
        panic!("not scan");
    };
    assert_eq!(scan.networks.len(), rohi_net::MAX_WIFI_SCAN_RESULTS);
    assert_eq!(scan.networks[0].security, pb::WifiSecurity::Wpa2Wpa3);
}

#[test]
fn every_sensor_fits_the_response_limit() {
    use rohi_hal::{
        Ags3871Measurements, Bmx280Measurements, Measurement, Measurements, NoiseMeasurements,
        RadSensMeasurements, Scd4xMeasurements, Sds011Measurements, SensorStatuses,
        Zmod4510Measurements,
    };
    let snapshot = MeasurementsSnapshot {
        measurements: Measurements {
            bmx280: Some(Bmx280Measurements {
                humidity: Some(Measurement::unavailable()),
                ..Default::default()
            }),
            scd4x: Some(Scd4xMeasurements::default()),
            sds011: Some(Sds011Measurements::default()),
            microphone: Some(NoiseMeasurements::default()),
            zmod4510: Some(Zmod4510Measurements::default()),
            ags3871: Some(Ags3871Measurements::default()),
            radsens: Some(RadSensMeasurements::default()),
        },
        sensor_statuses: SensorStatuses::new(),
        generated_at: ts(u64::MAX),
    };
    let mut fake = Fake::new();
    fake.snapshot = snapshot;
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetSensors(pb::GetSensorsRequest {})),
    );
    let Some(Reply::GetSensors(sensors)) = response.response else {
        panic!("not sensors");
    };
    assert_eq!(sensors.sensors.len(), SensorKind::COUNT);
}

#[test]
fn policy_gates_mutations_only() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, RequireEncryption);

    let read = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetConfig(pb::GetConfigRequest {})),
    );
    assert!(matches!(read.response, Some(Reply::GetConfig(_))));
    let scan = send(
        &controller,
        &PLAIN,
        &request(1, Call::ScanWifi(pb::ScanWifiRequest {})),
    );
    assert!(matches!(scan.response, Some(Reply::ScanWifi(_))));

    for call in [
        Call::ReconnectWifi(pb::ReconnectWifiRequest {}),
        Call::DisconnectWifi(pb::DisconnectWifiRequest {}),
    ] {
        let denied = send(&controller, &PLAIN, &request(7, call));
        assert_eq!(error_code(&denied), pb::ErrorCode::PermissionDenied);
        assert_eq!(denied.request_id, 7);
    }
    let denied = send(&controller, &PLAIN, &update(Some("office"), Some(None)));
    assert_eq!(error_code(&denied), pb::ErrorCode::PermissionDenied);
    assert!(fake.log.borrow().is_empty());

    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
    };
    let allowed = send(
        &controller,
        &encrypted,
        &request(7, Call::ReconnectWifi(pb::ReconnectWifiRequest {})),
    );
    assert!(matches!(allowed.response, Some(Reply::ReconnectWifi(_))));
    assert_eq!(fake.log.borrow().as_slice(), ["reconnect"]);
}

#[test]
fn authenticated_policy_needs_an_authenticated_link() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, RequireAuthenticated);
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
    };
    let denied = send(&controller, &encrypted, &update(Some("office"), Some(None)));
    assert_eq!(error_code(&denied), pb::ErrorCode::PermissionDenied);
    let allowed = send(&controller, &SECURE, &update(Some("office"), Some(None)));
    assert!(matches!(allowed.response, Some(Reply::UpdateConfig(_))));
}

#[test]
fn service_errors_map_to_protocol_errors() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    // The fake refuses `disconnect` with `Busy`.
    let response = send(
        &controller,
        &SECURE,
        &request(4, Call::DisconnectWifi(pb::DisconnectWifiRequest {})),
    );
    assert_eq!(error_code(&response), pb::ErrorCode::Busy);
    assert_eq!(response.request_id, 4);
}

#[test]
fn undersized_response_buffer_gets_an_error_not_a_panic() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let mut wire = [0u8; 32];
    let len = encode(
        &request(8, Call::GetSensors(pb::GetSensorsRequest {})),
        &mut wire,
    )
    .unwrap();

    let mut small = [0u8; 16];
    let n = block_on(controller.handle(&PLAIN, &wire[..len], &mut small));
    let response: pb::Response = decode(&small[..n]).unwrap();
    assert_eq!(error_code(&response), pb::ErrorCode::Internal);
    assert_eq!(response.request_id, 8);

    let mut tiny = [0u8; 2];
    assert_eq!(
        block_on(controller.handle(&PLAIN, &wire[..len], &mut tiny)),
        0
    );
}

#[test]
fn wifi_updates_need_the_wifi_control_capability() {
    let mut fake = Fake::new();
    fake.capabilities = Capabilities::CONFIG;
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &SECURE, &update(Some("office"), Some(None)));
    assert_eq!(error_code(&response), pb::ErrorCode::NotSupported);
    assert!(fake.log.borrow().is_empty());
}

const SECRET: [u8; 32] = [0x42; 32];

fn secret_key() -> Identity {
    Identity::from_secret(&SECRET).unwrap()
}

fn provision(secret: &[u8]) -> pb::Request {
    request(
        5,
        Call::ProvisionIdentity(pb::ProvisionIdentityRequest {
            ed25519_secret: heapless::Vec::from_slice(secret).unwrap(),
        }),
    )
}

fn get_identity() -> pb::Request {
    request(6, Call::GetIdentity(pb::GetIdentityRequest {}))
}

fn erase_identity() -> pb::Request {
    request(8, Call::EraseIdentity(pb::EraseIdentityRequest {}))
}

fn identity_info(response: pb::Response) -> (pb::IdentityState, Option<[u8; 32]>) {
    let Some(Reply::GetIdentity(out)) = response.response else {
        panic!("not an identity response");
    };
    let info = out.identity().expect("identity info");
    (
        info.state,
        info.public_key()
            .map(|key| key.as_slice().try_into().unwrap()),
    )
}

#[test]
fn identity_is_not_provisioned_by_default() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &PLAIN, &get_identity());
    assert_eq!(
        identity_info(response),
        (pb::IdentityState::NotProvisioned, None)
    );
}

#[test]
fn provisioning_stores_the_identity_and_returns_the_public_key() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &PLAIN, &provision(&SECRET));
    assert_eq!(response.request_id, 5);
    let Some(Reply::ProvisionIdentity(out)) = response.response else {
        panic!("not a provision response");
    };
    let expected = secret_key().public_key().to_bytes();
    assert_eq!(out.public_key.as_slice(), expected);
    assert_eq!(fake.log.borrow().as_slice(), ["store_identity"]);

    let state = identity_info(send(&controller, &PLAIN, &get_identity()));
    assert_eq!(state, (pb::IdentityState::Provisioned, Some(expected)));
}

#[test]
fn provisioning_twice_never_replaces_the_identity() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    send(&controller, &PLAIN, &provision(&SECRET));
    let other = [0x07; 32];
    let response = send(&controller, &PLAIN, &provision(&other));
    assert_eq!(
        error_code(&response),
        pb::ErrorCode::IdentityAlreadyProvisioned
    );
    assert_eq!(fake.log.borrow().as_slice(), ["store_identity"]);
    let (_, key) = identity_info(send(&controller, &PLAIN, &get_identity()));
    assert_eq!(key, Some(secret_key().public_key().to_bytes()));
}

#[test]
fn invalid_secret_length_stores_nothing() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    for secret in [&[][..], &[1; 16][..], &[1; 31][..]] {
        let response = send(&controller, &PLAIN, &provision(secret));
        assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);
    }
    assert!(fake.log.borrow().is_empty());
    assert!(fake.identity.borrow().is_none());

    // 33 bytes do not even fit the protocol bound.
    let mut wire = [0x5a; 35];
    wire[0] = 0x08;
    wire[1] = 0x01;
    wire[2] = 0xfa;
    wire[3] = 0x01;
    wire[4] = 31;
    wire[5] = 0x0a;
    wire[6] = 33;
    let response = call_raw(&controller, &PLAIN, &wire);
    assert_eq!(error_code(&response), pb::ErrorCode::InvalidArgument);
    assert!(fake.identity.borrow().is_none());
}

#[test]
fn storage_failure_is_reported_and_leaves_the_device_unprovisioned() {
    let mut fake = Fake::new();
    fake.store_identity_fails = true;
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &PLAIN, &provision(&SECRET));
    assert_eq!(error_code(&response), pb::ErrorCode::Unavailable);
    let (state, key) = identity_info(send(&controller, &PLAIN, &get_identity()));
    assert_eq!((state, key), (pb::IdentityState::NotProvisioned, None));
}

#[test]
fn erase_then_provision_again() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    send(&controller, &PLAIN, &provision(&SECRET));
    let erased = send(&controller, &PLAIN, &erase_identity());
    assert!(matches!(erased.response, Some(Reply::EraseIdentity(_))));
    assert_eq!(
        identity_info(send(&controller, &PLAIN, &get_identity())),
        (pb::IdentityState::NotProvisioned, None)
    );
    // Erasing nothing is fine.
    let again = send(&controller, &PLAIN, &erase_identity());
    assert!(matches!(again.response, Some(Reply::EraseIdentity(_))));

    let other = [0x07; 32];
    let response = send(&controller, &PLAIN, &provision(&other));
    let Some(Reply::ProvisionIdentity(out)) = response.response else {
        panic!("not a provision response");
    };
    let expected = Identity::from_secret(&other)
        .unwrap()
        .public_key()
        .to_bytes();
    assert_eq!(out.public_key.as_slice(), expected);
}

#[test]
fn identity_operations_need_the_identity_capability() {
    let mut fake = Fake::new();
    fake.capabilities = Capabilities::SENSORS | Capabilities::CONFIG | Capabilities::WIFI_CONTROL;
    let controller = Controller::new(&fake, OpenPolicy);
    for call in [get_identity(), provision(&SECRET), erase_identity()] {
        let response = send(&controller, &SECURE, &call);
        assert_eq!(error_code(&response), pb::ErrorCode::NotSupported);
    }
    assert!(fake.log.borrow().is_empty());
}

#[test]
fn provisioning_and_erase_are_gated_by_the_link_policy_reads_are_not() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, RequireEncryption);
    for call in [provision(&SECRET), erase_identity()] {
        let denied = send(&controller, &PLAIN, &call);
        assert_eq!(error_code(&denied), pb::ErrorCode::PermissionDenied);
    }
    assert!(fake.log.borrow().is_empty());
    assert!(matches!(
        send(&controller, &PLAIN, &get_identity()).response,
        Some(Reply::GetIdentity(_))
    ));

    let controller = Controller::new(&fake, RequireAuthenticated);
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
    };
    let denied = send(&controller, &encrypted, &provision(&SECRET));
    assert_eq!(error_code(&denied), pb::ErrorCode::PermissionDenied);
    let allowed = send(&controller, &SECURE, &provision(&SECRET));
    assert!(matches!(
        allowed.response,
        Some(Reply::ProvisionIdentity(_))
    ));
}

#[test]
fn no_response_contains_the_secret() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    for call in [
        provision(&SECRET),
        get_identity(),
        request(1, Call::GetConfig(pb::GetConfigRequest {})),
        erase_identity(),
        provision(&SECRET),
    ] {
        let mut wire = [0u8; limits::MAX_REQUEST_LEN];
        let len = encode(&call, &mut wire).unwrap();
        let mut out = [0u8; limits::MAX_RESPONSE_LEN];
        let n = block_on(controller.handle(&PLAIN, &wire[..len], &mut out));
        assert!(!out[..n].windows(8).any(|w| w == &SECRET[..8]));
    }
}

#[test]
fn generic_config_cannot_reach_the_identity() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    send(&controller, &PLAIN, &provision(&SECRET));
    let response = send(
        &controller,
        &PLAIN,
        &request(1, Call::GetConfig(pb::GetConfigRequest {})),
    );
    let Some(Reply::GetConfig(config)) = response.response else {
        panic!("not config");
    };
    assert!(config.wifi().is_none());

    let cleared = send(
        &controller,
        &PLAIN,
        &request(
            2,
            Call::UpdateConfig(pb::UpdateConfigRequest {
                wifi_update: Some(pb::UpdateConfigRequest_::WifiUpdate::ClearWifi(
                    pb::Empty {},
                )),
            }),
        ),
    );
    assert!(matches!(cleared.response, Some(Reply::UpdateConfig(_))));
    assert!(fake.identity.borrow().is_some());
}

fn get_time_status() -> pb::Request {
    request(1, Call::GetTimeStatus(pb::GetTimeStatusRequest {}))
}

#[test]
fn time_status_of_an_unsynchronized_device_has_no_time() {
    let fake = Fake::new();
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &PLAIN, &get_time_status());
    let Some(Reply::GetTimeStatus(time)) = response.response else {
        panic!("not time status");
    };
    assert_eq!(time.state, pb::TimeSyncState::Unsynchronized);
    assert_eq!(time.unix_time_ms(), None);
    assert_eq!(time.source(), None);
    assert_eq!(time.sync_age_ms(), None);
    assert_eq!(time.uncertainty_ms(), None);
    assert_eq!(time.last_failure(), None);
    assert_eq!(time.consecutive_failures, 0);
}

#[test]
fn time_status_reports_time_source_age_and_failure() {
    use core::time::Duration;
    use rohi_time::{SyncFailure, TimeSourceKind, TimeStatus, TimeSyncState, UnixTime};

    let mut fake = Fake::new();
    fake.time = DeviceTime {
        unix_time: Some(UnixTime::from_millis(1_780_000_000_250)),
        status: TimeStatus {
            state: TimeSyncState::Stale,
            source: Some(TimeSourceKind::Ntp),
            last_sync_age: Some(Duration::from_secs(7_200)),
            uncertainty: Some(Duration::from_millis(45)),
            consecutive_failures: 3,
            last_failure: Some(SyncFailure::Timeout),
        },
    };
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &PLAIN, &get_time_status());
    let Some(Reply::GetTimeStatus(time)) = response.response else {
        panic!("not time status");
    };
    assert_eq!(time.state, pb::TimeSyncState::Stale);
    assert_eq!(time.unix_time_ms(), Some(&1_780_000_000_250));
    assert_eq!(time.source(), Some(&pb::TimeSource::Ntp));
    assert_eq!(time.sync_age_ms(), Some(&7_200_000));
    assert_eq!(time.uncertainty_ms(), Some(&45));
    assert_eq!(time.consecutive_failures, 3);
    assert_eq!(time.last_failure(), Some(&pb::TimeSyncFailure::Timeout));
}

#[test]
fn time_status_needs_the_time_capability() {
    let mut fake = Fake::new();
    fake.capabilities = Capabilities::SENSORS | Capabilities::WIFI_STATUS;
    let controller = Controller::new(&fake, OpenPolicy);
    let response = send(&controller, &PLAIN, &get_time_status());
    assert_eq!(error_code(&response), pb::ErrorCode::NotSupported);
}
