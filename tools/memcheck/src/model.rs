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
//! The report schema: everything `memcheck` writes to `task-memory-<chip>.json`.
//!
//! Every number comes from a linked ELF or from a hardware observation that is identified as such. A value that cannot
//! be derived is `None` (rendered `unknown` or `not measured`), never `0`.

use serde::{Deserialize, Serialize};

/// Version of the JSON layout; bumped on every incompatible change.
pub const SCHEMA_VERSION: u32 = 1;

/// The result of one analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// [`SCHEMA_VERSION`] at the time the report was written.
    pub schema_version: u32,
    /// Chip name as given on the command line, e.g. `esp32c3`.
    pub chip: String,
    /// How the analyzed images were built.
    pub build: BuildInfo,
    /// Static RAM sections and the largest statics.
    pub ram: RamReport,
    /// Embassy task pools found in the production image.
    pub tasks: TasksReport,
    /// The physical stacks.
    pub stacks: StacksReport,
    /// LLVM stack-frame metadata of the analysis image.
    pub frames: FramesReport,
    /// Differences against a base report, when one was given.
    pub comparison: Option<Comparison>,
    /// Hardware observations of physical stacks, as supplied.
    pub hardware: Vec<HardwareResult>,
    /// Every enforced budget and its verdict.
    pub checks: Vec<Check>,
    /// Problems and notes, most severe first.
    pub diagnostics: Vec<Diagnostic>,
    /// The overall verdict.
    pub outcome: Outcome,
}

/// Build metadata. Free-form strings are passed by CI and are compared between a base and a pull request run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Compiler identification, e.g. the output of `rustc -Vv`.
    pub toolchain: Option<String>,
    /// Commit the image was built from.
    pub git_sha: Option<String>,
    /// Rust target triple.
    pub target: Option<String>,
    /// Cargo profile.
    pub profile: Option<String>,
    /// Cargo features.
    pub features: Option<String>,
    /// Additional compiler and linker flags of the production image.
    pub flags: Option<String>,
    /// Flags of the analysis image, which must differ from `flags` only by the stack-size emission.
    pub analysis_flags: Option<String>,
    /// Size in bytes of the production ELF.
    pub elf_bytes: u64,
    /// Size in bytes of the analysis ELF, when one was given.
    pub analysis_elf_bytes: Option<u64>,
}

/// How much to trust a derived value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Read directly from a sized symbol or a linker symbol.
    Exact,
    /// Derived by a heuristic; see the `evidence` text.
    Estimated,
    /// Could not be derived; the size is not reported.
    Unknown,
}

/// RAM sections of the image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RamReport {
    /// Writable data and zero-initialized sections, in address order.
    pub sections: Vec<SectionSize>,
    /// Sum of `.data` and `.data.*`.
    pub data_bytes: u64,
    /// Sum of `.bss` and `.bss.*`.
    pub bss_bytes: u64,
    /// Sum of `.noinit`.
    pub noinit_bytes: u64,
    /// `data + bss + noinit`: statically reserved RAM, without the stack.
    pub static_bytes: u64,
    /// Largest sized symbols in `.bss`, `.data` and `.noinit`.
    pub largest_statics: Vec<Symbol>,
}

/// One section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSize {
    /// Section name.
    pub name: String,
    /// Load address.
    pub address: u64,
    /// Size in bytes.
    pub size: u64,
}

/// One symbol with a size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    /// Demangled name without the hash.
    pub name: String,
    /// Section holding the symbol.
    pub section: String,
    /// Size in bytes.
    pub size: u64,
}

/// All Embassy task pools of the image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksReport {
    /// One entry per task, sorted by name.
    pub tasks: Vec<Task>,
    /// Sum of the reserved bytes of every task whose size is known.
    pub reserved_bytes: u64,
    /// Number of tasks whose size could not be derived.
    pub unknown: u32,
    /// How the total relates to the RAM sections.
    pub reconciliation: Reconciliation,
}

/// One compiled Embassy task or the async main entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Task function name; `main` for the `#[esp_rtos::main]` future.
    pub name: String,
    /// The matched ELF symbol, demangled, without hash.
    pub symbol: String,
    /// Bytes reserved for the whole pool (all instances), `None` when unknown.
    pub reserved_bytes: Option<u64>,
    /// Section of the pool.
    pub section: Option<String>,
    /// Instances in the pool. Not derivable from the symbol table; `None` unless the image carries it.
    pub pool_slots: Option<u32>,
    /// Bytes per instance, `None` when `pool_slots` is.
    pub element_bytes: Option<u64>,
    /// Stack the task is polled on.
    pub stack_owner: String,
    /// Trust in `reserved_bytes`.
    pub confidence: Confidence,
    /// Why this is believed.
    pub evidence: String,
}

/// Task memory against the section totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    /// Bytes of task pools that live in `.bss`.
    pub in_bss: u64,
    /// Bytes of task pools that live in `.data`.
    pub in_data: u64,
    /// Bytes of `.bss` that are not task pools.
    pub bss_other: u64,
    /// Bytes of `.data` that are not task pools.
    pub data_other: u64,
}

/// Physical stacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StacksReport {
    /// The main stack, when the linker symbols were found.
    pub main: Option<MainStack>,
    /// Stacks that cannot be listed from the image.
    pub other: Vec<OtherStack>,
}

/// The stack Embassy runs on (`esp-rtos` main thread).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainStack {
    /// Always `rtos/main`.
    pub name: String,
    /// Where the bounds come from.
    pub source: String,
    /// Highest address; the stack grows down from here.
    pub start: u64,
    /// Lowest address; going below it is an overflow.
    pub end: u64,
    /// `start - end`.
    pub allocated_bytes: u64,
    /// Size of the `.stack` section, when present; must equal `allocated_bytes`.
    pub section_bytes: Option<u64>,
    /// Section placed immediately below `end`: what an overflow corrupts first.
    pub below: Option<String>,
    /// Symbols that indicate an overflow guard. Presence is not proof that the guard is armed.
    pub guard_evidence: Vec<String>,
    /// Always `exact`.
    pub confidence: Confidence,
}

/// A stack or context that the ELF cannot describe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherStack {
    /// Owner.
    pub name: String,
    /// Always `not_detectable_statically` or `runtime_only`.
    pub status: String,
    /// What is known.
    pub note: String,
    /// Symbols found in the image that hint at the stack.
    pub evidence: Vec<String>,
}

/// Stack frames from `.stack_sizes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FramesReport {
    /// `present` or `missing`.
    pub metadata: String,
    /// Functions with a sized symbol in the analysis image.
    pub functions: u64,
    /// Functions with a stack-size entry.
    pub with_frame: u64,
    /// `with_frame / functions` in per mille; `None` when there are no functions.
    pub coverage_permille: Option<u32>,
    /// Largest frames first.
    pub top: Vec<Frame>,
    /// Every function with a frame, largest first. The comparison with the base uses it; `top` is for display.
    #[serde(default)]
    pub all: Vec<Frame>,
    /// Largest functions without a stack-size entry (code size, not frame).
    pub without_frame: Vec<CodeSize>,
    /// Frames of the configured focus areas.
    pub focus: Vec<FocusGroup>,
}

/// One function frame. A frame is the stack of that one function, not the depth of a call chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Demangled name without hash.
    pub symbol: String,
    /// Frame size in bytes.
    pub bytes: u64,
}

/// A function without frame metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSize {
    /// Demangled name without hash.
    pub symbol: String,
    /// Code size in bytes.
    pub bytes: u64,
}

/// Frames that match a configured path prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusGroup {
    /// Group name from the budget file.
    pub name: String,
    /// Number of matching functions with frame data.
    pub functions: u64,
    /// Largest matching frames.
    pub top: Vec<Frame>,
}

/// A single measured observation of a stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareResult {
    /// Observed stack, e.g. `rtos/main`.
    pub stack: String,
    /// Scenario, e.g. `rohi status logging`.
    pub scenario: String,
    /// Firmware commit the observation belongs to.
    pub firmware_sha: Option<String>,
    /// Peak use in bytes; `None` is `not_measured`, which is not zero.
    pub peak_used_bytes: Option<u64>,
    /// `allocated - peak`, when both are known.
    pub headroom_bytes: Option<i64>,
    /// `max(25 % of the stack, 4 KiB)`, or the documented override.
    pub required_margin_bytes: Option<u64>,
    /// `Some(true)` when the headroom meets the margin.
    pub margin_ok: Option<bool>,
    /// `enabled`, `disabled` or `not_measured`.
    pub guard_state: String,
    /// Free text.
    pub notes: Option<String>,
}

/// Differences against a base report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    /// Commit of the base.
    pub base_git_sha: Option<String>,
    /// `false` when toolchain, target, profile, features or flags differ.
    pub inputs_match: bool,
    /// What differs.
    pub input_differences: Vec<String>,
    /// RAM totals and main stack.
    pub totals: Vec<Delta>,
    /// Every RAM section present in either report.
    pub sections: Vec<Delta>,
    /// Tasks present now and not in the base.
    pub tasks_added: Vec<String>,
    /// Tasks present in the base and not now.
    pub tasks_removed: Vec<String>,
    /// Changed task pools.
    pub task_deltas: Vec<Delta>,
    /// Functions whose frame grew, largest growth first.
    pub frame_regressions: Vec<Delta>,
    /// Functions whose frame shrank, largest reduction first.
    #[serde(default)]
    pub frame_improvements: Vec<Delta>,
}

/// One compared quantity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    /// What is compared.
    pub name: String,
    /// Value in the base; `None` when absent.
    pub base: Option<u64>,
    /// Value now; `None` when absent.
    pub current: Option<u64>,
    /// `current - base`, with a missing side counted as zero.
    pub delta: i64,
}

/// Verdict of one budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Stable identifier, e.g. `max_bss`.
    pub id: String,
    /// What the budget protects.
    pub description: String,
    /// The limit.
    pub limit: u64,
    /// The measured value; `None` when it could not be derived.
    pub actual: Option<u64>,
    /// Result.
    pub status: CheckStatus,
}

/// Result of a budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Within the budget.
    Pass,
    /// Over the budget.
    Fail,
    /// The value could not be derived.
    Unknown,
}

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Makes the outcome `fail`.
    Error,
    /// Reported only.
    Warning,
    /// Context.
    Info,
}

/// A problem or note found while analyzing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Stable code, e.g. `stack-sizes-missing`.
    pub code: String,
    /// Human-readable text.
    pub message: String,
}

/// The overall verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Static evidence and hardware data are within the budgets.
    Pass,
    /// A budget or a requirement failed.
    Fail,
    /// Static checks pass; no hardware peak of the main stack was supplied.
    NeedsHardwareData,
}

impl Outcome {
    /// The text used in the Markdown report.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::NeedsHardwareData => "NEEDS HARDWARE DATA",
        }
    }
}
