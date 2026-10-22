//! End to end: the `rohi` client against the real dispatcher and BLE framing,
//! connected in memory. No hardware, no Bluetooth adapter.

use std::cell::RefCell;
use std::net::Ipv4Addr;
use std::time::Duration;

use rohi::model::*;
use rohi::{ClientTransport, Device, Error, Session};
use rohi_control::{
    Capabilities, ControlServices, Controller, DeviceInfo as ServiceInfo, OpenPolicy, ServiceError,
};
use rohi_control_ble::ControlBle;
use rohi_control_proto::control as pb;
use rohi_hal::units::{Celsius, Pascal, Ppm, RelativeHumidity};
use rohi_hal::{
    Bmx280Update, MeasurementStore, MeasurementUpdate, MeasurementsSnapshot, MonotonicTimestamp,
    Scd4xUpdate, SensorEvent,
};
use rohi_net::{
    BleConnectionInfo, BleError, BleState, BleTransport, NetworkInfo, WifiConfig as NetConfig,
    WifiNetwork as NetNetwork, WifiScanResults, WifiSecurity as NetSecurity, WifiSsid,
    WifiState as NetState, WifiStatus as NetStatus,
};
use tokio::sync::mpsc;

fn ts(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

// ---------------------------------------------------------------------------
// Device side
// ---------------------------------------------------------------------------

struct DeviceEnd {
    from_client: mpsc::UnboundedReceiver<Vec<u8>>,
    to_client: mpsc::UnboundedSender<Vec<u8>>,
    payload: usize,
}

impl BleTransport for DeviceEnd {
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
        match self.from_client.recv().await {
            Some(chunk) => {
                out[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
            None => std::future::pending().await,
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
        assert!(data.len() <= self.payload);
        self.to_client
            .send(data.to_vec())
            .map_err(|_| BleError::Disconnected)
    }

    fn connection_info(&self) -> Option<BleConnectionInfo> {
        Some(BleConnectionInfo::default())
    }

    fn max_payload(&self) -> usize {
        self.payload
    }

    fn state(&self) -> BleState {
        BleState::Connected(BleConnectionInfo::default())
    }

    fn connection_epoch(&self) -> u32 {
        1
    }
}

struct Services {
    capabilities: Capabilities,
    snapshot: MeasurementsSnapshot,
    config: RefCell<Option<NetConfig>>,
    requests: RefCell<Vec<&'static str>>,
}

impl Services {
    fn new() -> Self {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
            Bmx280Update::bme280(
                Celsius(21.5),
                RelativeHumidity(45.0),
                Pascal(100_800.0),
                ts(1_000),
            ),
        )));
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Scd4x(
            Scd4xUpdate {
                co2: Ppm(640.0),
                temperature: Celsius(22.0),
                humidity: RelativeHumidity(44.0),
                timestamp: ts(2_000),
            },
        )));
        Self {
            capabilities: Capabilities::ALL,
            snapshot: store.snapshot(ts(5_000)),
            config: RefCell::new(None),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl ControlServices for Services {
    fn device_info(&self) -> ServiceInfo {
        ServiceInfo {
            firmware: "fake",
            firmware_version: "1.0.0",
            board: "bench",
            chip: "esp32c3",
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn measurements(&self) -> Result<MeasurementsSnapshot, ServiceError> {
        self.requests.borrow_mut().push("sensors");
        Ok(self.snapshot)
    }

    async fn wifi_config(&self) -> Result<Option<NetConfig>, ServiceError> {
        Ok(*self.config.borrow())
    }

    async fn store_wifi_config(&self, config: Option<&NetConfig>) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("store");
        *self.config.borrow_mut() = config.copied();
        Ok(())
    }

    async fn apply_wifi_config(&self, _: Option<NetConfig>) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("apply");
        Ok(())
    }

    async fn wifi_scan(&self) -> Result<WifiScanResults, ServiceError> {
        let mut results = WifiScanResults::new();
        for i in 0..16u8 {
            let name = format!("network-number-{i:02}");
            results
                .push(NetNetwork {
                    ssid: WifiSsid::from_bytes(name.as_bytes()).unwrap(),
                    rssi_dbm: -40 - i as i8,
                    security: NetSecurity::Wpa2,
                    channel: 1 + i % 11,
                })
                .unwrap();
        }
        Ok(results)
    }

    fn wifi_status(&self) -> NetStatus {
        NetStatus::new(NetState::Connected)
    }

    fn wifi_network_info(&self) -> Option<NetworkInfo> {
        Some(NetworkInfo {
            ssid: WifiSsid::from_bytes(b"office").unwrap(),
            rssi_dbm: -55,
            ipv4: Ipv4Addr::new(10, 0, 0, 7),
            gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
            dns: None,
        })
    }

    fn wifi_reconnect(&self) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("reconnect");
        Ok(())
    }

    fn wifi_disconnect(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Busy)
    }
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

struct ClientEnd {
    to_device: mpsc::UnboundedSender<Vec<u8>>,
    from_device: mpsc::UnboundedReceiver<Vec<u8>>,
    payload: usize,
}

impl ClientTransport for ClientEnd {
    fn max_payload(&self) -> usize {
        self.payload
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.to_device
            .send(chunk.to_vec())
            .map_err(|_| Error::Transport("closed".into()))
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        self.from_device
            .recv()
            .await
            .ok_or_else(|| Error::Transport("closed".into()))
    }
}

fn link(payload: usize) -> (ClientEnd, DeviceEnd) {
    let (to_device, from_client) = mpsc::unbounded_channel();
    let (to_client, from_device) = mpsc::unbounded_channel();
    (
        ClientEnd {
            to_device,
            from_device,
            payload,
        },
        DeviceEnd {
            from_client,
            to_client,
            payload,
        },
    )
}

/// Runs `script` against a device that serves `services` until it finishes.
async fn with_device<F, Fut>(payload: usize, services: Services, script: F) -> Services
where
    F: FnOnce(Device<ClientEnd>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (client, device) = link(payload);
    let controller = Controller::new(services, OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    tokio::select! {
        () = async { server.run().await } => unreachable!(),
        () = script(Device::new(client)) => {}
    }
    drop(server);
    controller_into_services(controller)
}

fn controller_into_services(controller: Controller<Services, OpenPolicy>) -> Services {
    // The services only hold plain data; rebuild an equivalent for inspection.
    let services = controller.services();
    Services {
        capabilities: services.capabilities,
        snapshot: services.snapshot,
        config: RefCell::new(*services.config.borrow()),
        requests: RefCell::new(services.requests.borrow().clone()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn info_and_capabilities() {
    with_device(20, Services::new(), |mut device| async move {
        let info = device.info().await.unwrap();
        assert_eq!(info.firmware, "fake");
        assert_eq!(info.firmware_version, "1.0.0");
        assert_eq!(info.chip, "esp32c3");

        let capabilities = device.capabilities().await.unwrap();
        assert_eq!(
            capabilities,
            [
                Capability::Sensors,
                Capability::Config,
                Capability::WifiScan,
                Capability::WifiStatus,
                Capability::WifiControl
            ]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn sensors_come_back_in_canonical_units() {
    with_device(20, Services::new(), |mut device| async move {
        let sensors = device.sensors().await.unwrap();
        assert_eq!(sensors.uptime_ms, 5_000);
        assert_eq!(sensors.sensors.len(), 2);

        let bmx = &sensors.sensors[0];
        assert_eq!(bmx.name, "bmx280");
        let temperature = &bmx.measurements[0];
        assert_eq!(temperature.kind, MeasurementType::Temperature);
        assert_eq!(temperature.unit, "°C");
        assert_eq!(temperature.value, Some(21.5));
        assert_eq!(temperature.state, MeasurementState::Valid);
        assert_eq!(temperature.age_ms, Some(4_000));
        assert_eq!(bmx.measurements[2].unit, "Pa");
        assert_eq!(bmx.measurements[2].value, Some(100_800.0));

        let scd = &sensors.sensors[1];
        assert_eq!(scd.measurements[0].kind, MeasurementType::Co2);
        assert_eq!(scd.measurements[0].unit, "ppm");
        assert_eq!(scd.measurements[0].age_ms, Some(3_000));

        let text = rohi::format::sensors(&sensors);
        assert!(text.contains("bmx280"), "{text}");
        assert!(text.contains("21.5 °C"), "{text}");
        assert!(text.contains("100800 Pa"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_configuration_round_trip_never_exposes_the_password() {
    let services = with_device(20, Services::new(), |mut device| async move {
        assert_eq!(device.config().await.unwrap().wifi, None);

        device
            .update_wifi(&WifiUpdate {
                ssid: Some(b"office".to_vec()),
                password: PasswordChange::Set(b"hunter2hunter2".to_vec()),
            })
            .await
            .unwrap();
        let config = device.config().await.unwrap();
        let wifi = config.wifi.clone().unwrap();
        assert_eq!(wifi.ssid.to_text(), "office");
        assert!(wifi.password_configured);
        assert!(!format!("{config:?}").contains("hunter2"));
        assert!(!rohi::format::config(&config).contains("hunter2"));

        // Only the SSID changes: the password is kept.
        device
            .update_wifi(&WifiUpdate {
                ssid: Some(b"cafe".to_vec()),
                password: PasswordChange::Keep,
            })
            .await
            .unwrap();
        let wifi = device.config().await.unwrap().wifi.unwrap();
        assert_eq!(wifi.ssid.to_text(), "cafe");
        assert!(wifi.password_configured);

        // Explicit clear.
        device
            .update_wifi(&WifiUpdate {
                ssid: None,
                password: PasswordChange::Clear,
            })
            .await
            .unwrap();
        assert!(
            !device
                .config()
                .await
                .unwrap()
                .wifi
                .unwrap()
                .password_configured
        );

        device.forget_wifi().await.unwrap();
        assert_eq!(device.config().await.unwrap().wifi, None);

        // Invalid updates are typed errors.
        let error = device
            .update_wifi(&WifiUpdate {
                ssid: None,
                password: PasswordChange::Set(b"hunter2hunter2".to_vec()),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::InvalidArgument)));
        let error = device
            .update_wifi(&WifiUpdate {
                ssid: Some(vec![b'x'; 33]),
                password: PasswordChange::Keep,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, Error::InvalidInput(_)));
    })
    .await;
    // Each accepted update was persisted before it was applied.
    let requests = services.requests.borrow().clone();
    assert_eq!(
        requests,
        [
            "store", "apply", "store", "apply", "store", "apply", "store", "apply"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wifi_scan_status_and_control() {
    with_device(20, Services::new(), |mut device| async move {
        let networks = device.wifi_scan().await.unwrap();
        assert_eq!(networks.len(), 16);
        assert_eq!(networks[0].ssid.to_text(), "network-number-00");
        assert_eq!(networks[0].security, WifiSecurity::Wpa2);
        assert!(rohi::format::wifi_scan(&networks).contains("network-number-15"));

        let status = device.wifi_status().await.unwrap();
        assert_eq!(status.state, WifiState::Connected);
        let connection = status.connection.unwrap();
        assert_eq!(connection.ipv4, Ipv4Addr::new(10, 0, 0, 7));
        assert_eq!(connection.gateway, Some(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(connection.dns, None);

        device.wifi_reconnect().await.unwrap();
        let error = device.wifi_disconnect().await.unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::Busy)));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn works_with_a_large_mtu() {
    with_device(244, Services::new(), |mut device| async move {
        assert_eq!(device.wifi_scan().await.unwrap().len(), 16);
        assert_eq!(device.sensors().await.unwrap().sensors.len(), 2);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_capabilities_fail_without_sending_the_request() {
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS;
    let services = with_device(20, services, |mut device| async move {
        let error = device.wifi_scan().await.unwrap_err();
        assert!(matches!(error, Error::Unsupported(Capability::WifiScan)));
        assert!(matches!(
            device.config().await.unwrap_err(),
            Error::Unsupported(Capability::Config)
        ));
        assert_eq!(device.sensors().await.unwrap().sensors.len(), 2);
    })
    .await;
    assert_eq!(services.requests.borrow().as_slice(), ["sensors"]);
}

// ---------------------------------------------------------------------------
// Session behaviour
// ---------------------------------------------------------------------------

struct Silent;

impl ClientTransport for Silent {
    fn max_payload(&self) -> usize {
        20
    }

    async fn send(&mut self, _: &[u8]) -> rohi::Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        std::future::pending().await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn silent_device_times_out() {
    let mut session = Session::new(Silent);
    session.set_timeout(Duration::from_millis(50));
    let error = session
        .call(pb::Request_::Request::GetInfo(pb::GetInfoRequest {}))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Timeout));
}

/// Answers every request twice: first with a stale response of another
/// message, then with the right one.
struct StaleFirst {
    inner: ClientEnd,
    pending: Vec<Vec<u8>>,
}

impl ClientTransport for StaleFirst {
    fn max_payload(&self) -> usize {
        self.inner.max_payload()
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.inner.send(chunk).await
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        if let Some(chunk) = self.pending.pop() {
            return Ok(chunk);
        }
        let mut chunk = self.inner.recv().await?;
        // Pretend this is the end of an older message (id + 1).
        let mut stale = chunk.clone();
        stale[1] = stale[1].wrapping_add(1);
        self.pending.push(chunk.clone());
        chunk.clear();
        Ok(stale)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stale_responses_are_skipped() {
    let (client, device) = link(244);
    let controller = Controller::new(Services::new(), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let transport = StaleFirst {
        inner: client,
        pending: Vec::new(),
    };
    tokio::select! {
        () = async { server.run().await } => unreachable!(),
        () = async {
            let mut device = Device::new(transport);
            assert_eq!(device.info().await.unwrap().firmware, "fake");
        } => {}
    }
}
