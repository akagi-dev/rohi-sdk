use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use micropb_gen::{Config, Generator};

const PROTO: &str = "proto/rohi/control/v1/control.proto";

/// Compile-time bounds of the protocol. Written to `limits.rs` and applied to
/// the generated types, so the schema and the Rust constants cannot drift.
const LIMITS: &[(&str, usize, &str)] = &[
    (
        "MAX_REQUEST_LEN",
        256,
        "Maximum size of an encoded `Request`, in bytes.",
    ),
    (
        "MAX_RESPONSE_LEN",
        2048,
        "Maximum size of an encoded `Response`, in bytes.",
    ),
    (
        "MAX_TEXT_LEN",
        32,
        "Maximum length of a name or version string.",
    ),
    (
        "MAX_SSID_LEN",
        32,
        "Maximum length of a Wi-Fi SSID, in bytes.",
    ),
    (
        "MAX_WIFI_PASSWORD_LEN",
        64,
        "Maximum length of a Wi-Fi password, in bytes.",
    ),
    (
        "MAX_SENSORS",
        8,
        "Maximum number of sensors in one response.",
    ),
    (
        "MAX_MEASUREMENTS_PER_SENSOR",
        4,
        "Maximum number of measurements of one sensor.",
    ),
    (
        "MAX_WIFI_SCAN_RESULTS",
        16,
        "Maximum number of networks in one scan response.",
    ),
    (
        "MAX_CAPABILITIES",
        16,
        "Maximum number of advertised capabilities.",
    ),
    (
        "ED25519_KEY_LEN",
        32,
        "Size of an Ed25519 secret or public key, in bytes.",
    ),
];

/// Repeated fields are bounded in elements; strings and bytes in bytes.
const REPEATED: &[&str] = &[
    ".rohi.control.v1.GetCapabilitiesResponse.capabilities",
    ".rohi.control.v1.Sensor.measurements",
    ".rohi.control.v1.GetSensorsResponse.sensors",
    ".rohi.control.v1.ScanWifiResponse.networks",
];

fn limit(name: &str) -> usize {
    LIMITS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, value, _)| *value)
        .expect("unknown limit")
}

fn main() {
    println!("cargo:rerun-if-changed={PROTO}");
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    let mut limits = String::new();
    for (name, value, doc) in LIMITS {
        writeln!(limits, "/// {doc}\npub const {name}: usize = {value};").unwrap();
    }
    fs::write(out_dir.join("limits.rs"), limits).expect("write limits.rs");

    // The Debug removal below is intentional; do not turn it into build warnings.
    let mut generator = Generator::with_warning_callback(|_| {});
    generator.use_container_heapless();

    let bounded = [
        (".rohi.control.v1.GetInfoResponse.firmware", "MAX_TEXT_LEN"),
        (".rohi.control.v1.GetInfoResponse.board", "MAX_TEXT_LEN"),
        (
            ".rohi.control.v1.GetCapabilitiesResponse.capabilities",
            "MAX_CAPABILITIES",
        ),
        (".rohi.control.v1.Sensor.name", "MAX_TEXT_LEN"),
        (
            ".rohi.control.v1.Sensor.measurements",
            "MAX_MEASUREMENTS_PER_SENSOR",
        ),
        (".rohi.control.v1.GetSensorsResponse.sensors", "MAX_SENSORS"),
        (".rohi.control.v1.WifiConfig.ssid", "MAX_SSID_LEN"),
        (".rohi.control.v1.WifiConfigUpdate.ssid", "MAX_SSID_LEN"),
        (
            ".rohi.control.v1.WifiConfigUpdate.password",
            "MAX_WIFI_PASSWORD_LEN",
        ),
        (".rohi.control.v1.WifiNetwork.ssid", "MAX_SSID_LEN"),
        (".rohi.control.v1.WifiConnection.ssid", "MAX_SSID_LEN"),
        (
            ".rohi.control.v1.ScanWifiResponse.networks",
            "MAX_WIFI_SCAN_RESULTS",
        ),
        (
            ".rohi.control.v1.IdentityInfo.public_key",
            "ED25519_KEY_LEN",
        ),
        (
            ".rohi.control.v1.ProvisionIdentityRequest.ed25519_secret",
            "ED25519_KEY_LEN",
        ),
        (
            ".rohi.control.v1.ProvisionIdentityResponse.public_key",
            "ED25519_KEY_LEN",
        ),
    ];
    for (path, name) in bounded {
        let size = limit(name) as u32;
        let config = if REPEATED.contains(&path) {
            Config::new().max_len(size)
        } else {
            Config::new().max_bytes(size)
        };
        generator.configure(path, config);
    }

    // The Wi-Fi password must never reach a log: no derived `Debug` for the
    // message carrying it (the generator drops it for everything containing it).
    generator.configure(
        ".rohi.control.v1.WifiConfigUpdate",
        Config::new().no_debug_impl(true),
    );
    // Same for the identity secret.
    generator.configure(
        ".rohi.control.v1.ProvisionIdentityRequest",
        Config::new().no_debug_impl(true),
    );

    generator
        .compile_protos(&[PROTO], out_dir.join("control.rs"))
        .expect("compile control.proto");
}
