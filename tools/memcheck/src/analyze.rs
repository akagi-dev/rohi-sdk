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
//! Builds a [`Report`] from the images, the budgets and the optional base and hardware inputs.

use crate::budget::{ChipBudget, Regression};
use crate::compare;
use crate::hardware::Observation;
use crate::image::{Image, Kind, Sym};
use crate::model::{
    BuildInfo, Check, CheckStatus, CodeSize, Confidence, Diagnostic, FocusGroup, Frame,
    FramesReport, HardwareResult, MainStack, OtherStack, Outcome, RamReport, Reconciliation,
    Report, SCHEMA_VERSION, SectionSize, Severity, StacksReport, Symbol, Task, TasksReport,
};
use std::collections::{BTreeMap, BTreeSet};

/// Frames kept in the JSON report. The Markdown report shows the first [`RENDERED_FRAMES`].
pub const KEPT_FRAMES: usize = 50;
/// Frames shown in the Markdown report.
pub const RENDERED_FRAMES: usize = 20;
const KEPT_STATICS: usize = 15;
const KEPT_UNCOVERED: usize = 10;
const KEPT_FOCUS: usize = 5;
const MAIN_STACK: &str = "rtos/main";
const MIN_STACK_MARGIN: u64 = 4096;

/// Everything one analysis needs.
pub struct Inputs<'a> {
    /// Chip name.
    pub chip: &'a str,
    /// Build metadata; the byte sizes are filled in by [`analyze`].
    pub build: BuildInfo,
    /// Production image.
    pub image: &'a Image,
    /// Size in bytes of the production ELF file.
    pub elf_bytes: u64,
    /// Image built with `-Z emit-stack-sizes`, when separate from the production one.
    pub analysis: Option<(&'a Image, u64)>,
    /// Limits of the chip.
    pub budget: &'a ChipBudget,
    /// Allowed growth against the base.
    pub regression: &'a Regression,
    /// Report of the base branch.
    pub base: Option<&'a Report>,
    /// Hardware observations for this chip.
    pub hardware: &'a [Observation],
    /// Downgrade a build-input mismatch against the base to a warning.
    pub allow_input_mismatch: bool,
}

/// Demangles `raw` without the hash suffix; names that are not Rust symbols are returned unchanged.
pub fn pretty(raw: &str) -> String {
    format!("{:#}", rustc_demangle::demangle(raw))
}

/// Runs the analysis.
pub fn analyze(inputs: &Inputs<'_>) -> Report {
    let mut diagnostics = Vec::new();
    let mut checks = Vec::new();
    let mut build = inputs.build.clone();
    build.elf_bytes = inputs.elf_bytes;
    build.analysis_elf_bytes = inputs.analysis.map(|(_, bytes)| bytes);

    if inputs.image.symbols.is_empty() {
        diagnostics.push(error(
            "symbols-missing",
            "the ELF has no symbol table (stripped?); tasks, the stack and frames cannot be attributed",
        ));
    }

    let ram = ram_report(inputs.image);
    let tasks = tasks_report(inputs.image, &ram, inputs.budget, &mut diagnostics);
    let main = main_stack(inputs.image, &mut diagnostics);
    let stacks = StacksReport {
        main: main.clone(),
        other: other_stacks(inputs.image),
    };
    let frames = frames_report(inputs, &mut diagnostics);
    if let (Some(main), Some(largest)) = (&stacks.main, frames.top.first())
        && largest.bytes > main.allocated_bytes
    {
        diagnostics.push(warning(
            "frame-exceeds-stack",
            format!(
                "the frame of `{}` ({} B) is larger than the whole main stack ({} B); if it runs on that stack it overflows it",
                short(&largest.symbol),
                largest.bytes,
                main.allocated_bytes
            ),
        ));
    }

    if let Some((analysis, _)) = inputs.analysis {
        for name in [".bss", ".data", ".stack"] {
            let a = analysis.section(name).map(|s| s.size);
            let p = inputs.image.section(name).map(|s| s.size);
            if a != p {
                diagnostics.push(warning(
                    "analysis-image-differs",
                    format!(
                        "{name} is {} B in the production image and {} B in the analysis image; the images are not identical builds",
                        fmt_opt(p),
                        fmt_opt(a)
                    ),
                ));
            }
        }
    }

    let hardware = hardware_results(
        inputs.hardware,
        main.as_ref(),
        &build,
        &mut checks,
        &mut diagnostics,
    );

    budget_checks(inputs, &ram, &tasks, main.as_ref(), &frames, &mut checks);

    let mut report = Report {
        schema_version: SCHEMA_VERSION,
        chip: inputs.chip.to_string(),
        build,
        ram,
        tasks,
        stacks,
        frames,
        comparison: None,
        hardware,
        checks,
        diagnostics,
        outcome: Outcome::Pass,
    };

    if let Some(base) = inputs.base {
        let comparison = compare::compare(&report, base);
        compare::enforce(
            &comparison,
            inputs.regression,
            inputs.allow_input_mismatch,
            &mut report.checks,
            &mut report.diagnostics,
        );
        report.comparison = Some(comparison);
    }

    report.diagnostics.sort_by_key(|d| d.severity);
    report.outcome = outcome(&report);
    report
}

fn outcome(report: &Report) -> Outcome {
    let failed = report.checks.iter().any(|c| c.status == CheckStatus::Fail)
        || report
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error);
    if failed {
        Outcome::Fail
    } else if report
        .hardware
        .iter()
        .any(|h| h.stack == MAIN_STACK && h.peak_used_bytes.is_some())
    {
        Outcome::Pass
    } else {
        Outcome::NeedsHardwareData
    }
}

fn error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: code.to_string(),
        message: message.into(),
    }
}

fn warning(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        code: code.to_string(),
        message: message.into(),
    }
}

/// `text` cut to 120 characters, for messages.
fn short(text: &str) -> String {
    if text.chars().count() > 120 {
        text.chars().take(120).chain("…".chars()).collect()
    } else {
        text.to_string()
    }
}

fn fmt_opt(value: Option<u64>) -> String {
    value.map_or_else(|| "absent".to_string(), |v| v.to_string())
}

fn is_data(name: &str) -> bool {
    name == ".data" || name.starts_with(".data.")
}

fn is_bss(name: &str) -> bool {
    name == ".bss" || name.starts_with(".bss.")
}

fn is_ram_static(name: &str) -> bool {
    is_data(name) || is_bss(name) || name == ".noinit"
}

fn ram_report(image: &Image) -> RamReport {
    let mut sections: Vec<SectionSize> = image
        .sections
        .iter()
        .filter(|s| is_ram_static(&s.name) || s.name == ".stack")
        .map(|s| SectionSize {
            name: s.name.clone(),
            address: s.address,
            size: s.size,
        })
        .collect();
    sections.sort_by(|a, b| (a.address, &a.name).cmp(&(b.address, &b.name)));
    let sum = |pred: fn(&str) -> bool| {
        sections
            .iter()
            .filter(|s| pred(&s.name))
            .map(|s| s.size)
            .sum::<u64>()
    };
    let data_bytes = sum(is_data);
    let bss_bytes = sum(is_bss);
    let noinit_bytes = sum(|n| n == ".noinit");

    let mut seen = BTreeSet::new();
    let mut statics: Vec<Symbol> = image
        .symbols
        .iter()
        .filter(|s| s.size > 0 && s.section.as_deref().is_some_and(is_ram_static))
        .filter(|s| seen.insert((s.address, s.size)))
        .map(|s| Symbol {
            name: pretty(&s.raw),
            section: s.section.clone().unwrap_or_default(),
            size: s.size,
        })
        .collect();
    statics.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    statics.truncate(KEPT_STATICS);

    RamReport {
        sections,
        data_bytes,
        bss_bytes,
        noinit_bytes,
        static_bytes: data_bytes + bss_bytes + noinit_bytes,
        largest_statics: statics,
    }
}

/// The name of the task that owns the `POOL` static `path`, e.g. `control_task`.
fn task_name(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = path.split("::").collect();
    if segments.pop()? != "POOL" {
        return None;
    }
    while segments.last().is_some_and(|s| s.starts_with('{')) {
        segments.pop();
    }
    let name = segments.last()?;
    Some(name.strip_prefix("__embassy_").unwrap_or(name).to_string())
}

fn tasks_report(
    image: &Image,
    ram: &RamReport,
    budget: &ChipBudget,
    diagnostics: &mut Vec<Diagnostic>,
) -> TasksReport {
    let mut by_symbol: BTreeMap<String, Task> = BTreeMap::new();
    for sym in &image.symbols {
        let path = pretty(&sym.raw);
        let Some(name) = task_name(&path) else {
            continue;
        };
        if sym.kind == Kind::Func {
            continue;
        }
        let in_ram = sym.section.as_deref().is_some_and(is_ram_static);
        let (reserved, confidence, evidence) = if sym.size > 0 && in_ram {
            (
                Some(sym.size),
                Confidence::Exact,
                format!(
                    "sized `POOL` static in {}",
                    sym.section.as_deref().unwrap_or_default()
                ),
            )
        } else {
            diagnostics.push(warning(
                "task-size-unknown",
                format!(
                    "`{path}` has no size or is not in a RAM section; its memory is not reported"
                ),
            ));
            (
                None,
                Confidence::Unknown,
                "symbol present, size not derivable".to_string(),
            )
        };
        by_symbol.entry(path.clone()).or_insert(Task {
            name,
            symbol: path,
            reserved_bytes: reserved,
            section: sym.section.clone(),
            pool_slots: None,
            element_bytes: None,
            stack_owner: MAIN_STACK.to_string(),
            confidence,
            evidence,
        });
    }
    let mut tasks: Vec<Task> = by_symbol.into_values().collect();
    tasks.sort_by(|a, b| (&a.name, &a.symbol).cmp(&(&b.name, &b.symbol)));

    if tasks.is_empty() && !image.symbols.is_empty() && !budget.exceptions.allow_missing_tasks {
        diagnostics.push(error(
            "no-tasks",
            "no Embassy task pool (`POOL` static) was found; the task inventory cannot be produced",
        ));
    }

    let reserved_bytes = tasks.iter().filter_map(|t| t.reserved_bytes).sum();
    let unknown = u32::try_from(tasks.iter().filter(|t| t.reserved_bytes.is_none()).count())
        .unwrap_or(u32::MAX);
    let sum_in = |pred: fn(&str) -> bool| {
        tasks
            .iter()
            .filter(|t| t.section.as_deref().is_some_and(pred))
            .filter_map(|t| t.reserved_bytes)
            .sum::<u64>()
    };
    let in_bss = sum_in(is_bss);
    let in_data = sum_in(is_data);
    TasksReport {
        reconciliation: Reconciliation {
            in_bss,
            in_data,
            bss_other: ram.bss_bytes.saturating_sub(in_bss),
            data_other: ram.data_bytes.saturating_sub(in_data),
        },
        tasks,
        reserved_bytes,
        unknown,
    }
}

fn main_stack(image: &Image, diagnostics: &mut Vec<Diagnostic>) -> Option<MainStack> {
    let pair =
        |start: &str, end: &str| Some((image.symbol_address(start)?, image.symbol_address(end)?));
    let primary = pair("_stack_start", "_stack_end");
    let cpu0 = pair("_stack_start_cpu0", "_stack_end_cpu0");
    let (source, (start, end)) = match (primary, cpu0) {
        (Some(a), Some(b)) if a != b => {
            diagnostics.push(error(
                "stack-symbols-disagree",
                format!(
                    "`_stack_start`/`_stack_end` ({:#x}..{:#x}) differ from the cpu0 variants ({:#x}..{:#x})",
                    a.1, a.0, b.1, b.0
                ),
            ));
            return None;
        }
        (Some(a), _) => ("linker symbols `_stack_start` / `_stack_end`", a),
        (None, Some(b)) => ("linker symbols `_stack_start_cpu0` / `_stack_end_cpu0`", b),
        (None, None) => {
            diagnostics.push(error(
                "main-stack-missing",
                "no `_stack_start` / `_stack_end` linker symbols; the main stack allocation is unknown",
            ));
            return None;
        }
    };
    if start <= end {
        diagnostics.push(error(
            "stack-direction",
            format!(
                "`_stack_start` ({start:#x}) is not above `_stack_end` ({end:#x}); the stack is expected to grow down from `_stack_start`"
            ),
        ));
        return None;
    }
    let allocated = start - end;
    let section = image.section(".stack");
    let section_bytes = section.map(|s| s.size);
    if let Some(bytes) = section_bytes
        && bytes != allocated
    {
        diagnostics.push(error(
            "stack-section-mismatch",
            format!("the `.stack` section is {bytes} B but the linker symbols give {allocated} B"),
        ));
    }
    let below = image
        .sections
        .iter()
        .filter(|s| s.size > 0 && s.address + s.size == end && s.name != ".stack")
        .map(|s| s.name.clone())
        .next()
        .or_else(|| {
            image
                .sections
                .iter()
                .filter(|s| s.address + s.size <= end && is_ram_static(&s.name))
                .max_by_key(|s| s.address + s.size)
                .map(|s| s.name.clone())
        });
    let guard_evidence = image
        .symbols
        .iter()
        .filter(|s| {
            let name = pretty(&s.raw);
            s.raw == "__stack_chk_guard"
                || name.ends_with("::ensure_no_stack_overflow")
                || name.ends_with("::set_stack_watchpoint")
        })
        .map(|s| pretty(&s.raw))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Some(MainStack {
        name: MAIN_STACK.to_string(),
        source: source.to_string(),
        start,
        end,
        allocated_bytes: allocated,
        section_bytes,
        below,
        guard_evidence,
        confidence: Confidence::Exact,
    })
}

fn other_stacks(image: &Image) -> Vec<OtherStack> {
    let hints: Vec<String> = image
        .symbols
        .iter()
        .filter(|s| s.raw.contains("task_stack_size") || s.raw.contains("thread_stack"))
        .map(|s| pretty(&s.raw))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    vec![
        OtherStack {
            name: "native RTOS threads (radio, network, application)".to_string(),
            status: "not_detectable_statically".to_string(),
            note: "Threads are created at run time with stacks from the heap; the ELF symbols are not a complete list. Use a run-time inventory from hardware.".to_string(),
            evidence: hints,
        },
        OtherStack {
            name: "interrupt and executor contexts".to_string(),
            status: "runtime_only".to_string(),
            note: "Not distinguishable from the image; the Embassy tasks share the main stack.".to_string(),
            evidence: Vec::new(),
        },
    ]
}

fn frames_report(inputs: &Inputs<'_>, diagnostics: &mut Vec<Diagnostic>) -> FramesReport {
    let image = match inputs.analysis {
        Some((analysis, _)) => analysis,
        None => inputs.image,
    };
    let Some(stack_sizes) = &image.stack_sizes else {
        if !inputs.budget.exceptions.allow_missing_stack_sizes {
            diagnostics.push(error(
                "stack-sizes-missing",
                "the image has no `.stack_sizes` section; build the analysis image with `-Z emit-stack-sizes` and make sure the linker keeps it",
            ));
        }
        return FramesReport {
            metadata: "missing".to_string(),
            functions: 0,
            with_frame: 0,
            coverage_permille: None,
            top: Vec::new(),
            all: Vec::new(),
            without_frame: Vec::new(),
            focus: Vec::new(),
        };
    };
    let sizes: BTreeMap<u64, u64> = stack_sizes.iter().copied().collect();

    let mut seen = BTreeSet::new();
    let functions: Vec<&Sym> = image
        .symbols
        .iter()
        .filter(|s| s.kind == Kind::Func && s.size > 0)
        .filter(|s| seen.insert(s.address))
        .collect();
    let mut frames = Vec::new();
    let mut uncovered = Vec::new();
    for sym in &functions {
        match sizes.get(&sym.address) {
            Some(&bytes) => frames.push(Frame {
                symbol: pretty(&sym.raw),
                bytes,
            }),
            None => uncovered.push(CodeSize {
                symbol: pretty(&sym.raw),
                bytes: sym.size,
            }),
        }
    }
    let unnamed = sizes.len().saturating_sub(frames.len());
    if unnamed > 0 {
        diagnostics.push(warning(
            "frames-unnamed",
            format!("{unnamed} stack-size entries match no function symbol"),
        ));
    }
    frames.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.symbol.cmp(&b.symbol)));
    uncovered.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.symbol.cmp(&b.symbol)));

    let focus = inputs
        .budget
        .focus
        .iter()
        .map(|(name, prefixes)| {
            let matching: Vec<Frame> = frames
                .iter()
                .filter(|f| prefixes.iter().any(|p| f.symbol.contains(p.as_str())))
                .cloned()
                .collect();
            FocusGroup {
                name: name.clone(),
                functions: matching.len() as u64,
                top: matching.into_iter().take(KEPT_FOCUS).collect(),
            }
        })
        .collect();

    let total = functions.len() as u64;
    let with_frame = frames.len() as u64;
    let coverage_permille =
        (total > 0).then(|| u32::try_from(with_frame * 1000 / total).unwrap_or(1000));
    let all = frames.clone();
    frames.truncate(KEPT_FRAMES);
    uncovered.truncate(KEPT_UNCOVERED);
    FramesReport {
        metadata: "present".to_string(),
        functions: total,
        with_frame,
        coverage_permille,
        top: frames,
        all,
        without_frame: uncovered,
        focus,
    }
}

fn hardware_results(
    observations: &[Observation],
    main: Option<&MainStack>,
    build: &BuildInfo,
    checks: &mut Vec<Check>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<HardwareResult> {
    observations
        .iter()
        .filter_map(|o| {
            // A measurement of another firmware says nothing about this image.
            if let (Some(sha), Some(built)) = (&o.firmware_sha, &build.git_sha)
                && sha != built
            {
                diagnostics.push(warning(
                    "hardware-sha-mismatch",
                    format!(
                        "the observation `{}` is for firmware {sha}, the analyzed image is {built}; it is ignored",
                        o.scenario
                    ),
                ));
                return None;
            }
            let allocated = (o.stack == MAIN_STACK)
                .then_some(main.map(|m| m.allocated_bytes))
                .flatten();
            let margin = allocated.map(|a| {
                o.required_margin_bytes
                    .unwrap_or_else(|| (a / 4).max(MIN_STACK_MARGIN))
            });
            let headroom = match (allocated, o.peak_used_bytes) {
                (Some(a), Some(p)) => Some(
                    i64::try_from(a).unwrap_or(i64::MAX) - i64::try_from(p).unwrap_or(i64::MAX),
                ),
                _ => None,
            };
            let margin_ok = match (headroom, margin) {
                (Some(h), Some(m)) => Some(h >= i64::try_from(m).unwrap_or(i64::MAX)),
                _ => None,
            };
            if margin_ok == Some(false) {
                checks.push(Check {
                    id: format!("hardware_margin:{}", o.scenario),
                    description: format!(
                        "main-stack headroom in `{}` meets the safety margin",
                        o.scenario
                    ),
                    limit: margin.unwrap_or_default(),
                    actual: headroom.map(|h| u64::try_from(h).unwrap_or(0)),
                    status: CheckStatus::Fail,
                });
            }
            Some(HardwareResult {
                stack: o.stack.clone(),
                scenario: o.scenario.clone(),
                firmware_sha: o.firmware_sha.clone(),
                peak_used_bytes: o.peak_used_bytes,
                headroom_bytes: headroom,
                required_margin_bytes: margin,
                margin_ok,
                guard_state: o
                    .guard_state
                    .clone()
                    .unwrap_or_else(|| "not_measured".to_string()),
                notes: o.notes.clone(),
            })
        })
        .collect()
}

fn budget_checks(
    inputs: &Inputs<'_>,
    ram: &RamReport,
    tasks: &TasksReport,
    main: Option<&MainStack>,
    frames: &FramesReport,
    checks: &mut Vec<Check>,
) {
    let budget = inputs.budget;
    let mut upper = |id: &str, description: &str, limit: Option<u64>, actual: Option<u64>| {
        if let Some(limit) = limit {
            checks.push(Check {
                id: id.to_string(),
                description: description.to_string(),
                limit,
                actual,
                status: match actual {
                    Some(a) if a <= limit => CheckStatus::Pass,
                    Some(_) => CheckStatus::Fail,
                    None => CheckStatus::Unknown,
                },
            });
        }
    };
    upper(
        "max_data",
        "`.data` stays within its budget",
        budget.max_data,
        Some(ram.data_bytes),
    );
    upper(
        "max_bss",
        "`.bss` (statics, task pools, wireless heap) stays within its budget",
        budget.max_bss,
        Some(ram.bss_bytes),
    );
    upper(
        "max_task_ram",
        "the Embassy task pools stay within their budget",
        budget.max_task_ram,
        Some(tasks.reserved_bytes),
    );
    let max_frame =
        (frames.metadata == "present").then(|| frames.top.first().map_or(0, |f| f.bytes));
    upper(
        "max_frame",
        "the largest single function frame stays within its budget",
        budget.max_frame,
        max_frame,
    );
    if let Some(limit) = budget.min_main_stack {
        let actual = main.map(|m| m.allocated_bytes);
        checks.push(Check {
            id: "min_main_stack".to_string(),
            description: "the main stack keeps its declared minimum".to_string(),
            limit,
            actual,
            status: match actual {
                Some(a) if a >= limit => CheckStatus::Pass,
                Some(_) => CheckStatus::Fail,
                None => CheckStatus::Unknown,
            },
        });
    }
}
