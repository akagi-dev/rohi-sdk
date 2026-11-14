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
//! Actions: explicit runtime operations a service offers, with typed
//! arguments and results.
//!
//! Status is read-only; everything that *does* something (scan, reconnect,
//! provision an identity) is an action declared by the owning service through
//! [`Action`]. Clients discover actions and their argument and result fields
//! dynamically (`ListActions`, `GetActionSchema`, `InvokeAction`).
//!
//! ```
//! use rohi_service::{ActionAccess, ActionDescriptor, FieldDescriptor, Unit, ValueKind};
//!
//! const SCAN_RESULT: &[FieldDescriptor] = &[
//!     FieldDescriptor::new(1, "ssid", ValueKind::String),
//!     FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
//! ];
//!
//! const ACTIONS: &[ActionDescriptor] = &[
//!     ActionDescriptor::new(1, "scan", ActionAccess::Read).results(SCAN_RESULT),
//!     ActionDescriptor::new(2, "reconnect", ActionAccess::Mutate),
//! ];
//!
//! assert_eq!(ACTIONS[1].access, ActionAccess::Mutate);
//! ```
//!
//! A service implements [`Action`] next to its [`Service`] identity. The
//! action registry ([`ActionRegistry`](crate::ActionRegistry), a tuple of
//! `&S`) checks the arguments and calls [`Action::invoke`]:
//!
//! ```
//! use embassy_futures::block_on;
//! use rohi_service::{
//!     Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, ActionRegistry,
//!     FieldDescriptor, Service, Unit, Value, ValueKind,
//! };
//!
//! #[derive(Service)]
//! struct WifiService;
//!
//! const SCAN_RESULT: &[FieldDescriptor] = &[
//!     FieldDescriptor::new(1, "ssid", ValueKind::String),
//!     FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
//! ];
//!
//! impl Action for WifiService {
//!     const ACTIONS: &'static [ActionDescriptor] = &[
//!         ActionDescriptor::new(1, "scan", ActionAccess::Read).results(SCAN_RESULT),
//!         ActionDescriptor::new(2, "reconnect", ActionAccess::Mutate),
//!     ];
//!
//!     async fn invoke(
//!         &self,
//!         action: u32,
//!         _args: &[(u32, Value)],
//!         out: &mut impl ActionOutput,
//!     ) -> Result<(), ActionError> {
//!         match action {
//!             1 => {
//!                 // One record per network found.
//!                 out.text(1, "home").map_err(|_| ActionError::Internal)?;
//!                 out.i64(2, -61).map_err(|_| ActionError::Internal)?;
//!                 out.next_record().map_err(|_| ActionError::Internal)?;
//!                 out.text(1, "guest").map_err(|_| ActionError::Internal)?;
//!                 out.i64(2, -80).map_err(|_| ActionError::Internal)?;
//!                 Ok(())
//!             }
//!             2 => Ok(()),
//!             _ => Err(ActionError::UnknownAction),
//!         }
//!     }
//! }
//!
//! let wifi = WifiService;
//! let actions = (&wifi,);
//! let records = block_on(actions.invoke(WifiService::ID, 1, &[])).unwrap();
//! let records = records.records();
//! assert_eq!(records.len(), 2);
//! assert_eq!(records[0][1], (2, Value::I64(-61)));
//! assert_eq!(
//!     block_on(actions.invoke(WifiService::ID, 9, &[])),
//!     Err(ActionError::UnknownAction)
//! );
//! ```
//!
//! An action takes typed **arguments** (all required, checked against the
//! descriptor before [`Action::invoke`] runs: every argument is known, of the
//! right kind and present exactly once) and returns zero or more **records**
//! of typed fields described by [`ActionDescriptor::results`]: a scan returns
//! one record per network, a reconnect none. A record is started with
//! [`ActionOutput::next_record`]; the first field written without it starts
//! the first record.
//!
//! Every action declares the [`ActionAccess`] a call needs: the link policy of
//! the control plane ([`rohi-control`](../../rohi_control/index.html)) decides
//! whether a link has it, this crate only carries the declaration.
//! [`secret`](ActionDescriptor::secret) is a separate property: it marks
//! arguments that are secrets (identity provisioning), which are never logged
//! and are wiped by the control plane after the call. It does not change the
//! access level, and an action with [`ActionAccess::Provision`] does not
//! necessarily take secrets.
//!
//! An action fails with an [`ActionError`]: `InvalidArgument`, `Busy`,
//! `Unavailable`, `Internal` or `Rejected("code")`, a refusal with a stable
//! machine-readable reason such as `already_provisioned`.
//!
//! Limits: at most [`MAX_ACTION_ARGS`] arguments, [`MAX_RECORDS`] records of
//! at most [`MAX_RECORD_FIELDS`] fields; a result string or bytes value is at
//! most [`MAX_FIELD_VALUE_LEN`](crate::MAX_FIELD_VALUE_LEN) bytes, an argument
//! at most [`MAX_VALUE_LEN`](crate::MAX_VALUE_LEN). An action has at most one
//! enum ([`MAX_SERVICE_ENUMS`](crate::MAX_SERVICE_ENUMS)).
//!
//! # Streams
//!
//! A result that does not fit the bounded records (a log history, a bundle) is
//! returned as a **stream**: the action is declared with
//! [`ActionDescriptor::stream`] and calls [`ActionOutput::stream`] with a
//! [`StreamInfo`], a snapshot of up to [`MAX_STREAM_FILES`] named files. The client
//! reads each file in chunks of at most [`MAX_STREAM_CHUNK`] bytes by offset
//! ([`Action::read_stream`], repeatable and idempotent) and closes the stream
//! ([`Action::close_stream`]). The service releases what a stream holds when it is
//! closed, when it times out and when the control link is lost
//! ([`Action::link_lost`]); a service that has no free stream answers
//! [`ActionError::Busy`]. An action returns records or a stream, never both.
//!
//! Who may use a stream is decided by the control plane, not by the service: the
//! controller lets only the connection that invoked the action read or close the
//! stream, and with the privilege of that action, so a service needs no session
//! logic of its own (see `rohi-control`, "Stream ownership").

use heapless::Vec;

use crate::MAX_NAME_LEN;
use crate::field::{EnumDescriptor, FieldDescriptor, enum_allows};
use crate::service::Service;
use crate::status::{StatusWriter, WriteError, log_rejected_field};
use rohi_config::Value;

/// Maximum number of arguments of an action.
pub const MAX_ACTION_ARGS: usize = 3;
/// Maximum number of records an action returns.
pub const MAX_RECORDS: usize = 6;
/// Maximum number of fields of one record.
pub const MAX_RECORD_FIELDS: usize = 4;
/// Maximum number of files of one stream.
pub const MAX_STREAM_FILES: usize = 24;
/// Maximum number of bytes of one stream chunk.
pub const MAX_STREAM_CHUNK: usize = 512;

/// What a link must be allowed to do to invoke an action.
///
/// The control plane maps it to the privilege its link policy checks; a
/// service declares it and never enforces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionAccess {
    /// Reads device state or runs an operation that changes nothing.
    Read,
    /// Changes the runtime or the stored configuration.
    Mutate,
    /// Changes what identifies or provisions the device: the identity or the
    /// whole configuration storage. A link policy may demand more than for
    /// [`Mutate`](Self::Mutate).
    Provision,
}

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
    /// What the link must be allowed to do to invoke the action.
    pub access: ActionAccess,
    /// Whether the arguments contain secrets: they are never logged and are
    /// overwritten after the call.
    pub secret_args: bool,
    /// Whether the action returns a stream instead of records, see
    /// [`ActionOutput::stream`].
    pub stream: bool,
}

impl ActionDescriptor {
    /// An action that needs `access`, without arguments or results.
    pub const fn new(id: u32, name: &'static str, access: ActionAccess) -> Self {
        Self {
            id,
            name,
            args: &[],
            results: &[],
            enums: &[],
            access,
            secret_args: false,
            stream: false,
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

    /// Declares that the action returns a stream (see
    /// [`ActionOutput::stream`]) instead of records; its
    /// [`results`](Self::results) stay empty.
    pub const fn stream(mut self) -> Self {
        self.stream = true;
        self
    }

    /// Marks the arguments as secret: they are never logged and are wiped
    /// after the call. The [`access`](Self::access) is declared separately.
    pub const fn secret(mut self) -> Self {
        self.secret_args = true;
        self
    }
}

/// The action capability of a [`Service`]: explicit operations a client can
/// run, with typed arguments and results.
///
/// `Action` is a capability of the service, not a service per command:
/// `WifiService` exposes `scan`, `reconnect` and `disconnect`. A
/// service implements it independently of [`Status`](crate::Status).
///
/// The action registry has already checked the arguments against the
/// descriptor when [`invoke`](Self::invoke) runs; the control plane has
/// authorized the call and wipes secret arguments after it returns.
#[allow(async_fn_in_trait)]
pub trait Action: Service {
    /// The actions of the service, see [`ActionDescriptor`].
    const ACTIONS: &'static [ActionDescriptor];

    /// Runs one of [`ACTIONS`](Self::ACTIONS); results are written to `out`
    /// as records.
    ///
    /// # Errors
    ///
    /// [`ActionError::UnknownAction`] for an id outside [`ACTIONS`](Self::ACTIONS),
    /// [`ActionError::Rejected`] for a refusal with a stable reason and the
    /// other variants as described there. A failed write into `out` converts
    /// to [`ActionError::Internal`] with `?`; nothing is returned to the
    /// client when the action fails.
    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError>;

    /// Reads a chunk of one file of an open stream this service returned,
    /// at `offset`, into `buf` (at most [`MAX_STREAM_CHUNK`] bytes).
    ///
    /// Idempotent: the same offset returns the same bytes while the stream
    /// is open. The default answers that the service has no streams.
    ///
    /// # Errors
    ///
    /// [`ActionError::UnknownStream`] when the stream or file is not open,
    /// [`ActionError::InvalidArgument`] for an offset beyond the file.
    async fn read_stream(
        &self,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        let _ = (stream, file, offset, buf);
        Err(ActionError::UnknownStream)
    }

    /// Closes a stream and releases what it holds. The default answers that
    /// the service has no streams.
    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        let _ = stream;
        Err(ActionError::UnknownStream)
    }

    /// The control link was lost: release every stream the client left open.
    fn link_lost(&self) {}
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
    /// No open stream or file with this id: it was closed, timed out or
    /// never existed.
    UnknownStream,
    /// The action ran and was refused for a reason a client may react to;
    /// the code is stable and machine-readable, for example
    /// `already_provisioned`.
    Rejected(&'static str),
    /// Unexpected failure.
    Internal,
}

/// The records an action returned.
pub type Records = Vec<Vec<(u32, Value), MAX_RECORD_FIELDS>, MAX_RECORDS>;

/// One file of a [`StreamInfo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamFile {
    /// Stable within the stream.
    pub id: u32,
    /// Name the client writes the file under, at most [`MAX_NAME_LEN`] bytes.
    pub name: heapless::String<MAX_NAME_LEN>,
    /// Size of the file in bytes.
    pub size_bytes: u64,
}

/// A snapshot of files a client reads in chunks instead of in one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// Unique among the open streams of the service.
    pub id: u32,
    /// At most [`MAX_STREAM_FILES`].
    pub files: Vec<StreamFile, MAX_STREAM_FILES>,
}

/// What an action returned.
#[allow(
    clippy::large_enum_variant,
    reason = "no heap: both variants live on the stack of the control task"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum ActionResult {
    /// Typed records, see [`ActionOutput::next_record`].
    Records(Records),
    /// A stream, see [`ActionOutput::stream`].
    Stream(StreamInfo),
}

impl ActionResult {
    /// The records, empty for a stream.
    pub fn records(&self) -> &[Vec<(u32, Value), MAX_RECORD_FIELDS>] {
        match self {
            Self::Records(records) => records,
            Self::Stream(_) => &[],
        }
    }
}

/// A chunk read from a stream file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamChunk {
    /// Bytes written to the buffer.
    pub len: usize,
    /// Whether the chunk ends the file.
    pub eof: bool,
}

/// Receives the records of an action. Besides the field writers of
/// [`StatusWriter`], [`next_record`](Self::next_record) starts a new record;
/// the first field written without it starts the first record.
pub trait ActionOutput: StatusWriter {
    /// Starts a new record.
    fn next_record(&mut self) -> Result<(), WriteError>;

    /// Returns a stream instead of records. Only for an action declared with
    /// [`ActionDescriptor::stream`] and only when no record was written. The
    /// default refuses it.
    fn stream(&mut self, info: StreamInfo) -> Result<(), WriteError> {
        let _ = info;
        Err(WriteError::WrongKind)
    }
}

pub(crate) struct RecordCollector {
    results: &'static [FieldDescriptor],
    enums: &'static [EnumDescriptor],
    streams: bool,
    pub(crate) records: Records,
    pub(crate) stream: Option<StreamInfo>,
}

impl RecordCollector {
    pub(crate) fn new(action: &ActionDescriptor) -> Self {
        Self {
            results: action.results,
            enums: action.enums,
            streams: action.stream,
            records: Vec::new(),
            stream: None,
        }
    }
}

impl StatusWriter for RecordCollector {
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        self.store(id, value)
            .inspect_err(|error| log_rejected_field("action", id, *error))
    }
}

impl RecordCollector {
    fn store(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
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

    fn stream(&mut self, info: StreamInfo) -> Result<(), WriteError> {
        if !self.streams || !self.records.is_empty() || self.stream.is_some() {
            log::debug!(target: "service", "Action stream result rejected");
            return Err(WriteError::WrongKind);
        }
        self.stream = Some(info);
        Ok(())
    }
}

/// Checks `args` against the descriptor: every argument is known, of the
/// right kind, and present exactly once.
pub(crate) fn check_args(
    action: &ActionDescriptor,
    args: &[(u32, Value)],
) -> Result<(), ActionError> {
    if args.len() != action.args.len() {
        log_bad_args(action.name, "argument count", args.len() as u32);
        return Err(ActionError::InvalidArgument);
    }
    for field in action.args {
        let mut matching = args.iter().filter(|(id, _)| *id == field.id);
        let (Some((_, value)), None) = (matching.next(), matching.next()) else {
            log_bad_args(action.name, "missing or repeated argument", field.id);
            return Err(ActionError::InvalidArgument);
        };
        if value.kind() != field.kind || !enum_ok(action.enums, field, value) {
            log_bad_args(action.name, "wrong kind or enum variant", field.id);
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

/// Argument values are never logged: an action may take secrets.
#[cold]
#[inline(never)]
fn log_bad_args(action: &'static str, reason: &'static str, detail: u32) {
    log::debug!(
        target: "service",
        "Action arguments rejected: {} (action={} id={})",
        reason,
        action,
        detail
    );
}
