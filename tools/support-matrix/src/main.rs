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
//! `rohi-support-matrix check` validates the matrix, `render` prints the Markdown page,
//! `render --write` rewrites `docs/support/matrix.md`.

use rohi_support_matrix::{Matrix, RENDERED_PATH, render, repo_root, validate};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();
    let matrix = match Matrix::load(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let errors = validate(&matrix, &root);
    for e in &errors {
        eprintln!("error: {e}");
    }
    if !errors.is_empty() {
        return ExitCode::FAILURE;
    }

    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check"] => {
            let page = std::fs::read_to_string(root.join(RENDERED_PATH)).unwrap_or_default();
            if page != render(&matrix) {
                eprintln!(
                    "error: {RENDERED_PATH} is stale, run `cargo run -p rohi-support-matrix -- render --write`"
                );
                return ExitCode::FAILURE;
            }
        }
        ["render"] => print!("{}", render(&matrix)),
        ["render", "--write"] => {
            if let Err(e) = std::fs::write(root.join(RENDERED_PATH), render(&matrix)) {
                eprintln!("error: {RENDERED_PATH}: {e}");
                return ExitCode::FAILURE;
            }
        }
        _ => {
            eprintln!("usage: rohi-support-matrix <check | render [--write]>");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
