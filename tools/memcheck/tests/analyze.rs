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
mod common;

use common::{Fixture, Function, budget, run, run_with, run_with_limits};
use rohi_memcheck::budget::{ApprovedGrowth, BudgetFile};
use rohi_memcheck::hardware::Observation;
use rohi_memcheck::model::{CheckStatus, Confidence, Outcome, Report, Severity};
use rohi_memcheck::render::render;

fn check<'a>(report: &'a Report, id: &str) -> &'a rohi_memcheck::model::Check {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no check {id}: {:?}", report.checks))
}

fn has_error(report: &Report, code: &str) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error && d.code == code)
}

fn observation(peak: Option<u64>) -> Observation {
    Observation {
        chip: "esp32c3".to_string(),
        stack: "rtos/main".to_string(),
        scenario: "rohi status logging".to_string(),
        firmware_sha: Some("abc123".to_string()),
        peak_used_bytes: peak,
        guard_state: Some("enabled".to_string()),
        required_margin_bytes: None,
        notes: None,
    }
}

fn golden(name: &str, actual: &str) {
    let path = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "{name} differs from the golden file; run `UPDATE_GOLDEN=1 cargo test -p rohi-memcheck`"
    );
}

#[test]
fn every_task_pool_is_found_without_an_allowlist() {
    let report = run(&Fixture::firmware(), None, &[]);
    let names: Vec<_> = report.tasks.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["ble_task", "control_task", "logging_task", "reboot_task"]
    );
    assert_eq!(report.tasks.reserved_bytes, 17440 + 7064 + 2304 + 72);
    let control = &report.tasks.tasks[1];
    assert_eq!(control.reserved_bytes, Some(17440));
    assert_eq!(control.confidence, Confidence::Exact);
    assert_eq!(control.stack_owner, "rtos/main");
    assert_eq!(
        control.pool_slots, None,
        "the slot count is not derivable from symbols"
    );
    assert_eq!(
        report.tasks.reconciliation.in_bss,
        report.tasks.reserved_bytes
    );
    assert_eq!(
        report.tasks.reconciliation.bss_other,
        40000 - report.tasks.reserved_bytes
    );
}

#[test]
fn a_new_task_appears_and_is_listed_against_the_base() {
    let base = run(&Fixture::firmware(), None, &[]);
    let mut fixture = Fixture::firmware();
    fixture.tasks.push((&["app", "sensors"], "scd4x_task", 400));
    fixture.bss = 40400;
    let report = run(&fixture, Some(&base), &[]);

    assert!(report.tasks.tasks.iter().any(|t| t.name == "scd4x_task"));
    let comparison = report.comparison.expect("compared");
    assert_eq!(comparison.tasks_added, ["app::sensors::scd4x_task::POOL"]);
    assert!(comparison.tasks_removed.is_empty());
}

#[test]
fn a_removed_task_is_listed_not_dropped() {
    let base = run(&Fixture::firmware(), None, &[]);
    let mut fixture = Fixture::firmware();
    fixture.tasks.pop();
    let report = run(&fixture, Some(&base), &[]);
    assert_eq!(
        report.comparison.expect("compared").tasks_removed,
        ["app::reboot::reboot_task::POOL"]
    );
}

#[test]
fn an_unsized_pool_is_unknown_not_zero() {
    let mut fixture = Fixture::firmware();
    fixture.tasks.push((&["app", "net"], "wifi_task", 0));
    let report = run(&fixture, None, &[]);
    let wifi = report
        .tasks
        .tasks
        .iter()
        .find(|t| t.name == "wifi_task")
        .expect("listed");
    assert_eq!(wifi.reserved_bytes, None);
    assert_eq!(wifi.confidence, Confidence::Unknown);
    assert_eq!(report.tasks.unknown, 1);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == "task-size-unknown")
    );
}

#[test]
fn a_stripped_image_fails_instead_of_reporting_nothing() {
    let mut fixture = Fixture::firmware();
    fixture.symbols = false;
    let report = run(&fixture, None, &[]);
    assert!(has_error(&report, "symbols-missing"));
    assert_eq!(report.outcome, Outcome::Fail);
    assert_eq!(
        check(&report, "min_main_stack").status,
        CheckStatus::Unknown
    );
}

#[test]
fn the_main_stack_comes_from_the_linker_symbols() {
    let report = run(&Fixture::firmware(), None, &[]);
    let main = report.stacks.main.expect("main stack");
    assert_eq!(main.allocated_bytes, 0x5608);
    assert_eq!(main.section_bytes, Some(0x5608));
    assert_eq!(main.start, 0x3fcc_e400);
    assert!(report.stacks.other.iter().all(|s| s.status != "exact"));
    assert!(
        report
            .stacks
            .other
            .iter()
            .any(|s| s.status == "not_detectable_statically")
    );
    assert!(
        report
            .stacks
            .other
            .iter()
            .any(|s| s.status == "runtime_only")
    );
}

#[test]
fn inconsistent_stack_symbols_are_reported() {
    let mut fixture = Fixture::firmware();
    fixture.stack_section = Some(1000);
    assert!(has_error(
        &run(&fixture, None, &[]),
        "stack-section-mismatch"
    ));

    let mut fixture = Fixture::firmware();
    fixture.stack = Some((0x1000, 0x2000));
    assert!(has_error(&run(&fixture, None, &[]), "stack-direction"));

    let mut fixture = Fixture::firmware();
    fixture.stack = None;
    let report = run(&fixture, None, &[]);
    assert!(has_error(&report, "main-stack-missing"));
    assert_eq!(report.stacks.main, None);
}

#[test]
fn a_lowered_main_stack_budget_fails() {
    let mut chip = budget().0;
    chip.min_main_stack = Some(0x5608 + 1);
    let report = run_with(&Fixture::firmware(), &chip, None, &[], false);
    assert_eq!(check(&report, "min_main_stack").status, CheckStatus::Fail);
    assert_eq!(report.outcome, Outcome::Fail);
}

#[test]
fn a_bigger_task_pool_fails_the_ram_budget() {
    let mut fixture = Fixture::firmware();
    fixture.tasks[0].2 = 17440 + 20000;
    fixture.bss = 60000;
    let report = run(&fixture, None, &[]);
    assert_eq!(check(&report, "max_task_ram").status, CheckStatus::Fail);
    assert_eq!(check(&report, "max_bss").status, CheckStatus::Fail);
    assert_eq!(report.outcome, Outcome::Fail);
}

#[test]
fn frames_are_read_from_stack_sizes_and_coverage_is_reported() {
    let report = run(&Fixture::firmware(), None, &[]);
    let frames = &report.frames;
    assert_eq!(frames.metadata, "present");
    assert_eq!((frames.functions, frames.with_frame), (4, 3));
    assert_eq!(frames.coverage_permille, Some(750));
    assert_eq!(frames.top[0].bytes, 2100);
    assert!(
        frames.top[0]
            .symbol
            .contains("rohi_logging::service::collect")
    );
    assert_eq!(frames.without_frame[0].bytes, 3000);
    let focus = frames
        .focus
        .iter()
        .find(|g| g.name == "control_dispatch")
        .expect("focus");
    assert_eq!(focus.top[0].bytes, 1400);
    assert_eq!(check(&report, "max_frame").actual, Some(2100));
}

#[test]
fn missing_stack_sizes_fail_unless_approved() {
    let mut fixture = Fixture::firmware();
    fixture.stack_sizes = false;
    let report = run(&fixture, None, &[]);
    assert_eq!(report.frames.metadata, "missing");
    assert!(has_error(&report, "stack-sizes-missing"));
    assert_eq!(report.outcome, Outcome::Fail);

    let mut chip = budget().0;
    chip.exceptions.allow_missing_stack_sizes = true;
    let report = run_with(&fixture, &chip, None, &[], false);
    assert!(!has_error(&report, "stack-sizes-missing"));
    assert_eq!(check(&report, "max_frame").status, CheckStatus::Unknown);
}

#[test]
fn a_frame_larger_than_the_budget_fails() {
    let mut fixture = Fixture::firmware();
    fixture.functions[0].frame = Some(4001);
    let report = run(&fixture, None, &[]);
    assert_eq!(check(&report, "max_frame").status, CheckStatus::Fail);
}

#[test]
fn growth_below_the_limit_is_reported_but_allowed() {
    let base = run(&Fixture::firmware(), None, &[]);
    let mut fixture = Fixture::firmware();
    fixture.tasks[0].2 += 400;
    fixture.bss += 400;
    fixture.functions[1].frame = Some(2100 + 150);
    let report = run(&fixture, Some(&base), &[]);

    let comparison = report.comparison.as_ref().expect("compared");
    assert_eq!(comparison.task_deltas[0].delta, 400);
    assert_eq!(comparison.frame_regressions[0].delta, 150);
    assert!(report.checks.iter().all(|c| c.status != CheckStatus::Fail));
    assert_eq!(report.outcome, Outcome::NeedsHardwareData);
}

#[test]
fn growth_above_the_limit_fails() {
    let base = run(&Fixture::firmware(), None, &[]);
    let mut fixture = Fixture::firmware();
    fixture.tasks[0].2 += 600;
    fixture.bss += 600;
    fixture.functions[1].frame = Some(2100 + 300);
    let report = run(&fixture, Some(&base), &[]);

    assert_eq!(
        check(&report, "growth:task:app::control::control_task::POOL").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report, "growth:frame:rohi_logging::service::collect").status,
        CheckStatus::Fail
    );
    assert_eq!(check(&report, "growth:bss").status, CheckStatus::Pass);
    assert_eq!(report.outcome, Outcome::Fail);
}

#[test]
fn a_comparison_between_different_build_inputs_is_an_error() {
    let mut base = run(&Fixture::firmware(), None, &[]);
    base.build.toolchain = Some("rustc 1.98.0".to_string());

    let report = run(&Fixture::firmware(), Some(&base), &[]);
    assert!(!report.comparison.as_ref().expect("compared").inputs_match);
    assert!(has_error(&report, "inputs-differ"));
    assert_eq!(report.outcome, Outcome::Fail);

    let report = run_with(&Fixture::firmware(), &budget().0, Some(&base), &[], true);
    assert!(!has_error(&report, "inputs-differ"));
    assert!(report.diagnostics.iter().any(|d| d.code == "inputs-differ"));
}

#[test]
fn without_hardware_data_the_outcome_needs_it() {
    let report = run(&Fixture::firmware(), None, &[]);
    assert_eq!(report.outcome, Outcome::NeedsHardwareData);
    assert!(report.hardware.is_empty());

    let unmeasured = run(&Fixture::firmware(), None, &[observation(None)]);
    assert_eq!(unmeasured.hardware[0].peak_used_bytes, None);
    assert_eq!(unmeasured.hardware[0].headroom_bytes, None);
    assert_eq!(unmeasured.outcome, Outcome::NeedsHardwareData);
}

#[test]
fn a_measured_peak_needs_a_quarter_of_the_stack_as_headroom() {
    // 22 024 B stack: 25 % is 5 506 B, above the 4 KiB floor.
    let ok = run(&Fixture::firmware(), None, &[observation(Some(15000))]);
    assert_eq!(ok.hardware[0].headroom_bytes, Some(7024));
    assert_eq!(ok.hardware[0].required_margin_bytes, Some(5506));
    assert_eq!(ok.outcome, Outcome::Pass);

    let tight = run(&Fixture::firmware(), None, &[observation(Some(17000))]);
    assert_eq!(tight.hardware[0].margin_ok, Some(false));
    assert_eq!(tight.outcome, Outcome::Fail);
}

#[test]
fn a_small_stack_uses_the_four_kib_floor() {
    let mut fixture = Fixture::firmware();
    fixture.stack = Some((0x3fcc_8df8 + 8192, 0x3fcc_8df8));
    fixture.stack_section = Some(8192);
    let mut chip = budget().0;
    chip.min_main_stack = None;
    let report = run_with(&fixture, &chip, None, &[observation(Some(4096))], false);
    assert_eq!(report.hardware[0].required_margin_bytes, Some(4096));
    assert_eq!(report.hardware[0].margin_ok, Some(true));
}

#[test]
fn an_observation_of_another_build_is_flagged() {
    let mut obs = observation(Some(1000));
    obs.firmware_sha = Some("other".to_string());
    let report = run(&Fixture::firmware(), None, &[obs]);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == "hardware-sha-mismatch")
    );
}

#[test]
fn json_and_markdown_reports_match_the_golden_files() {
    let report = run(&Fixture::firmware(), None, &[]);
    let json = serde_json::to_string_pretty(&report).expect("serialize") + "\n";
    golden("report-esp32c3.json", &json);
    golden("report-esp32c3.md", &render(std::slice::from_ref(&report)));

    let round_trip: Report = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(round_trip, report);
}

#[test]
fn the_markdown_report_shows_the_comparison() {
    let base = run(&Fixture::firmware(), None, &[]);
    let mut fixture = Fixture::firmware();
    fixture.tasks.push((&["app", "sensors"], "scd4x_task", 400));
    fixture.bss = 40400;
    fixture.functions.push(Function {
        path: "app::extra",
        code: 10,
        frame: Some(64),
    });
    let report = run(&fixture, Some(&base), &[]);
    golden(
        "report-esp32c3-compared.md",
        &render(std::slice::from_ref(&report)),
    );
}

#[test]
fn a_frame_larger_than_the_main_stack_is_flagged() {
    let mut fixture = Fixture::firmware();
    fixture.functions[0].frame = Some(0x5608 + 1);
    let mut chip = budget().0;
    chip.max_frame = None;
    let report = run_with(&fixture, &chip, None, &[], false);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == "frame-exceeds-stack")
    );
    assert_ne!(report.outcome, Outcome::Fail, "a warning does not gate");
}

#[test]
fn an_approved_frame_growth_raises_the_limit_for_that_function_only() {
    let base = run(&Fixture::firmware(), None, &[]);
    let mut fixture = Fixture::firmware();
    fixture.functions[0].frame = Some(1400 + 300);
    fixture.functions[1].frame = Some(2100 + 300);

    let mut limits = budget().1;
    let failed = run_with_limits(&fixture, &limits, &base);
    assert_eq!(failed.outcome, Outcome::Fail);

    limits.approved_frame_growth = vec![ApprovedGrowth {
        symbol: "rohi_logging::service".to_string(),
        max_growth: 400,
        reason: "inlined callee".to_string(),
    }];
    let report = run_with_limits(&fixture, &limits, &base);
    let failing: Vec<_> = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(failing, ["growth:frame:rohi_control::controller::handle"]);
}

#[test]
fn the_budget_file_reads_approved_frame_growth() {
    let file = BudgetFile::parse(
        r#"
[regression]
[[regression.approved_frame_growth]]
symbol = "control_task"
max_growth = 2048
reason = "execute was inlined into it; its own frame shrank by far more"
"#,
    )
    .expect("parses");
    assert_eq!(file.regression.approved_frame_growth[0].max_growth, 2048);
    assert!(BudgetFile::parse("[[regression.approved_frame_growth]]\nsymbol = \"x\"\n").is_err());
}
