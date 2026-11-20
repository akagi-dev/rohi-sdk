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
use crate::tasks::{OnDemand, RawMutex};

/// Packet profile slots of the device, addressed by index `0` and `1`: the storage of the policy,
/// of the runtime state and of the stored section is sized for this many. A stored section with
/// another count is refused, not truncated. Each profile has one public selection and at most one
/// private recipient; Robonomics is the only compiled packet format.
pub const PROFILES: usize = 2;

/// The packet profiles of the device.
pub type ProfileSet = TelemetryConfig<PROFILES>;

/// The persisted section of the packet profiles of the device.
pub type Section = TelemetrySection<PROFILES>;

/// The effective packet profiles, for the capacities of the device.
pub type State = TelemetryState<RawMutex, PROFILES>;

static STATE: OnDemand<State> = OnDemand::new();

/// The effective packet profiles, with the change signal consumers wait on.
pub fn state() -> &'static State {
    STATE.get(State::new)
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
                    state().publish(&empty_profiles(state().generation()));
                    return;
                }
                info!(
                    "Telemetry: {} packet profile(s), generation {}",
                    section.0.profiles.len(),
                    section.0.generation
                );
                state().publish(&section.0);
            }
            None => {
                info!("Telemetry: no packet profiles");
                // The generation continues: a recreated profile is a new
                // incarnation even after a factory reset of the policy.
                state().publish(&empty_profiles(state().generation()));
            }
        }
    }

    fn unreadable(error: &dyn fmt::Display) {
        warn!(
            "Telemetry: stored profiles unreadable, nothing is packaged: {}",
            error
        );
        state().publish(&empty_profiles(state().generation()));
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
    TelemetryService<RawMutex, StoredProfiles, RobonomicsCapabilities, PROFILES>;

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

#[cold]
#[inline(never)]
fn warn_replaced(target: &'static str, error: &dyn fmt::Display) {
    warn!(target: target, "Stored section not readable, a commit replaces it (err={})", error);
}

fn empty_profiles(generation: u32) -> ProfileSet {
    ProfileSet {
        generation,
        ..ProfileSet::default()
    }
}

impl TelemetryStore<PROFILES> for StoredProfiles {
    async fn load(&self) -> Result<ProfileSet, StoreError> {
        match self.0.get::<Section>().await {
            Ok(Some(section)) => {
                if let Err(error) = validate_profiles(&section.0) {
                    warn_invalid_profiles(error);
                    state().publish(&empty_profiles(state().generation()));
                    Err(StoreError::Internal)
                } else {
                    Ok(section.0)
                }
            }
            Ok(None) => Ok(empty_profiles(state().generation())),
            // A section of another version or format is not read and not migrated: the next
            // commit replaces it, so the device is never stuck on it.
            Err(ServiceError::Section(error)) => {
                warn_replaced("telemetry", &error);
                Ok(empty_profiles(state().generation()))
            }
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
    use rohi_sdk::telemetry::{DataSelection, MeasurementId, RecipientAddress, Selection};

    #[test]
    fn missing_profiles_keep_the_runtime_generation() {
        assert_eq!(empty_profiles(42).generation, 42);
    }

    #[test]
    fn invalid_persisted_profiles_are_rejected() {
        let mut profiles = ProfileSet::default();
        profiles.profiles[0].revision = profiles.generation + 1;

        assert_eq!(
            validate_profiles(&profiles),
            Err(ProfileError::InconsistentGeneration)
        );
    }

    #[test]
    fn invalid_persisted_selections_are_rejected() {
        let mut profiles = ProfileSet::default();
        profiles.profiles[0].public.measurements = Selection::NONE.with(MeasurementId(250));

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
    fn an_absent_section_discloses_nothing_and_keeps_the_generation() {
        let profiles = empty_profiles(9);
        assert_eq!(profiles.generation, 9);
        for index in 0..PROFILES as u8 {
            let profile = profiles.get(index).unwrap();
            assert_eq!(profile.public, DataSelection::NONE);
            assert!(profile.private.is_none());
        }
        assert!(profiles.get(PROFILES as u8).is_none());
        let _ = RecipientAddress([1; 32]);
    }
}
