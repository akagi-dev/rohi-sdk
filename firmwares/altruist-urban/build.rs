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
