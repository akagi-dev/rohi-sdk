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

//! Identity of this firmware build, from the `build.rs` of the crate.
//!
//! [`BUILD`] is `<platform>/<version>+<sha8>.<YYYYMMDD>`, for example
//! `urban-esp32c3/0.8.0+5c582f4a.20261122`: the platform (`urban-esp32c3`,
//! `urban-esp32c6` or `urban-std`), `CARGO_PKG_VERSION`, the first eight
//! characters of the Git commit (`ROHI_BUILD_SHA` overrides `git` in CI) and
//! the UTC date of `BUILD_TIMESTAMP` (`SOURCE_DATE_EPOCH` makes it
//! reproducible). The Control API reports it as `build` and the telemetry
//! HTTP requests carry it as `User-Agent`.

use rohi_sdk::connectivity_http::MAX_USER_AGENT_LEN;

/// Firmware name, without a version.
pub const FIRMWARE: &str = env!("CARGO_PKG_NAME");

/// Board and chip.
pub const BOARD: &str = env!("ROHI_BUILD_PLATFORM");

/// The build identifier.
pub const BUILD: &str = concat!(
    env!("ROHI_BUILD_PLATFORM"),
    "/",
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("ROHI_BUILD_SHA8"),
    ".",
    env!("ROHI_BUILD_DATE"),
);

const _: () = assert!(
    BUILD.len() <= MAX_USER_AGENT_LEN,
    "the build identifier does not fit the HTTP request head or the Control `build` field"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_has_the_documented_shape() {
        let (platform, rest) = BUILD.split_once('/').unwrap();
        assert_eq!(platform, BOARD);
        let (version, rest) = rest.split_once('+').unwrap();
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        let (sha8, date) = rest.split_once('.').unwrap();
        assert_eq!(sha8.len(), 8);
        assert!(sha8.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(date.len(), 8);
        assert!(date.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(FIRMWARE, "altruist-urban");
    }
}
