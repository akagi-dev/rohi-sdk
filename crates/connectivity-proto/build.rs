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

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use micropb_gen::{Config, Generator};

/// Vendored schema, a verbatim copy of the revision in `UPSTREAM_REV`.
const PROTO_ROOT: &str = "proto";
const PROTOS: &[&str] = &[
    "proto/core/v1/message.proto",
    "proto/crypto/v1/envelope.proto",
    "proto/device/v1/insight.proto",
    "proto/device/v1/urban.proto",
    "proto/sensor/v1/measurement.proto",
    "proto/sensor/v1/sensor.proto",
];

/// Compile-time bounds of the telemetry frame. Written to `limits.rs` and
/// applied to the generated types, so the schema and the Rust constants cannot
/// drift.
const LIMITS: &[(&str, usize, &str)] = &[
    (
        "ED25519_KEY_LEN",
        32,
        "Size of an Ed25519 public key (`SignedEnvelope.sensor_id`), in bytes.",
    ),
    (
        "ED25519_SIGNATURE_LEN",
        64,
        "Size of an Ed25519 signature, in bytes.",
    ),
    (
        "NONCE_LEN",
        16,
        "Size of the nonce produced by this SDK, in bytes.",
    ),
    (
        "MAX_NONCE_LEN",
        32,
        "Maximum nonce size accepted when decoding an envelope, in bytes.",
    ),
    (
        "MAX_SENSORS_PER_MESSAGE",
        12,
        "Maximum number of sensor entries in one telemetry message.",
    ),
    (
        "MAX_MESSAGE_LEN",
        400,
        "Maximum size of an encoded `core.v1.Message`, in bytes.",
    ),
    (
        "MAX_ENVELOPE_LEN",
        544,
        "Maximum size of an encoded `crypto.v1.SignedEnvelope`, in bytes.",
    ),
];

fn limit(name: &str) -> u32 {
    LIMITS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, value, _)| *value as u32)
        .expect("unknown limit")
}

fn main() {
    println!("cargo:rerun-if-changed={PROTO_ROOT}");
    println!("cargo:rerun-if-changed=UPSTREAM_REV");
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    let mut limits = String::new();
    for (name, value, doc) in LIMITS {
        writeln!(limits, "/// {doc}\npub const {name}: usize = {value};").unwrap();
    }
    fs::write(out_dir.join("limits.rs"), limits).expect("write limits.rs");

    let mut generator = Generator::with_warning_callback(|_| {});
    generator.use_container_heapless();
    generator.add_protoc_arg(format!("-I{PROTO_ROOT}"));

    let sensors = limit("MAX_SENSORS_PER_MESSAGE");
    let repeated = [
        (".device.v1.Urban.public", sensors),
        (".device.v1.Insight.public", sensors),
        (".device.v1.EncryptedUrban.sensors", sensors),
        (".device.v1.EncryptedInsight.sensors", sensors),
    ];
    for (path, size) in repeated {
        generator.configure(path, Config::new().max_len(size));
    }

    let bytes = [
        (".crypto.v1.SignedEnvelope.sensor_id", "ED25519_KEY_LEN"),
        (".crypto.v1.SignedEnvelope.nonce", "MAX_NONCE_LEN"),
        (".crypto.v1.SignedEnvelope.message", "MAX_MESSAGE_LEN"),
        (
            ".crypto.v1.SignedEnvelope.signature",
            "ED25519_SIGNATURE_LEN",
        ),
    ];
    for (path, name) in bytes {
        generator.configure(path, Config::new().max_bytes(limit(name)));
    }

    // Private (encrypted) sections and batches are not produced by this SDK:
    // they are not generated, and decoding skips them as unknown fields.
    for path in [
        ".crypto.v1.SignedEnvelopeBatch",
        ".device.v1.Urban.private",
        ".device.v1.Insight.private",
    ] {
        generator.configure(path, Config::new().skip(true));
    }

    generator
        .compile_protos(PROTOS, out_dir.join("connectivity.rs"))
        .expect("compile connectivity protocol");
}
