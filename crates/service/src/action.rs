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
//! use rohi_service::{
//!     ActionAccess, ActionDescriptor, FieldDescriptor, ResultFormat, Unit, ValueKind,
//! };
//!
//! const SCAN_RESULT: &[FieldDescriptor] = &[
//!     FieldDescriptor::new(1, "ssid", ValueKind::String),
//!     FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
//! ];
//!
//! const ACTIONS: &[ActionDescriptor] = &[
//!     ActionDescriptor::new(1, "scan", ActionAccess::Read)
//!         .format(ResultFormat::Records)
//!         .results(SCAN_RESULT),
//!     ActionDescriptor::new(2, "reconnect", ActionAccess::Mutate),
//! ];
//!
//! assert_eq!(ACTIONS[1].access, ActionAccess::Mutate);
//! assert_eq!(ACTIONS[1].format, ResultFormat::Empty);
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
//!     ActionResult, FieldDescriptor, ResultFormat, Service, Unit, Value, ValueKind,
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
//!         ActionDescriptor::new(1, "scan", ActionAccess::Read)
//!             .format(ResultFormat::Records)
//!             .results(SCAN_RESULT),
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
//! let result = block_on(actions.invoke(WifiService::ID, 1, &[])).unwrap();
//! let records = result.records();
//! assert_eq!(records.len(), 2);
//! assert_eq!(records[0][1], (2, Value::I64(-61)));
//! assert_eq!(
//!     block_on(actions.invoke(WifiService::ID, 2, &[])),
//!     Ok(ActionResult::Empty)
//! );
//! assert_eq!(
//!     block_on(actions.invoke(WifiService::ID, 9, &[])),
//!     Err(ActionError::UnknownAction)
//! );
//! ```
//!
//! An action takes typed **arguments** (all required, checked against the
//! descriptor before [`Action::invoke`] runs: every argument is known, of the
//! right kind and present exactly once) and returns one result in the
//! [`ResultFormat`] it declares.
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
//! # Result formats
//!
//! The names describe the logical shape and the guarantees, not how many
//! messages the transfer took. An action declares **one** format at design
//! time with [`ActionDescriptor::format`] and never switches between formats
//! with the size of a result; the descriptor is checked when the registry is
//! built.
//!
//! | Format | Result | Lifecycle | Example |
//! | --- | --- | --- | --- |
//! | [`Empty`](ResultFormat::Empty) | nothing, by contract | one reply | `wireless.wifi_disconnect` |
//! | [`Inline`](ResultFormat::Inline) | one record of typed fields | one reply | `control.tcp_generate_psk` |
//! | [`Records`](ResultFormat::Records) | a finite collection of records, paginated by the control plane | snapshot, continuation | `wireless.wifi_scan` |
//! | [`Feed`](ResultFormat::Feed) | one bounded page of a changing, cursor-addressed source | stateless polling | `logging.tail` |
//! | [`Stream`](ResultFormat::Stream) | a binary snapshot of named files | open, read, close | `logging.download` |
//!
//! An empty collection is an empty `Records`, never `Empty`; `Inline` is one
//! object, not a one-element collection. `Empty` has no result fields,
//! `Inline` and `Records` have [`ActionDescriptor::results`] (a field written
//! at most once per record, at most [`MAX_RECORD_FIELDS`] of them), `Feed`
//! and `Stream` have none.
//!
//! # Inline
//!
//! The action writes the fields of one record with the [`StatusWriter`]
//! methods of [`ActionOutput`]. A secret result (a generated key) is an
//! `Inline` result of a [`secure_link`](ActionDescriptor::secure_link) action.
//!
//! # Records
//!
//! A short collection (at most [`MAX_RECORDS`] records) is written directly:
//! [`ActionOutput::next_record`] starts a record, the first field written
//! without it starts the first record. A longer collection stays in the
//! service in its native compact form: the action creates a **snapshot** and
//! reports it with [`ActionOutput::snapshot`] ([`SnapshotInfo`]: an id unique
//! among the snapshots of the service and the number of records). The control
//! plane then reads it page by page with [`Action::read_records`]: the records
//! from an index on, at most [`MAX_RECORDS`] of them, a record never split.
//! Reading is repeatable: the same index returns the same records while the
//! snapshot is held. The service releases it when the client closes it
//! ([`Action::close_records`]), when it expires (the service keeps its own
//! deadline) and when the control link is lost ([`Action::link_lost`]); a
//! service with no free snapshot answers [`ActionError::Busy`].
//!
//! # Feed
//!
//! A source that changes while a client reads it (the log tail) is neither a
//! collection nor a stream: the service keeps nothing for the client and the
//! action calls [`ActionOutput::feed`] once with a [`FeedInfo`] and at most
//! [`MAX_FEED_CHUNK`] bytes of data. An empty page means "caught up", not
//! "complete"; the client continues with the cursor of the page, and the page
//! reports a reset to a new epoch and the entries lost to overwriting.
//!
//! # Streams
//!
//! A binary result that does not fit a message (a log history, a bundle) is a
//! **stream**: the action calls [`ActionOutput::stream`] with a
//! [`StreamInfo`], a snapshot of up to [`MAX_STREAM_FILES`] named files. The
//! client reads each file in chunks of at most [`MAX_STREAM_CHUNK`] bytes by
//! offset ([`Action::read_stream`], repeatable and idempotent) and closes the
//! stream ([`Action::close_stream`]). The service releases what a stream holds
//! when it is closed, when it times out and when the control link is lost
//! ([`Action::link_lost`]); a service that has no free stream answers
//! [`ActionError::Busy`].
//!
//! Who may read a stream or a snapshot is decided by the control plane, not by
//! the service: the controller lets only the connection that invoked the
//! action read or close it, and with the privilege of that action, so a
//! service needs no session logic of its own (see `rohi-control`, "Ownership of
//! streams and snapshots").
//!
//! Limits: at most [`MAX_ACTION_ARGS`] arguments; a result string or bytes
//! value is at most [`MAX_FIELD_VALUE_LEN`](crate::MAX_FIELD_VALUE_LEN)
//! bytes, an argument at most [`MAX_VALUE_LEN`](crate::MAX_VALUE_LEN). An
//! action has at most one enum ([`MAX_SERVICE_ENUMS`](crate::MAX_SERVICE_ENUMS)).
//!
//! # Addresses and bitsets
//!
//! A [`ValueKind::Address`](crate::ValueKind::Address) argument is a raw 32-byte public key (a `Bytes`
//! value of exactly [`ADDRESS_LEN`](crate::ADDRESS_LEN) bytes). A `Bytes`
//! argument declared with [`FieldDescriptor::with_selection`] is a bitset over
//! a catalog, at most [`MAX_SELECTIONS`](crate::MAX_SELECTIONS) per action:
//! see [`SelectionDescriptor`](crate::SelectionDescriptor). The control plane
//! checks the exact length before the action runs; names are for clients.

use heapless::Vec;

use crate::MAX_NAME_LEN;
use crate::field::{EnumDescriptor, FieldDescriptor, enum_allows};
use crate::service::Service;
use crate::status::{StatusWriter, WriteError, log_rejected_field};
use rohi_config::Value;

/// Maximum number of arguments of an action.
pub const MAX_ACTION_ARGS: usize = 3;
/// Maximum number of records of one page of a [`Records`](ResultFormat::Records) result.
pub const MAX_RECORDS: usize = 6;
/// Maximum number of fields of one record.
pub const MAX_RECORD_FIELDS: usize = 4;
/// Maximum number of files of one stream.
pub const MAX_STREAM_FILES: usize = 24;
/// Maximum number of bytes of one stream chunk.
pub const MAX_STREAM_CHUNK: usize = 512;
/// Maximum number of bytes of data of one feed page, see [`FeedPage`].
pub const MAX_FEED_CHUNK: usize = 512;

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

/// What an action returns: the logical shape and the guarantees of its result,
/// independent of how many messages carry it. See the
/// [module documentation](self#result-formats).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultFormat {
    /// Nothing, by contract: the operation succeeded and has no data.
    Empty,
    /// One bounded record of named, typed fields.
    Inline,
    /// A finite collection of records with one schema, paginated by the
    /// control plane when it does not fit one message (possibly empty).
    Records,
    /// One bounded page of a changing, cursor-addressed source.
    Feed,
    /// A binary snapshot of named files read in chunks.
    Stream,
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
    /// Fields of the record (`Inline`) or of every record (`Records`); empty
    /// for the other formats. At most [`MAX_RECORD_FIELDS`].
    pub results: &'static [FieldDescriptor],
    /// The enums the argument and result fields refer to.
    pub enums: &'static [EnumDescriptor],
    /// What the link must be allowed to do to invoke the action.
    pub access: ActionAccess,
    /// Whether the arguments contain secrets: they are never logged and are
    /// overwritten after the call.
    pub secret_args: bool,
    /// Whether the action runs only on a link its transport has secured (encrypted,
    /// and on TCP authenticated), whatever the access mode of the endpoint: it carries
    /// key material of the transport itself and must never travel in the clear.
    pub secure_link: bool,
    /// The format of the result, see [`ResultFormat`].
    pub format: ResultFormat,
}

impl ActionDescriptor {
    /// An action that needs `access`, without arguments, returning
    /// [`ResultFormat::Empty`].
    pub const fn new(id: u32, name: &'static str, access: ActionAccess) -> Self {
        Self {
            id,
            name,
            args: &[],
            results: &[],
            enums: &[],
            access,
            secret_args: false,
            secure_link: false,
            format: ResultFormat::Empty,
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

    /// Sets the format of the result, see [`ResultFormat`]. `Inline` and
    /// `Records` need [`results`](Self::results), the other formats have none.
    pub const fn format(mut self, format: ResultFormat) -> Self {
        self.format = format;
        self
    }

    /// Marks the arguments as secret: they are never logged and are wiped
    /// after the call. The [`access`](Self::access) is declared separately.
    pub const fn secret(mut self) -> Self {
        self.secret_args = true;
        self
    }

    /// Marks the action as one that runs only on a secured link, see
    /// [`secure_link`](Self::secure_link). The access level is declared separately.
    pub const fn secure_link(mut self) -> Self {
        self.secure_link = true;
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

    /// Runs one of [`ACTIONS`](Self::ACTIONS); the result is written to `out`
    /// in the [`ResultFormat`] the action declares.
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

    /// Writes the records of a snapshot of the action `action` that
    /// [`invoke`](Self::invoke) reported with [`ActionOutput::snapshot`],
    /// starting at the record `from`, to `out`: as many as `out` accepts (at
    /// most [`MAX_RECORDS`]), at least one while `from` is below the number of
    /// records of the snapshot, a record never split.
    ///
    /// Idempotent: the same `from` returns the same records while the snapshot
    /// is held. The default answers that the service has no snapshots.
    ///
    /// # Errors
    ///
    /// [`ActionError::UnknownSnapshot`] when the snapshot is not held: it was
    /// closed, expired or never existed.
    async fn read_records(
        &self,
        action: u32,
        snapshot: u32,
        from: u32,
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        let _ = (action, snapshot, from, out);
        Err(ActionError::UnknownSnapshot)
    }

    /// Releases a snapshot. The default answers that the service has no
    /// snapshots.
    async fn close_records(&self, snapshot: u32) -> Result<(), ActionError> {
        let _ = snapshot;
        Err(ActionError::UnknownSnapshot)
    }

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

    /// The control link was lost: release every stream and snapshot the
    /// client left open.
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
    /// No held snapshot with this id: it was closed, expired or never existed.
    UnknownSnapshot,
    /// The action ran and was refused for a reason a client may react to;
    /// the code is stable and machine-readable, for example
    /// `already_provisioned`.
    Rejected(&'static str),
    /// Unexpected failure.
    Internal,
}

/// One record of typed fields: the result of an `Inline` action, or one element
/// of a `Records` result.
pub type Record = Vec<(u32, Value), MAX_RECORD_FIELDS>;

/// The records of one page of a `Records` result.
pub type Records = Vec<Record, MAX_RECORDS>;

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

/// A snapshot of a finite collection that stays in the service, see
/// [`ActionOutput::snapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// Unique among the snapshots of the service, never reused for another one
    /// while an old token can still refer to it.
    pub id: u32,
    /// Number of records of the snapshot; at least one.
    pub total: u16,
}

/// One page of a `Records` result.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordsPage {
    /// The records of the page: from the start of the result, or from the
    /// index that was read, in order.
    pub records: Records,
    /// The snapshot the result continues in. `None` for a result that fits one
    /// page.
    pub snapshot: Option<SnapshotInfo>,
}

/// What an action returned, one case per [`ResultFormat`].
#[allow(
    clippy::large_enum_variant,
    reason = "no heap: all variants live on the stack of the control task"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum ActionResult {
    /// Nothing, see [`ResultFormat::Empty`].
    Empty,
    /// One record, see [`ResultFormat::Inline`].
    Inline(Record),
    /// The first page of a collection, see [`ResultFormat::Records`].
    Records(RecordsPage),
    /// A cursor page, see [`ActionOutput::feed`].
    Feed(FeedPage),
    /// A stream, see [`ActionOutput::stream`].
    Stream(StreamInfo),
}

impl ActionResult {
    /// The records of a `Records` result (its first page), empty for the other
    /// formats.
    pub fn records(&self) -> &[Record] {
        match self {
            Self::Records(page) => &page.records,
            _ => &[],
        }
    }

    /// The record of an `Inline` result.
    pub fn inline(&self) -> Option<&Record> {
        match self {
            Self::Inline(record) => Some(record),
            _ => None,
        }
    }
}

/// Where a feed stands. The data is a whole number of the entries of the
/// source, the contract of the source says what an entry is (a log line, for
/// example).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedInfo {
    /// Identifies the epoch of the source (a boot); sequences are valid inside
    /// one epoch only.
    pub session: u32,
    /// Sequence of the last entry of the page; the cursor of the next request.
    pub cursor: u64,
    /// Sequence of the oldest entry held; 1 when there is none.
    pub oldest_sequence: u64,
    /// Sequence of the newest entry held; 0 when there is none.
    pub newest_sequence: u64,
    /// The request belonged to another epoch: the page starts over.
    pub reset: bool,
    /// Entries overwritten between the cursor of the request and this page.
    pub gap: u64,
}

/// One bounded page a feed action returned, see [`ActionOutput::feed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedPage {
    /// Where the page stands.
    pub info: FeedInfo,
    /// At most [`MAX_FEED_CHUNK`] bytes; empty when nothing is newer.
    pub data: Vec<u8, MAX_FEED_CHUNK>,
}

/// A chunk read from a stream file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamChunk {
    /// Bytes written to the buffer.
    pub len: usize,
    /// Whether the chunk ends the file.
    pub eof: bool,
}

/// Receives the result of an action, according to its [`ResultFormat`].
///
/// Besides the field writers of [`StatusWriter`], which write the fields of the
/// record of an `Inline` result or of the records of a `Records` result,
/// [`next_record`](Self::next_record) starts a record (the first field written
/// without it starts the first record). Exactly one result is written: a call
/// that does not belong to the declared format, or a second result, is refused
/// with [`WriteError::WrongKind`], and an action that returns without its
/// result fails with [`ActionError::Internal`].
pub trait ActionOutput: StatusWriter {
    /// Starts a new record. For an `Inline` result only before the first
    /// field; [`WriteError::Full`] after [`MAX_RECORDS`] records.
    fn next_record(&mut self) -> Result<(), WriteError>;

    /// Reports a snapshot of a collection that stays in the service instead of
    /// writing its records: only for a `Records` action, only once and only
    /// when no record was written. The default refuses it.
    fn snapshot(&mut self, info: SnapshotInfo) -> Result<(), WriteError> {
        let _ = info;
        Err(WriteError::WrongKind)
    }

    /// Returns a stream. Only for a `Stream` action and only once. The default
    /// refuses it.
    fn stream(&mut self, info: StreamInfo) -> Result<(), WriteError> {
        let _ = info;
        Err(WriteError::WrongKind)
    }

    /// Returns one feed page. Only for a `Feed` action and only once. The
    /// default refuses it.
    ///
    /// # Errors
    ///
    /// [`WriteError::WrongKind`] when the action does not return a feed,
    /// [`WriteError::TooLong`] when `data` exceeds [`MAX_FEED_CHUNK`] bytes.
    fn feed(&mut self, info: FeedInfo, data: &[u8]) -> Result<(), WriteError> {
        let _ = (info, data);
        Err(WriteError::WrongKind)
    }
}

pub(crate) struct RecordCollector {
    format: ResultFormat,
    results: &'static [FieldDescriptor],
    enums: &'static [EnumDescriptor],
    // Reading a held snapshot: only records may be written.
    reading: bool,
    pub(crate) records: Records,
    pub(crate) snapshot: Option<SnapshotInfo>,
    pub(crate) stream: Option<StreamInfo>,
    pub(crate) feed: Option<FeedPage>,
}

impl RecordCollector {
    pub(crate) fn new(action: &ActionDescriptor, reading: bool) -> Self {
        Self {
            format: action.format,
            results: action.results,
            enums: action.enums,
            reading,
            records: Vec::new(),
            snapshot: None,
            stream: None,
            feed: None,
        }
    }

    // Whether anything but records was written.
    fn has_other(&self) -> bool {
        self.snapshot.is_some() || self.stream.is_some() || self.feed.is_some()
    }

    /// Checks that exactly one result of the declared format was written.
    pub(crate) fn check(&self) -> Check {
        let none = self.snapshot.is_none() && self.stream.is_none() && self.feed.is_none();
        match self.format {
            ResultFormat::Empty if none && self.records.is_empty() => Check::Result,
            ResultFormat::Inline if none && self.records.len() == 1 => Check::Result,
            ResultFormat::Records if none => Check::Result,
            ResultFormat::Records
                if self.records.is_empty() && self.stream.is_none() && self.feed.is_none() =>
            {
                self.snapshot.map_or(Check::Bad, Check::Snapshot)
            }
            ResultFormat::Feed
                if self.feed.is_some()
                    && self.records.is_empty()
                    && self.snapshot.is_none()
                    && self.stream.is_none() =>
            {
                Check::Result
            }
            ResultFormat::Stream
                if self.stream.is_some()
                    && self.records.is_empty()
                    && self.snapshot.is_none()
                    && self.feed.is_none() =>
            {
                Check::Result
            }
            _ => Check::Bad,
        }
    }

    /// The result that [`check`](Self::check) accepted as [`Check::Result`].
    pub(crate) fn take_result(&mut self) -> ActionResult {
        match self.format {
            ResultFormat::Empty => ActionResult::Empty,
            ResultFormat::Inline => ActionResult::Inline(self.records.pop().unwrap_or_default()),
            ResultFormat::Records => ActionResult::Records(RecordsPage {
                records: core::mem::take(&mut self.records),
                snapshot: None,
            }),
            ResultFormat::Feed => self
                .feed
                .take()
                .map_or(ActionResult::Empty, ActionResult::Feed),
            ResultFormat::Stream => self
                .stream
                .take()
                .map_or(ActionResult::Empty, ActionResult::Stream),
        }
    }

    /// Reuses the collector to read the records of the snapshot it reported.
    pub(crate) fn start_read(&mut self) {
        self.records.clear();
        self.snapshot = None;
        self.reading = true;
    }

    pub(crate) fn take_records(&mut self) -> Records {
        core::mem::take(&mut self.records)
    }
}

/// What a [`RecordCollector`] gathered.
pub(crate) enum Check {
    /// One result of the declared format.
    Result,
    /// A snapshot of the collection: its first page is read next.
    Snapshot(SnapshotInfo),
    /// Nothing, too much, or something of another format.
    Bad,
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
        if !field.admits(&value) || !enum_ok(self.enums, field, &value) {
            return Err(WriteError::WrongKind);
        }
        if self.has_other() {
            return Err(WriteError::WrongKind);
        }
        if self.records.is_empty() {
            self.next_record()?;
        }
        let record = self.records.last_mut().ok_or(WriteError::Full)?;
        // A field is written at most once per record.
        if record.iter().any(|(written, _)| *written == id) {
            return Err(WriteError::WrongKind);
        }
        record.push((id, value)).map_err(|_| WriteError::Full)
    }
}

impl ActionOutput for RecordCollector {
    fn next_record(&mut self) -> Result<(), WriteError> {
        let one_record = self.format == ResultFormat::Inline;
        if self.has_other()
            || (one_record && !self.records.is_empty())
            || !matches!(self.format, ResultFormat::Inline | ResultFormat::Records)
        {
            return Err(WriteError::WrongKind);
        }
        self.records.push(Vec::new()).map_err(|_| WriteError::Full)
    }

    fn snapshot(&mut self, info: SnapshotInfo) -> Result<(), WriteError> {
        if self.reading
            || self.format != ResultFormat::Records
            || !self.records.is_empty()
            || self.has_other()
            || info.total == 0
        {
            log::debug!(target: "service", "Action snapshot result rejected");
            return Err(WriteError::WrongKind);
        }
        self.snapshot = Some(info);
        Ok(())
    }

    fn stream(&mut self, info: StreamInfo) -> Result<(), WriteError> {
        if self.format != ResultFormat::Stream || !self.records.is_empty() || self.has_other() {
            log::debug!(target: "service", "Action stream result rejected");
            return Err(WriteError::WrongKind);
        }
        self.stream = Some(info);
        Ok(())
    }

    fn feed(&mut self, info: FeedInfo, data: &[u8]) -> Result<(), WriteError> {
        if self.format != ResultFormat::Feed || !self.records.is_empty() || self.has_other() {
            log::debug!(target: "service", "Action feed result rejected");
            return Err(WriteError::WrongKind);
        }
        let data = Vec::from_slice(data).map_err(|_| WriteError::TooLong)?;
        self.feed = Some(FeedPage { info, data });
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
        if !field.admits(value) || !enum_ok(action.enums, field, value) {
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
