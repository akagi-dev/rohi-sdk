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
//! Data model of the support matrix, one type per table of `matrix.toml`.

use serde::Deserialize;

/// The whole document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matrix {
    /// Version of this schema, see `docs/support/overview.md`.
    pub schema_version: u32,
    /// Date of the last review of the data (`YYYY-MM-DD`).
    pub reviewed: String,
    #[serde(default)]
    pub hardware: Vec<Hardware>,
    #[serde(default)]
    pub firmware: Vec<Firmware>,
    #[serde(default)]
    pub capability: Vec<Capability>,
    #[serde(default)]
    pub entry: Vec<Entry>,
    #[serde(default)]
    pub use_case: Vec<UseCase>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// A board variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hardware {
    pub id: String,
    pub name: String,
    /// Hardware revision or variant covered by the entries (a model id where upstream publishes one).
    pub revision: String,
    /// Lowercase chip name, matched against `Firmware::chips`.
    pub chip: String,
    pub rohi_support: RohiSupport,
    /// Firmware (ids) that target this board.
    #[serde(default)]
    pub firmware: Vec<String>,
    pub summary: String,
    /// Evidence ids of the authoritative upstream hardware documentation.
    #[serde(default)]
    pub upstream: Vec<String>,
}

/// A firmware, from this repository or from another project.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Firmware {
    pub id: String,
    pub name: String,
    pub owner: Owner,
    /// Repository path (ROHI firmware) or upstream URL (external firmware).
    pub location: String,
    /// Chips the firmware can be built for.
    pub chips: Vec<String>,
    pub summary: String,
}

/// A sensor, measurement, interface or operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// What is produced or offered, with units.
    pub provides: String,
    /// Repository path of the SDK implementation; absent when ROHI has none.
    pub source: Option<String>,
}

/// The status of one capability on one board with one firmware.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub hardware: String,
    /// Absent when no firmware targets the board.
    pub firmware: Option<String>,
    pub capability: String,
    /// Whether the board carries the hardware needed by the capability.
    pub board_part: BoardPart,
    pub implementation: Implementation,
    pub integration: Integration,
    pub host_tests: HostTests,
    pub simulation: Simulation,
    pub target_build: TargetBuild,
    pub hardware_verification: HardwareVerification,
    #[serde(default)]
    pub limitations: Vec<String>,
    /// Evidence ids.
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// An end-user goal and whether the repository can reach it today.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseCase {
    pub id: String,
    pub title: String,
    pub verdict: Verdict,
    pub summary: String,
    /// Capability ids that take part in the use case.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Work that is still missing.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// A source of truth behind a claim.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub kind: EvidenceKind,
    pub title: String,
    /// Repository path (optionally with `#anchor`) or an `https://` URL.
    pub reference: String,
}

macro_rules! status {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// The spelling used in `matrix.toml`.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
    };
}

status! {
    /// How a ROHI firmware covers a board.
    RohiSupport {
        Supported = "supported",
        OtherFirmwareOnly = "other-firmware-only",
        NotIntegrated = "not-integrated",
    }
}

status! {
    /// Who maintains a firmware.
    Owner { Rohi = "rohi", External = "external" }
}

status! {
    Kind {
        Sensor = "sensor",
        Interface = "interface",
        Service = "service",
        Operation = "operation",
    }
}

status! {
    /// Does the board carry the hardware the capability needs.
    BoardPart {
        Documented = "documented",
        Undocumented = "undocumented",
        NotOnBoard = "not-on-board",
        NotApplicable = "not-applicable",
    }
}

status! {
    Implementation {
        Implemented = "implemented",
        Partial = "partial",
        NotImplemented = "not-implemented",
        External = "external",
    }
}

status! {
    Integration {
        Integrated = "integrated",
        NotIntegrated = "not-integrated",
        NotApplicable = "not-applicable",
    }
}

status! {
    HostTests { Passing = "passing", None = "none" }
}

status! {
    Simulation { Yes = "yes", None = "none" }
}

status! {
    TargetBuild {
        CiBuilt = "ci-built",
        NotBuilt = "not-built",
        NotApplicable = "not-applicable",
    }
}

status! {
    HardwareVerification {
        Verified = "verified",
        Failed = "failed",
        NotVerified = "not-verified",
        NotApplicable = "not-applicable",
    }
}

status! {
    Verdict {
        Supported = "supported",
        Partial = "partial",
        Unsupported = "unsupported",
    }
}

status! {
    EvidenceKind {
        Source = "source",
        Test = "test",
        Ci = "ci",
        HwReport = "hw-report",
        Upstream = "upstream",
    }
}
