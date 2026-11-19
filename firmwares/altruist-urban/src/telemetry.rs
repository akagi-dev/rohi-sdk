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
use rohi_sdk::telemetry::ProfileError;
use rohi_sdk::telemetry::TelemetryConfig;
use rohi_sdk::telemetry::managed::{
    StoreError, TelemetrySection, TelemetryService, TelemetryState, TelemetryStore,
};
use rohi_sdk::telemetry_robonomics::RobonomicsCapabilities;

use crate::bindings::PrivateBinding;
use crate::config::{ConfigHandle, ServiceError};
use crate::tasks::RawMutex;

/// Packet profiles of the device: the storage of the policy, of the runtime state and of the
/// stored section is sized for this many. A stored section with more is refused, not truncated.
pub const PROFILES: usize = 2;
/// Private recipients of a profile; Robonomics is the only compiled packet format.
pub const RECIPIENTS: usize = 1;

/// The packet profiles of the device.
pub type ProfileSet = TelemetryConfig<PROFILES, RECIPIENTS>;

/// The persisted section of the packet profiles of the device.
pub type Section = TelemetrySection<PROFILES, RECIPIENTS>;

/// The effective packet profiles, for the capacities of the device.
pub type State = TelemetryState<RawMutex, PROFILES, RECIPIENTS>;

static STATE: State = TelemetryState::new();

/// The effective packet profiles, with the change signal consumers wait on.
pub fn state() -> &'static State {
    &STATE
}

/// The stored telemetry profiles: published to [`state`] when committed,
/// removed or loaded at boot.
pub struct TelemetryBinding;

impl PrivateBinding for TelemetryBinding {
    type Section = Section;

    fn publish(section: Option<&Section>) {
        match section {
            Some(section) => {
                if let Err(error) = validate_profiles(&section.0) {
                    warn_invalid_profiles(error);
                    STATE.publish(&empty_profiles(STATE.generation()));
                    return;
                }
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
                STATE.publish(&empty_profiles(STATE.generation()));
            }
        }
    }

    fn unreadable(error: &dyn fmt::Display) {
        warn!(
            "Telemetry: stored profiles unreadable, nothing is packaged: {}",
            error
        );
        STATE.publish(&empty_profiles(STATE.generation()));
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
pub type UrbanTelemetry =
    TelemetryService<RawMutex, StoredProfiles, RobonomicsCapabilities, PROFILES, RECIPIENTS>;

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

fn validate_profiles(profiles: &ProfileSet) -> Result<(), ProfileError> {
    profiles.validate(&RobonomicsCapabilities)
}

#[cold]
#[inline(never)]
fn warn_invalid_profiles(error: ProfileError) {
    warn!(
        target: "telemetry",
        "Stored profiles are invalid, nothing is packaged (err={:?})",
        error
    );
}

fn empty_profiles(generation: u32) -> ProfileSet {
    ProfileSet {
        generation,
        ..ProfileSet::default()
    }
}

impl TelemetryStore<PROFILES, RECIPIENTS> for StoredProfiles {
    async fn load(&self) -> Result<ProfileSet, StoreError> {
        match self.0.get::<Section>().await {
            Ok(Some(section)) => {
                if let Err(error) = validate_profiles(&section.0) {
                    warn_invalid_profiles(error);
                    STATE.publish(&empty_profiles(STATE.generation()));
                    Err(StoreError::Internal)
                } else {
                    Ok(section.0)
                }
            }
            Ok(None) => Ok(empty_profiles(STATE.generation())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rohi_sdk::telemetry::{PacketFormatId, PacketProfileId};

    #[test]
    fn missing_profiles_keep_the_runtime_generation() {
        assert_eq!(empty_profiles(42).generation, 42);
    }

    #[test]
    fn invalid_persisted_profiles_are_rejected() {
        let mut profiles = ProfileSet::default();
        profiles
            .add_profile("weather", PacketFormatId::Robonomics)
            .unwrap();
        profiles.profiles[0].revision = profiles.generation + 1;

        assert_eq!(validate_profiles(&profiles), Err(ProfileError::NotFound));
    }

    #[test]
    fn invalid_persisted_selections_are_rejected() {
        let mut profiles = ProfileSet::default();
        profiles
            .add_profile("weather", PacketFormatId::Robonomics)
            .unwrap();
        profiles.profiles[0].public.measurements =
            rohi_sdk::telemetry::Selection::NONE.with(rohi_sdk::telemetry::MeasurementId(250));

        assert_eq!(
            validate_profiles(&profiles),
            Err(ProfileError::Selection(
                rohi_sdk::telemetry::SelectionError::Unsupported(
                    rohi_sdk::telemetry::MeasurementId(250)
                )
            ))
        );
    }

    #[test]
    fn an_absent_section_cannot_reuse_a_profile_incarnation() {
        let profiles = empty_profiles(9);
        assert_eq!(profiles.generation, 9);
        assert!(
            profiles
                .get(PacketProfileId::new("weather").unwrap().as_str())
                .is_none()
        );
    }
}
