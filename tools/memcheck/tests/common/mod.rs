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
//! Synthetic ELF images that look like the sections and symbols of a linked Altruist Urban firmware.
#![allow(dead_code)]

use object::write::{Object, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
};
use rohi_memcheck::analyze::{Inputs, analyze};
use rohi_memcheck::budget::{BudgetFile, ChipBudget, Regression};
use rohi_memcheck::hardware::Observation;
use rohi_memcheck::image::Image;
use rohi_memcheck::model::{BuildInfo, Report};

/// A function with a stack frame.
pub struct Function {
    pub path: &'static str,
    pub code: u64,
    pub frame: Option<u64>,
}

/// What the synthetic image contains.
pub struct Fixture {
    /// `(module path, task fn)` and the bytes of the pool.
    pub tasks: Vec<(&'static [&'static str], &'static str, u64)>,
    /// `(start, end)` of the main stack; `None` leaves the linker symbols out.
    pub stack: Option<(u64, u64)>,
    /// Size of the `.stack` section.
    pub stack_section: Option<u64>,
    pub data: u64,
    pub bss: u64,
    pub functions: Vec<Function>,
    pub stack_sizes: bool,
    pub symbols: bool,
}

impl Fixture {
    pub fn firmware() -> Fixture {
        Fixture {
            tasks: vec![
                (&["app", "control"], "control_task", 17440),
                (&["app", "control"], "ble_task", 7064),
                (&["app", "logging"], "logging_task", 2304),
                (&["app", "reboot"], "reboot_task", 72),
            ],
            stack: Some((0x3fcc_e400, 0x3fcc_8df8)),
            stack_section: Some(0x5608),
            data: 1000,
            bss: 40000,
            functions: vec![
                Function {
                    path: "rohi_control::controller::handle",
                    code: 900,
                    frame: Some(1400),
                },
                Function {
                    path: "rohi_logging::service::collect",
                    code: 800,
                    frame: Some(2100),
                },
                Function {
                    path: "app::main",
                    code: 120,
                    frame: Some(48),
                },
                Function {
                    path: "vendor_blob",
                    code: 3000,
                    frame: None,
                },
            ],
            stack_sizes: true,
            symbols: true,
        }
    }

    /// Writes the image.
    pub fn elf(&self) -> Vec<u8> {
        let mut obj = Object::new(BinaryFormat::Elf, Architecture::Riscv32, Endianness::Little);
        let text = obj.add_section(vec![], b".text".to_vec(), SectionKind::Text);
        let data = obj.add_section(vec![], b".data".to_vec(), SectionKind::Data);
        let bss = obj.add_section(vec![], b".bss".to_vec(), SectionKind::UninitializedData);
        obj.append_section_data(data, &vec![0; self.data as usize], 8);
        obj.append_section_bss(bss, self.bss, 8);
        if let Some(size) = self.stack_section {
            let stack = obj.add_section(vec![], b".stack".to_vec(), SectionKind::UninitializedData);
            obj.append_section_bss(stack, size, 4);
        }

        let mut sizes = Vec::new();
        for f in &self.functions {
            let offset = obj.append_section_data(text, &vec![0; f.code as usize], 4);
            if self.symbols {
                symbol(
                    &mut obj,
                    &function_symbol(f.path),
                    offset,
                    f.code,
                    SymbolKind::Text,
                    SymbolSection::Section(text),
                );
            }
            if let Some(frame) = f.frame {
                sizes.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_le_bytes());
                uleb(&mut sizes, frame);
            }
        }
        if self.stack_sizes {
            let section = obj.add_section(vec![], b".stack_sizes".to_vec(), SectionKind::Metadata);
            obj.append_section_data(section, &sizes, 1);
        }

        if self.symbols {
            let mut taken = 0;
            for (modules, task, bytes) in &self.tasks {
                let name = pool_symbol(modules, task);
                let offset = taken;
                taken += bytes;
                symbol(
                    &mut obj,
                    &name,
                    offset,
                    *bytes,
                    SymbolKind::Data,
                    SymbolSection::Section(bss),
                );
            }
            if let Some((start, end)) = self.stack {
                for (name, value) in [("_stack_start", start), ("_stack_end", end)] {
                    symbol(
                        &mut obj,
                        name,
                        value,
                        0,
                        SymbolKind::Data,
                        SymbolSection::Absolute,
                    );
                }
            }
        }
        obj.write().expect("ELF written")
    }
}

fn symbol(
    obj: &mut Object,
    name: &str,
    value: u64,
    size: u64,
    kind: SymbolKind,
    section: SymbolSection,
) {
    obj.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value,
        size,
        kind,
        scope: SymbolScope::Linkage,
        weak: false,
        section,
        flags: SymbolFlags::None,
    });
}

fn uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Rust v0 mangling of `krate` followed by `(namespace, identifier)` pairs, outermost first in the path.
pub fn mangle(krate: &str, rest: &[(char, &str)]) -> String {
    let mut out = String::from("_R");
    for (ns, _) in rest.iter().rev() {
        out.push('N');
        out.push(*ns);
    }
    out.push_str(&format!("Cs1234_{}{krate}", krate.len()));
    for (_, ident) in rest {
        out.push_str(&format!("{}{ident}", ident.len()));
    }
    out
}

fn function_symbol(path: &str) -> String {
    let idents: Vec<&str> = path.split("::").collect();
    let (krate, rest) = idents.split_first().expect("a path has a crate");
    let pairs: Vec<(char, &str)> = rest
        .iter()
        .enumerate()
        .map(|(i, id)| (if i + 1 == rest.len() { 'v' } else { 't' }, *id))
        .collect();
    mangle(krate, &pairs)
}

fn pool_symbol(modules: &[&str], task: &str) -> String {
    let (krate, mods) = modules.split_first().expect("a task has a crate");
    let mut pairs: Vec<(char, &str)> = mods.iter().map(|m| ('t', *m)).collect();
    pairs.push(('v', task));
    pairs.push(('v', "POOL"));
    mangle(krate, &pairs)
}

pub fn build(chip: &str) -> BuildInfo {
    BuildInfo {
        toolchain: Some("rustc 1.99.1".to_string()),
        git_sha: Some("abc123".to_string()),
        target: Some(format!("riscv32-{chip}")),
        profile: Some("firmware".to_string()),
        features: Some(chip.to_string()),
        flags: Some("-C force-frame-pointers".to_string()),
        analysis_flags: Some("-C force-frame-pointers -Z emit-stack-sizes".to_string()),
        ..BuildInfo::default()
    }
}

pub const BUDGETS: &str = r#"
[regression]
max_section_growth = 1000
max_task_growth = 500
max_frame_growth = 200

[chips.esp32c3]
max_data = 2000
max_bss = 50000
max_task_ram = 30000
min_main_stack = 20000
max_frame = 4000

[chips.esp32c3.focus]
control_dispatch = ["rohi_control::controller"]
logging = ["rohi_logging::service"]
"#;

pub fn budget() -> (ChipBudget, Regression) {
    let file = BudgetFile::parse(BUDGETS).expect("budgets parse");
    (file.chips["esp32c3"].clone(), file.regression)
}

/// Analyzes `fixture` with the standard budgets.
pub fn run(fixture: &Fixture, base: Option<&Report>, hardware: &[Observation]) -> Report {
    run_with(fixture, &budget().0, base, hardware, false)
}

pub fn run_with(
    fixture: &Fixture,
    budget: &ChipBudget,
    base: Option<&Report>,
    hardware: &[Observation],
    allow_input_mismatch: bool,
) -> Report {
    let elf = fixture.elf();
    let image = Image::parse(&elf).expect("fixture parses");
    analyze(&Inputs {
        chip: "esp32c3",
        build: build("esp32c3"),
        image: &image,
        elf_bytes: elf.len() as u64,
        analysis: None,
        budget,
        regression: &budget_regression(),
        base,
        hardware,
        allow_input_mismatch,
    })
}

fn budget_regression() -> Regression {
    budget().1
}

/// Analyzes `fixture` against `base` with the given growth limits.
pub fn run_with_limits(fixture: &Fixture, limits: &Regression, base: &Report) -> Report {
    let elf = fixture.elf();
    let image = Image::parse(&elf).expect("fixture parses");
    analyze(&Inputs {
        chip: "esp32c3",
        build: build("esp32c3"),
        image: &image,
        elf_bytes: elf.len() as u64,
        analysis: None,
        budget: &budget().0,
        regression: limits,
        base: Some(base),
        hardware: &[],
        allow_input_mismatch: false,
    })
}
