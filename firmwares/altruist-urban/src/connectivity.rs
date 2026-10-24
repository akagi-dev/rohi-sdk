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

use embassy_time::Delay;
use esp_hal::rng::Rng;
use log::{info, warn};
use rohi_connectivity::config::{DEFAULT_INTERVAL_S, DEFAULT_PORT};
use rohi_connectivity::{
    ConnectivityConfig, ConnectivityService, ConnectivityShared, ConnectivityStatus, Host,
    HttpTransport, NONCE_LEN, NonceError, NonceSource, ServiceConfig,
};
use rohi_hal::runtime::MeasurementHandle;
use rohi_hal_esp::EmbassyClock;
use rohi_net::NetworkHandle;

use crate::config::{ConfigHandle, ServiceError};
use crate::identity::StaticSigner;
use crate::resources::{CONNECTIVITY_OUTBOX_DEPTH, Ip};
use crate::tasks::RawMutex;
use crate::time;

static SHARED: ConnectivityShared<RawMutex> = ConnectivityShared::new();

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
fn build_defaults() -> ConnectivityConfig {
    let mut config = ConnectivityConfig::disabled();
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
pub async fn load_config(config: ConfigHandle) -> Option<ConnectivityConfig> {
    let loaded = match config.get::<ConnectivityConfig>().await {
        Ok(Some(stored)) => stored,
        Ok(None) => build_defaults(),
        Err(ServiceError::Unavailable(_)) => build_defaults(),
        Err(error) => {
            warn!("Connectivity: stored configuration unusable: {}", error);
            build_defaults()
        }
    };
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

#[embassy_executor::task]
pub async fn connectivity_task(
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
    config: ConnectivityConfig,
) {
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
        EmbassyClock,
        time::service(),
        &signer,
        EspNonce,
        transport,
        Delay,
    );
    service.run().await
}
