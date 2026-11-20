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

//! The composition root of the Altruist Urban on the host.
//!
//! It resembles the firmware `main`: one owner per resource, spawned in the
//! same order, with host backends where the firmware has ESP ones.
//!
//! ```text
//! main
//!  ├── open storage ............. FileFlash       (firmware: rohi-cfg flash)
//!  ├── bind listeners ........... control (TCP), management
//!  │
//!  ├── ConfigOwner task ......... same ConfigStore, same bindings
//!  ├── store task ............... same MeasurementStore owner
//!  ├── WifiManager task ......... same manager over SimWifi
//!  ├── NTP / telemetry tasks .... same services over the host IP stack
//!  ├── sensor tasks ............. same Runner over simulated drivers (`sensor-simulator`)
//!  └── control tasks ............ same Controller over TCP and/or BLE
//! ```

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher as _;
#[cfg(feature = "std-tcp")]
use std::net::Ipv4Addr;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;

use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;
#[cfg(feature = "std-tcp")]
use log::warn;
use log::{error, info};
use rohi_sdk::config::{ConfigStore, materialize};
use rohi_sdk::endpoints::{ControlConfig, ControlOverrides};
use rohi_sdk::host::NativeIp;
use rohi_sdk::net::NetworkHandle;
use rohi_sdk::sensors::runtime::MeasurementHandle;
use rohi_sdk::sensors::{FreshnessPolicy, MeasurementStore, SensorKind};
use rohi_sdk::sim::ip::SimIp;
use rohi_sdk::sim::log_storage::FileLogFlash;
use rohi_sdk::sim::management::{self, Hooks};
use rohi_sdk::sim::scenario::Scenario;
use rohi_sdk::sim::storage::FileFlash;
use rohi_sdk::sim::wifi::SimWifi;
use rohi_sdk::sim::world;
use rohi_sdk::time::EmbassyClock;
use static_cell::StaticCell;

use super::args::Args;
use super::control::{self, HostIp};
use super::logging;
use super::process::{
    Failure, Interfaces, SHUTDOWN, StartupError, announce, finish, finished,
    install_signal_handler, ready_line,
};
use crate::bindings::ConfigRuntime;
use crate::config::ConfigOwner;
use crate::tasks::store;
use crate::{connectivity, net, reboot, time};

/// Time the shutdown gives running tasks before the process ends.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(100);

/// The name of the device when neither the command line nor the scenario say.
const DEFAULT_NAME: &str = "urban-std";

/// Entropy for the nonces of the telemetry packets.
///
/// Without a seed it comes from the randomness of the host; with `--seed` it
/// is a reproducible sequence. One sequence serves the publisher and the
/// export, so a nonce is never repeated.
enum Sequence {
    Host(RandomState, u64),
    Seeded(u64),
}

static SEQUENCE: Mutex<Option<Sequence>> = Mutex::new(None);

/// Chooses the source of the nonces; call once before the telemetry runs.
pub fn init_entropy(seed: Option<u64>) {
    if let Ok(mut sequence) = SEQUENCE.lock() {
        *sequence = Some(match seed {
            Some(seed) => Sequence::Seeded(seed.max(1)),
            None => Sequence::Host(RandomState::new(), 0),
        });
    }
}

/// [`crate::connectivity::EntropyFn`] of the host platform.
pub fn host_entropy(dst: &mut [u8]) -> bool {
    let Ok(mut guard) = SEQUENCE.lock() else {
        return false;
    };
    let Some(sequence) = guard.as_mut() else {
        return false;
    };
    for chunk in dst.chunks_mut(8) {
        let word = match sequence {
            Sequence::Host(random, counter) => {
                *counter += 1;
                random.hash_one(*counter)
            }
            Sequence::Seeded(state) => {
                *state ^= *state >> 12;
                *state ^= *state << 25;
                *state ^= *state >> 27;
                state.wrapping_mul(0x2545_F491_4F6C_DD1D)
            }
        };
        chunk.copy_from_slice(&word.to_le_bytes()[..chunk.len()]);
    }
    true
}

static IP: StaticCell<HostIp> = StaticCell::new();
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

fn bind(address: SocketAddr, what: &str) -> Result<TcpListener, StartupError> {
    TcpListener::bind(address).map_err(|e| {
        StartupError::new(
            Failure::Bind,
            format!("cannot listen on {address} for {what}: {e}"),
        )
    })
}

/// Everything prepared before the executor runs.
struct Prepared {
    args: Args,
    scenario: Scenario,
    storage: FileFlash,
    log_storage: FileLogFlash,
    /// `None`: the TCP control listener could not be bound (reported as `failed`).
    #[cfg(feature = "std-tcp")]
    control: Option<TcpListener>,
    manage: TcpListener,
}

/// Refuses an address that exposes the control or management interface to the
/// network unless the operator asked for it.
fn check_exposure(address: IpAddr, what: &str, allowed: bool) -> Result<(), StartupError> {
    if address.is_loopback() || allowed {
        return Ok(());
    }
    Err(StartupError::new(
        Failure::Bind,
        format!(
            "refusing to listen on {address} for {what}: it can reconfigure, provision and \
             erase the device and its requests are neither encrypted nor hidden; use a \
             loopback address or pass --unsafe-allow-remote"
        ),
    ))
}

/// The stored `control` section, read before the listener is bound: the
/// defaults when nothing is stored or the store cannot be read.
fn stored_control(storage: &FileFlash) -> ControlConfig {
    let Ok(mut store) = ConfigStore::open(storage.clone()) else {
        return ControlConfig::default();
    };
    match store.get::<ControlOverrides>() {
        Ok(Some(user)) => materialize(&ControlOverrides::default(), &user),
        _ => ControlConfig::default(),
    }
}

/// Random bytes of the operating system, for the nonces of the TCP handshake.
/// Fails closed: without them no client is served.
#[cfg(unix)]
fn os_entropy(dst: &mut [u8]) -> bool {
    use std::io::Read as _;

    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(dst))
        .is_ok()
}

#[cfg(not(unix))]
fn os_entropy(_: &mut [u8]) -> bool {
    false
}

fn prepare(args: Args) -> Result<Prepared, StartupError> {
    check_exposure(
        args.management_address(),
        "management",
        args.unsafe_allow_remote,
    )?;
    let scenario = match &args.scenario {
        Some(path) => {
            Scenario::load(path).map_err(|e| StartupError::new(Failure::Scenario, e.to_string()))?
        }
        None => Scenario::default(),
    };
    let storage = if args.ephemeral {
        FileFlash::ephemeral()
    } else {
        FileFlash::open(&args.storage)
            .map_err(|e| StartupError::new(Failure::Storage, e.to_string()))?
    };
    let log_storage = if args.ephemeral {
        FileLogFlash::ephemeral()
    } else {
        FileLogFlash::open(&args.log_storage())
            .map_err(|e| StartupError::new(Failure::Storage, e.to_string()))?
    };
    let stored = stored_control(&storage);
    let (_control_port, manage_port) = args.ports(stored.tcp_port);
    #[cfg(feature = "std-tcp")]
    let control = {
        let address = args.control_address(Ipv4Addr::from(stored.tcp_bind_address));
        check_exposure(address, "control", args.unsafe_allow_remote)?;
        match bind(SocketAddr::new(address, _control_port), "control") {
            Ok(listener) => Some(listener),
            // The port was asked for on the command line: the operator wants to know. A
            // stored or default port that is taken only fails the endpoint, which the status
            // reports, so the rest of the device still starts.
            Err(error) if args.control_port.is_some() => return Err(error),
            Err(error) => {
                warn!("control: {error}");
                None
            }
        }
    };
    let manage = bind(
        SocketAddr::new(args.management_address(), manage_port),
        "management",
    )?;
    crate::control::set_access_entropy(os_entropy);
    let name = args
        .name
        .clone()
        .or_else(|| scenario.device_name.clone())
        .unwrap_or_else(|| DEFAULT_NAME.to_owned());
    world::init(&scenario, args.seed, name);
    init_entropy(args.seed);
    Ok(Prepared {
        args,
        scenario,
        storage,
        log_storage,
        #[cfg(feature = "std-tcp")]
        control,
        manage,
    })
}

/// What the management interface reaches in the application.
const HOOKS: Hooks = Hooks {
    snapshot: || Box::pin(async { store::handle().snapshot().await.ok() }),
    shutdown: || SHUTDOWN.signal(()),
    sensors: cfg!(feature = "sensor-simulator"),
};

/// Spawns the tasks of the device and announces readiness.
fn start(spawner: Spawner, prepared: Prepared) -> Result<(), StartupError> {
    let Prepared {
        args,
        scenario,
        storage,
        log_storage,
        #[cfg(feature = "std-tcp")]
        control,
        manage,
    } = prepared;
    let platform = |what: &str, e: std::io::Error| {
        StartupError::new(Failure::Platform, format!("{what}: {e}"))
    };
    let name = world::world().lock().device_name.clone();
    info!(
        "Starting {name} ({} {})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );

    // The log task is the only user of the log flash; boot messages wait in its
    // queue until it runs.
    logging::start(spawner, log_storage.clone());

    // The configuration owner is the single owner of the store; it publishes
    // every section to its consumer before they start.
    let runtime = ConfigRuntime::new(net::handle(), logging::apply);
    let mut owner = ConfigOwner::open(storage.clone());
    owner.publish_boot(&runtime);
    #[cfg(feature = "std-tcp")]
    if let Some(path) = &args.control_psk_file {
        let key = zeroize::Zeroizing::new(
            super::args::read_psk_file(path).map_err(|e| platform("control key file", e))?,
        );
        crate::control::set_tcp_key(Some(*key));
        crate::control::access().set_tcp_psk(true);
        info!(
            "The TCP control key of this run comes from {} (it replaces the stored one)",
            path.display()
        );
    }
    #[cfg(feature = "std-tcp")]
    if let Some(mode) = args.control_access_mode() {
        if mode == rohi_sdk::control::TransportAccessMode::Open {
            warn!("The TCP control endpoint is open for this run: privileged requests need no key");
        } else {
            info!(
                "The TCP control access mode of this run is {} (it replaces the stored one)",
                mode.as_str()
            );
        }
        crate::control::access().set_mode(rohi_sdk::control::Transport::Tcp, mode);
    }
    crate::tasks::spawn(spawner, "config_task", config_task(owner, runtime));

    let mut interfaces = Interfaces::default();
    #[cfg(feature = "std-tcp")]
    {
        crate::control::access().expect(rohi_sdk::control::Transport::Tcp);
        interfaces.control = control
            .as_ref()
            .map(TcpListener::local_addr)
            .transpose()
            .map_err(|e| platform("control address", e))?;
        control::record_tcp(interfaces.control);
    }
    let manage_address = manage
        .local_addr()
        .map_err(|e| platform("management address", e))?;
    interfaces.manage = Some(manage_address);

    crate::tasks::spawn(
        spawner,
        "reboot_task",
        reboot_task(
            storage.clone(),
            log_storage.clone(),
            args.clone(),
            interfaces.control.map_or(0, |address| address.port()),
            manage_address.port(),
        ),
    );

    crate::tasks::spawn(spawner, "store_task", store_task(scenario.stale_after_ms));

    let ip: &'static HostIp = IP.init(SimIp::new(NativeIp::new()));
    let network = NetworkHandle::new(ip);
    crate::tasks::spawn(spawner, "wifi_task", wifi_task());
    crate::tasks::spawn(spawner, "ntp_task", ntp_task(network));
    crate::tasks::spawn(
        spawner,
        "connectivity_task",
        connectivity_task(network, store::handle()),
    );

    #[cfg(feature = "std-ble")]
    let ble = {
        let ble =
            rohi_sdk::host::ble::start(crate::control::ble_config(), args.ble_adapter.clone());
        interfaces.ble = Some(
            args.ble_adapter
                .clone()
                .unwrap_or_else(|| "default".to_owned()),
        );
        ble
    };
    #[cfg(feature = "std-ble")]
    let probe = ble.probe();
    #[cfg(not(feature = "std-ble"))]
    let probe = control::NoBle;
    let controller = control::controller(Some(network), probe);
    crate::tasks::spawn(spawner, "control_task", control::control_task(controller));
    #[cfg(feature = "std-ble")]
    {
        crate::control::access().expect(rohi_sdk::control::Transport::Ble);
        crate::tasks::spawn(spawner, "ble_task", control::ble_task(ble));
        crate::control::access().set_state(
            rohi_sdk::control::Transport::Ble,
            rohi_sdk::control::EndpointState::Running,
        );
    }
    #[cfg(feature = "std-tcp")]
    if let Some(control) = control {
        match rohi_sdk::host::TcpAcceptor::new(control) {
            Ok(acceptor) => crate::tasks::spawn(spawner, "tcp_task", control::tcp_task(acceptor)),
            Err(error) => {
                warn!("control: the TCP listener cannot start: {error}");
                control::record_tcp(None);
            }
        }
    }

    management::start(spawner, manage, storage.clone(), HOOKS)
        .map_err(|e| platform("management listener", e))?;

    #[cfg(feature = "sensor-simulator")]
    super::sensors::start(spawner, args.seed);
    management::start_timeline(spawner, scenario.events, storage.clone(), HOOKS);
    crate::tasks::spawn(
        spawner,
        "shutdown_task",
        shutdown_task(storage, log_storage),
    );

    announce(&ready_line(&interfaces), args.ready_file.as_deref());
    Ok(())
}

#[embassy_executor::task]
async fn config_task(mut owner: ConfigOwner<FileFlash>, runtime: ConfigRuntime) {
    owner.run(&runtime).await
}

#[embassy_executor::task]
async fn store_task(stale_after_ms: Option<u64>) {
    store::run(EmbassyClock, configure_store(stale_after_ms), || {}).await
}

#[embassy_executor::task]
async fn wifi_task() {
    net::run(SimWifi::new(), net::manager_config()).await
}

#[embassy_executor::task]
async fn ntp_task(network: NetworkHandle<'static, HostIp>) {
    time::run(network).await
}

#[embassy_executor::task]
async fn connectivity_task(
    network: NetworkHandle<'static, HostIp>,
    measurements: MeasurementHandle<'static, crate::tasks::RawMutex>,
) {
    connectivity::run(network, connectivity::packets(measurements, host_entropy)).await
}

#[embassy_executor::task]
async fn reboot_task(
    storage: FileFlash,
    log_storage: FileLogFlash,
    args: Args,
    control: u16,
    manage: u16,
) {
    reboot::due().await;
    storage.flush();
    log_storage.flush();
    restart(&args, control, manage);
    // Without `exec` the best a process can do is to end; a supervisor restarts it.
    error!("Reboot: cannot restart in place, exiting");
    SHUTDOWN.signal(());
}

/// Replaces the process by a fresh one with the same command line and the
/// ports this one bound.
#[cfg(unix)]
fn restart(args: &Args, control: u16, manage: u16) {
    use std::os::unix::process::CommandExt as _;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1));
    if args.reboot_ports.is_none() {
        command
            .arg("--reboot-ports")
            .arg(control.to_string())
            .arg(manage.to_string());
    }
    let error = command.exec();
    error!("Reboot: exec failed: {error}");
}

#[cfg(not(unix))]
fn restart(_: &Args, _: u16, _: u16) {}

#[embassy_executor::task]
async fn shutdown_task(storage: FileFlash, log_storage: FileLogFlash) {
    SHUTDOWN.wait().await;
    info!("Shutting down");
    // Let the response of a `shutdown` command or a final log line out.
    Timer::after_millis(SHUTDOWN_GRACE.as_millis() as u64).await;
    storage.flush();
    log_storage.flush();
    finish();
}

/// Applies the freshness policy of the scenario to the store.
pub fn configure_store(stale_after_ms: Option<u64>) -> impl FnOnce(&mut MeasurementStore) {
    move |store| {
        if let Some(ms) = stale_after_ms {
            for kind in [
                SensorKind::Bmx280,
                SensorKind::Scd4x,
                SensorKind::Sds011,
                SensorKind::Microphone,
            ] {
                store.set_freshness_policy(kind, FreshnessPolicy::new(ms));
            }
        }
    }
}

/// Runs the device until it is told to stop.
pub fn run(args: Args) -> Result<(), StartupError> {
    logging::init_logger(args.log_level);
    let prepared = prepare(args)?;
    install_signal_handler()?;
    let executor = EXECUTOR.init(Executor::new());
    let mut startup = Some(prepared);
    let mut failure = None;
    executor.run_until(
        |spawner| {
            if let Some(prepared) = startup.take()
                && let Err(error) = start(spawner, prepared)
            {
                failure = Some(error);
                finish();
            }
        },
        finished,
    );
    match failure {
        Some(error) => Err(error),
        None => {
            info!("Stopped");
            Ok(())
        }
    }
}

/// The entry point of the binary.
pub fn main() -> ExitCode {
    use clap::Parser as _;

    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("altruist-urban: {error}");
            error.exit_code()
        }
    }
}
