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
use std::net::{SocketAddr, TcpListener};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;
use log::{error, info};
use rohi_sdk::connectivity::{NONCE_LEN, NonceError, NonceSource};
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

/// Nonces for the telemetry frames.
///
/// Without a seed they come from the randomness of the host; with `--seed`
/// they are a reproducible sequence. Clones share the sequence, so a restart of
/// the telemetry service never repeats a nonce.
#[derive(Clone)]
pub struct HostNonce(Arc<Mutex<Sequence>>);

enum Sequence {
    Host(RandomState, u64),
    Seeded(u64),
}

impl HostNonce {
    /// A source of nonces, reproducible with `seed`.
    pub fn new(seed: Option<u64>) -> Self {
        Self(Arc::new(Mutex::new(match seed {
            Some(seed) => Sequence::Seeded(seed.max(1)),
            None => Sequence::Host(RandomState::new(), 0),
        })))
    }
}

impl NonceSource for HostNonce {
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        let mut sequence = self.0.lock().map_err(|_| NonceError)?;
        for chunk in dst.chunks_mut(8) {
            let word = match &mut *sequence {
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
        Ok(())
    }
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
    #[cfg(feature = "std-tcp")]
    control: TcpListener,
    manage: TcpListener,
}

fn prepare(args: Args) -> Result<Prepared, StartupError> {
    if !args.bind.is_loopback() && !args.unsafe_allow_remote {
        return Err(StartupError::new(
            Failure::Bind,
            format!(
                "refusing to listen on {}: the control and management interfaces have no \
                 authentication; use a loopback address or pass --unsafe-allow-remote",
                args.bind
            ),
        ));
    }
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
    let (_control_port, manage_port) = args.ports();
    #[cfg(feature = "std-tcp")]
    let control = bind(SocketAddr::new(args.bind, _control_port), "control")?;
    let manage = bind(SocketAddr::new(args.bind, manage_port), "management")?;
    let name = args
        .name
        .clone()
        .or_else(|| scenario.device_name.clone())
        .unwrap_or_else(|| DEFAULT_NAME.to_owned());
    world::init(&scenario, args.seed, name);
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
    crate::tasks::spawn(spawner, "config_task", config_task(owner, runtime));

    let mut interfaces = Interfaces::default();
    #[cfg(feature = "std-tcp")]
    {
        interfaces.control = Some(
            control
                .local_addr()
                .map_err(|e| platform("control address", e))?,
        );
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
        connectivity_task(network, store::handle(), args.seed),
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
    #[cfg(feature = "std-ble")]
    crate::tasks::spawn(spawner, "ble_task", control::ble_task(ble, controller));
    #[cfg(feature = "std-tcp")]
    {
        let acceptor = rohi_sdk::host::TcpAcceptor::new(control)
            .map_err(|e| platform("control listener", e))?;
        crate::tasks::spawn(spawner, "tcp_task", control::tcp_task(acceptor, controller));
    }
    #[cfg(not(feature = "std-tcp"))]
    let _ = controller;

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
    seed: Option<u64>,
) {
    connectivity::run(network, measurements, HostNonce::new(seed)).await
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
