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
//! Schema and reference-integrity checks.

use crate::model::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Supported schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Directory whose sensor drivers must all be represented by a capability.
const SENSOR_DIR: &str = "crates/hal/src/sensors";

/// Checks `matrix` against the repository at `root`. Returns every violation found (empty means valid).
pub fn validate(matrix: &Matrix, root: &Path) -> Vec<String> {
    let mut v = Validator {
        root,
        errors: Vec::new(),
    };
    v.run(matrix);
    v.errors
}

struct Validator<'a> {
    root: &'a Path,
    errors: Vec<String>,
}

impl Validator<'_> {
    fn err(&mut self, message: String) {
        self.errors.push(message);
    }

    fn run(&mut self, m: &Matrix) {
        if m.schema_version != SCHEMA_VERSION {
            self.err(format!(
                "schema_version {} is not supported (expected {SCHEMA_VERSION})",
                m.schema_version
            ));
        }
        if !is_date(&m.reviewed) {
            self.err(format!(
                "reviewed `{}` is not a YYYY-MM-DD date",
                m.reviewed
            ));
        }

        let hardware = self.unique("hardware", m.hardware.iter().map(|h| &h.id));
        let firmware = self.unique("firmware", m.firmware.iter().map(|f| &f.id));
        let capability = self.unique("capability", m.capability.iter().map(|c| &c.id));
        let evidence = self.unique("evidence", m.evidence.iter().map(|e| &e.id));
        self.unique("use_case", m.use_case.iter().map(|u| &u.id));

        let evidence_kind: HashMap<&str, EvidenceKind> =
            m.evidence.iter().map(|e| (e.id.as_str(), e.kind)).collect();
        let mut used: HashSet<&str> = HashSet::new();

        for e in &m.evidence {
            self.evidence(e);
        }
        for f in &m.firmware {
            self.firmware(f);
        }
        for c in &m.capability {
            if let Some(source) = &c.source {
                self.path(&format!("capability `{}` source", c.id), source);
            }
        }
        for h in &m.hardware {
            self.hardware(h, &firmware, &evidence);
            used.extend(h.upstream.iter().map(String::as_str));
        }

        let mut seen = HashSet::new();
        for e in &m.entry {
            let name = format!(
                "entry {}/{}/{}",
                e.hardware,
                e.firmware.as_deref().unwrap_or("-"),
                e.capability
            );
            if !seen.insert((&e.hardware, &e.firmware, &e.capability)) {
                self.err(format!("{name}: duplicated"));
            }
            used.extend(e.evidence.iter().map(String::as_str));
            self.entry(
                m,
                e,
                &name,
                &hardware,
                &firmware,
                &capability,
                &evidence_kind,
            );
        }

        for h in &m.hardware {
            self.rohi_support(m, h);
        }
        for u in &m.use_case {
            for c in &u.capabilities {
                if !capability.contains(c.as_str()) {
                    self.err(format!("use_case `{}`: unknown capability `{c}`", u.id));
                }
            }
        }
        for e in &m.evidence {
            if !used.contains(e.id.as_str()) {
                self.err(format!("evidence `{}` is not referenced by anything", e.id));
            }
        }
        self.sensor_coverage(m);
    }

    fn unique<'a>(
        &mut self,
        table: &str,
        ids: impl Iterator<Item = &'a String>,
    ) -> HashSet<&'a str> {
        let mut set = HashSet::new();
        for id in ids {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                self.err(format!("{table} id `{id}` must be kebab-case"));
            }
            if !set.insert(id.as_str()) {
                self.err(format!("{table} id `{id}` is duplicated"));
            }
        }
        set
    }

    fn path(&mut self, what: &str, reference: &str) {
        let path = reference.split('#').next().unwrap_or(reference);
        if path.is_empty()
            || path.starts_with('/')
            || path.contains("..")
            || !self.root.join(path).exists()
        {
            self.err(format!(
                "{what}: `{reference}` does not exist in the repository"
            ));
        }
    }

    fn evidence(&mut self, e: &Evidence) {
        let what = format!("evidence `{}`", e.id);
        let url = e.reference.starts_with("https://");
        match e.kind {
            EvidenceKind::Source | EvidenceKind::Test | EvidenceKind::Ci => {
                if url {
                    self.err(format!(
                        "{what}: {} evidence must be a repository path",
                        e.kind.as_str()
                    ));
                } else {
                    self.path(&what, &e.reference);
                }
            }
            EvidenceKind::Upstream => {
                if !url {
                    self.err(format!("{what}: upstream evidence must be an https:// URL"));
                }
            }
            EvidenceKind::HwReport => {
                if !url {
                    self.path(&what, &e.reference);
                }
            }
        }
    }

    fn firmware(&mut self, f: &Firmware) {
        let what = format!("firmware `{}`", f.id);
        match f.owner {
            Owner::Rohi => self.path(&what, &f.location),
            Owner::External => {
                if !f.location.starts_with("https://") {
                    self.err(format!(
                        "{what}: external firmware needs an https:// location"
                    ));
                }
            }
        }
        if f.chips.is_empty() {
            self.err(format!("{what}: no chips"));
        }
    }

    fn hardware(&mut self, h: &Hardware, firmware: &HashSet<&str>, evidence: &HashSet<&str>) {
        for f in &h.firmware {
            if !firmware.contains(f.as_str()) {
                self.err(format!("hardware `{}`: unknown firmware `{f}`", h.id));
            }
        }
        for e in &h.upstream {
            if !evidence.contains(e.as_str()) {
                self.err(format!("hardware `{}`: unknown evidence `{e}`", h.id));
            }
        }
        if h.upstream.is_empty() {
            self.err(format!(
                "hardware `{}`: no upstream documentation linked",
                h.id
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn entry(
        &mut self,
        m: &Matrix,
        e: &Entry,
        name: &str,
        hardware: &HashSet<&str>,
        firmware: &HashSet<&str>,
        capability: &HashSet<&str>,
        evidence: &HashMap<&str, EvidenceKind>,
    ) {
        if !hardware.contains(e.hardware.as_str()) {
            self.err(format!("{name}: unknown hardware"));
        }
        if !capability.contains(e.capability.as_str()) {
            self.err(format!("{name}: unknown capability"));
        }
        let mut kinds = HashSet::new();
        for id in &e.evidence {
            match evidence.get(id.as_str()) {
                Some(kind) => {
                    kinds.insert(*kind);
                }
                None => self.err(format!("{name}: unknown evidence `{id}`")),
            }
        }

        let hw = m.hardware.iter().find(|h| h.id == e.hardware);
        let fw = e
            .firmware
            .as_ref()
            .and_then(|id| m.firmware.iter().find(|f| &f.id == id));
        if let Some(id) = &e.firmware {
            if !firmware.contains(id.as_str()) {
                self.err(format!("{name}: unknown firmware `{id}`"));
            }
            if let Some(hw) = hw {
                if !hw.firmware.contains(id) {
                    self.err(format!(
                        "{name}: firmware is not listed for hardware `{}`",
                        hw.id
                    ));
                }
                if let Some(fw) = fw
                    && !fw.chips.contains(&hw.chip)
                {
                    self.err(format!(
                        "{name}: firmware does not target chip `{}`",
                        hw.chip
                    ));
                }
            }
        }

        let owner = fw.map(|f| f.owner);
        let rohi_firmware = owner == Some(Owner::Rohi);
        let external = e.implementation == Implementation::External;
        if external != (owner == Some(Owner::External)) {
            self.err(format!(
                "{name}: implementation `external` is required exactly for external firmware"
            ));
        }
        let has_code = matches!(
            e.implementation,
            Implementation::Implemented | Implementation::Partial
        );
        if has_code && !kinds.contains(&EvidenceKind::Source) {
            self.err(format!("{name}: implemented needs `source` evidence"));
        }
        if external && !kinds.contains(&EvidenceKind::Upstream) {
            self.err(format!(
                "{name}: external implementation needs `upstream` evidence"
            ));
        }
        if e.integration == Integration::Integrated && !(rohi_firmware && has_code) {
            self.err(format!(
                "{name}: integrated needs a ROHI firmware and an implemented capability"
            ));
        }
        if e.integration == Integration::NotApplicable && rohi_firmware {
            self.err(format!(
                "{name}: a ROHI firmware entry must be integrated or not-integrated"
            ));
        }
        if e.host_tests == HostTests::Passing && !(has_code && kinds.contains(&EvidenceKind::Test))
        {
            self.err(format!(
                "{name}: passing host tests need an implementation and `test` evidence"
            ));
        }
        if e.simulation == Simulation::Yes && !kinds.contains(&EvidenceKind::Test) {
            self.err(format!("{name}: simulation needs `test` evidence"));
        }
        if e.target_build == TargetBuild::CiBuilt
            && !(rohi_firmware
                && e.integration == Integration::Integrated
                && kinds.contains(&EvidenceKind::Ci))
        {
            self.err(format!(
                "{name}: ci-built needs an integrated ROHI firmware and `ci` evidence"
            ));
        }
        if matches!(
            e.hardware_verification,
            HardwareVerification::Verified | HardwareVerification::Failed
        ) && !(e.integration == Integration::Integrated
            && kinds.contains(&EvidenceKind::HwReport))
        {
            self.err(format!(
                "{name}: a hardware result needs an integrated firmware and `hw-report` evidence"
            ));
        }
        if e.hardware_verification == HardwareVerification::Verified
            && e.board_part == BoardPart::NotOnBoard
        {
            self.err(format!(
                "{name}: cannot be verified on a board without the part"
            ));
        }
        if e.board_part == BoardPart::Documented && !kinds.contains(&EvidenceKind::Upstream) {
            self.err(format!(
                "{name}: a documented board part needs `upstream` evidence"
            ));
        }
        if e.implementation == Implementation::NotImplemented
            && (e.integration == Integration::Integrated
                || e.host_tests == HostTests::Passing
                || e.target_build == TargetBuild::CiBuilt)
        {
            self.err(format!(
                "{name}: not-implemented contradicts integration or test status"
            ));
        }
    }

    fn rohi_support(&mut self, m: &Matrix, h: &Hardware) {
        let entries: Vec<&Entry> = m.entry.iter().filter(|e| e.hardware == h.id).collect();
        let integrated = entries
            .iter()
            .any(|e| e.integration == Integration::Integrated);
        let external = entries
            .iter()
            .any(|e| e.implementation == Implementation::External);
        let rohi_firmware = h.firmware.iter().any(|id| {
            m.firmware
                .iter()
                .any(|f| &f.id == id && f.owner == Owner::Rohi)
        });
        let ok = match h.rohi_support {
            RohiSupport::Supported => integrated && rohi_firmware,
            RohiSupport::OtherFirmwareOnly => !integrated && !rohi_firmware && external,
            RohiSupport::NotIntegrated => !integrated && !rohi_firmware && !external,
        };
        if !ok {
            self.err(format!(
                "hardware `{}`: rohi_support `{}` contradicts its firmware and entries",
                h.id,
                h.rohi_support.as_str()
            ));
        }
        if entries.is_empty() {
            self.err(format!("hardware `{}`: no entries", h.id));
        }
    }

    fn sensor_coverage(&mut self, m: &Matrix) {
        let Ok(dir) = std::fs::read_dir(self.root.join(SENSOR_DIR)) else {
            self.err(format!("{SENSOR_DIR} is missing"));
            return;
        };
        for item in dir.flatten() {
            let file = item.file_name().to_string_lossy().into_owned();
            let stem = file.strip_suffix(".rs").unwrap_or(&file);
            if stem == "mod" {
                continue;
            }
            let covered = m.capability.iter().any(|c| {
                c.kind == Kind::Sensor
                    && c.source.as_deref().is_some_and(|s| {
                        s.strip_prefix(SENSOR_DIR)
                            .and_then(|rest| rest.strip_prefix('/'))
                            .is_some_and(|rest| rest.split(['/', '.']).next() == Some(stem))
                    })
            });
            if !covered {
                self.err(format!(
                    "sensor driver `{stem}` has no capability in the matrix"
                ));
            }
        }
    }
}

fn is_date(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b.iter().enumerate().all(|(i, c)| {
            (i == 4 || i == 7) == (*c == b'-') || (i != 4 && i != 7 && c.is_ascii_digit())
        })
    {
        return false;
    }
    let year = text[0..4].parse::<u32>().ok();
    let month = text[5..7].parse::<u32>().ok();
    let day = text[8..10].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return false;
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=max_day).contains(&day)
}
