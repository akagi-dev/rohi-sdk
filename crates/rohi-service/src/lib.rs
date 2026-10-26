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
#![no_std]

//! Management plane primitives of ROHI firmware.
//!
//! ```text
//!                 control protocol / CLI
//!                          |
//!        generic: Value, FieldDescriptor, registries
//!                          |
//!      +-------------------+-------------------+
//!      |                                       |
//!   config (desired state)               status (observed state)
//!   ManagedConfig + ManagedSection       ManagedService + Tracked
//!      |                                       |
//!   typed module-owned data              live runtime state
//! ```
//!
//! The control protocol and the `rohi` tool know no module by name. Each
//! module describes its fields once and converts between the generic
//! [`Value`] and its typed Rust fields; everything else (patching, secret
//! redaction, revisions, discovery) is generic and lives here.
//!
//! * [`config`]: [`ManagedConfig`] sections with atomic patch/reset and the
//!   [`ConfigRegistry`].
//! * [`status`]: [`ManagedService`]s with lifecycle state, health, readiness,
//!   active issues, revisions and the [`ServiceRegistry`].
//!
//! Registries are plain tuples of sections or services; adding a module means
//! adding it to the tuple.

pub mod action;
pub mod config;
mod field;
pub mod status;
mod value;

pub use action::{
    ActionDescriptor, ActionError, ActionOutput, MAX_ACTION_ARGS, MAX_RECORD_FIELDS, MAX_RECORDS,
    Records,
};
pub use config::{
    ConfigEntry, ConfigError, ConfigRegistry, ConfigSectionOps, EntryState, FieldError,
    ManagedConfig, ManagedSection, Mutation, MutationOp, Patched, SectionInfo, SectionSchema,
    SectionStore, SectionValues, StoreError,
};
pub use field::{ConfigField, FieldDescriptor};
pub use status::{
    DeviceHealth, Issue, IssueSeverity, ManagedService, ServiceHealth, ServiceInfo, ServiceOps,
    ServiceRegistry, ServiceState, ServiceStatus, ServiceStatusSummary, ServiceSummary, Startup,
    StatusError, StatusWriter, Tracked, WriteError, default_device_health,
};
pub use value::{Value, ValueKind};

/// Maximum length of a string or bytes [`Value`], in bytes.
pub const MAX_VALUE_LEN: usize = 64;
/// Maximum length of an enum variant, in bytes.
pub const MAX_VARIANT_LEN: usize = 16;
/// Maximum number of status fields of a service.
pub const MAX_FIELDS: usize = 20;
/// Maximum number of fields of a config section.
pub const MAX_CONFIG_FIELDS: usize = 16;
/// Maximum number of mutations in one patch.
pub const MAX_MUTATIONS: usize = 8;
/// Maximum number of active issues reported for one service.
pub const MAX_ISSUES: usize = 4;
/// Maximum number of sections or services in one registry.
pub const MAX_REGISTRY_ENTRIES: usize = 8;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
