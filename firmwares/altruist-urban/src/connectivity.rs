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
//! Telemetry uplink: signed `connectivity-protocol` frames over HTTP.
//!
//! The service runs in its own task and only reads measurement snapshots, so
//! Wi-Fi, DNS, TCP, server, time or authorization failures never reach the
//! sensors. Configuration is `ConnectivityConfig::default()`, the build-time
//! [`firmware_overrides`] (`ROHI_CONNECTIVITY_ENDPOINT`, an
//! `http://host[:port]` URI, `ROHI_CONNECTIVITY_NODE_ID`,
//! `ROHI_CONNECTIVITY_INTERVAL_S`; without an endpoint nothing is enabled) and the overrides of the persistent
//! `ConnectivityConfig` section.
//!
//! The task is always running once the network is up and follows the
//! configuration: every commit or removal of the section is published by the
//! configuration owner task (see [`publish`]) and the service is stopped,
//! rebuilt and restarted with the new values, or left stopped when the new
//! configuration is disabled or invalid. Frames still queued in the outbox of
//! the replaced service are dropped.

use core::cell::RefCell;
use core::convert::Infallible;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::signal::Signal;
use embassy_time::Delay;
use esp_hal::rng::Rng;
use log::{info, warn};
use rohi_config::Override;
use rohi_connectivity::{
    ConnectivityConfig, ConnectivityOverrides, ConnectivityService, ConnectivityShared,
    ConnectivityStatus, EndpointUri, HttpTransport, NONCE_LEN, NonceError, NonceSource,
    ServiceConfig,
};
use rohi_hal::runtime::MeasurementHandle;
use rohi_hal_esp::EmbassyClock;
use rohi_net::NetworkHandle;

use crate::identity::StaticSigner;
use crate::resources::{CONNECTIVITY_OUTBOX_DEPTH, Ip};
use crate::tasks::RawMutex;
use crate::{location, time};

static SHARED: ConnectivityShared<RawMutex> = ConnectivityShared::new();

/// The latest effective configuration, see [`publish`].
static UPDATES: Signal<RawMutex, ConnectivityConfig> = Signal::new();

/// Hands the effective `ConnectivityConfig` (the defaults, the
/// [`firmware_overrides`] and the committed user overrides) to the running task.
/// Synchronous, so the configuration owner can call it in the commit step;
/// only the latest value matters.
pub fn publish(config: ConnectivityConfig) {
    UPDATES.signal(config);
}

/// Status published by the telemetry service.
pub fn shared() -> &'static ConnectivityShared<RawMutex> {
    &SHARED
}

/// The configuration the telemetry service currently runs; `None` while it is
/// disabled or the configuration is invalid.
pub fn effective_config() -> Option<ConnectivityConfig> {
    EFFECTIVE.lock(|cell| cell.borrow().clone())
}

static EFFECTIVE: BlockingMutex<RawMutex, RefCell<Option<ConnectivityConfig>>> =
    BlockingMutex::new(RefCell::new(None));

/// Diagnostics of the telemetry service.
pub fn status() -> ConnectivityStatus {
    SHARED.status()
}

/// Nonces from the hardware RNG.
///
/// The RNG is a true random source while the radio runs, which is whenever
/// telemetry can be delivered; nonces only have to be unique for replay
/// protection, which holds in either mode.
struct EspNonce;

impl NonceSource for EspNonce {
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        Rng::new().read(dst);
        Ok(())
    }
}

fn env_number<T: core::str::FromStr>(value: Option<&str>) -> Option<T> {
    value.and_then(|value| value.parse().ok())
}

/// Policy compiled into the firmware: nothing is enabled unless a valid endpoint
/// (`http://host[:port]`) is set, and the other variables are ignored without
/// one. It is the firmware
/// layer under the overrides of the user.
pub fn firmware_overrides() -> ConnectivityOverrides {
    let mut out = ConnectivityOverrides::default();
    let Some(endpoint) = option_env!("ROHI_CONNECTIVITY_ENDPOINT").and_then(EndpointUri::new)
    else {
        return out;
    };
    out.endpoint = Override::Value(Some(endpoint));
    if let Some(node_id) = env_number(option_env!("ROHI_CONNECTIVITY_NODE_ID")) {
        out.node_id = Override::Value(node_id);
    }
    if let Some(interval) = env_number(option_env!("ROHI_CONNECTIVITY_INTERVAL_S")) {
        out.interval_s = Override::Value(interval);
    }
    out
}

/// `None` when `loaded` is disabled (it has no endpoint) or invalid.
fn effective(loaded: ConnectivityConfig) -> Option<ConnectivityConfig> {
    if !loaded.is_enabled() {
        info!("Connectivity: disabled");
        return None;
    }
    match loaded.validate() {
        Ok(()) => Some(loaded),
        Err(error) => {
            warn!("Connectivity: {}, telemetry disabled", error);
            None
        }
    }
}

/// Runs the service for `config` until the task is cancelled.
async fn serve(
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
    config: &ConnectivityConfig,
) -> Infallible {
    let Some(endpoint) = config.endpoint() else {
        // `effective` only hands over a valid, enabled configuration.
        return core::future::pending().await;
    };
    info!(
        "Connectivity: {}:{} every {} s",
        endpoint.host, endpoint.port, config.interval_s
    );
    let signer = StaticSigner;
    let transport = HttpTransport::new(network, endpoint, Delay);
    let mut service: ConnectivityService<
        '_,
        RawMutex,
        _,
        _,
        _,
        _,
        _,
        _,
        _,
        CONNECTIVITY_OUTBOX_DEPTH,
    > = ConnectivityService::new(
        ServiceConfig::new(config.node_id, config.interval_ms()),
        &SHARED,
        measurements,
        location::shared(),
        EmbassyClock,
        time::service(),
        &signer,
        EspNonce,
        transport,
        Delay,
    );
    service.run().await
}

/// Starts the telemetry task, see [`connectivity_task`]. It reads snapshots
/// only: it can neither delay nor stop the sensors, whatever the uplink does.
pub fn start(
    spawner: Spawner,
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
) {
    spawner.spawn(connectivity_task(network, measurements).unwrap());
}

/// Runs the telemetry service while it is enabled and restarts it with every
/// published configuration.
#[embassy_executor::task]
pub async fn connectivity_task(
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
) {
    // The configuration owner publishes the effective configuration at boot,
    // before this task runs, like after every commit.
    let mut current = effective(UPDATES.wait().await);
    loop {
        EFFECTIVE.lock(|cell| *cell.borrow_mut() = current.clone());
        let published = match &current {
            Some(config) => {
                match select(serve(network, measurements, config), UPDATES.wait()).await {
                    Either::First(never) => match never {},
                    Either::Second(published) => published,
                }
            }
            None => UPDATES.wait().await,
        };
        info!("Connectivity: configuration changed");
        SHARED.reset();
        current = effective(published);
    }
}
