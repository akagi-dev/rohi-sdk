use std::time::{SystemTime, UNIX_EPOCH};

/// 2026-01-01T00:00:00Z: no firmware of this SDK was built earlier.
const EARLIEST_BUILD_TIME: u64 = 1_767_225_600;

fn main() {
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    // Build time as Unix seconds, the floor of plausible wall-clock time (see
    // `src/time.rs`). `SOURCE_DATE_EPOCH` makes it reproducible, but only a
    // value that can be a real build time is used: Nix shells set it to
    // 1980-01-01, which would make every current time look too far in the
    // future.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let build_time = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&time| time >= EARLIEST_BUILD_TIME)
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        });
    println!("cargo:rustc-env=ROHI_BUILD_UNIX_TIME={build_time}");
}
