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

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// 2026-01-01T00:00:00Z, the floor that `src/time.rs` asserts.
const MIN_BUILD_TIMESTAMP: u64 = 1_767_225_600;

fn main() {
    // Only the ESP binary is linked with the `esp-hal` linker script.
    println!("cargo:rustc-link-arg-bin=altruist-urban-firmware=-Tlinkall.x");

    // `BUILD_TIMESTAMP`: the build time as Unix seconds. It is the floor of
    // plausible wall-clock time, see `src/time.rs`. `SOURCE_DATE_EPOCH` makes
    // it, and the build date below, reproducible; a value before the floor
    // (the 1980 epoch that Nix shells export) is not a build time and is
    // ignored.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let build_timestamp = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.trim().parse::<u64>().ok())
        .filter(|epoch| *epoch >= MIN_BUILD_TIMESTAMP)
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        });
    println!("cargo:rustc-env=BUILD_TIMESTAMP={build_timestamp}");

    // The parts of the build identifier, see `src/build.rs`.
    println!("cargo:rustc-env=ROHI_BUILD_PLATFORM={}", platform());
    println!("cargo:rustc-env=ROHI_BUILD_SHA8={}", sha8());
    println!(
        "cargo:rustc-env=ROHI_BUILD_DATE={}",
        utc_date(build_timestamp)
    );
}

/// The board of the build, from the selected platform feature.
fn platform() -> &'static str {
    let enabled = |feature: &str| std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some();
    if enabled("ESP32C3") {
        "urban-esp32c3"
    } else if enabled("ESP32C6") {
        "urban-esp32c6"
    } else {
        "urban-std"
    }
}

/// The first eight hex digits of the commit: `ROHI_BUILD_SHA` (CI) or `git`,
/// `00000000` when neither tells.
fn sha8() -> String {
    println!("cargo:rerun-if-env-changed=ROHI_BUILD_SHA");
    for path in ["HEAD", "refs/heads"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={}", path.trim());
        }
    }
    let sha = std::env::var("ROHI_BUILD_SHA")
        .ok()
        .or_else(|| git(&["rev-parse", "HEAD"]));
    sha.map(|sha| sha.trim().to_ascii_lowercase())
        .filter(|sha| sha.len() >= 8 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map_or_else(|| "00000000".to_owned(), |sha| sha[..8].to_owned())
}

/// The output of `git ARGS`, `None` if it fails.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `YYYYMMDD` of Unix seconds, in UTC.
fn utc_date(timestamp: u64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let days = (timestamp / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}")
}
