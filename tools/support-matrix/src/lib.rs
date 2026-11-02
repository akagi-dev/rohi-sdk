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
//! Validator and Markdown renderer of the ROHI support matrix.
//!
//! The matrix (`support-matrix/matrix.toml`) describes what ROHI supports **per board, firmware and capability**
//! and keeps the evidence levels apart: a capability can be implemented in an SDK crate, integrated in a firmware,
//! built for a target, covered by host tests and verified on hardware independently of each other. Missing evidence
//! is reported as `not-verified`, never as success or failure.
//!
//! * [`Matrix::load`] parses the file, [`validate`] checks the schema and the reference integrity.
//! * [`render`] produces the human-readable page `docs/support/matrix.md`; a test fails if the committed page is stale.
//!
//! The authoritative field and status definitions are in the book page `docs/support/overview.md`.

mod model;
mod render;
mod validate;

pub use model::*;
pub use render::render;
pub use validate::validate;

use std::path::{Path, PathBuf};

/// Location of the matrix relative to the repository root.
pub const MATRIX_PATH: &str = "support-matrix/matrix.toml";
/// Location of the generated page relative to the repository root.
pub const RENDERED_PATH: &str = "docs/support/matrix.md";

/// Repository root of this workspace.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .components()
        .collect()
}

impl Matrix {
    /// Parses a matrix document.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Reads and parses the matrix of the repository at `root`.
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(MATRIX_PATH);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
