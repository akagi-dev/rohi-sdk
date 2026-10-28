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
use rohi_service::{
    ActionRegistry, ConfigRegistry, DeviceHealth, ServiceStatusSummary, StatusRegistry,
    default_device_health,
};

/// Static description of the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Firmware name and version, for example `altruist-urban-0.1.0`.
    pub firmware: &'static str,
    /// Board and chip, for example `urban-esp32c3`.
    pub board: &'static str,
}

/// What the dispatcher needs from the firmware: the device description and
/// the registries it serves.
///
/// The dispatcher knows no module. Config sections, service status and service
/// actions are discovered through three independent registries of
/// `rohi-service`; a service appears in the status and the action registry
/// only if it has that capability.
pub trait ControlBackend {
    /// The config sections of the device.
    type Config: ConfigRegistry;
    /// The services that report status.
    type Statuses: StatusRegistry;
    /// The services that offer actions.
    type Actions: ActionRegistry;

    /// Static device description.
    fn device_info(&self) -> DeviceInfo;

    /// Device monotonic time.
    fn uptime_ms(&self) -> u64;

    /// The config registry.
    fn config(&self) -> &Self::Config;

    /// The status registry.
    fn statuses(&self) -> &Self::Statuses;

    /// The action registry.
    fn actions(&self) -> &Self::Actions;

    /// Schedules a reboot of the device and returns `true`, or returns `false`
    /// when the device cannot reboot. It must not reboot before it returns: the
    /// response to the request still has to be sent, so the firmware defers the
    /// reset.
    fn reboot(&self) -> bool {
        false
    }

    /// Health of the whole device, decided by the firmware from the compact
    /// status of its services. The default is
    /// [`default_device_health`]; override it for redundancy rules the
    /// default cannot know.
    fn device_health(&self, services: &[ServiceStatusSummary]) -> DeviceHealth {
        default_device_health(services)
    }
}

/// A [`ControlBackend`] built from its parts.
pub struct Device<C, S, A> {
    info: DeviceInfo,
    uptime_ms: fn() -> u64,
    config: C,
    statuses: S,
    actions: A,
    reboot: Option<fn()>,
}

impl<C: ConfigRegistry, S: StatusRegistry, A: ActionRegistry> Device<C, S, A> {
    /// A device with the given registries; `uptime_ms` reads the monotonic
    /// clock.
    ///
    /// Two services of one registry that share an id, or an invalid action
    /// table, fail the build: requests would reach the wrong service.
    ///
    /// # Panics
    ///
    /// When two config sections share an id, or when two services of the
    /// status and the action registry share an id but not the name (a hash
    /// collision of two different services). This is checked in every build,
    /// once, at start.
    pub fn new(
        info: DeviceInfo,
        uptime_ms: fn() -> u64,
        config: C,
        statuses: S,
        actions: A,
    ) -> Self {
        const {
            assert!(S::IDS_UNIQUE, "two status services share an id");
            assert!(A::IDS_UNIQUE, "two action services share an id");
            assert!(
                A::SCHEMAS_VALID,
                "invalid action table: ids and names must be unique and fit"
            );
        };
        assert!(config.ids_are_unique(), "two config sections share an id");
        assert!(
            same_services_share_names(&statuses, &actions),
            "two different services share an id"
        );
        Self {
            info,
            uptime_ms,
            config,
            statuses,
            actions,
            reboot: None,
        }
    }

    /// Makes the device rebootable: `reboot` schedules the reset, see
    /// [`ControlBackend::reboot`].
    pub const fn with_reboot(mut self, reboot: fn()) -> Self {
        self.reboot = Some(reboot);
        self
    }
}

impl<C: ConfigRegistry, S: StatusRegistry, A: ActionRegistry> ControlBackend for Device<C, S, A> {
    type Config = C;
    type Statuses = S;
    type Actions = A;

    fn device_info(&self) -> DeviceInfo {
        self.info
    }

    fn uptime_ms(&self) -> u64 {
        (self.uptime_ms)()
    }

    fn config(&self) -> &C {
        &self.config
    }

    fn statuses(&self) -> &S {
        &self.statuses
    }

    fn actions(&self) -> &A {
        &self.actions
    }

    fn reboot(&self) -> bool {
        self.reboot.map(|reboot| reboot()).is_some()
    }
}

/// Whether services that are in both registries agree on the name of an id.
fn same_services_share_names<S: StatusRegistry, A: ActionRegistry>(
    statuses: &S,
    actions: &A,
) -> bool {
    (0..actions.service_count()).all(|index| {
        let Some(identity) = actions.service_identity(index) else {
            return true;
        };
        (0..statuses.service_count()).all(|other| {
            statuses
                .service_info(other)
                .is_none_or(|info| info.id != identity.id || info.name == identity.name)
        })
    })
}
