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
use rohi_config::{Text, Variant};
use rohi_connectivity::{ConnectivityConfig, ConnectivityOverrides};
use rohi_control::{ControlBackend, Controller, Device as BackendDevice, DeviceInfo, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_identity::{Identity, IdentityService, IdentityStore, IdentityStoreError, PublicKey};
use rohi_location::LocationConfig;
use rohi_location::{LocationOverrides, LocationService, LocationShared};
use rohi_net::{BleConnectionInfo, BleError, BleState, BleTransport};
use rohi_net::{WifiConfig, WifiOverrides};
use rohi_service::{
    Action, ActionDescriptor, ActionError, ActionOutput, Config, EnumDescriptor, EnumVariant,
    FIRST_SERVICE_ISSUE, FieldDescriptor, FieldError, Issue, IssueDescriptor, IssueSeverity,
    Section, SectionStore, Service, ServiceHealth, ServiceState, ServiceSummary, Status,
    StatusWriter, Statuses, StoreError, Unit, Value, ValueKind, WriteError,
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

impl<C: Clone + Default> Mem<C> {
    /// The effective config of the stored overrides: the defaults with them applied.
    pub fn effective<T>(&self) -> T
    where
        T: rohi_config::HasOverrides<Overrides = C> + Default,
    {
        let stored = self.value.borrow().clone().unwrap();
        rohi_config::materialize(&C::default(), &stored)
    }
}

impl<C: Config + Default> SectionStore<C> for Mem<C::Overrides> {
    fn firmware_overrides(&self) -> C::Overrides {
        C::Overrides::default()
    }

    async fn load(&self) -> Result<Option<C::Overrides>, StoreError> {
        Ok(self.value.borrow().clone())
    }

    async fn store(&self, overrides: &C::Overrides, _: &C) -> Result<(), StoreError> {
        self.writes.set(self.writes.get() + 1);
        *self.value.borrow_mut() = Some(overrides.clone());
        Ok(())
    }

    async fn remove(&self, _: &C) -> Result<(), StoreError> {
        self.writes.set(self.writes.get() + 1);
        *self.value.borrow_mut() = None;
        Ok(())
    }

    fn restart_required(&self) -> bool {
        self.restart
    }
}

const GADGET_MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    1,
    &[EnumVariant::new(0, "fast"), EnumVariant::new(1, "slow")],
)];

/// The defaults are valid; a mode belongs to a labelled gadget.
fn gadget_is_valid(gadget: &Gadget) -> Result<(), FieldError> {
    if gadget.mode.is_some() && gadget.label.is_empty() {
        return Err(FieldError::Invalid);
    }
    Ok(())
}

/// A section that exists only in the tests.
#[derive(Clone, Debug, PartialEq, Config)]
#[config(name = "gadget", overrides = GadgetOverrides, version = 3, id = 9, validate = gadget_is_valid, enums = GADGET_MODES)]
pub struct Gadget {
    #[config(id = 1, with = Text)]
    pub label: String,
    #[config(id = 2, unit = Percent, min = 0, max = 100)]
    pub level: u64,
    #[config(id = 3, with = Variant, enum_id = 1)]
    pub mode: Option<u32>,
    /// Shown, never changed and never stored.
    #[config(id = 4, read_only, with = Text)]
    pub serial: String,
}

impl Default for Gadget {
    fn default() -> Self {
        Self {
            label: String::new(),
            level: 50,
            mode: None,
            serial: "SN-1".into(),
        }
    }
}

/// A service that exists only in the tests.
#[derive(Default, Service)]
pub struct Radio {
    pub connected: Cell<bool>,
    pub degraded: Cell<bool>,
    pub calls: RefCell<Vec<String>>,
}

impl Status for Radio {
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "ssid", ValueKind::String),
        FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
        FieldDescriptor::new(3, "mode", ValueKind::Enum).with_enum(1),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        1,
        &[EnumVariant::new(0, "sta"), EnumVariant::new(1, "ap")],
    )];
    const ISSUES: &'static [IssueDescriptor] =
        &[IssueDescriptor::new(FIRST_SERVICE_ISSUE, "link_lost")];

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
                code: FIRST_SERVICE_ISSUE,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if self.connected.get() {
            out.text(1, "office")?;
            out.i64(2, -61)?;
        }
        out.variant(3, 0)
    }
}

impl Action for Radio {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "scan")
            .results(&[
                FieldDescriptor::new(1, "ssid", ValueKind::String),
                FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
                FieldDescriptor::new(3, "security", ValueKind::Enum).with_enum(1),
            ])
            .enums(&[EnumDescriptor::new(
                1,
                &[EnumVariant::new(0, "open"), EnumVariant::new(1, "wpa2")],
            )]),
        ActionDescriptor::new(2, "reconnect").privileged(),
        ActionDescriptor::new(3, "tune").args(&[FieldDescriptor::new(
            1,
            "channel",
            ValueKind::U64,
        )]),
        ActionDescriptor::new(4, "explode"),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        let name = Self::ACTIONS
            .iter()
            .find(|a| a.id == action)
            .map(|a| a.name);
        self.calls.borrow_mut().push(name.unwrap_or("?").to_owned());
        match action {
            1 => {
                for (ssid, rssi, security) in [("office", -50, 1), ("guest", -70, 0)] {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    out.text(1, ssid).map_err(|_| ActionError::Internal)?;
                    out.i64(2, rssi).map_err(|_| ActionError::Internal)?;
                    out.variant(3, security)
                        .map_err(|_| ActionError::Internal)?;
                }
                Ok(())
            }
            2 => Ok(()),
            3 => {
                self.calls
                    .borrow_mut()
                    .push(format!("channel={}", args[0].1.as_u64().unwrap()));
                Ok(())
            }
            4 => Err(ActionError::Rejected("not_today")),
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

/// The location section: publishes to the shared location like the firmware.
#[derive(Clone)]
pub struct LocationStore {
    pub mem: Mem<LocationOverrides>,
    pub shared: Rc<LocationShared<NoopRawMutex>>,
}

impl SectionStore<LocationConfig> for LocationStore {
    fn firmware_overrides(&self) -> LocationOverrides {
        LocationOverrides::default()
    }

    async fn load(&self) -> Result<Option<LocationOverrides>, StoreError> {
        SectionStore::<LocationConfig>::load(&self.mem).await
    }

    async fn store(
        &self,
        overrides: &LocationOverrides,
        effective: &LocationConfig,
    ) -> Result<(), StoreError> {
        SectionStore::<LocationConfig>::store(&self.mem, overrides, effective).await?;
        self.shared.publish_configured(Some(effective));
        Ok(())
    }

    async fn remove(&self, effective: &LocationConfig) -> Result<(), StoreError> {
        SectionStore::<LocationConfig>::remove(&self.mem, effective).await?;
        self.shared.publish_configured(Some(effective));
        Ok(())
    }
}

/// The state of the fake device, kept by the test so it can look at it after
/// the command ran.
pub struct Bench {
    pub wifi: Mem<WifiOverrides>,
    pub telemetry: Mem<ConnectivityOverrides>,
    pub gadget: Mem<GadgetOverrides>,
    pub location: LocationStore,
    pub identity: MemIdentity,
    pub radio: Radio,
}

impl Bench {
    pub fn new() -> Self {
        let bench = Self {
            wifi: Mem::new(Rc::default(), false),
            telemetry: Mem::new(Rc::default(), false),
            gadget: Mem::new(Rc::default(), true),
            location: LocationStore {
                mem: Mem::new(Rc::default(), false),
                shared: Rc::default(),
            },
            identity: MemIdentity::default(),
            radio: Radio::default(),
        };
        bench.radio.connected.set(true);
        bench
    }

    pub fn writes(&self) -> u32 {
        self.wifi.writes.get()
            + self.telemetry.writes.get()
            + self.gadget.writes.get()
            + self.location.mem.writes.get()
    }
}

impl Default for Bench {
    fn default() -> Self {
        Self::new()
    }
}

type Sections = (
    Section<WifiConfig, Mem<WifiOverrides>, NoopRawMutex>,
    Section<ConnectivityConfig, Mem<ConnectivityOverrides>, NoopRawMutex>,
    Section<Gadget, Mem<GadgetOverrides>, NoopRawMutex>,
    Section<LocationConfig, LocationStore, NoopRawMutex>,
);

type StatusServices<'a> = (
    &'a Radio,
    &'a IdentityService<MemIdentity>,
    &'a LocationService<'a, NoopRawMutex>,
);

type ActionServices<'a> = (&'a Radio, &'a IdentityService<MemIdentity>);

pub type Backend<'a> =
    BackendDevice<Sections, Statuses<NoopRawMutex, StatusServices<'a>>, ActionServices<'a>>;

thread_local! {
    static REBOOTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn reboot() {
    REBOOTS.with(|count| count.set(count.get() + 1));
}

/// How many reboots the fake devices of this test thread were asked for.
pub fn reboots() -> u32 {
    REBOOTS.with(std::cell::Cell::get)
}

fn uptime() -> u64 {
    90_000
}

pub fn backend(bench: &Bench) -> Backend<'_> {
    // The services are shared by the status and the action registry; a test
    // process may leak the few bytes.
    let identity: &_ = Box::leak(Box::new(IdentityService(bench.identity.clone())));
    let location: &_ = Box::leak(Box::new(LocationService(&bench.location.shared)));
    BackendDevice::new(
        DeviceInfo {
            firmware: "fake-1.0.0",
            board: "bench-esp32c3",
        },
        uptime,
        (
            Section::new(bench.wifi.clone()),
            Section::new(bench.telemetry.clone()),
            Section::new(bench.gadget.clone()),
            Section::new(bench.location.clone()),
        ),
        Statuses::new((&bench.radio, identity, location)),
        (&bench.radio, identity),
    )
    .with_reboot(reboot)
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
