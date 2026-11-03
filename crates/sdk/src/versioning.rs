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
//! Versioning and ABI.
//!
//! Several things in the SDK are contracts between independently updated
//! parties: a firmware and the data an older firmware wrote, a device and its
//! desktop client. They have their own version rules; ordinary Rust API
//! changes follow semver of the crates.
//!
//! | Contract | Compatibility rule | Where it is specified |
//! |----------|--------------------|-----------------------|
//! | **Persistent storage** (container, section IDs, CBOR payloads) | New firmware reads old data; old firmware preserves unknown sections. Renaming a type, renumbering a field or changing a representation is an ABI break. The overrides sections (`WifiConfig`, `NetworkConfig`, `BleSettings`, `ConnectivityConfig`, `LocationConfig`, `NtpServersConfig`) changed from a CBOR array to a CBOR map without a version bump; old data is not migrated and must be reset |  [Persistent ABI](https://rohi.akagi.dev/reference/persistent-abi.html), [`rohi-config`](../../rohi_config/index.html) |
//! | **Control protocol** (protobuf messages, `PROTOCOL_VERSION`) | Field numbers and enum values are stable and only ever added; clients discover sections, services and actions with the `List...` requests instead of inferring features from the firmware version; the protocol is under development and `PROTOCOL_VERSION` (currently 1) is not bumped yet | [`rohi-control-proto`](../../rohi_control_proto/index.html), [`rohi-control`](../../rohi_control/index.html) |
//! | **BLE framing** (frame header, GATT UUIDs) | Frame `version` byte (`FRAME_VERSION`, currently 1); incompatible changes bump it | [`rohi_control_ble::frame`](../../rohi_control_ble/frame/index.html), [`rohi-control-ble`](../../rohi_control_ble/index.html) |
//! | **Rust APIs of the crates** | Semver; `ControlBackend`, the `rohi-service` traits and the backend traits are extension points and change deliberately (see changelogs in commit history) | the rustdoc of each crate |
//! | **Partition table / flash layout** | Mirrored by constants in `rohi_config_esp::layout`; a host test keeps both in sync | [`rohi_config_esp::layout`](../../rohi_config_esp/layout/index.html) |
//!
//! # Rules of thumb
//!
//! - Never reuse or renumber a persistent field or protobuf field number; only
//!   append.
//! - Pin every persistent encoding with a golden test; a failing golden test
//!   is a decision, not an inconvenience.
//! - A schema change that old data must survive needs a section version bump
//!   and a migration hook (`decode_versioned`).
//! - Version policy is explicit: the default decoder accepts only
//!   `stored version == T::VERSION`; CBOR's tolerance of unknown fields does
//!   not replace it.
//! - Secrets are write-only on every interface and never printed (the Wi-Fi
//!   password, the device identity secret).
//! - Update the documentation in the same change when you touch any of these:
//!   the rustdoc of the owning crate and, for the persistent ABI, the
//!   [reference page](https://rohi.akagi.dev/reference/persistent-abi.html)
//!   (see the
//!   [documentation policy](https://rohi.akagi.dev/contributing/docs-style.html)).
