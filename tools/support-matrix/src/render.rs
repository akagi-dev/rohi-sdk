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
///////////////////////////////////////////////////////////////////////////////
//! Markdown rendering of the support matrix (`docs/support/matrix.md`).

use crate::model::*;
use std::fmt::Write;

const BLOB: &str = "https://github.com/akagi-dev/rohi-sdk/blob/master/";
const TREE: &str = "https://github.com/akagi-dev/rohi-sdk/tree/master/";

/// Renders the page. The output is deterministic, so a committed copy can be compared byte for byte.
pub fn render(m: &Matrix) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<!-- Generated from support-matrix/matrix.toml by `cargo run -p rohi-support-matrix -- render`. Do not edit. -->\n"
    );
    let _ = writeln!(out, "# Support matrix\n");
    let _ = writeln!(
        out,
        "Reviewed on {}, schema version {}. The data is published as [`support-matrix/matrix.toml`]({BLOB}support-matrix/matrix.toml); \
         how to read the statuses is explained in [Reading the matrix](overview.md).\n",
        m.reviewed, m.schema_version
    );

    let _ = writeln!(out, "## Boards\n");
    let _ = writeln!(
        out,
        "| Board | Revision | Chip | ROHI support | Firmware | Upstream documentation |"
    );
    let _ = writeln!(
        out,
        "|-------|----------|------|--------------|----------|------------------------|"
    );
    for h in &m.hardware {
        let firmware = h
            .firmware
            .iter()
            .filter_map(|id| m.firmware.iter().find(|f| &f.id == id))
            .map(|f| format!("{} ({})", f.name, owner_text(f.owner)))
            .collect::<Vec<_>>()
            .join("<br>");
        let _ = writeln!(
            out,
            "| {} | {} | {} | **{}** | {} | {} |",
            esc(&h.name),
            esc(&h.revision),
            h.chip,
            h.rohi_support.as_str(),
            if firmware.is_empty() {
                "none".into()
            } else {
                firmware
            },
            links(m, &h.upstream)
        );
    }
    let _ = writeln!(out);
    for h in &m.hardware {
        let _ = writeln!(
            out,
            "- **{}** ({}): {}",
            esc(&h.name),
            h.chip,
            esc(&h.summary)
        );
    }

    let _ = writeln!(out, "\n## Firmware\n");
    let _ = writeln!(out, "| Firmware | Maintainer | Chips | Location |");
    let _ = writeln!(out, "|----------|------------|-------|----------|");
    for f in &m.firmware {
        let location = match f.owner {
            Owner::Rohi => format!("[`{}`]({TREE}{})", f.location, f.location),
            Owner::External => format!("<{}>", f.location),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            esc(&f.name),
            owner_text(f.owner),
            f.chips.join(", "),
            location
        );
    }
    let _ = writeln!(out);
    for f in &m.firmware {
        let _ = writeln!(out, "- **{}**: {}", esc(&f.name), esc(&f.summary));
    }

    let _ = writeln!(out, "\n## Capabilities per board and firmware\n");
    for h in &m.hardware {
        let mut firmware: Vec<Option<&String>> = Vec::new();
        for e in m.entry.iter().filter(|e| e.hardware == h.id) {
            if !firmware.contains(&e.firmware.as_ref()) {
                firmware.push(e.firmware.as_ref());
            }
        }
        for fw in firmware {
            let title = match fw.and_then(|id| m.firmware.iter().find(|f| &f.id == id)) {
                Some(f) => format!("{} with {}", h.name, f.name),
                None => format!("{}: no firmware targets this board", h.name),
            };
            let _ = writeln!(out, "### {} ({})\n", esc(&title), h.chip);
            table(&mut out, m, h, fw);
        }
    }

    let _ = writeln!(out, "## Use cases\n");
    let _ = writeln!(out, "| Use case | Verdict | Summary |");
    let _ = writeln!(out, "|----------|---------|---------|");
    for u in &m.use_case {
        let _ = writeln!(
            out,
            "| {} | **{}** | {} |",
            esc(&u.title),
            u.verdict.as_str(),
            esc(&u.summary)
        );
    }
    for u in m.use_case.iter().filter(|u| !u.missing.is_empty()) {
        let _ = writeln!(out, "\nStill missing for \"{}\":\n", esc(&u.title));
        for item in &u.missing {
            let _ = writeln!(out, "- {}", esc(item));
        }
    }
    out
}

fn table(out: &mut String, m: &Matrix, h: &Hardware, fw: Option<&String>) {
    let entries: Vec<&Entry> = m
        .entry
        .iter()
        .filter(|e| e.hardware == h.id && e.firmware.as_ref() == fw)
        .collect();
    let _ = writeln!(
        out,
        "| Capability | Board part | Implementation | Integration | Host tests | Simulation | Target build | Hardware | Evidence |"
    );
    let _ = writeln!(
        out,
        "|------------|------------|----------------|-------------|------------|------------|--------------|----------|----------|"
    );
    for e in &entries {
        let cap = m.capability.iter().find(|c| c.id == e.capability);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | **{}** | {} |",
            cap.map_or(e.capability.clone(), |c| esc(&c.name)),
            e.board_part.as_str(),
            e.implementation.as_str(),
            e.integration.as_str(),
            e.host_tests.as_str(),
            e.simulation.as_str(),
            e.target_build.as_str(),
            e.hardware_verification.as_str(),
            links(m, &e.evidence)
        );
    }
    let _ = writeln!(out);
    for e in entries.iter().filter(|e| !e.limitations.is_empty()) {
        let name = m
            .capability
            .iter()
            .find(|c| c.id == e.capability)
            .map_or(e.capability.clone(), |c| c.name.clone());
        let _ = writeln!(out, "Limitations of {}:\n", esc(&name));
        for l in &e.limitations {
            let _ = writeln!(out, "- {}", esc(l));
        }
        let _ = writeln!(out);
    }
}

fn links(m: &Matrix, ids: &[String]) -> String {
    ids.iter()
        .filter_map(|id| m.evidence.iter().find(|e| &e.id == id))
        .map(|e| {
            let url = if e.reference.starts_with("https://") {
                e.reference.clone()
            } else {
                format!("{BLOB}{}", e.reference)
            };
            format!("{} [{}]({url})", e.kind.as_str(), esc(&e.title))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn owner_text(owner: Owner) -> &'static str {
    match owner {
        Owner::Rohi => "ROHI SDK",
        Owner::External => "other project",
    }
}

fn esc(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}
