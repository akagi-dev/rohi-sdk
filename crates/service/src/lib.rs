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
#![no_std]

//! Services, configuration sections, status and actions of the Robonomics
//! Open Hardware Interface (ROHI): the one place where a firmware module
//! describes itself to the outside world.
//!
//! # Overview
//!
//! A module offers up to three things, each through generic types, so the
//! control protocol, the `rohi` tool and any other client never know a module
//! by name. A module describes its fields once and converts between the
//! generic [`Value`] and its typed Rust fields; everything else (patching,
//! secret redaction, status bookkeeping, discovery) is generic and lives here.
//! Use this crate when you write a firmware module that should be visible,
//! configurable or controllable from outside; the transport and the protocol
//! are somebody else's job (see [Architecture](#architecture-and-integration)).
//!
//! | Plane | Question | Owner type | Wire operations |
//! |-------|----------|------------|-----------------|
//! | **Config** | what is the device *set up* to do? (desired, persistent) | [`Config`] in a [`Section`] | `ListConfigSections`, `GetConfigSchema`, `GetConfigSection`, `PatchConfigSection`, `ResetConfigSection` |
//! | **Status** | what is happening *now*? (observed, read-only) | [`Status`] | `GetServicesStatus`, `GetServiceSchema`, `GetServiceStatus` |
//! | **Actions** | what can I *ask it to do*? (explicit operations) | [`Action`] | `ListActions`, `GetActionSchema`, `InvokeAction` |
//!
//! Config, status and actions are separate on purpose: a service can exist
//! without a config section, a section can exist without a service, and an
//! action is never smuggled into status. The user-facing view of status is
//! described in [Status & diagnostics](https://rohi.akagi.dev/use/desktop/status.html).
//!
//! # Quick Example
//!
//! A service with a status, published through a [`Statuses`] registry. A
//! service is identified by [`#[derive(Service)]`](rohi_service_derive::Service),
//! a config section by [`#[derive(Config)]`](rohi_config_derive::Config); see
//! [`config::Section`] for a config example and [`action`] for actions.
//!
//! ```
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_service::{
//!     FieldDescriptor, Service, ServiceHealth, ServiceState, ServiceSummary, Status,
//!     StatusRegistry, StatusWriter, Statuses, Unit, Value, ValueKind, WriteError,
//! };
//!
//! // The id is the FNV-1a hash of the name "pump".
//! #[derive(Service)]
//! struct PumpService {
//!     pressure_pa: u64,
//! }
//!
//! impl Status for PumpService {
//!     const VERSION: u16 = 1;
//!     const STATUS_SCHEMA: &'static [FieldDescriptor] =
//!         &[FieldDescriptor::new(1, "pressure", ValueKind::U64).unit(Unit::Pascal)];
//!
//!     async fn summary(&self) -> ServiceSummary {
//!         ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
//!     }
//!
//!     async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
//!         out.u64(1, self.pressure_pa)
//!     }
//! }
//!
//! let pump = PumpService { pressure_pa: 101_325 };
//! let statuses = Statuses::<NoopRawMutex, _>::new((&pump,));
//!
//! let status = block_on(statuses.status(PumpService::ID, 5_000)).unwrap();
//! assert_eq!(status.summary.state, ServiceState::Running);
//! assert_eq!(status.fields[0], (1, Value::U64(101_325)));
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Type | What it is |
//! |---------|------|------------|
//! | Service identity | [`Service`] | name and id (`service_id`, FNV-1a 32 of the name), see [`service`] |
//! | Typed config | [`Config`] | the *effective* configuration a module consumes; `#[derive(Config)]` |
//! | Managed section | [`Section`] | a config with its [`SectionStore`] and one transaction lock |
//! | Status | [`Status`] | read-only snapshot of the live state, see [`status`] |
//! | Action | [`Action`] | an explicit operation with typed arguments and records, see [`action`] |
//! | Registries | [`ConfigRegistry`], [`StatusRegistry`], [`ActionRegistry`] | tuples that dispatch by numeric id, see [`registry`] |
//!
//! ## Values and fields
//!
//! Everything crossing the boundary is a [`Value`]: `Bool`, `I64`, `U64`,
//! `F32`, `String`, `Bytes` or `Enum`. Floats are single precision; a counter,
//! a timestamp or an id is an integer. An `Enum` value is a **number**
//! (`u32`). `Value`'s `Debug` shows only its kind, so a secret never reaches a
//! log by accident.
//!
//! One field is described once, as a `const`, by a [`FieldDescriptor`]
//! (`id`, `name`, `kind`, [`Unit`], `enum_id`); a config field extends it to a
//! [`ConfigField`] with `writable`, `secret`, `optional` and typed [`Bounds`].
//! The unit is a code, not text, and there is no description: documentation
//! belongs to the rustdoc and the book, not to every response of the firmware.
//! The named variants of an enum field are listed once per section, service or
//! action as an [`EnumDescriptor`]. Field ids are stable and only ever added;
//! names may use dotted paths for nested fields (`bmx280.temperature`).
//!
//! ## Identity and persistent ABI
//!
//! A service id is the 32-bit FNV-1a hash of its name ([`service::service_id`]);
//! a section id is the 64-bit FNV-1a hash of the *config type name* (computed
//! by [`rohi_config`]). Names, ids and field ids are wire and storage ABI:
//! rename or renumber them and clients or stored data no longer match.
//!
//! # Architecture and Integration
//!
//! ```text
//!        rohi-control (dispatcher)  ──►  ControlBackend { config(), statuses(), actions() }
//!                                              │               │                │
//!                                      ConfigRegistry   StatusRegistry   ActionRegistry
//!                                      (tuple)          (Statuses over   (tuple of &S)
//!                                          │             a tuple of &S)        │
//!                                Section<C, Store, M>          │               │
//!                                          │                   └─ service S: Status [+ Action]
//!                                typed section C: Config          live runtime state of S
//! ```
//!
//! The crate is `no_std`, allocation free and portable. It depends neither on
//! the control protocol nor on any feature crate; each feature crate
//! implements the traits for its own types ([`rohi-net`](../rohi_net/index.html),
//! [`rohi-connectivity`](../rohi_connectivity/index.html),
//! [`rohi-time`](../rohi_time/index.html), [`rohi-hal`](../rohi_hal/index.html),
//! [`rohi-identity`](../rohi_identity/index.html),
//! [`rohi-location`](../rohi_location/index.html)) and the dispatcher in
//! [`rohi-control`](../rohi_control/index.html) serves the registries; the
//! transports ([`rohi-control-ble`](../rohi_control_ble/index.html)) are
//! layered on top of that. The generic [`Value`], [`ValueKind`] and
//! [`FieldError`] come from [`rohi_config`], which also owns the persisted
//! override model and the flash container; the derive macros come from
//! [`rohi_config_derive`] (re-exported as [`Config`]) and
//! [`rohi_service_derive`] (re-exported as [`Service`]).
//!
//! The firmware (`firmwares/altruist-urban/src/control.rs`) builds the
//! registries over its runtime handles and gives them to the dispatcher.
//! [`registry`] lists which crate offers which section, service and action.
//!
//! # API Navigation
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`service`] | [`Service`] identity, [`service_id`] and the id algorithm, `#[derive(Service)]`. |
//! | [`config`] | [`Config`] sections: effective configuration, overrides, patch semantics, secrets, atomic reset, [`Section`], [`ConfigUpdate`], [`ConfigRegistry`]. |
//! | [`status`] | [`Status`]: lifecycle state, health, readiness, issues, [`Startup`], device health. |
//! | [`action`] | [`Action`] descriptors, arguments, records and [`ActionError`]. |
//! | [`registry`] | [`StatusRegistry`] / [`ActionRegistry`], the tracker bookkeeping, schema validation, which crate offers what. |
//!
//! # Guarantees and Limitations
//!
//! * **Errors, not truncation.** A value or name that does not fit a limit is
//!   an error ([`WriteError::TooLong`], [`FieldError`], a build error for
//!   schemas), never cut.
//! * **Schemas are checked at build time.** Names, field and enum tables and
//!   the match between a section's writable fields and its overrides are
//!   `const` assertions; an invalid declaration does not compile. Duplicate
//!   ids across a device are a build error for services and a panic in
//!   `Device::new` of `rohi-control` for config sections (in every build).
//! * **Atomic patches.** A patch is validated as a whole and stored or
//!   dropped as a whole, under a per-section lock; see [`config`].
//! * **Secrets are write-only** and wiped by the control plane after use.
//! * **Persistence and runtime publication** are the contract of the
//!   firmware's [`SectionStore`]; this crate cannot enforce them.
//! * **No `std`, no allocation.** All collections are `heapless`; async code
//!   runs on any executor, with the mutex type chosen by the firmware
//!   (`embassy-sync` raw mutex).
//!
//! ## Limits
//!
//! Bounds keep every message small enough for a microcontroller stack. The
//! constants of this crate are the in-memory side of the protocol limits,
//! which are defined once in the build script of `rohi-control-proto` and
//! listed with the control plane in [`rohi-control`](../rohi_control/index.html).
//! Schema limits are checked while building a section or a service, see
//! [`registry`].
//!
//! | Limit | Value |
//! |-------|-------|
//! | status fields per service ([`MAX_FIELDS`]) | 20 |
//! | fields per config section ([`MAX_CONFIG_FIELDS`]) | 16 |
//! | mutations per patch ([`MAX_MUTATIONS`]) | 8 |
//! | active issues per service ([`MAX_ISSUES`]) | 4 |
//! | issue descriptors per service ([`MAX_ISSUE_DESCRIPTORS`]) | 12 |
//! | action arguments ([`MAX_ACTION_ARGS`]) | 3 |
//! | action records ([`MAX_RECORDS`]) | 6 |
//! | files of a stream ([`MAX_STREAM_FILES`]), bytes of a stream chunk ([`MAX_STREAM_CHUNK`]) | 24, 512 |
//! | fields per record ([`MAX_RECORD_FIELDS`]) | 4 |
//! | string or bytes status value or action result ([`MAX_FIELD_VALUE_LEN`]) | 32 bytes |
//! | string or bytes config value or action argument ([`MAX_VALUE_LEN`]) | 64 bytes |
//! | section, service, action, field or issue name ([`MAX_NAME_LEN`]) | 24 bytes |
//! | enum variant name ([`MAX_VARIANT_NAME_LEN`]) | 16 bytes |
//! | enums per section, service or action ([`MAX_ENUMS`]) | 1 |
//! | variants per enum ([`MAX_ENUM_VARIANTS`]) | 9 |
//! | sections or services in a registry ([`MAX_REGISTRY_ENTRIES`]) | 12 |

extern crate self as rohi_service;

pub mod action;
pub mod config;
mod field;
mod isolated;
pub mod registry;
mod schema;
pub mod service;
pub mod status;

pub use action::{
    Action, ActionDescriptor, ActionError, ActionOutput, ActionResult, MAX_ACTION_ARGS,
    MAX_RECORD_FIELDS, MAX_RECORDS, MAX_STREAM_CHUNK, MAX_STREAM_FILES, Records, StreamChunk,
    StreamFile, StreamInfo,
};
pub use config::{
    Config, ConfigEntry, ConfigError, ConfigRegistry, ConfigSectionOps, ConfigUpdate, EntryState,
    FactoryReset, Mutation, MutationOp, Patched, RestartRequired, Section, SectionInfo,
    SectionSchema, SectionStore, SectionValues, StoreError,
};
pub use field::{Bounds, ConfigField, EnumDescriptor, EnumVariant, FieldDescriptor, Unit};
pub use isolated::{Isolated, isolated};
pub use registry::{ActionRegistry, ServiceIdentity, StatusRegistry, StatusSet, Statuses, Tracker};
pub use rohi_config::{FieldError, MAX_VALUE_LEN, Value, ValueKind};
pub use rohi_config_derive::Config;
pub use rohi_service_derive::Service;
pub use service::{Service, assert_service_name, distinct_ids, service_id, service_name_is_valid};
pub use status::{
    DeviceHealth, FIRST_SERVICE_ISSUE, ISSUE_STARTUP_FAILED, Issue, IssueDescriptor, IssueSeverity,
    ServiceHealth, ServiceInfo, ServiceSchema, ServiceState, ServiceStatus, ServiceStatusSummary,
    ServiceSummary, Startup, Status, StatusError, StatusWriter, WriteError, default_device_health,
};

/// Maximum length of a string or bytes status value or action result, in
/// bytes; longer values are an error, never cut. Arguments and config values
/// may be [`MAX_VALUE_LEN`] long.
pub const MAX_FIELD_VALUE_LEN: usize = 32;
/// Maximum length of a section, service, action, field or issue name, in
/// bytes. Names are checked when a section or service is built.
pub const MAX_NAME_LEN: usize = 24;
/// Maximum length of an enum variant name, in bytes.
pub const MAX_VARIANT_NAME_LEN: usize = 16;
/// Maximum number of enums of a section, service or action. A hard constraint
/// of the compact control protocol, which describes enum variants once per
/// schema: a schema that needs more enums does not fit it.
pub const MAX_ENUMS: usize = 1;
/// Maximum number of variants of an enum.
pub const MAX_ENUM_VARIANTS: usize = 9;
/// Maximum number of status fields of a service.
pub const MAX_FIELDS: usize = 20;
/// Maximum number of fields of a config section.
pub const MAX_CONFIG_FIELDS: usize = 16;
/// Maximum number of mutations in one patch.
pub const MAX_MUTATIONS: usize = 8;
/// Maximum number of active issues reported for one service.
pub const MAX_ISSUES: usize = 4;
/// Maximum number of issues the schema of a service describes.
pub const MAX_ISSUE_DESCRIPTORS: usize = 12;
/// Maximum number of sections or services in one registry.
pub const MAX_REGISTRY_ENTRIES: usize = 12;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
