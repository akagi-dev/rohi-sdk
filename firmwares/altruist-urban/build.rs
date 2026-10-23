use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    // Build time as Unix seconds, the floor of plausible wall-clock time (see
    // `src/time.rs`). `SOURCE_DATE_EPOCH` makes it reproducible.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let build_time = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        });
    println!("cargo:rustc-env=ROHI_BUILD_UNIX_TIME={build_time}");
}
