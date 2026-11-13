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
//! `memcheck analyze` writes a report for one chip, `memcheck report` renders reports as Markdown.

use clap::{Args, Parser, Subcommand};
use rohi_memcheck::analyze::{Inputs, analyze};
use rohi_memcheck::budget::BudgetFile;
use rohi_memcheck::hardware::ObservationFile;
use rohi_memcheck::image::Image;
use rohi_memcheck::model::{BuildInfo, Outcome, Report};
use rohi_memcheck::render::{render, render_summary};
use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Static task memory and stack budget analyzer for firmware ELF images"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze the images of one chip and write `task-memory-<chip>.json`.
    Analyze(Box<AnalyzeArgs>),
    /// Render reports as Markdown and fail when one of them failed.
    Report(ReportArgs),
}

#[derive(Args)]
struct AnalyzeArgs {
    /// Chip name; selects the `[chips.<chip>]` budgets and the hardware observations.
    #[arg(long)]
    chip: String,
    /// The production ELF.
    #[arg(long)]
    elf: PathBuf,
    /// The ELF built with `-Z emit-stack-sizes`, when the production one has no `.stack_sizes`.
    #[arg(long)]
    analysis_elf: Option<PathBuf>,
    /// Budget file.
    #[arg(long)]
    budgets: PathBuf,
    /// Report of the base branch for the same chip.
    #[arg(long)]
    base: Option<PathBuf>,
    /// Hardware observations.
    #[arg(long)]
    hardware: Option<PathBuf>,
    /// Where to write the JSON report.
    #[arg(long)]
    out_json: PathBuf,
    /// Where to write the Markdown report of this chip.
    #[arg(long)]
    out_md: Option<PathBuf>,
    /// Compiler identification.
    #[arg(long)]
    toolchain: Option<String>,
    /// Commit of the image.
    #[arg(long)]
    git_sha: Option<String>,
    /// Rust target triple.
    #[arg(long)]
    target: Option<String>,
    /// Cargo profile.
    #[arg(long)]
    profile: Option<String>,
    /// Cargo features.
    #[arg(long)]
    features: Option<String>,
    /// Extra compiler and linker flags of the production image.
    #[arg(long, allow_hyphen_values = true)]
    flags: Option<String>,
    /// Flags of the analysis image.
    #[arg(long, allow_hyphen_values = true)]
    analysis_flags: Option<String>,
    /// Compare with a base built from different inputs (warning instead of error).
    #[arg(long)]
    allow_input_mismatch: bool,
    /// Treat the absence of hardware data as a failure.
    #[arg(long)]
    require_hardware: bool,
}

#[derive(Args)]
struct ReportArgs {
    /// JSON reports.
    #[arg(required = true)]
    reports: Vec<PathBuf>,
    /// Where to write the complete Markdown report.
    #[arg(long)]
    out: PathBuf,
    /// Where to write the short Markdown that lists only what needs attention.
    #[arg(long)]
    summary_out: Option<PathBuf>,
    /// Treat the absence of hardware data as a failure.
    #[arg(long)]
    require_hardware: bool,
}

fn read(path: &PathBuf) -> Result<Vec<u8>, Box<dyn Error>> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn verdict(outcomes: impl IntoIterator<Item = Outcome>, require_hardware: bool) -> ExitCode {
    let failed = outcomes
        .into_iter()
        .any(|o| o == Outcome::Fail || (require_hardware && o == Outcome::NeedsHardwareData));
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_analyze(args: AnalyzeArgs) -> Result<ExitCode, Box<dyn Error>> {
    let elf = read(&args.elf)?;
    let image = Image::parse(&elf).map_err(|e| format!("{}: {e}", args.elf.display()))?;
    let analysis = match &args.analysis_elf {
        Some(path) => {
            let data = read(path)?;
            let parsed = Image::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
            Some((parsed, data.len() as u64))
        }
        None => None,
    };
    let budgets = BudgetFile::parse(&String::from_utf8(read(&args.budgets)?)?)?;
    let chip_budget = budgets.chip(&args.chip)?;
    let base: Option<Report> = match &args.base {
        Some(path) => Some(serde_json::from_slice(&read(path)?)?),
        None => None,
    };
    let hardware = match &args.hardware {
        Some(path) => {
            ObservationFile::parse(&String::from_utf8(read(path)?)?)?.for_chip(&args.chip)
        }
        None => Vec::new(),
    };

    let report = analyze(&Inputs {
        chip: &args.chip,
        build: BuildInfo {
            toolchain: args.toolchain,
            git_sha: args.git_sha,
            target: args.target,
            profile: args.profile,
            features: args.features,
            flags: args.flags,
            analysis_flags: args.analysis_flags,
            ..BuildInfo::default()
        },
        image: &image,
        elf_bytes: elf.len() as u64,
        analysis: analysis.as_ref().map(|(i, n)| (i, *n)),
        budget: chip_budget,
        regression: &budgets.regression,
        base: base.as_ref(),
        hardware: &hardware,
        allow_input_mismatch: args.allow_input_mismatch,
    });

    std::fs::write(
        &args.out_json,
        serde_json::to_string_pretty(&report)? + "\n",
    )?;
    let markdown = render(std::slice::from_ref(&report));
    if let Some(path) = &args.out_md {
        std::fs::write(path, &markdown)?;
    }
    eprintln!("{}: {}", args.chip, report.outcome.label());
    Ok(verdict([report.outcome], args.require_hardware))
}

fn run_report(args: ReportArgs) -> Result<ExitCode, Box<dyn Error>> {
    let mut reports = Vec::new();
    for path in &args.reports {
        let report: Report =
            serde_json::from_slice(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
        reports.push(report);
    }
    std::fs::write(&args.out, render(&reports))?;
    if let Some(path) = &args.summary_out {
        std::fs::write(path, render_summary(&reports))?;
    }
    Ok(verdict(
        reports.iter().map(|r| r.outcome),
        args.require_hardware,
    ))
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Analyze(args) => run_analyze(*args),
        Command::Report(args) => run_report(args),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
