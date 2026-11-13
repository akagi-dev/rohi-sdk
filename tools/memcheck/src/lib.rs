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
//! Static task memory and stack budget analyzer for ROHI firmware images.
//!
//! `memcheck` reads a linked ELF, lists every compiled Embassy task pool, reports the `esp-rtos` main stack from
//! the linker symbols, reads LLVM `.stack_sizes` frames, compares against a base report and enforces per-chip budgets.
//!
//! # Overview
//!
//! The analyzer is a host tool. It never runs firmware and never claims a runtime peak: static future storage, the
//! physical stack allocation, single-function frames and measured stack peaks are kept apart in the report. A peak is
//! only reported when a hardware observation is supplied, and an unknown value is `unknown`, never `0`.
//!
//! # Quick Example
//!
//! ```text
//! memcheck analyze --chip esp32c3 --elf target/.../altruist-urban-firmware \
//!     --analysis-elf /tmp/analysis/.../altruist-urban-firmware \
//!     --budgets support/memcheck-budgets.toml \
//!     --out-json task-memory-esp32c3.json
//! memcheck report task-memory-esp32c3.json task-memory-esp32c6.json --out task-memory-report.md
//! ```
//!
//! `scripts/memcheck.sh <chip>` builds both images and runs the analyzer.
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | task pool | the `POOL` static of an Embassy task: bytes reserved for all instances, from the symbol size |
//! | main stack | `_stack_start`..`_stack_end`; every Embassy task is polled on it |
//! | frame | `.stack_sizes` entry of one function; not a call depth |
//! | observation | a measured stack peak from a device, attached through a JSON file |
//!
//! # Architecture and Integration
//!
//! ```text
//! ELF (production) ──┐
//! ELF (analysis)  ───┼─► [`analyze::analyze`] ─► [`model::Report`] ─► JSON, Markdown
//! budgets.toml  ─────┤        ▲
//! base report.json ──┤        └─ [`compare::compare`], [`compare::enforce`]
//! hardware.json  ────┘
//! ```
//!
//! # API Navigation
//!
//! | Item | Purpose |
//! |------|---------|
//! | [`analyze::analyze`] | build a report from the inputs |
//! | [`model::Report`] | the JSON schema |
//! | [`budget::BudgetFile`] | per-chip limits and approved exceptions |
//! | [`hardware::ObservationFile`] | device observations |
//! | [`render::render`] | Markdown rendering |
//!
//! # Guarantees and Limitations
//!
//! - The task inventory is read from symbols: a stripped image gives an error, not an empty list.
//! - The number of instances of a pool is not derivable from the symbol table and is reported as unknown.
//! - Native RTOS threads and interrupt contexts are created at run time and are reported as such.
//! - The analysis image must be built with `-Z emit-stack-sizes`; its `.stack_sizes` must survive linking.

pub mod analyze;
pub mod budget;
pub mod compare;
pub mod hardware;
pub mod image;
pub mod model;
pub mod render;
