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
use rohi_identity::{Identity, PublicKey};
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
    identity: RefCell<Option<Identity>>,
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
            identity: RefCell::new(None),
        }
    }
}

impl ControlServices for Services {
    fn device_info(&self) -> ServiceInfo {
        ServiceInfo {
            firmware: "fake-1.0.0",
            board: "bench-esp32c3",
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
        Ok(self.config.borrow().clone())
    }

    async fn set_wifi_config(&self, config: Option<&NetConfig>) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("set");
        *self.config.borrow_mut() = config.cloned();
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

    async fn identity(&self) -> Result<Option<PublicKey>, ServiceError> {
        Ok(self.identity.borrow().as_ref().map(Identity::public_key))
    }

    async fn store_identity(&self, identity: &Identity) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("store_identity");
        *self.identity.borrow_mut() = Some(identity.clone());
        Ok(())
    }

    async fn erase_identity(&self) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("erase_identity");
        *self.identity.borrow_mut() = None;
        Ok(())
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
        config: RefCell::new(services.config.borrow().clone()),
        requests: RefCell::new(services.requests.borrow().clone()),
        identity: RefCell::new(services.identity.borrow().clone()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn info_and_capabilities() {
    with_device(20, Services::new(), |mut device| async move {
        let info = device.info().await.unwrap();
        assert_eq!(info.firmware, "fake-1.0.0");
        assert_eq!(info.board, "bench-esp32c3");

        let capabilities = device.capabilities().await.unwrap();
        assert_eq!(
            capabilities,
            [
                Capability::Sensors,
                Capability::Config,
                Capability::WifiScan,
                Capability::WifiStatus,
                Capability::WifiControl,
                Capability::Identity
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
    assert_eq!(requests, ["set", "set", "set", "set"]);
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
            assert_eq!(device.info().await.unwrap().firmware, "fake-1.0.0");
        } => {}
    }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// The public Substrate development phrase. Never a production master.
const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

fn dev_identity(number: u64) -> rohi::DerivedIdentity {
    rohi::derive_identity(DEV_PHRASE, number).unwrap()
}

fn stored_key(services: &Services) -> Option<[u8; 32]> {
    services
        .identity
        .borrow()
        .as_ref()
        .map(|identity| identity.public_key().to_bytes())
}

#[tokio::test(flavor = "current_thread")]
async fn fresh_device_is_not_provisioned() {
    with_device(244, Services::new(), |mut device| async move {
        let identity = device.identity().await.unwrap();
        assert_eq!(identity.state, IdentityState::NotProvisioned);
        assert_eq!(identity.public_key, None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn identity_lifecycle() {
    let derived = dev_identity(42);
    let expected = derived.public_key();
    let services = with_device(20, Services::new(), |mut device| async move {
        let key = device.provision_identity(&derived).await.unwrap();
        assert_eq!(key, expected);

        let identity = device.identity().await.unwrap();
        assert_eq!(identity.state, IdentityState::Provisioned);
        assert_eq!(identity.public_key, Some(expected));

        // Never silently replaced, not even by the same identity.
        for number in [42, 43] {
            let error = device
                .provision_identity(&dev_identity(number))
                .await
                .unwrap_err();
            assert!(
                matches!(error, Error::Device(ErrorCode::IdentityAlreadyProvisioned)),
                "{error}"
            );
        }
        assert_eq!(device.identity().await.unwrap().public_key, Some(expected));

        device.erase_identity().await.unwrap();
        assert_eq!(
            device.identity().await.unwrap().state,
            IdentityState::NotProvisioned
        );
        // Erasing nothing is fine.
        device.erase_identity().await.unwrap();

        // The same master and number restore the same identity.
        let again = device.provision_identity(&dev_identity(42)).await.unwrap();
        assert_eq!(again, expected);
    })
    .await;
    assert_eq!(
        stored_key(&services),
        Some(dev_identity(42).public_key().to_bytes())
    );
    assert_eq!(
        services.requests.borrow().as_slice(),
        [
            "store_identity",
            "erase_identity",
            "erase_identity",
            "store_identity"
        ]
    );
}

/// Corrupts the 32 bytes `needle` in chunks coming from the device.
struct Tamper {
    inner: ClientEnd,
    needle: [u8; 32],
}

impl ClientTransport for Tamper {
    fn max_payload(&self) -> usize {
        self.inner.max_payload()
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.inner.send(chunk).await
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        let mut chunk = self.inner.recv().await?;
        if let Some(at) = chunk.windows(32).position(|window| window == self.needle) {
            chunk[at] ^= 0x01;
        }
        Ok(chunk)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_different_public_key_from_the_device_is_an_error() {
    let derived = dev_identity(42);
    let (client, device) = link(244);
    let controller = Controller::new(Services::new(), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let transport = Tamper {
        inner: client,
        needle: derived.public_key().to_bytes(),
    };
    tokio::select! {
        () = async { server.run().await } => unreachable!(),
        () = async {
            let mut device = Device::new(transport);
            let error = device.provision_identity(&derived).await.unwrap_err();
            assert!(matches!(error, Error::PublicKeyMismatch { .. }), "{error}");
            assert!(!error.to_string().contains("secret"));
        } => {}
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_device_without_identity_support_is_reported() {
    let mut services = Services::new();
    services.capabilities = Capabilities::SENSORS | Capabilities::CONFIG;
    with_device(244, services, |mut device| async move {
        assert!(matches!(
            device.identity().await,
            Err(Error::Unsupported(Capability::Identity))
        ));
        assert!(matches!(
            device.provision_identity(&dev_identity(1)).await,
            Err(Error::Unsupported(Capability::Identity))
        ));
        assert!(matches!(
            device.erase_identity().await,
            Err(Error::Unsupported(Capability::Identity))
        ));
    })
    .await;
}

// ---------------------------------------------------------------------------
// `rohi identity` command line
// ---------------------------------------------------------------------------

mod cli {
    use clap::Parser;
    use rohi::cli::{Cli, Command, IdentityAction, execute, prepare};

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("rohi").chain(args.iter().copied()))
    }

    struct SeedFile(std::path::PathBuf);

    impl SeedFile {
        fn new(name: &str, contents: &str) -> Self {
            let path = std::env::temp_dir().join(format!("rohi-{}-{name}", std::process::id()));
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for SeedFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn run(services: Services, args: &[&str]) -> (Services, rohi::Result<()>) {
        let cli = parse(args).unwrap();
        let identity = match prepare(&cli.command) {
            Ok(identity) => identity,
            Err(error) => return (services, Err(error)),
        };
        let json = cli.json;
        let command = cli.command;
        let (client, device) = link(244);
        let controller = Controller::new(services, OpenPolicy);
        let mut server = ControlBle::new(device, &controller);
        let result = tokio::select! {
            () = async { server.run().await } => unreachable!(),
            result = async {
                let mut device = Device::new(client);
                execute(&mut device, command, identity, json).await
            } => result,
        };
        drop(server);
        (controller_into_services(controller), result)
    }

    fn is_identity(command: &Command, check: impl Fn(&Option<IdentityAction>) -> bool) -> bool {
        matches!(command, Command::Identity { action } if check(action))
    }

    #[test]
    fn id_is_an_alias_of_identity() {
        for name in ["identity", "id"] {
            let cli = parse(&[name]).unwrap();
            assert!(is_identity(&cli.command, Option::is_none), "{name}");

            let cli = parse(&[name, "provision", "-n", "42", "--seed", "master.suri"]).unwrap();
            assert!(
                is_identity(&cli.command, |action| matches!(
                    action,
                    Some(IdentityAction::Provision(p)) if p.number == 42
                )),
                "{name}"
            );

            let cli = parse(&[name, "erase", "--yes"]).unwrap();
            assert!(
                is_identity(&cli.command, |action| matches!(
                    action,
                    Some(IdentityAction::Erase { yes: true })
                )),
                "{name}"
            );
        }
    }

    #[test]
    fn number_has_a_long_and_a_short_form() {
        for flag in ["-n", "--number"] {
            let cli = parse(&["id", "provision", flag, "7", "--seed", "s"]).unwrap();
            assert!(is_identity(&cli.command, |action| matches!(
                action,
                Some(IdentityAction::Provision(p)) if p.number == 7
            )));
        }
    }

    #[test]
    fn number_and_seed_are_required() {
        assert!(parse(&["id", "provision", "--seed", "s"]).is_err());
        assert!(parse(&["id", "provision", "-n", "1"]).is_err());
        assert!(parse(&["id", "provision"]).is_err());
    }

    #[test]
    fn invalid_numbers_are_rejected() {
        for number in ["abc", "-1", "1.5", "", "18446744073709551616"] {
            assert!(
                parse(&["id", "provision", "-n", number, "--seed", "s"]).is_err(),
                "{number:?}"
            );
        }
    }

    #[test]
    fn no_other_identity_subcommands() {
        for name in ["show", "status", "generate", "get-public-key"] {
            assert!(parse(&["identity", name]).is_err(), "{name}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn show_provision_and_erase_through_both_names() {
        let seed = SeedFile::new("lifecycle.suri", &format!("{DEV_PHRASE}\n"));
        let expected = dev_identity(42).public_key();
        for name in ["identity", "id"] {
            let (services, result) = run(Services::new(), &[name]).await;
            result.unwrap();

            let args = [name, "provision", "-n", "42", "--seed", seed.path()];
            let (services, result) = run(services, &args).await;
            result.unwrap();
            assert_eq!(stored_key(&services), Some(expected.to_bytes()), "{name}");

            let (services, result) = run(services, &[name]).await;
            result.unwrap();

            let (services, result) = run(services, &args).await;
            assert!(
                matches!(
                    result,
                    Err(Error::Device(ErrorCode::IdentityAlreadyProvisioned))
                ),
                "{name}"
            );
            assert_eq!(stored_key(&services), Some(expected.to_bytes()));

            let (services, result) = run(services, &[name, "erase", "--yes"]).await;
            result.unwrap();
            assert_eq!(stored_key(&services), None, "{name}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unreadable_or_invalid_seed_never_reaches_the_device() {
        let (services, result) = run(
            Services::new(),
            &["id", "provision", "-n", "1", "--seed", "/nonexistent/x"],
        )
        .await;
        assert!(matches!(result, Err(Error::InvalidInput(_))));
        assert!(services.requests.borrow().is_empty());

        let seed = SeedFile::new("bad.suri", "definitely not a secret uri");
        let (services, result) = run(
            Services::new(),
            &["id", "provision", "-n", "1", "--seed", seed.path()],
        )
        .await;
        let error = result.unwrap_err();
        assert!(matches!(error, Error::InvalidInput(_)));
        assert!(!error.to_string().contains("definitely"));
        assert!(services.requests.borrow().is_empty());
    }
}
