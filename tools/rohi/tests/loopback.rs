//! End to end: the `rohi` client against the real dispatcher and BLE framing,
//! connected in memory. No hardware, no Bluetooth adapter.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use rohi::model::*;
use rohi::{ClientTransport, Device, Error, Session};
use rohi_control::{Capabilities, Controller, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_control_proto::control as pb;

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
