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

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::signal::Signal;
use embassy_time::Delay;
use log::{info, warn};
use rohi_sdk::config::Override;
use rohi_sdk::connectivity::{
    ConnectivityConfig, ConnectivityOverrides, ConnectivityService, ConnectivityShared,
    ConnectivityStatus, EndpointUri, HttpTransport, NonceSource, ServiceConfig,
};
use rohi_sdk::net::{IpBackend, NetworkHandle};
use rohi_sdk::sensors::runtime::MeasurementHandle;
use rohi_sdk::time::EmbassyClock;

use crate::identity::StaticSigner;
use crate::tasks::RawMutex;
use crate::{location, time};

/// Frames the telemetry outbox holds while the uplink is down. Each frame is
/// up to `rohi_connectivity_proto::limits::MAX_ENVELOPE_LEN` bytes; when the
/// outbox is full new frames are skipped, never queued over old ones.
pub const CONNECTIVITY_OUTBOX_DEPTH: usize = 4;

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
async fn serve<I: IpBackend, N: NonceSource>(
    network: NetworkHandle<'static, I>,
    measurements: MeasurementHandle<'static, RawMutex>,
    nonce: N,
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
        nonce,
        transport,
        Delay,
    );
    service.run().await
}

/// Runs the telemetry service while it is enabled and restarts it with every
/// published configuration; never returns. The platform runs it in its
/// telemetry task. It reads snapshots only: it can neither delay nor stop the
/// sensors, whatever the uplink does.
///
/// `nonce` produces the frame nonces: a true random source on hardware, a
/// seeded one for a reproducible simulation.
pub async fn run<I: IpBackend, N: NonceSource + Clone>(
    network: NetworkHandle<'static, I>,
    measurements: MeasurementHandle<'static, RawMutex>,
    nonce: N,
) -> ! {
    // The configuration owner publishes the effective configuration at boot,
    // before this task runs, like after every commit.
    let mut current = effective(UPDATES.wait().await);
    loop {
        EFFECTIVE.lock(|cell| *cell.borrow_mut() = current.clone());
        let published = match &current {
            Some(config) => {
                match select(
                    serve(network, measurements, nonce.clone(), config),
                    UPDATES.wait(),
                )
                .await
                {
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
