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
//! Budgets: absolute per-chip limits and allowed growth against a base.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Failure to read the budget file.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    /// TOML syntax or type error.
    #[error("invalid budget file: {0}")]
    Parse(#[from] toml::de::Error),
    /// The chip has no section.
    #[error("the budget file has no [chips.{0}] section")]
    NoChip(String),
}

/// The whole budget file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetFile {
    /// Allowed growth against the base report.
    #[serde(default)]
    pub regression: Regression,
    /// Per-chip budgets.
    #[serde(default)]
    pub chips: BTreeMap<String, ChipBudget>,
}

/// Growth against the base that fails the check. A growth below the limit is reported but allowed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Regression {
    /// Largest allowed growth of `.data`, `.bss` or the sum of task pools.
    #[serde(default = "default_section_growth")]
    pub max_section_growth: u64,
    /// Largest allowed growth of one task pool.
    #[serde(default = "default_task_growth")]
    pub max_task_growth: u64,
    /// Largest allowed growth of one function frame.
    #[serde(default = "default_frame_growth")]
    pub max_frame_growth: u64,
    /// Approved exceptions to `max_frame_growth`, for example a frame that grew because a call was inlined into
    /// it while the frames it replaced shrank by more. Each needs a reason.
    #[serde(default)]
    pub approved_frame_growth: Vec<ApprovedGrowth>,
}

/// A reviewed exception to the frame growth limit.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedGrowth {
    /// Part of the demangled function name; the exception applies to every function that contains it.
    pub symbol: String,
    /// Largest allowed growth of such a frame.
    pub max_growth: u64,
    /// Why the growth is acceptable.
    pub reason: String,
}

fn default_section_growth() -> u64 {
    4096
}

fn default_task_growth() -> u64 {
    1024
}

fn default_frame_growth() -> u64 {
    512
}

impl Default for Regression {
    fn default() -> Self {
        Regression {
            max_section_growth: default_section_growth(),
            max_task_growth: default_task_growth(),
            max_frame_growth: default_frame_growth(),
            approved_frame_growth: Vec::new(),
        }
    }
}

/// Budgets of one chip. An absent limit is not enforced.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChipBudget {
    /// Upper bound of `.data`.
    pub max_data: Option<u64>,
    /// Upper bound of `.bss`, which includes the wireless heap and the task pools.
    pub max_bss: Option<u64>,
    /// Upper bound of the sum of the task pools.
    pub max_task_ram: Option<u64>,
    /// Lower bound of the main stack.
    pub min_main_stack: Option<u64>,
    /// Upper bound of a single function frame.
    pub max_frame: Option<u64>,
    /// Frame focus groups: name to demangled path prefixes.
    #[serde(default)]
    pub focus: BTreeMap<String, Vec<String>>,
    /// Approved exceptions.
    #[serde(default)]
    pub exceptions: Exceptions,
}

/// Exceptions that must be approved in review, because each one weakens the gate.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exceptions {
    /// Accept an image without `.stack_sizes`.
    #[serde(default)]
    pub allow_missing_stack_sizes: bool,
    /// Accept an image without the task pool inventory (stripped symbols).
    #[serde(default)]
    pub allow_missing_tasks: bool,
}

impl BudgetFile {
    /// Parses the TOML text.
    pub fn parse(text: &str) -> Result<BudgetFile, BudgetError> {
        Ok(toml::from_str(text)?)
    }

    /// The budgets of `chip`.
    pub fn chip(&self, chip: &str) -> Result<&ChipBudget, BudgetError> {
        self.chips
            .get(chip)
            .ok_or_else(|| BudgetError::NoChip(chip.to_string()))
    }
}
