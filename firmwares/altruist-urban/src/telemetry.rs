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
//! The `telemetry` service of the device: the persistent packet profiles.
//!
//! The profiles are one private section kept by the configuration owner task,
//! like the identity: not a managed config section, edited only through the
//! Actions of the `telemetry` service. A commit publishes the new policy to
//! [`state`] in the same step (see [`crate::config`]), so the packet builder
//! never sees a policy that is not stored.

use core::fmt;

use log::{info, warn};
use rohi_sdk::telemetry::managed::{
    ProfileSet, StoreError, TelemetrySection, TelemetryService, TelemetryState, TelemetryStore,
};
use rohi_sdk::telemetry_robonomics::RobonomicsCapabilities;

use crate::bindings::PrivateBinding;
use crate::config::{ConfigHandle, ServiceError};
use crate::tasks::RawMutex;

static STATE: TelemetryState<RawMutex> = TelemetryState::new();

/// The effective packet profiles, with the change signal consumers wait on.
pub fn state() -> &'static TelemetryState<RawMutex> {
    &STATE
}

/// The stored telemetry profiles: published to [`state`] when committed,
/// removed or loaded at boot.
pub struct TelemetryBinding;

impl PrivateBinding for TelemetryBinding {
    type Section = TelemetrySection;

    fn publish(section: Option<&TelemetrySection>) {
        match section {
            Some(section) => {
                info!(
                    "Telemetry: {} packet profile(s), generation {}",
                    section.0.profiles.len(),
                    section.0.generation
                );
                STATE.publish(&section.0);
            }
            None => {
                info!("Telemetry: no packet profiles");
                // The generation continues: a recreated profile is a new
                // incarnation even after a factory reset of the policy.
                STATE.publish(&ProfileSet {
                    generation: STATE.generation(),
                    ..ProfileSet::default()
                });
            }
        }
    }

    fn unreadable(error: &dyn fmt::Display) {
        warn!(
            "Telemetry: stored profiles unreadable, nothing is packaged: {}",
            error
        );
    }
}

/// [`TelemetryStore`] over the configuration owner task.
#[derive(Clone, Copy)]
pub struct StoredProfiles(ConfigHandle);

/// The profile store behind `config`.
pub const fn store(config: ConfigHandle) -> StoredProfiles {
    StoredProfiles(config)
}

/// The `telemetry` service as the firmware composes it.
pub type UrbanTelemetry = TelemetryService<RawMutex, StoredProfiles, RobonomicsCapabilities>;

/// The service over the profiles stored behind `config`.
pub const fn service(config: ConfigHandle) -> UrbanTelemetry {
    TelemetryService::new(store(config), RobonomicsCapabilities)
}

fn store_error(error: ServiceError) -> StoreError {
    match error {
        ServiceError::Unavailable(_) => StoreError::Unavailable,
        _ => StoreError::Internal,
    }
}

impl TelemetryStore for StoredProfiles {
    async fn load(&self) -> Result<ProfileSet, StoreError> {
        match self.0.get::<TelemetrySection>().await {
            Ok(section) => Ok(section.map(|section| section.0).unwrap_or_default()),
            Err(error) => Err(store_error(error)),
        }
    }

    async fn commit(&self, candidate: &ProfileSet) -> Result<(), StoreError> {
        // The owner task commits and publishes in one step.
        self.0
            .set(&TelemetrySection(candidate.clone()))
            .await
            .map_err(store_error)
    }
}
