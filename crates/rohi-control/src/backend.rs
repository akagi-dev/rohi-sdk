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
    ConfigRegistry, DeviceHealth, ServiceRegistry, ServiceStatusSummary, default_device_health,
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
/// The dispatcher knows no module. Config sections, services and their
/// actions are discovered through the registries of `rohi-service`.
pub trait ControlBackend {
    /// The config sections of the device.
    type Config: ConfigRegistry;
    /// The services of the device.
    type Services: ServiceRegistry;

    /// Static device description.
    fn device_info(&self) -> DeviceInfo;

    /// Device monotonic time.
    fn uptime_ms(&self) -> u64;

    /// The config registry.
    fn config(&self) -> &Self::Config;

    /// The service registry.
    fn services(&self) -> &Self::Services;

    /// Health of the whole device, decided by the firmware from the compact
    /// status of its services. The default is
    /// [`default_device_health`]; override it for redundancy rules the
    /// default cannot know.
    fn device_health(&self, services: &[ServiceStatusSummary]) -> DeviceHealth {
        default_device_health(services)
    }
}

/// A [`ControlBackend`] built from its parts.
pub struct Device<C, S> {
    info: DeviceInfo,
    uptime_ms: fn() -> u64,
    config: C,
    services: S,
}

impl<C: ConfigRegistry, S: ServiceRegistry> Device<C, S> {
    /// A device with the given registries; `uptime_ms` reads the monotonic
    /// clock.
    pub const fn new(info: DeviceInfo, uptime_ms: fn() -> u64, config: C, services: S) -> Self {
        Self {
            info,
            uptime_ms,
            config,
            services,
        }
    }
}

impl<C: ConfigRegistry, S: ServiceRegistry> ControlBackend for Device<C, S> {
    type Config = C;
    type Services = S;

    fn device_info(&self) -> DeviceInfo {
        self.info
    }

    fn uptime_ms(&self) -> u64 {
        (self.uptime_ms)()
    }

    fn config(&self) -> &C {
        &self.config
    }

    fn services(&self) -> &S {
        &self.services
    }
}
