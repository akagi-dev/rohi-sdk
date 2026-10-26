//! In-memory ROHI device and client link shared by the integration tests.
//!
//! The device is built from the real generic pieces (`rohi-control`,
//! `rohi-service`, the real Wi-Fi and telemetry config sections, the real
//! identity service) plus a few fakes. One section (`gadget`) and one service
//! (`radio`) exist only here: the tool has never heard of them, which is the
//! point.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi::{ClientTransport, Device, Error};
use rohi_connectivity::ConnectivityConfig;
use rohi_control::{ControlBackend, Controller, Device as BackendDevice, DeviceInfo, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_identity::{Identity, IdentityService, IdentityStore, IdentityStoreError, PublicKey};
use rohi_net::wifi::WifiConfig;
use rohi_net::{BleConnectionInfo, BleError, BleState, BleTransport};
use rohi_service::{
    ActionDescriptor, ActionError, ActionOutput, ConfigField, FieldDescriptor, FieldError, Issue,
    IssueSeverity, ManagedConfig, ManagedSection, ManagedService, SectionStore, ServiceHealth,
    ServiceState, ServiceSummary, StatusWriter, StoreError, Tracked, Value, ValueKind, WriteError,
};
use tokio::sync::mpsc;

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

/// A persisted section in memory.
#[derive(Clone)]
pub struct Mem<C> {
    pub value: Rc<RefCell<Option<C>>>,
    pub writes: Rc<Cell<u32>>,
    pub restart: bool,
}

impl<C> Mem<C> {
    fn new(value: Rc<RefCell<Option<C>>>, restart: bool) -> Self {
        Self {
            value,
            writes: Rc::new(Cell::new(0)),
            restart,
        }
    }
}

impl<C: Clone> SectionStore<C> for Mem<C> {
    async fn load(&self) -> Result<Option<C>, StoreError> {
        Ok(self.value.borrow().clone())
    }

    async fn store(&self, config: &C) -> Result<(), StoreError> {
        self.writes.set(self.writes.get() + 1);
        *self.value.borrow_mut() = Some(config.clone());
        Ok(())
    }

    async fn remove(&self) -> Result<(), StoreError> {
        self.writes.set(self.writes.get() + 1);
        *self.value.borrow_mut() = None;
        Ok(())
    }

    fn restart_required(&self) -> bool {
        self.restart
    }
}

/// A section that exists only in the tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Gadget {
    pub label: String,
    pub level: u64,
    pub mode: Option<String>,
}

impl ManagedConfig for Gadget {
    const NAME: &'static str = "gadget";
    const VERSION: u16 = 3;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(1, "label", ValueKind::String).describe("What it is called"),
        ConfigField::new(2, "level", ValueKind::U64)
            .describe("How much")
            .unit("%")
            .range(0.0, 100.0),
        ConfigField::new(3, "mode", ValueKind::Enum)
            .describe("How it behaves")
            .variants(&["fast", "slow"])
            .optional(),
        ConfigField::new(4, "serial", ValueKind::String)
            .describe("Burned in")
            .read_only(),
    ];

    fn defaults() -> Self {
        Self {
            label: String::new(),
            level: 50,
            mode: None,
        }
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        Ok(match id {
            1 => Value::text(&self.label),
            2 => Some(Value::U64(self.level)),
            3 => self.mode.as_deref().and_then(Value::variant),
            4 => Value::text("SN-1"),
            _ => return Err(FieldError::UnknownField),
        })
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            1 => self.label = value.as_str().unwrap().to_owned(),
            2 => self.level = value.as_u64().unwrap(),
            3 => self.mode = Some(value.as_str().unwrap().to_owned()),
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            3 => {
                self.mode = None;
                Ok(())
            }
            _ => Err(FieldError::NotOptional),
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        if self.label.is_empty() {
            return Err(FieldError::Invalid);
        }
        Ok(())
    }
}

/// A service that exists only in the tests.
#[derive(Default)]
pub struct Radio {
    pub connected: Cell<bool>,
    pub degraded: Cell<bool>,
    pub calls: RefCell<Vec<String>>,
}

impl ManagedService for &Radio {
    const NAME: &'static str = "radio";
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "ssid", ValueKind::String).describe("Connected network"),
        FieldDescriptor::new(2, "rssi", ValueKind::I64)
            .describe("Signal strength")
            .unit("dBm"),
        FieldDescriptor::new(3, "mode", ValueKind::Enum).variants(&["sta", "ap"]),
    ];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new("scan")
            .describe("Scan for networks")
            .results(&[
                FieldDescriptor::new(1, "ssid", ValueKind::String),
                FieldDescriptor::new(2, "rssi", ValueKind::I64).unit("dBm"),
                FieldDescriptor::new(3, "security", ValueKind::Enum).variants(&["open", "wpa2"]),
            ]),
        ActionDescriptor::new("reconnect")
            .describe("Connect again")
            .privileged(),
        ActionDescriptor::new("tune")
            .describe("Set the channel")
            .args(&[FieldDescriptor::new(1, "channel", ValueKind::U64)]),
        ActionDescriptor::new("explode").describe("Always refused"),
    ];

    async fn summary(&self) -> ServiceSummary {
        if self.degraded.get() {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Degraded, false)
        } else if self.connected.get() {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        } else {
            ServiceSummary::new(ServiceState::Starting, ServiceHealth::Unknown, false)
        }
    }

    async fn issues(&self, out: &mut heapless::Vec<Issue, { rohi_service::MAX_ISSUES }>) {
        if self.degraded.get() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code: "link_lost",
                message: "The link was lost",
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if self.connected.get() {
            out.text(1, "office")?;
            out.i64(2, -61)?;
        }
        out.variant(3, "sta")
    }

    async fn invoke(
        &self,
        action: &str,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        self.calls.borrow_mut().push(action.to_owned());
        match action {
            "scan" => {
                for (ssid, rssi, security) in [("office", -50, "wpa2"), ("guest", -70, "open")] {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    out.text(1, ssid).map_err(|_| ActionError::Internal)?;
                    out.i64(2, rssi).map_err(|_| ActionError::Internal)?;
                    out.variant(3, security)
                        .map_err(|_| ActionError::Internal)?;
                }
                Ok(())
            }
            "reconnect" => Ok(()),
            "tune" => {
                self.calls
                    .borrow_mut()
                    .push(format!("channel={}", args[0].1.as_u64().unwrap()));
                Ok(())
            }
            "explode" => Err(ActionError::Rejected("not_today")),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[derive(Clone, Default)]
pub struct MemIdentity(pub Rc<RefCell<Option<Identity>>>);

impl IdentityStore for MemIdentity {
    async fn public_key(&self) -> Result<Option<PublicKey>, IdentityStoreError> {
        Ok(self.0.borrow().as_ref().map(Identity::public_key))
    }

    async fn store(&self, identity: &Identity) -> Result<(), IdentityStoreError> {
        *self.0.borrow_mut() = Some(identity.clone());
        Ok(())
    }

    async fn erase(&self) -> Result<(), IdentityStoreError> {
        *self.0.borrow_mut() = None;
        Ok(())
    }
}

/// The state of the fake device, kept by the test so it can look at it after
/// the command ran.
pub struct Bench {
    pub wifi: Mem<WifiConfig>,
    pub telemetry: Mem<ConnectivityConfig>,
    pub gadget: Mem<Gadget>,
    pub identity: MemIdentity,
    pub radio: Radio,
}

impl Bench {
    pub fn new() -> Self {
        let bench = Self {
            wifi: Mem::new(Rc::default(), false),
            telemetry: Mem::new(Rc::default(), false),
            gadget: Mem::new(Rc::default(), true),
            identity: MemIdentity::default(),
            radio: Radio::default(),
        };
        bench.radio.connected.set(true);
        bench
    }

    pub fn writes(&self) -> u32 {
        self.wifi.writes.get() + self.telemetry.writes.get() + self.gadget.writes.get()
    }
}

impl Default for Bench {
    fn default() -> Self {
        Self::new()
    }
}

type Sections = (
    ManagedSection<WifiConfig, Mem<WifiConfig>, NoopRawMutex>,
    ManagedSection<ConnectivityConfig, Mem<ConnectivityConfig>, NoopRawMutex>,
    ManagedSection<Gadget, Mem<Gadget>, NoopRawMutex>,
);

type Services<'a> = (
    Tracked<&'a Radio, NoopRawMutex>,
    Tracked<IdentityService<MemIdentity>, NoopRawMutex>,
);

pub type Backend<'a> = BackendDevice<Sections, Services<'a>>;

fn uptime() -> u64 {
    90_000
}

pub fn backend(bench: &Bench) -> Backend<'_> {
    BackendDevice::new(
        DeviceInfo {
            firmware: "fake-1.0.0",
            board: "bench-esp32c3",
        },
        uptime,
        (
            ManagedSection::new(bench.wifi.clone()),
            ManagedSection::new(bench.telemetry.clone()),
            ManagedSection::new(bench.gadget.clone()),
        ),
        (
            Tracked::new(&bench.radio),
            Tracked::new(IdentityService(bench.identity.clone())),
        ),
    )
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

/// Runs `script` against the fake device until it finishes.
pub async fn with_device<F, Fut>(payload: usize, bench: &Bench, script: F)
where
    F: FnOnce(Device<ClientEnd>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (client, device) = link(payload);
    let controller = Controller::new(backend(bench), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    tokio::select! {
        () = async { server.run().await } => unreachable!(),
        () = script(Device::new(client)) => {}
    }
}

/// Keeps the trait in scope for helpers that need `uptime_ms`.
pub fn uptime_of(backend: &Backend<'_>) -> u64 {
    backend.uptime_ms()
}
