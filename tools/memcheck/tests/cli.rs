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

use common::{BUDGETS, Fixture};
use rohi_memcheck::model::{Outcome, Report};
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("memcheck-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn memcheck(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_memcheck"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run memcheck")
}

fn analyze(dir: &Path, elf: &str, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "analyze",
        "--chip",
        "esp32c3",
        "--elf",
        elf,
        "--budgets",
        "budgets.toml",
        "--out-json",
        "out.json",
    ];
    args.extend_from_slice(extra);
    memcheck(dir, &args)
}

#[test]
fn analyze_writes_a_report_and_exits_zero_when_only_hardware_data_is_missing() {
    let dir = scratch("ok");
    std::fs::write(dir.join("fw.elf"), Fixture::firmware().elf()).expect("write elf");
    std::fs::write(dir.join("budgets.toml"), BUDGETS).expect("write budgets");

    let out = analyze(
        &dir,
        "fw.elf",
        &["--git-sha", "abc123", "--out-md", "out.md"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Report =
        serde_json::from_slice(&std::fs::read(dir.join("out.json")).expect("json")).expect("parse");
    assert_eq!(report.outcome, Outcome::NeedsHardwareData);
    assert_eq!(report.build.git_sha.as_deref(), Some("abc123"));
    assert!(
        std::fs::read_to_string(dir.join("out.md"))
            .expect("md")
            .contains("**Outcome: NEEDS HARDWARE DATA**")
    );

    let out = analyze(&dir, "fw.elf", &["--require-hardware"]);
    assert!(
        !out.status.success(),
        "missing hardware data must fail when it is required"
    );

    let out = memcheck(&dir, &["report", "out.json", "--out", "all.md"]);
    assert!(out.status.success());
    assert!(dir.join("all.md").exists());
    let out = memcheck(
        &dir,
        &[
            "report",
            "out.json",
            "--out",
            "all.md",
            "--require-hardware",
        ],
    );
    assert!(!out.status.success());
}

#[test]
fn analyze_exits_non_zero_on_a_budget_violation() {
    let dir = scratch("fail");
    let mut fixture = Fixture::firmware();
    fixture.bss = 90000;
    std::fs::write(dir.join("fw.elf"), fixture.elf()).expect("write elf");
    std::fs::write(dir.join("budgets.toml"), BUDGETS).expect("write budgets");

    let out = analyze(&dir, "fw.elf", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("FAIL"));
    assert!(
        dir.join("out.json").exists(),
        "the report is written even when it fails"
    );
}

#[test]
fn an_unreadable_input_is_an_error_not_a_pass() {
    let dir = scratch("bad");
    std::fs::write(dir.join("budgets.toml"), BUDGETS).expect("write budgets");
    std::fs::write(dir.join("junk.elf"), b"not an elf").expect("write junk");

    let out = analyze(&dir, "junk.elf", &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("error"));
    let out = analyze(&dir, "missing.elf", &[]);
    assert!(!out.status.success());
}
