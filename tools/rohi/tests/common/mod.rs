//! In-memory ROHI device and client link shared by the integration tests.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;

use rohi::{ClientTransport, Device, Error};
use rohi_control::{
    Capabilities, ControlServices, Controller, DeviceInfo as ServiceInfo, DeviceTime, OpenPolicy,
    ServiceError,
};
use rohi_control_ble::ControlBle;
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

pub fn ts(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

// ---------------------------------------------------------------------------
// Device side
// ---------------------------------------------------------------------------

pub struct DeviceEnd {
    pub from_client: mpsc::UnboundedReceiver<Vec<u8>>,
    pub to_client: mpsc::UnboundedSender<Vec<u8>>,
    pub payload: usize,
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

pub struct Services {
    pub capabilities: Capabilities,
    pub snapshot: MeasurementsSnapshot,
    pub config: RefCell<Option<NetConfig>>,
    pub requests: RefCell<Vec<&'static str>>,
    pub identity: RefCell<Option<Identity>>,
    pub time: DeviceTime,
    pub wifi: RefCell<WifiScript>,
}

/// What the device reports about Wi-Fi: the answers to the next status
/// requests, then the last one for good.
pub struct WifiScript {
    pub script: VecDeque<(NetStatus, Option<NetworkInfo>)>,
    pub current: (NetStatus, Option<NetworkInfo>),
}

pub fn network(ssid: &str) -> NetworkInfo {
    NetworkInfo {
        ssid: WifiSsid::from_bytes(ssid.as_bytes()).unwrap(),
        rssi_dbm: -55,
        ipv4: Ipv4Addr::new(10, 0, 0, 7),
        gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
        dns: None,
    }
}

impl Services {
    pub fn new() -> Self {
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
            time: DeviceTime::UNSYNCHRONIZED,
            wifi: RefCell::new(WifiScript {
                script: VecDeque::new(),
                current: (NetStatus::new(NetState::Connected), Some(network("office"))),
            }),
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
        let mut wifi = self.wifi.borrow_mut();
        if let Some(next) = wifi.script.pop_front() {
            wifi.current = next;
        }
        wifi.current.0
    }

    fn wifi_network_info(&self) -> Option<NetworkInfo> {
        self.wifi.borrow().current.1.clone()
    }

    fn wifi_reconnect(&self) -> Result<(), ServiceError> {
        self.requests.borrow_mut().push("reconnect");
        Ok(())
    }

    fn wifi_disconnect(&self) -> Result<(), ServiceError> {
        Err(ServiceError::Busy)
    }

    fn time_status(&self) -> DeviceTime {
        self.time
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

pub struct ClientEnd {
    pub to_device: mpsc::UnboundedSender<Vec<u8>>,
    pub from_device: mpsc::UnboundedReceiver<Vec<u8>>,
    pub payload: usize,
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

pub fn link(payload: usize) -> (ClientEnd, DeviceEnd) {
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
pub async fn with_device<F, Fut>(payload: usize, services: Services, script: F) -> Services
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

pub fn controller_into_services(controller: Controller<Services, OpenPolicy>) -> Services {
    // The services only hold plain data; rebuild an equivalent for inspection.
    let services = controller.services();
    Services {
        capabilities: services.capabilities,
        snapshot: services.snapshot,
        config: RefCell::new(services.config.borrow().clone()),
        requests: RefCell::new(services.requests.borrow().clone()),
        identity: RefCell::new(services.identity.borrow().clone()),
        time: services.time,
        wifi: RefCell::new(WifiScript {
            script: services.wifi.borrow().script.clone(),
            current: services.wifi.borrow().current.clone(),
        }),
    }
}
