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
//! Actions: explicit runtime operations a service offers, with typed
//! arguments and results.
//!
//! Status is read-only; anything that changes the runtime (scan, reconnect,
//! provision an identity) is an action declared by the owning service. Clients
//! discover actions and their argument and result fields dynamically.
//!
//! An action returns zero or more *records*; each record is a list of field
//! values described by [`ActionDescriptor::results`]. A scan returns one
//! record per network, a reconnect returns none.

use heapless::Vec;

use crate::field::{EnumDescriptor, FieldDescriptor, enum_allows};
use crate::service::Service;
use crate::status::{StatusWriter, WriteError};
use crate::value::Value;

/// Maximum number of arguments of an action.
pub const MAX_ACTION_ARGS: usize = 3;
/// Maximum number of records an action returns.
pub const MAX_RECORDS: usize = 6;
/// Maximum number of fields of one record.
pub const MAX_RECORD_FIELDS: usize = 4;

/// Static description of an action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionDescriptor {
    /// Stable numeric id within the service, the address of requests. Only
    /// ever added, never reused.
    pub id: u32,
    /// Stable name within the service, for example `scan`.
    pub name: &'static str,
    /// Arguments; all are required. At most [`MAX_ACTION_ARGS`].
    pub args: &'static [FieldDescriptor],
    /// Fields of every returned record. At most [`MAX_RECORD_FIELDS`].
    pub results: &'static [FieldDescriptor],
    /// The enums the argument and result fields refer to.
    pub enums: &'static [EnumDescriptor],
    /// Whether the link policy must grant write privilege.
    pub privileged: bool,
    /// Whether the arguments contain secrets: they are never logged and are
    /// overwritten after the call.
    pub secret_args: bool,
}

impl ActionDescriptor {
    /// An unprivileged action without arguments or results.
    pub const fn new(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            args: &[],
            results: &[],
            enums: &[],
            privileged: false,
            secret_args: false,
        }
    }

    /// Sets the arguments.
    pub const fn args(mut self, args: &'static [FieldDescriptor]) -> Self {
        self.args = args;
        self
    }

    /// Sets the result fields.
    pub const fn results(mut self, results: &'static [FieldDescriptor]) -> Self {
        self.results = results;
        self
    }

    /// Sets the enums the fields refer to.
    pub const fn enums(mut self, enums: &'static [EnumDescriptor]) -> Self {
        self.enums = enums;
        self
    }

    /// Requires write privilege.
    pub const fn privileged(mut self) -> Self {
        self.privileged = true;
        self
    }

    /// Marks the arguments as secret; implies [`privileged`](Self::privileged).
    pub const fn secret(mut self) -> Self {
        self.secret_args = true;
        self.privileged = true;
        self
    }
}

/// The action capability of a [`Service`]: explicit operations a client can
/// run, with typed arguments and results.
///
/// `Action` is a capability of the service, not a service per command:
/// `WifiService` exposes `scan`, `connect`, `reconnect` and `disconnect`. A
/// service implements it independently of [`Status`](crate::Status).
///
/// The action registry has already authorized the call and checked the
/// arguments against the descriptor when [`invoke`](Self::invoke) runs;
/// secret arguments are wiped by the caller after it returns.
#[allow(async_fn_in_trait)]
pub trait Action: Service {
    /// The actions of the service, see [`ActionDescriptor`].
    const ACTIONS: &'static [ActionDescriptor];

    /// Runs one of [`ACTIONS`](Self::ACTIONS); results are written to `out`
    /// as records.
    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError>;
}

/// Why an action failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// No service with this id.
    UnknownService,
    /// The service has no such action.
    UnknownAction,
    /// An argument is missing, unknown, of the wrong kind or invalid.
    InvalidArgument,
    /// The service is busy; retry later.
    Busy,
    /// The service is currently unavailable.
    Unavailable,
    /// The action ran and was refused for a reason a client may react to;
    /// the code is stable and machine-readable, for example
    /// `already_provisioned`.
    Rejected(&'static str),
    /// Unexpected failure.
    Internal,
}

/// The records an action returned.
pub type Records = Vec<Vec<(u32, Value), MAX_RECORD_FIELDS>, MAX_RECORDS>;

/// Receives the records of an action. Besides the field writers of
/// [`StatusWriter`], [`next_record`](Self::next_record) starts a new record;
/// the first field written without it starts the first record.
pub trait ActionOutput: StatusWriter {
    /// Starts a new record.
    fn next_record(&mut self) -> Result<(), WriteError>;
}

pub(crate) struct RecordCollector {
    results: &'static [FieldDescriptor],
    enums: &'static [EnumDescriptor],
    pub(crate) records: Records,
}

impl RecordCollector {
    pub(crate) fn new(action: &ActionDescriptor) -> Self {
        Self {
            results: action.results,
            enums: action.enums,
            records: Vec::new(),
        }
    }
}

impl StatusWriter for RecordCollector {
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        let field = self
            .results
            .iter()
            .find(|field| field.id == id)
            .ok_or(WriteError::UnknownField)?;
        if field.kind != value.kind() || !enum_ok(self.enums, field, &value) {
            return Err(WriteError::WrongKind);
        }
        if self.records.is_empty() {
            self.next_record()?;
        }
        let record = self.records.last_mut().ok_or(WriteError::Full)?;
        record.push((id, value)).map_err(|_| WriteError::Full)
    }
}

impl ActionOutput for RecordCollector {
    fn next_record(&mut self) -> Result<(), WriteError> {
        self.records.push(Vec::new()).map_err(|_| WriteError::Full)
    }
}

/// Checks `args` against the descriptor: every argument is known, of the
/// right kind, and present exactly once.
pub(crate) fn check_args(
    action: &ActionDescriptor,
    args: &[(u32, Value)],
) -> Result<(), ActionError> {
    if args.len() != action.args.len() {
        return Err(ActionError::InvalidArgument);
    }
    for field in action.args {
        let mut matching = args.iter().filter(|(id, _)| *id == field.id);
        let (Some((_, value)), None) = (matching.next(), matching.next()) else {
            return Err(ActionError::InvalidArgument);
        };
        if value.kind() != field.kind || !enum_ok(action.enums, field, value) {
            return Err(ActionError::InvalidArgument);
        }
    }
    Ok(())
}

/// Whether an enum value is a variant of the enum of its field; any other
/// value passes.
pub(crate) fn enum_ok(enums: &[EnumDescriptor], field: &FieldDescriptor, value: &Value) -> bool {
    match value {
        Value::Enum(variant) => enum_allows(enums, field.enum_id, *variant),
        _ => true,
    }
}
