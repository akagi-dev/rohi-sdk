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

//! The composition root of the simulator.
//!
//! It resembles the firmware `main`: one owner per resource, spawned in the
//! same order, with native backends where the firmware has ESP ones.
//!
//! ```text
//! main
//!  ├── open storage ............. FileFlash       (firmware: rohi-cfg flash)
//!  ├── bind control/management .. TCP listeners   (firmware: BLE radio)
//!  │
//!  ├── ConfigOwner task ......... same ConfigStore, same bindings
//!  ├── store task ............... same MeasurementStore owner
//!  ├── WifiManager task ......... same manager over NativeWifi
//!  ├── NTP / telemetry tasks .... same services over NativeIp
//!  ├── sensor tasks ............. same Runner over simulated drivers
//!  └── control task ............. same ControlBle + Controller over TCP
//! ```

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher as _;
use std::net::{SocketAddr, TcpListener};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use altruist_urban::bindings::ConfigRuntime;
use altruist_urban::config::ConfigOwner;
use altruist_urban::tasks::store;
use altruist_urban::{connectivity, net, reboot, time};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;
use log::{error, info};
use rohi_sdk::connectivity::{NONCE_LEN, NonceError, NonceSource};
use rohi_sdk::hal::runtime::{EmbassyClock, MeasurementHandle};
use rohi_sdk::hal::{FreshnessPolicy, MeasurementStore, SensorKind};
use rohi_sdk::net::NetworkHandle;
use static_cell::StaticCell;

use crate::args::Args;
use crate::diagnostics::{
    Failure, SHUTDOWN, StartupError, announce, finish, finished, init_logging,
    install_signal_handler, ready_line,
};
use crate::network::NativeIp;
use crate::scenario::Scenario;
use crate::storage::FileFlash;
use crate::wifi::NativeWifi;
use crate::{control, management, sensors, world};

/// Time the shutdown gives running tasks before the process ends.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(100);

/// Nonces for the telemetry frames.
///
/// Without a seed they come from the randomness of the host; with `--seed`
/// they are a reproducible sequence. Clones share the sequence, so a restart of
/// the telemetry service never repeats a nonce.
#[derive(Clone)]
pub struct SimNonce(Arc<Mutex<Sequence>>);

enum Sequence {
    Host(RandomState, u64),
    Seeded(u64),
}

impl SimNonce {
    pub fn new(seed: Option<u64>) -> Self {
        Self(Arc::new(Mutex::new(match seed {
            Some(seed) => Sequence::Seeded(seed.max(1)),
            None => Sequence::Host(RandomState::new(), 0),
        })))
    }
}

impl NonceSource for SimNonce {
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

static IP: StaticCell<NativeIp> = StaticCell::new();
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
    control: TcpListener,
    manage: TcpListener,
}

fn prepare(args: Args) -> Result<Prepared, StartupError> {
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
    let (control_port, manage_port) = args.ports();
    let control = bind(SocketAddr::new(args.bind, control_port), "control")?;
    let manage = bind(SocketAddr::new(args.bind, manage_port), "management")?;
    let name = scenario
        .device_name
        .clone()
        .unwrap_or_else(|| args.name.clone());
    let name = if args.name != "urban-sim" {
        args.name.clone()
    } else {
        name
    };
    world::init(&scenario, args.seed, name);
    Ok(Prepared {
        args,
        scenario,
        storage,
        control,
        manage,
    })
}

/// Spawns the tasks of the device and announces readiness.
fn start(spawner: Spawner, prepared: Prepared) -> Result<(), StartupError> {
    let Prepared {
        args,
        scenario,
        storage,
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

    // The configuration owner is the single owner of the store; it publishes
    // every section to its consumer before they start.
    let runtime = ConfigRuntime::new(net::handle());
    let mut owner = ConfigOwner::open(storage.clone());
    owner.publish_boot(&runtime);
    spawner.spawn(config_task(owner, runtime).unwrap());

    spawner.spawn(
        reboot_task(
            storage.clone(),
            args.clone(),
            control
                .local_addr()
                .map_err(|e| platform("control address", e))?,
            manage
                .local_addr()
                .map_err(|e| platform("management address", e))?,
        )
        .unwrap(),
    );

    spawner.spawn(store_task(scenario.stale_after_ms).unwrap());

    let ip: &'static NativeIp = IP.init(NativeIp::new());
    let network = NetworkHandle::new(ip);
    spawner.spawn(wifi_task().unwrap());
    spawner.spawn(ntp_task(network).unwrap());
    spawner.spawn(connectivity_task(network, store::handle(), args.seed).unwrap());

    let control_address = control
        .local_addr()
        .map_err(|e| platform("control address", e))?;
    control::start(spawner, control, &name, Some(network))
        .map_err(|e| platform("control listener", e))?;
    let manage_address = management::start(spawner, manage, storage.clone())
        .map_err(|e| platform("management listener", e))?;

    sensors::start(spawner, args.seed);
    management::start_timeline(spawner, scenario.events, storage.clone());
    spawner.spawn(shutdown_task(storage).unwrap());

    announce(
        &ready_line(control_address, manage_address),
        args.ready_file.as_deref(),
    );
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
    net::run(NativeWifi::new(), net::manager_config()).await
}

#[embassy_executor::task]
async fn ntp_task(network: NetworkHandle<'static, NativeIp>) {
    time::run(network).await
}

#[embassy_executor::task]
async fn connectivity_task(
    network: NetworkHandle<'static, NativeIp>,
    measurements: MeasurementHandle<'static, altruist_urban::tasks::RawMutex>,
    seed: Option<u64>,
) {
    connectivity::run(network, measurements, SimNonce::new(seed)).await
}

#[embassy_executor::task]
async fn reboot_task(storage: FileFlash, args: Args, control: SocketAddr, manage: SocketAddr) {
    reboot::due().await;
    storage.flush();
    restart(&args, control, manage);
    // Without `exec` the best a process can do is to end; a supervisor restarts it.
    error!("Reboot: cannot restart in place, exiting");
    SHUTDOWN.signal(());
}

/// Replaces the process by a fresh one with the same command line and the
/// ports this one bound.
#[cfg(unix)]
fn restart(args: &Args, control: SocketAddr, manage: SocketAddr) {
    use std::os::unix::process::CommandExt as _;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(exe);
    command.args(std::env::args_os().skip(1));
    if args.reboot_ports.is_none() {
        command
            .arg("--reboot-ports")
            .arg(control.port().to_string())
            .arg(manage.port().to_string());
    }
    let error = command.exec();
    error!("Reboot: exec failed: {error}");
}

#[cfg(not(unix))]
fn restart(_: &Args, _: SocketAddr, _: SocketAddr) {}

#[embassy_executor::task]
async fn shutdown_task(storage: FileFlash) {
    SHUTDOWN.wait().await;
    info!("Shutting down");
    // Let the response of a `shutdown` command or a final log line out.
    Timer::after_millis(SHUTDOWN_GRACE.as_millis() as u64).await;
    storage.flush();
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

/// Runs the simulator until it is told to stop.
pub fn run(args: Args) -> Result<(), StartupError> {
    init_logging(args.log_level);
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
            eprintln!("altruist-sim: {error}");
            error.exit_code()
        }
    }
}
