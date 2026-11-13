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
//! Markdown rendering of one or more reports.

use crate::analyze::RENDERED_FRAMES;
use crate::model::{CheckStatus, Comparison, Confidence, Outcome, Report, Severity};
use std::fmt::Write;

/// Longest symbol shown in the Markdown; the JSON keeps the full name.
const MAX_NAME: usize = 150;

fn bytes(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_string(), |v| format!("{v} B"))
}

fn signed(value: i64) -> String {
    if value > 0 {
        format!("+{value} B")
    } else {
        format!("{value} B")
    }
}

fn delta_of(comparison: Option<&Comparison>, name: &str) -> String {
    let Some(c) = comparison else {
        return "n/a".to_string();
    };
    let found = c
        .sections
        .iter()
        .chain(&c.task_deltas)
        .find(|d| d.name == name);
    match found {
        Some(d) => signed(d.delta),
        None if c.tasks_added.iter().any(|t| t == name) => "new".to_string(),
        None => "0 B".to_string(),
    }
}

fn cell(text: &str) -> String {
    let text: String = if text.chars().count() > MAX_NAME {
        text.chars().take(MAX_NAME).chain("…".chars()).collect()
    } else {
        text.to_string()
    };
    text.replace('|', "\\|")
}

/// `esp32c3` as `ESP32-C3`.
pub fn chip_label(chip: &str) -> String {
    let upper = chip.to_uppercase();
    match upper.strip_prefix("ESP32") {
        Some(rest) if rest.len() == 2 => format!("ESP32-{rest}"),
        _ => upper,
    }
}

/// Renders the reports as one Markdown document.
pub fn render(reports: &[Report]) -> String {
    let mut out = String::from("# Task Memory & Stack Budget\n");
    for report in reports {
        section(&mut out, report);
    }
    out
}

fn section(out: &mut String, report: &Report) {
    let _ = write!(
        out,
        "\n## {} — Task Memory & Stack Budget\n\n",
        chip_label(&report.chip)
    );
    let build = &report.build;
    let _ = writeln!(
        out,
        "Build: toolchain `{}`, commit `{}`, target `{}`, profile `{}`, features `{}`.\n",
        build.toolchain.as_deref().unwrap_or("unset"),
        build.git_sha.as_deref().unwrap_or("unset"),
        build.target.as_deref().unwrap_or("unset"),
        build.profile.as_deref().unwrap_or("unset"),
        build.features.as_deref().unwrap_or("unset"),
    );
    let comparison = report.comparison.as_ref();
    if let Some(c) = comparison {
        let _ = writeln!(
            out,
            "Compared with base `{}`; build inputs {}.\n",
            c.base_git_sha.as_deref().unwrap_or("unknown"),
            if c.inputs_match {
                "are identical".to_string()
            } else {
                format!("DIFFER ({})", c.input_differences.join("; "))
            }
        );
    }

    out.push_str("### RTOS threads / physical stacks\n\n");
    out.push_str("| name | allocated | peak used | headroom | source |\n|---|---:|---|---|---|\n");
    if let Some(main) = &report.stacks.main {
        let measured: Vec<_> = report
            .hardware
            .iter()
            .filter(|h| h.stack == main.name && h.peak_used_bytes.is_some())
            .collect();
        let (peak, headroom) = match measured.iter().max_by_key(|h| h.peak_used_bytes) {
            Some(h) => (
                format!("{} (`{}`)", bytes(h.peak_used_bytes), cell(&h.scenario)),
                h.headroom_bytes
                    .map_or("unknown".to_string(), |v| format!("{v} B")),
            ),
            None => (
                "not measured (hardware only)".to_string(),
                "not measured".to_string(),
            ),
        };
        let _ = writeln!(
            out,
            "| {} | {} B | {} | {} | {} |",
            main.name, main.allocated_bytes, peak, headroom, main.source
        );
    } else {
        out.push_str("| rtos/main | unknown | not measured | not measured | not found |\n");
    }
    for other in &report.stacks.other {
        let _ = writeln!(
            out,
            "| {} | {} | not measured | not measured | {} |",
            other.name,
            other.status,
            cell(&other.note)
        );
    }
    if let Some(main) = &report.stacks.main {
        let _ = writeln!(
            out,
            "\n`rtos/main` grows down from `{:#x}` to `{:#x}`; below it: {}. Overflow guard evidence: {}.",
            main.start,
            main.end,
            main.below
                .as_deref()
                .map_or("unknown".to_string(), |s| format!(
                    "`{s}` (an overflow corrupts it first)"
                )),
            if main.guard_evidence.is_empty() {
                "none found".to_string()
            } else {
                format!(
                    "{} (presence only, not proof that it is armed)",
                    main.guard_evidence.join(", ")
                )
            }
        );
    }

    out.push_str("\n### Embassy tasks / future pools\n\n");
    out.push_str("| name | reserved RAM | pool slots | stack owner | confidence | Δ base |\n|---|---:|---|---|---|---:|\n");
    for task in &report.tasks.tasks {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            task.name,
            bytes(task.reserved_bytes),
            task.pool_slots
                .map_or("unknown".to_string(), |s| s.to_string()),
            task.stack_owner,
            match task.confidence {
                Confidence::Exact => "exact",
                Confidence::Estimated => "estimated",
                Confidence::Unknown => "unknown",
            },
            delta_of(comparison, &task.symbol),
        );
    }
    let _ = writeln!(
        out,
        "\nTotal: {} B in {} tasks ({} unknown). In `.bss`: {} B of {} B (other {} B); in `.data`: {} B of {} B (other {} B).",
        report.tasks.reserved_bytes,
        report.tasks.tasks.len(),
        report.tasks.unknown,
        report.tasks.reconciliation.in_bss,
        report.ram.bss_bytes,
        report.tasks.reconciliation.bss_other,
        report.tasks.reconciliation.in_data,
        report.ram.data_bytes,
        report.tasks.reconciliation.data_other,
    );
    if let Some(c) = comparison {
        for name in &c.tasks_added {
            let _ = writeln!(out, "- new task: `{name}`");
        }
        for name in &c.tasks_removed {
            let _ = writeln!(out, "- removed task: `{name}`");
        }
    }

    out.push_str("\n### RAM sections\n\n| section | size | Δ base |\n|---|---:|---:|\n");
    for s in &report.ram.sections {
        let _ = writeln!(
            out,
            "| {} | {} B | {} |",
            s.name,
            s.size,
            delta_of(comparison, &s.name)
        );
    }
    let _ = writeln!(
        out,
        "| static total (data + bss + noinit) | {} B | |",
        report.ram.static_bytes
    );
    out.push_str("\nLargest statics:\n\n| symbol | section | size |\n|---|---|---:|\n");
    for s in report.ram.largest_statics.iter().take(10) {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} B |",
            cell(&s.name),
            s.section,
            s.size
        );
    }

    out.push_str("\n### Function stack frames\n\n");
    out.push_str("A frame is the stack of one function. It is not the stack depth of a task: call chains, inlining, dynamic dispatch, async suspension and interrupts are not analyzed.\n\n");
    if report.frames.metadata == "present" {
        let _ = writeln!(
            out,
            "Coverage: {} of {} functions have frame metadata ({}). Functions without it (assembly, ROM and vendor blobs) have an unknown frame.\n",
            report.frames.with_frame,
            report.frames.functions,
            report
                .frames
                .coverage_permille
                .map_or("unknown".to_string(), |p| format!(
                    "{}.{} %",
                    p / 10,
                    p % 10
                )),
        );
        out.push_str("| frame | function |\n|---:|---|\n");
        for f in report.frames.top.iter().take(RENDERED_FRAMES) {
            let _ = writeln!(out, "| {} B | `{}` |", f.bytes, cell(&f.symbol));
        }
        for group in &report.frames.focus {
            let _ = writeln!(
                out,
                "\nFocus `{}` ({} functions with frame data):\n\n| frame | function |\n|---:|---|",
                group.name, group.functions
            );
            for f in &group.top {
                let _ = writeln!(out, "| {} B | `{}` |", f.bytes, cell(&f.symbol));
            }
        }
        if let Some(c) = comparison
            && !c.frame_regressions.is_empty()
        {
            out.push_str("\nLargest frame growth against the base:\n\n| growth | now | function |\n|---:|---:|---|\n");
            for d in c.frame_regressions.iter().take(10) {
                let _ = writeln!(
                    out,
                    "| {} | {} B | `{}` |",
                    signed(d.delta),
                    d.current.unwrap_or(0),
                    cell(&d.name)
                );
            }
        }
        if let Some(c) = comparison
            && !c.frame_improvements.is_empty()
        {
            out.push_str("\nLargest frame reduction against the base:\n\n| change | now | function |\n|---:|---:|---|\n");
            for d in c.frame_improvements.iter().take(10) {
                let change = signed(d.delta);
                let now = d
                    .current
                    .map_or_else(|| "unknown".to_string(), |n| format!("{n} B"));
                let _ = writeln!(out, "| {change} | {now} | `{}` |", cell(&d.name));
            }
        }
    } else {
        out.push_str("Frame metadata is missing; frame sizes are unknown.\n");
    }

    out.push_str("\n### Budgets\n\n| budget | limit | actual | result |\n|---|---:|---:|---|\n");
    for check in &report.checks {
        let _ = writeln!(
            out,
            "| {} | {} B | {} | {} |",
            cell(&check.id),
            check.limit,
            bytes(check.actual),
            match check.status {
                CheckStatus::Pass => "pass",
                CheckStatus::Fail => "**FAIL**",
                CheckStatus::Unknown => "unknown",
            }
        );
    }

    if !report.diagnostics.is_empty() {
        out.push_str("\n### Diagnostics\n\n");
        for d in &report.diagnostics {
            let label = match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
            };
            let _ = writeln!(out, "- **{label}** `{}`: {}", d.code, d.message);
        }
    }

    let _ = writeln!(out, "\n**Outcome: {}**", report.outcome.label());
    if report.outcome == Outcome::NeedsHardwareData {
        out.push_str(
            "\nThe static evidence is within the budgets. The stack peak is not measured: attach hardware observations (see `docs/validation/resource-budget.md`).\n",
        );
    }
}

/// Longest list in the summary; the rest is only in the full report.
const SUMMARY_ROWS: usize = 8;

/// A bound check at or above this share of its limit is reported as close to it.
const NEAR_LIMIT_PERCENT: u64 = 90;

fn with_delta(value: u64, delta: Option<i64>) -> String {
    match delta {
        Some(d) if d != 0 => format!("{value} B ({})", signed(d)),
        _ => format!("{value} B"),
    }
}

fn total_delta(comparison: Option<&Comparison>, name: &str) -> Option<i64> {
    comparison?
        .totals
        .iter()
        .find(|d| d.name == name)
        .map(|d| d.delta)
}

/// Renders only what needs attention: the numbers of each chip in one row, then failed or close-to-limit
/// budgets, warnings, new, removed or grown tasks, sections and frames. [`render`] has everything.
pub fn render_summary(reports: &[Report]) -> String {
    let mut out = String::from("# Task Memory & Stack Budget\n\n");
    out.push_str("| chip | outcome | `.bss` | `.data` | task pools | main stack | largest frame |\n|---|---|---:|---:|---:|---:|---:|\n");
    for report in reports {
        let comparison = report.comparison.as_ref();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            chip_label(&report.chip),
            report.outcome.label(),
            with_delta(report.ram.bss_bytes, total_delta(comparison, ".bss")),
            with_delta(report.ram.data_bytes, total_delta(comparison, ".data")),
            with_delta(
                report.tasks.reserved_bytes,
                total_delta(comparison, "task pools")
            ),
            report
                .stacks
                .main
                .as_ref()
                .map_or("unknown".to_string(), |m| format!(
                    "{} B",
                    m.allocated_bytes
                )),
            match (report.frames.metadata.as_str(), report.frames.top.first()) {
                ("present", Some(f)) => format!("{} B", f.bytes),
                _ => "unknown".to_string(),
            },
        );
    }
    for report in reports {
        summary_section(&mut out, report);
    }
    out.push_str(
        "\nA frame is the stack of one function, not the stack depth of a task. \
         `NEEDS HARDWARE DATA` means the static evidence is within the budgets and no stack peak is measured.\n",
    );
    out
}

fn summary_section(out: &mut String, report: &Report) {
    let mut lines: Vec<String> = Vec::new();

    for d in &report.diagnostics {
        let label = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => continue,
        };
        lines.push(format!("**{label}** `{}`: {}", d.code, d.message));
    }

    let mut checks = Vec::new();
    for c in &report.checks {
        let near = c.id != "min_main_stack"
            && c.status == CheckStatus::Pass
            && c.limit > 0
            && c.actual.is_some_and(|a| {
                a.saturating_mul(100) >= c.limit.saturating_mul(NEAR_LIMIT_PERCENT)
            });
        let text = match c.status {
            CheckStatus::Fail => "**FAIL**",
            CheckStatus::Unknown => "unknown",
            CheckStatus::Pass if near => "close to the limit",
            CheckStatus::Pass => continue,
        };
        checks.push(format!(
            "budget `{}`: {} (limit {} B, actual {})",
            cell(&c.id),
            text,
            c.limit,
            bytes(c.actual)
        ));
    }
    lines.extend(checks);

    if let Some(c) = &report.comparison {
        for name in &c.tasks_added {
            lines.push(format!("new task: `{name}`"));
        }
        for name in &c.tasks_removed {
            lines.push(format!("removed task: `{name}`"));
        }
        let mut changed: Vec<_> = c.task_deltas.iter().collect();
        changed.sort_by_key(|d| std::cmp::Reverse(d.delta.unsigned_abs()));
        for d in changed.iter().take(SUMMARY_ROWS) {
            lines.push(format!(
                "task `{}`: {} → {} ({})",
                cell(&d.name),
                bytes(d.base),
                bytes(d.current),
                signed(d.delta)
            ));
        }
        for d in c.sections.iter().filter(|d| d.delta != 0) {
            lines.push(format!("section `{}`: {}", d.name, signed(d.delta)));
        }
        for d in c.frame_regressions.iter().take(SUMMARY_ROWS) {
            lines.push(format!(
                "frame growth `{}`: {} B → {} B ({})",
                cell(&d.name),
                d.base.unwrap_or(0),
                d.current.unwrap_or(0),
                signed(d.delta)
            ));
        }
        if c.frame_regressions.len() > SUMMARY_ROWS {
            lines.push(format!(
                "… and {} more frame regressions in the full report",
                c.frame_regressions.len() - SUMMARY_ROWS
            ));
        }
        if changed.len() > SUMMARY_ROWS {
            lines.push(format!(
                "… and {} more changed tasks in the full report",
                changed.len() - SUMMARY_ROWS
            ));
        }
    }

    let _ = write!(out, "\n### {}\n\n", chip_label(&report.chip));
    if lines.is_empty() {
        out.push_str("No anomalies.\n");
        return;
    }
    for line in &lines {
        let _ = writeln!(out, "- {line}");
    }
}
