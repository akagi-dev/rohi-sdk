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
//! A managed config section with a node id, an interval and an optional
//! endpoint, named `connectivity`.
//!
//! The tool knows no module by name; the tests need one real-looking
//! section with a numeric range, a unit and an optional text value. The
//! firmware has no such section any more (routes are edited with Actions), so
//! the section lives here, as a fixture.

use rohi_config::Text;
use rohi_connectivity::{Endpoint, EndpointUri};
use rohi_service::{Config, FieldError};

const MIN_INTERVAL_S: u32 = 10;
const MAX_INTERVAL_S: u32 = 24 * 60 * 60;

fn validate_section(config: &ConnectivityConfig) -> Result<(), FieldError> {
    if let Some(uri) = &config.endpoint
        && uri.endpoint().is_none()
    {
        return Err(FieldError::Invalid);
    }
    Ok(())
}

/// The fixture section.
#[derive(Clone, PartialEq, Eq, Debug, Config)]
#[config(
    name = "connectivity",
    overrides = ConnectivityOverrides,
    version = 1,
    validate = validate_section
)]
pub struct ConnectivityConfig {
    /// A node identifier.
    #[config(id = 2)]
    pub node_id: u64,
    /// Seconds between frames.
    #[config(id = 3, unit = S, min = MIN_INTERVAL_S, max = MAX_INTERVAL_S)]
    pub interval_s: u32,
    /// `http://host[:port]`; `None` disables.
    #[config(id = 4, with = Text)]
    pub endpoint: Option<EndpointUri>,
}

impl ConnectivityConfig {
    /// The parts of the endpoint.
    pub fn endpoint(&self) -> Option<Endpoint<'_>> {
        self.endpoint.as_ref()?.endpoint()
    }
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self {
            node_id: 0,
            interval_s: 60,
            endpoint: None,
        }
    }
}
