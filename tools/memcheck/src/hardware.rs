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
//! Hardware observations of physical stacks, attached to the static report.

use serde::{Deserialize, Serialize};

/// Failure to read the observations.
#[derive(Debug, thiserror::Error)]
pub enum HardwareError {
    /// JSON syntax or type error.
    #[error("invalid hardware observations: {0}")]
    Parse(#[from] serde_json::Error),
}

/// The observation file: `{"observations": [...]}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationFile {
    /// Observations of any chip; the analyzer keeps those of its chip.
    pub observations: Vec<Observation>,
}

/// One measurement on a device. A missing `peak_used_bytes` is `not_measured`, which is not zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// Chip name, e.g. `esp32c3`.
    pub chip: String,
    /// Stack, e.g. `rtos/main`.
    pub stack: String,
    /// Scenario, e.g. `rohi status logging`.
    pub scenario: String,
    /// Firmware commit.
    #[serde(default)]
    pub firmware_sha: Option<String>,
    /// Peak use in bytes.
    #[serde(default)]
    pub peak_used_bytes: Option<u64>,
    /// `enabled`, `disabled` or `not_measured`.
    #[serde(default)]
    pub guard_state: Option<String>,
    /// Device-specific margin that replaces `max(25 % of the stack, 4 KiB)`; must be justified in `notes`.
    #[serde(default)]
    pub required_margin_bytes: Option<u64>,
    /// Free text: board, revision, how the peak was measured.
    #[serde(default)]
    pub notes: Option<String>,
}

impl ObservationFile {
    /// Parses the JSON text.
    pub fn parse(text: &str) -> Result<ObservationFile, HardwareError> {
        Ok(serde_json::from_str(text)?)
    }

    /// The observations of `chip`.
    pub fn for_chip(&self, chip: &str) -> Vec<Observation> {
        self.observations
            .iter()
            .filter(|o| o.chip == chip)
            .cloned()
            .collect()
    }
}
