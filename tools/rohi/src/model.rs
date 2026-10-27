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
//! Plain Rust model of the control protocol.
//!
//! These types are what applications use. They hold no protobuf types and no
//! device module is known by name: sections, services, actions and fields are
//! whatever the device describes. With the `serde` feature they serialize to
//! JSON.

use core::fmt;

#[cfg(feature = "serde")]
use serde::{Serialize, Serializer};

use rohi_control_proto::control as pb;

macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $name:ident: $proto:path {
            $($(#[$vmeta:meta])* $variant:ident = $pb:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "serde", derive(Serialize))]
        #[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
            /// A value this version of the library does not know.
            Unrecognized,
        }

        impl $name {
            pub(crate) fn from_proto(value: $proto) -> Self {
                $(if value == <$proto>::$pb { return Self::$variant; })+
                Self::Unrecognized
            }

            /// Stable lower-case name.
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                    Self::Unrecognized => "unrecognized",
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

open_enum! {
    /// Typed error of the control protocol.
    ErrorCode: pb::ErrorCode {
        InvalidArgument = InvalidArgument => "invalid argument",
        NotFound = NotFound => "not found",
        NotSupported = NotSupported => "not supported",
        Busy = Busy => "busy",
        Unavailable = Unavailable => "unavailable",
        PermissionDenied = PermissionDenied => "permission denied",
        Internal = Internal => "internal error",
        Conflict = Conflict => "conflict",
        Rejected = Rejected => "rejected",
    }
}

open_enum! {
    /// Lifecycle state of a service.
    ServiceState: pb::ServiceState {
        Disabled = Disabled => "disabled",
        Stopped = Stopped => "stopped",
        Starting = Starting => "starting",
        Running = Running => "running",
        Stopping = Stopping => "stopping",
        Failed = Failed => "failed",
    }
}

open_enum! {
    /// Operational quality of a service.
    ServiceHealth: pb::ServiceHealth {
        Unknown = Unknown => "unknown",
        Ok = Ok => "ok",
        Degraded = Degraded => "degraded",
        Error = Error => "error",
    }
}

open_enum! {
    /// Health of the whole device, decided by the firmware.
    DeviceHealth: pb::DeviceHealth {
        Ok = Ok => "ok",
        Degraded = Degraded => "degraded",
        Error = Error => "error",
    }
}

open_enum! {
    /// Severity of an active issue.
    IssueSeverity: pb::IssueSeverity {
        Info = Info => "info",
        Warning = Warning => "warning",
        Error = Error => "error",
        Critical = Critical => "critical",
    }
}

open_enum! {
    /// Type of a value.
    ValueKind: pb::ValueKind {
        Bool = Bool => "bool",
        I64 = I64 => "i64",
        U64 = U64 => "u64",
        F32 = F32 => "f32",
        String = String => "string",
        Bytes = Bytes => "bytes",
        Enum = Enum => "enum",
    }
}

/// A generic typed value of a config field, a status field or an action.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `true` or `false`.
    Bool(bool),
    /// Signed integer.
    I64(i64),
    /// Unsigned integer.
    U64(u64),
    /// Single-precision float.
    F32(f32),
    /// Text.
    String(String),
    /// Bytes; text and JSON use lower-case hex.
    Bytes(Vec<u8>),
    /// A variant of an enum field, by the name the schema gives it. The wire
    /// carries a number; the client translates with the schema.
    Enum(String),
}

impl Value {
    /// The kind of the value.
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::I64(_) => ValueKind::I64,
            Self::U64(_) => ValueKind::U64,
            Self::F32(_) => ValueKind::F32,
            Self::String(_) => ValueKind::String,
            Self::Bytes(_) => ValueKind::Bytes,
            Self::Enum(_) => ValueKind::Enum,
        }
    }
}

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::I64(value) => write!(f, "{value}"),
            Self::U64(value) => write!(f, "{value}"),
            Self::F32(value) => write!(f, "{value}"),
            Self::String(value) | Self::Enum(value) => f.write_str(value),
            Self::Bytes(value) => f.write_str(&hex(value)),
        }
    }
}

#[cfg(feature = "serde")]
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::I64(value) => serializer.serialize_i64(*value),
            Self::U64(value) => serializer.serialize_u64(*value),
            // The shortest decimal of the f32, not its binary expansion as f64.
            Self::F32(value) => match value.to_string().parse::<f64>() {
                Ok(number) if value.is_finite() => serializer.serialize_f64(number),
                _ => serializer.serialize_none(),
            },
            Self::String(value) | Self::Enum(value) => serializer.serialize_str(value),
            Self::Bytes(value) => serializer.serialize_str(&hex(value)),
        }
    }
}

/// Description of one field.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct FieldDescriptor {
    /// Stable id within the section, service or action.
    pub id: u32,
    /// Name; nested fields use dotted paths.
    pub name: String,
    /// Type.
    pub kind: ValueKind,
    /// Unit symbol, empty when unitless.
    pub unit: String,
    /// Names of the allowed values of an enum field.
    pub variants: Vec<String>,
    /// The number of each of `variants` on the wire.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) variant_values: Vec<u32>,
}

/// A field value together with the id of its field.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct FieldValue {
    /// Field id.
    pub field_id: u32,
    /// The value.
    pub value: Value,
}

/// Static description of the device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct DeviceInfo {
    /// Firmware name and version.
    pub firmware: String,
    /// Board and chip.
    pub board: String,
}

// -- config -------------------------------------------------------------------

/// A config section as listed by the device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct SectionInfo {
    /// Numeric id, the address of requests.
    pub id: u64,
    /// Section name.
    pub name: String,
    /// Version of the section schema.
    pub version: u32,
    /// Changes after every successful write or reset.
    pub revision: u32,
}

/// A config field.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ConfigField {
    /// Common description.
    pub field: FieldDescriptor,
    /// Whether clients may change it.
    pub writable: bool,
    /// Write-only: never returned.
    pub secret: bool,
    /// May be unset.
    pub optional: bool,
}

/// Schema of a config section.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ConfigSchema {
    /// The section.
    pub section: SectionInfo,
    /// Its fields.
    pub fields: Vec<ConfigField>,
}

/// State of a field in [`ConfigValues`].
#[derive(Clone, Debug, PartialEq)]
pub enum EntryState {
    /// The current value.
    Value(Value),
    /// A set secret.
    Redacted,
}

/// One set field.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigEntry {
    /// Field id.
    pub field_id: u32,
    /// Value or redaction.
    pub state: EntryState,
}

/// The readable content of a config section.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigValues {
    /// The section.
    pub section: SectionInfo,
    /// Whether anything is persisted.
    pub configured: bool,
    /// The fields that are set.
    pub entries: Vec<ConfigEntry>,
}

/// What a mutation does.
#[derive(Clone, Debug, PartialEq)]
pub enum MutationOp {
    /// Sets the field.
    Set(Value),
    /// Unsets the field.
    Unset,
}

/// One change of a patch.
#[derive(Clone, Debug, PartialEq)]
pub struct Mutation {
    /// Field id.
    pub field_id: u32,
    /// The change.
    pub op: MutationOp,
}

/// Outcome of a patch or reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Applied {
    /// Revision after the change.
    pub revision: u32,
    /// Whether the change needs a restart.
    pub restart_required: bool,
}

// -- status -------------------------------------------------------------------

/// A service as listed by the device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ServiceInfo {
    /// Numeric id, the address of requests.
    pub id: u32,
    /// Service name.
    pub name: String,
    /// Version of the status schema.
    pub version: u32,
}

/// Compact status of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ServiceSummary {
    /// Numeric id, the address of requests.
    pub id: u32,
    /// Service name.
    pub service: String,
    /// Version of the status schema.
    pub version: u32,
    /// Lifecycle state.
    pub state: ServiceState,
    /// Operational quality.
    pub health: ServiceHealth,
    /// Whether the service can currently do its job.
    pub ready: bool,
    /// Changes when state, health, readiness or issues change.
    pub revision: u32,
    /// Time in the current state.
    pub state_age_ms: u64,
}

/// Compact status of the whole device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ServicesStatus {
    /// Device uptime.
    pub uptime_ms: u64,
    /// Health decided by the firmware.
    pub health: DeviceHealth,
    /// All services.
    pub services: Vec<ServiceSummary>,
}

/// Status schema of a service.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ServiceSchema {
    /// The service.
    pub service: ServiceInfo,
    /// Status fields.
    pub fields: Vec<FieldDescriptor>,
    /// The issues the service can report.
    pub issues: Vec<IssueInfo>,
}

/// The name of an issue code, from the schema of its service.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct IssueInfo {
    /// Numeric code.
    pub code: u32,
    /// Stable machine-readable name.
    pub name: String,
}

/// An active issue of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Issue {
    /// Severity.
    pub severity: IssueSeverity,
    /// Numeric code.
    pub id: u32,
    /// Stable machine-readable name of the code, from the service schema.
    pub code: String,
    /// How long it has been active.
    pub age_ms: u64,
}

/// Detailed status of a service.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceStatus {
    /// Compact status.
    pub summary: ServiceSummary,
    /// Service-specific values.
    pub fields: Vec<FieldValue>,
    /// Active issues.
    pub issues: Vec<Issue>,
}

// -- actions ------------------------------------------------------------------

/// An action as listed by the device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ActionInfo {
    /// Id of the service that offers it.
    pub service_id: u32,
    /// Name of the service that offers it.
    pub service: String,
    /// Numeric id within the service, the address of requests.
    pub id: u32,
    /// Action name.
    pub name: String,
    /// Whether the link must satisfy the write policy.
    pub privileged: bool,
    /// Whether the arguments are secrets; they must be read with `NAME=-`.
    pub secret_args: bool,
}

/// Schema of an action.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ActionSchema {
    /// The action.
    pub action: ActionInfo,
    /// Arguments; all are required.
    pub args: Vec<FieldDescriptor>,
    /// Fields of every result record.
    pub results: Vec<FieldDescriptor>,
}

/// One result record of an action.
pub type Record = Vec<FieldValue>;
