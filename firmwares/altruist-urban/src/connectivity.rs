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
//! sensors. Configuration comes from the persistent `ConnectivityConfig`
//! section; when none is stored, the build-time defaults apply
//! (`ROHI_CONNECTIVITY_HOST`, `ROHI_CONNECTIVITY_PORT`,
//! `ROHI_CONNECTIVITY_NODE_ID`, `ROHI_CONNECTIVITY_INTERVAL_S`). Without a
//! host nothing is enabled.
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
use esp_hal::rng::Rng;
use log::{info, warn};
use rohi_config::ConfigOverrides;
use rohi_connectivity::config::{DEFAULT_INTERVAL_S, DEFAULT_PORT};
use rohi_connectivity::{
    ConnectivityConfig, ConnectivityOverrides, ConnectivityService, ConnectivityShared,
    ConnectivityStatus, Host, HttpTransport, NONCE_LEN, NonceError, NonceSource, ServiceConfig,
};
use rohi_hal::runtime::MeasurementHandle;
use rohi_hal_esp::EmbassyClock;
use rohi_net::NetworkHandle;

use crate::config::{ConfigHandle, ServiceError};
use crate::identity::StaticSigner;
use crate::resources::{CONNECTIVITY_OUTBOX_DEPTH, Ip};
use crate::tasks::RawMutex;
use crate::{location, time};

static SHARED: ConnectivityShared<RawMutex> = ConnectivityShared::new();

/// The latest stored configuration (`None`: the section is absent), see
/// [`publish`].
static UPDATES: Signal<RawMutex, Option<ConnectivityConfig>> = Signal::new();

/// Hands a committed (or removed) `ConnectivityConfig` section to the running
/// task. Synchronous, so the configuration owner can call it in the commit
/// step; only the latest value matters.
pub fn publish(stored: Option<ConnectivityConfig>) {
    UPDATES.signal(stored);
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

/// Configuration compiled into the firmware: disabled unless a host is set.
/// It applies while the user overrides nothing; overrides are applied to
/// [`ConnectivityConfig::default`].
fn build_defaults() -> ConnectivityConfig {
    let mut config = ConnectivityConfig::default();
    let Some(host) = option_env!("ROHI_CONNECTIVITY_HOST").and_then(Host::new) else {
        return config;
    };
    config.enabled = true;
    config.host = host;
    config.port = env_number(option_env!("ROHI_CONNECTIVITY_PORT")).unwrap_or(DEFAULT_PORT);
    config.node_id = env_number(option_env!("ROHI_CONNECTIVITY_NODE_ID")).unwrap_or(0);
    config.interval_s =
        env_number(option_env!("ROHI_CONNECTIVITY_INTERVAL_S")).unwrap_or(DEFAULT_INTERVAL_S);
    config
}

/// The stored configuration, or the build-time defaults. `None` when
/// telemetry is disabled or the configuration is invalid.
async fn load_config(config: ConfigHandle) -> Option<ConnectivityConfig> {
    let loaded = match config.get::<ConnectivityOverrides>().await {
        Ok(Some(overrides)) if !overrides.is_empty() => overrides.effective(),
        Ok(_) => build_defaults(),
        Err(ServiceError::Unavailable(_)) => build_defaults(),
        Err(error) => {
            warn!("Connectivity: stored configuration unusable: {}", error);
            build_defaults()
        }
    };
    effective(loaded)
}

/// The configuration to run for a stored section, the build-time defaults
/// when there is none.
fn resolve(stored: Option<ConnectivityConfig>) -> Option<ConnectivityConfig> {
    effective(stored.unwrap_or_else(build_defaults))
}

/// `None` when `loaded` is disabled or invalid.
fn effective(loaded: ConnectivityConfig) -> Option<ConnectivityConfig> {
    if !loaded.enabled {
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
    info!(
        "Connectivity: {}:{} every {} s",
        config.host.as_str(),
        config.port,
        config.interval_s
    );
    let signer = StaticSigner;
    let transport = HttpTransport::new(network, config.endpoint(), Delay);
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

/// Runs the telemetry service while it is enabled and restarts it with every
/// published configuration.
#[embassy_executor::task]
pub async fn connectivity_task(
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
    config: ConfigHandle,
) {
    let mut current = load_config(config).await;
    loop {
        EFFECTIVE.lock(|cell| *cell.borrow_mut() = current.clone());
        let stored = match &current {
            Some(config) => {
                match select(serve(network, measurements, config), UPDATES.wait()).await {
                    Either::First(never) => match never {},
                    Either::Second(stored) => stored,
                }
            }
            None => UPDATES.wait().await,
        };
        info!("Connectivity: configuration changed");
        SHARED.reset();
        current = resolve(stored);
    }
}
