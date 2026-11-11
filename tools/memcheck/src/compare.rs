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
//! Comparison of a report with the report of the base branch.

use crate::budget::Regression;
use crate::model::{Check, CheckStatus, Comparison, Delta, Diagnostic, Report, Severity};
use std::collections::BTreeMap;

fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn delta(name: impl Into<String>, base: Option<u64>, current: Option<u64>) -> Delta {
    Delta {
        name: name.into(),
        base,
        current,
        delta: signed(current.unwrap_or(0)) - signed(base.unwrap_or(0)),
    }
}

fn input_differences(current: &Report, base: &Report) -> Vec<String> {
    let (c, b) = (&current.build, &base.build);
    let mut out = Vec::new();
    let mut check = |name: &str, a: &Option<String>, b: &Option<String>| {
        if a != b {
            out.push(format!(
                "{name}: base `{}`, now `{}`",
                b.as_deref().unwrap_or("unset"),
                a.as_deref().unwrap_or("unset")
            ));
        }
    };
    check("toolchain", &c.toolchain, &b.toolchain);
    check("target", &c.target, &b.target);
    check("profile", &c.profile, &b.profile);
    check("features", &c.features, &b.features);
    check("flags", &c.flags, &b.flags);
    check("analysis flags", &c.analysis_flags, &b.analysis_flags);
    if current.chip != base.chip {
        out.push(format!(
            "chip: base `{}`, now `{}`",
            base.chip, current.chip
        ));
    }
    out
}

/// Compares `current` with `base`.
pub fn compare(current: &Report, base: &Report) -> Comparison {
    let differences = input_differences(current, base);

    let totals = vec![
        delta(
            ".data",
            Some(base.ram.data_bytes),
            Some(current.ram.data_bytes),
        ),
        delta(
            ".bss",
            Some(base.ram.bss_bytes),
            Some(current.ram.bss_bytes),
        ),
        delta(
            "task pools",
            Some(base.tasks.reserved_bytes),
            Some(current.tasks.reserved_bytes),
        ),
        delta(
            "main stack",
            base.stacks.main.as_ref().map(|m| m.allocated_bytes),
            current.stacks.main.as_ref().map(|m| m.allocated_bytes),
        ),
    ];

    let section_sizes = |r: &Report| -> BTreeMap<String, u64> {
        r.ram
            .sections
            .iter()
            .map(|s| (s.name.clone(), s.size))
            .collect()
    };
    let (now, then) = (section_sizes(current), section_sizes(base));
    let names: std::collections::BTreeSet<_> = now.keys().chain(then.keys()).cloned().collect();
    let sections = names
        .into_iter()
        .map(|n| delta(n.clone(), then.get(&n).copied(), now.get(&n).copied()))
        .collect();

    let tasks = |r: &Report| -> BTreeMap<String, Option<u64>> {
        r.tasks
            .tasks
            .iter()
            .map(|t| (t.symbol.clone(), t.reserved_bytes))
            .collect()
    };
    let (now, then) = (tasks(current), tasks(base));
    let tasks_added = now
        .keys()
        .filter(|k| !then.contains_key(*k))
        .cloned()
        .collect();
    let tasks_removed = then
        .keys()
        .filter(|k| !now.contains_key(*k))
        .cloned()
        .collect();
    let task_deltas = now
        .iter()
        .filter_map(|(symbol, bytes)| {
            let before = then.get(symbol)?;
            (before != bytes).then(|| delta(symbol.clone(), *before, *bytes))
        })
        .collect();

    let frames = |r: &Report| -> BTreeMap<String, u64> {
        r.frames
            .top
            .iter()
            .map(|f| (f.symbol.clone(), f.bytes))
            .collect()
    };
    let (now, then) = (frames(current), frames(base));
    let mut frame_regressions: Vec<Delta> = now
        .iter()
        .filter_map(|(symbol, bytes)| {
            let before = then.get(symbol)?;
            (bytes > before).then(|| delta(symbol.clone(), Some(*before), Some(*bytes)))
        })
        .collect();
    frame_regressions.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.name.cmp(&b.name)));
    let mut frame_improvements: Vec<Delta> = now
        .iter()
        .filter_map(|(symbol, bytes)| {
            let before = then.get(symbol)?;
            (bytes < before).then(|| delta(symbol.clone(), Some(*before), Some(*bytes)))
        })
        .collect();
    // A frame that fell out of the kept list is at most as large as the smallest kept one.
    let floor = if current.frames.top.len() >= crate::analyze::KEPT_FRAMES {
        current.frames.top.last().map_or(0, |f| f.bytes)
    } else {
        0
    };
    if current.frames.metadata == "present" && base.frames.metadata == "present" {
        for (symbol, before) in &then {
            if !now.contains_key(symbol) && *before > floor {
                frame_improvements.push(Delta {
                    name: symbol.clone(),
                    base: Some(*before),
                    current: None,
                    delta: signed(floor) - signed(*before),
                });
            }
        }
    }
    frame_improvements.sort_by(|a, b| a.delta.cmp(&b.delta).then_with(|| a.name.cmp(&b.name)));

    Comparison {
        base_git_sha: base.build.git_sha.clone(),
        inputs_match: differences.is_empty(),
        input_differences: differences,
        totals,
        sections,
        tasks_added,
        tasks_removed,
        task_deltas,
        frame_regressions,
        frame_improvements,
    }
}

/// Turns the comparison into checks. Growth below the limits is only reported.
pub fn enforce(
    comparison: &Comparison,
    limits: &Regression,
    allow_input_mismatch: bool,
    checks: &mut Vec<Check>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !comparison.inputs_match {
        diagnostics.push(Diagnostic {
            severity: if allow_input_mismatch {
                Severity::Warning
            } else {
                Severity::Error
            },
            code: "inputs-differ".to_string(),
            message: format!(
                "the base and the current build inputs differ ({}); a comparison is only meaningful for identical inputs",
                comparison.input_differences.join("; ")
            ),
        });
        if !allow_input_mismatch {
            return;
        }
    }
    let mut push = |id: String, description: String, limit: u64, delta: i64| {
        checks.push(Check {
            id,
            description,
            limit,
            actual: Some(u64::try_from(delta.max(0)).unwrap_or(0)),
            status: if delta > signed(limit) {
                CheckStatus::Fail
            } else {
                CheckStatus::Pass
            },
        });
    };
    for total in &comparison.totals {
        if let Some(id) = match total.name.as_str() {
            ".data" => Some("growth:data"),
            ".bss" => Some("growth:bss"),
            "task pools" => Some("growth:task_pools"),
            _ => None,
        } {
            push(
                id.to_string(),
                format!("{} grew by no more than the allowed amount", total.name),
                limits.max_section_growth,
                total.delta,
            );
        }
    }
    for task in &comparison.task_deltas {
        if task.delta > signed(limits.max_task_growth) {
            push(
                format!("growth:task:{}", task.name),
                format!(
                    "task pool `{}` grew by no more than the allowed amount",
                    task.name
                ),
                limits.max_task_growth,
                task.delta,
            );
        }
    }
    for frame in &comparison.frame_regressions {
        let limit = limits
            .approved_frame_growth
            .iter()
            .filter(|approved| frame.name.contains(approved.symbol.as_str()))
            .map(|approved| approved.max_growth)
            .max()
            .unwrap_or(limits.max_frame_growth);
        if frame.delta > signed(limit) {
            push(
                format!("growth:frame:{}", frame.name),
                format!(
                    "the frame of `{}` grew by no more than the allowed amount",
                    frame.name
                ),
                limit,
                frame.delta,
            );
        }
    }
}
