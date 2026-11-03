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
//! Validates the published matrix and exercises every rule of the validator on small broken variants.

use rohi_support_matrix::{Matrix, RENDERED_PATH, render, repo_root, validate};

fn published() -> Matrix {
    Matrix::load(&repo_root()).expect("support-matrix/matrix.toml parses")
}

fn errors_of(matrix: &Matrix) -> Vec<String> {
    validate(matrix, &repo_root())
}

#[test]
fn published_matrix_is_valid() {
    let errors = errors_of(&published());
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn rendered_page_is_current() {
    let page = std::fs::read_to_string(repo_root().join(RENDERED_PATH))
        .expect("docs/support/matrix.md exists");
    assert!(
        page == render(&published()),
        "docs/support/matrix.md is stale: run `cargo run -p rohi-support-matrix -- render --write`"
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let text = std::fs::read_to_string(repo_root().join("support-matrix/matrix.toml")).unwrap();
    let broken = text.replacen(
        "schema_version = 1",
        "schema_version = 1\nsupported = true",
        1,
    );
    assert!(Matrix::parse(&broken).is_err());
}

#[test]
fn unknown_status_is_rejected() {
    let text = std::fs::read_to_string(repo_root().join("support-matrix/matrix.toml")).unwrap();
    let broken = text.replacen(
        "hardware_verification = \"not-verified\"",
        "hardware_verification = \"works\"",
        1,
    );
    assert!(Matrix::parse(&broken).is_err());
}

fn expect_error(mutate: impl FnOnce(&mut Matrix), needle: &str) {
    let mut m = published();
    mutate(&mut m);
    let errors = errors_of(&m);
    assert!(
        errors.iter().any(|e| e.contains(needle)),
        "expected an error containing `{needle}`, got {errors:?}"
    );
}

#[test]
fn hardware_result_needs_a_report() {
    use rohi_support_matrix::HardwareVerification;
    expect_error(
        |m| m.entry[0].hardware_verification = HardwareVerification::Verified,
        "hw-report",
    );
}

#[test]
fn dangling_references_are_rejected() {
    expect_error(
        |m| m.entry[0].capability = "nope".into(),
        "unknown capability",
    );
    expect_error(
        |m| m.entry[0].evidence.push("nope".into()),
        "unknown evidence",
    );
    expect_error(
        |m| m.entry[0].firmware = Some("nope".into()),
        "unknown firmware",
    );
    expect_error(
        |m| m.use_case[0].capabilities.push("nope".into()),
        "unknown capability",
    );
}

#[test]
fn missing_evidence_path_is_rejected() {
    expect_error(
        |m| m.evidence[5].reference = "crates/nowhere.rs".into(),
        "does not exist",
    );
}

#[test]
fn duplicated_ids_are_rejected() {
    expect_error(
        |m| {
            let id = m.capability[0].id.clone();
            m.capability[1].id = id;
        },
        "duplicated",
    );
}

#[test]
fn integration_needs_a_rohi_firmware() {
    use rohi_support_matrix::Integration;
    let index = published()
        .entry
        .iter()
        .position(|e| e.firmware.as_deref() == Some("altruist-firmware"))
        .unwrap();
    expect_error(
        |m| m.entry[index].integration = Integration::Integrated,
        "integrated needs",
    );
}

#[test]
fn support_claim_must_match_entries() {
    use rohi_support_matrix::RohiSupport;
    expect_error(
        |m| {
            let h = m
                .hardware
                .iter_mut()
                .find(|h| h.id == "altruist-insight-esp32c6")
                .unwrap();
            h.rohi_support = RohiSupport::Supported;
        },
        "contradicts",
    );
}

#[test]
fn every_sensor_driver_needs_a_capability() {
    expect_error(
        |m| {
            let c = m.capability.iter_mut().find(|c| c.id == "scd4x").unwrap();
            c.source = None;
        },
        "sensor driver `scd4x`",
    );
}
