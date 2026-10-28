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

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    // `BUILD_TIMESTAMP`: the build time as Unix seconds. It is the floor of
    // plausible wall-clock time, see `src/time.rs`.
    let build_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    println!("cargo:rustc-env=BUILD_TIMESTAMP={build_timestamp}");
}
