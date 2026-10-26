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
//! The location as a service of the status plane.

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;
use rohi_service::{
    FieldDescriptor, Issue, IssueSeverity, MAX_ISSUES, ManagedService, ServiceHealth, ServiceState,
    ServiceSummary, StatusWriter, ValueKind, WriteError,
};

use crate::{LocationShared, LocationSourceKind};

/// Field ids of the `location` service status.
pub mod status_field {
    /// Latitude, degrees.
    pub const LATITUDE: u32 = 1;
    /// Longitude, degrees.
    pub const LONGITUDE: u32 = 2;
    /// Height above mean sea level, metres; absent when unknown.
    pub const HEIGHT_M: u32 = 3;
    /// `configured` or `gps`.
    pub const SOURCE: u32 = 4;
}

/// The current location as a `location` service.
///
/// Not knowing where the device is is not an error, so the service is
/// `disabled` until a location is set; consumers that need it (telemetry)
/// report their own degradation.
pub struct LocationService<'a, M: RawMutex>(pub &'a LocationShared<M>);

impl<M: RawMutex> ManagedService for LocationService<'_, M> {
    const NAME: &'static str = "location";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::LATITUDE, "latitude", ValueKind::F64)
            .describe("Latitude, WGS84")
            .unit("deg"),
        FieldDescriptor::new(status_field::LONGITUDE, "longitude", ValueKind::F64)
            .describe("Longitude, WGS84")
            .unit("deg"),
        FieldDescriptor::new(status_field::HEIGHT_M, "height_m", ValueKind::F64)
            .describe("Height above mean sea level")
            .unit("m"),
        FieldDescriptor::new(status_field::SOURCE, "source", ValueKind::Enum)
            .describe("Where the location comes from")
            .variants(&["configured", "gps"]),
    ];
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        if self.0.current().is_some() {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        } else {
            ServiceSummary::new(ServiceState::Disabled, ServiceHealth::Unknown, false)
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if self.0.current().is_none() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Info,
                code: "not_set",
                message: "No location is set; `rohi config set location latitude=.. longitude=..`",
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let Some(location) = self.0.current() else {
            return Ok(());
        };
        out.f64(status_field::LATITUDE, location.latitude)?;
        out.f64(status_field::LONGITUDE, location.longitude)?;
        if let Some(height) = location.height_m {
            out.f64(status_field::HEIGHT_M, height)?;
        }
        out.variant(
            status_field::SOURCE,
            match location.source {
                LocationSourceKind::Configured => "configured",
                LocationSourceKind::Gps => "gps",
            },
        )
    }
}
