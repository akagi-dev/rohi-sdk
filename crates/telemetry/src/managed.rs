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
//! The managed `telemetry` service: the three profile Actions and status.
//!
//! The persisted state is one [`TelemetrySection`], a typed bounded value
//! kept by the firmware's configuration owner like the identity: it is not a
//! generic config section, clients change it only through the Actions below.
//! Every mutation loads the committed value, applies the change to a
//! candidate, validates it (including the 1 024-byte section budget) and
//! asks the store to commit it atomically; the runtime sees the new value
//! only after a successful commit. A refused or failed edit changes nothing.
//!
//! # Model
//!
//! Profiles are **slots addressed by index** (`0`, `1`, ...; Altruist Urban
//! has two): they are never created or deleted, have no name and no format
//! (the one compiled in is used). Each slot has a public selection and at most
//! one private section (one recipient and its selection). Both slots start with
//! empty selections: the existence of a slot publishes and discloses nothing.
//!
//! # Actions
//!
//! | Action | Arguments | Access | Result |
//! |--------|-----------|--------|--------|
//! | `set_public` | `index`, `selection` | Mutate | |
//! | `set_private` | `index`, `recipient`, `selection` | Mutate | |
//! | `measurements` | `offset` | Read | `bit_index`, `name` |
//!
//! `selection` arguments are 32-byte bitsets over the `measurements` catalog
//! (a generic selection descriptor), `recipient` is a 32-byte address.
//! `set_public` replaces the public selection of the slot. `set_private`
//! makes `recipient` the only recipient of the slot, replacing the earlier
//! one, with `selection`; **`selection = none` (all zero bytes) removes the
//! private section** (the recipient is then ignored). An `index` that is not a
//! slot of the build is refused with `invalid_index`, a selection with a bit
//! the format cannot encode with `unsupported_measurement` and an unusable
//! recipient key with `invalid_recipient`. The `measurements` offset is a
//! zero-based ordinal; an empty page ends the listing. Private sections are
//! encrypted inside the packet by the packet format, whatever the control
//! link: the Actions only choose who may read what.
//!
//! # Status
//!
//! Static slots, not a dynamic tree: `generation`, and for each of the
//! `PROFILES` slots of the build (1 to 3 are supported, see [`Slots`]) the
//! fields `profile<i>.public` (bitset), `profile<i>.private` (whether a
//! private recipient exists), `profile<i>.recipient` (its public key, present
//! only when `private` is true; a public key is not a secret and fits a status
//! value), `profile<i>.private_selection` (bitset, all zero without a private
//! section) and `profile<i>.revision`.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use heapless::Vec;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{ConfigSection, MAX_SECTION_PAYLOAD};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, FieldDescriptor, Issue,
    IssueDescriptor, IssueSeverity, MAX_ISSUES, MAX_RECORDS, Service, ServiceHealth, ServiceState,
    ServiceSummary, Status, StatusWriter, Value, ValueKind, WriteError,
};

use crate::catalog::CATALOG;
use crate::profile::TelemetryConfig;
use crate::profile::{FormatCapabilities, PacketProfile, ProfileError, RecipientAddress};
use crate::section::ProfileStamp;
use crate::selection::{DataSelection, Selection};

/// The persisted section: all profile slots of a device that keeps `PROFILES` of them
/// (a const parameter chosen by the firmware; Altruist Urban: 2). The capacity is the type,
/// so it is also the storage and a stored section of another size does not decode: it is
/// refused, never truncated. Bumping `version` is a storage ABI change and an unreadable or
/// obsolete section is never defaulted to publishing anything.
///
/// Version 2 is the indexed-slot model; the named profiles of version 1 are not read.
#[derive(Clone, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen, ConfigSection)]
#[cbor(transparent)]
#[config(id = 0x7c1e_89f0_a500_7b17, version = 2)]
pub struct TelemetrySection<const PROFILES: usize>(#[n(0)] pub TelemetryConfig<PROFILES>);

/// Ids of the Actions of the `telemetry` service.
pub mod action {
    /// Replaces the public selection of a slot.
    pub const SET_PUBLIC: u32 = 1;
    /// Sets or removes the private section of a slot.
    pub const SET_PRIVATE: u32 = 2;
    /// Pages the measurement catalog.
    pub const MEASUREMENTS: u32 = 3;
}

/// Field ids of the Actions.
pub mod action_field {
    /// Argument of `set_public` and `set_private`: the profile slot.
    pub const INDEX: u32 = 1;
    /// Argument of `set_public`: the bitset.
    pub const SELECTION: u32 = 2;
    /// Argument of `set_private`: the recipient address.
    pub const RECIPIENT: u32 = 2;
    /// Argument of `set_private`: the bitset (the recipient takes id 2).
    pub const PRIVATE_SELECTION: u32 = 3;
    /// Argument of `measurements`: the zero-based ordinal of the first record.
    pub const OFFSET: u32 = 1;
    /// Result of `measurements`: stable bit index.
    pub const BIT_INDEX: u32 = 1;
    /// Result of `measurements`: canonical name.
    pub const NAME: u32 = 2;
}

/// Field ids of the status of the service.
pub mod status_field {
    /// The generation counter of the persisted policy.
    pub const GENERATION: u32 = 1;

    /// Public selection of slot `slot`, see [`profile`].
    pub const PUBLIC: u32 = 0;
    /// Whether slot `slot` has a private recipient, see [`profile`].
    pub const PRIVATE: u32 = 1;
    /// Public key of the private recipient of slot `slot`, see [`profile`].
    pub const RECIPIENT: u32 = 2;
    /// Private selection of slot `slot`, see [`profile`].
    pub const PRIVATE_SELECTION: u32 = 3;
    /// Policy revision of slot `slot`, see [`profile`].
    pub const REVISION: u32 = 4;

    /// The id of field `n` ([`PUBLIC`], [`PRIVATE`], [`RECIPIENT`],
    /// [`PRIVATE_SELECTION`] or [`REVISION`]) of profile slot `slot`.
    pub const fn profile(slot: usize, n: u32) -> u32 {
        10 * (slot as u32 + 1) + n
    }
}

/// Issue codes of the service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE;

    /// The profiles cannot be read from storage.
    pub const STORAGE_UNAVAILABLE: u32 = FIRST_SERVICE_ISSUE;
}

const SERVICE_ID: u32 = rohi_service::service_id("telemetry");

/// Failure of the persistence behind the profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The storage is unavailable or the stored section is unreadable.
    Unavailable,
    /// Unexpected failure.
    Internal,
}

impl From<StoreError> for ActionError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Unavailable => Self::Unavailable,
            StoreError::Internal => Self::Internal,
        }
    }
}

/// Persistence of the profiles, implemented by the firmware over the
/// configuration owner. Only the telemetry section may be touched.
///
/// `commit` is a durable mutation: it either persists the whole candidate
/// atomically and publishes it to the runtime (see [`TelemetryState`]), or
/// fails and changes nothing.
#[allow(async_fn_in_trait)]
pub trait TelemetryStore<const PROFILES: usize> {
    /// The committed profiles; every slot empty when none were stored.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; an unreadable section is an error, not an empty set.
    async fn load(&self) -> Result<TelemetryConfig<PROFILES>, StoreError>;

    /// Atomically persists `candidate` and publishes it.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; nothing changed.
    async fn commit(&self, candidate: &TelemetryConfig<PROFILES>) -> Result<(), StoreError>;
}

/// The effective profiles as the runtime sees them, with a change signal.
///
/// The store publishes here after a successful commit. Consumers (the packet
/// builder, connectivity) resolve a `profile_index` to a [`ProfileStamp`],
/// check that a stamp is still current before they start work, and wait for
/// changes to invalidate queued packets. Nothing here is held across I/O.
pub struct TelemetryState<M: RawMutex, const PROFILES: usize> {
    effective: BlockingMutex<M, RefCell<TelemetryConfig<PROFILES>>>,
    changed: Signal<M, u32>,
}

impl<M: RawMutex, const P: usize> Default for TelemetryState<M, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex, const P: usize> TelemetryState<M, P> {
    /// Every slot empty: nothing is packaged.
    pub const fn new() -> Self {
        Self {
            effective: BlockingMutex::new(RefCell::new(TelemetryConfig::EMPTY)),
            changed: Signal::new(),
        }
    }

    /// Makes `profiles` the effective policy and wakes the waiter. Called
    /// synchronously in the commit step, so no packet can be admitted under
    /// the old policy after the commit completed.
    pub fn publish(&self, profiles: &TelemetryConfig<P>) {
        let generation = profiles.generation;
        self.effective
            .lock(|effective| *effective.borrow_mut() = profiles.clone());
        self.changed.signal(generation);
    }

    /// The generation of the effective policy.
    pub fn generation(&self) -> u32 {
        self.effective
            .lock(|effective| effective.borrow().generation)
    }

    /// The stamp of profile slot `profile_index`, `None` if the build has no
    /// such slot.
    pub fn resolve(&self, profile_index: u8) -> Option<ProfileStamp> {
        self.effective.lock(|effective| {
            effective
                .borrow()
                .get(profile_index)
                .map(|profile| ProfileStamp {
                    profile_index,
                    revision: profile.revision,
                })
        })
    }

    /// Whether `stamp` is still the current policy of its slot.
    pub fn is_current(&self, stamp: &ProfileStamp) -> bool {
        self.resolve(stamp.profile_index).as_ref() == Some(stamp)
    }

    /// Runs `f` on slot `profile_index` of the effective policy.
    pub fn with_profile<T>(
        &self,
        profile_index: u8,
        f: impl FnOnce(&PacketProfile) -> T,
    ) -> Option<T> {
        self.effective
            .lock(|effective| effective.borrow().get(profile_index).map(f))
    }

    /// Waits for the next change of the policy and returns its generation.
    pub async fn changed(&self) -> u32 {
        self.changed.wait().await
    }
}

/// The `telemetry` service over a [`TelemetryStore`] and the capabilities of
/// the packet format compiled in.
pub struct TelemetryService<M: RawMutex, S, C, const PROFILES: usize> {
    store: S,
    caps: C,
    // Serializes read-modify-write: no lost updates between Actions.
    edit: Mutex<M, ()>,
}

impl<M: RawMutex, S, C, const P: usize> Service for TelemetryService<M, S, C, P> {
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "telemetry";
}

impl<M: RawMutex, S: TelemetryStore<P>, C: FormatCapabilities, const P: usize>
    TelemetryService<M, S, C, P>
{
    /// A service over `store`; `caps` says what the format can encode.
    pub const fn new(store: S, caps: C) -> Self {
        Self {
            store,
            caps,
            edit: Mutex::new(()),
        }
    }

    async fn edit(
        &self,
        change: impl FnOnce(&mut TelemetryConfig<P>, &C) -> Result<(), ProfileError>,
    ) -> Result<(), ActionError> {
        let _guard = self.edit.lock().await;
        let mut candidate = self.store.load().await?;
        change(&mut candidate, &self.caps).map_err(reject)?;
        if minicbor::len(TelemetrySectionRef(&candidate)) > MAX_SECTION_PAYLOAD {
            return Err(ActionError::Rejected("section_too_large"));
        }
        self.store.commit(&candidate).await?;
        Ok(())
    }
}

/// Borrowed encoding of the section for the size check.
struct TelemetrySectionRef<'a, const P: usize>(&'a TelemetryConfig<P>);

impl<C, const P: usize> Encode<C> for TelemetrySectionRef<'_, P> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }
}

impl<C, const P: usize> CborLen<C> for TelemetrySectionRef<'_, P> {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        self.0.cbor_len(ctx)
    }
}

fn reject(error: ProfileError) -> ActionError {
    ActionError::Rejected(match error {
        ProfileError::InvalidIndex => "invalid_index",
        ProfileError::Selection(_) => "unsupported_measurement",
        ProfileError::EmptyPrivateSelection => "empty_selection",
        ProfileError::InvalidRecipient => "invalid_recipient",
        ProfileError::InconsistentGeneration => "inconsistent_generation",
    })
}

fn arg(args: &[(u32, Value)], id: u32) -> Result<&Value, ActionError> {
    args.iter()
        .find(|(field, _)| *field == id)
        .map(|(_, value)| value)
        .ok_or(ActionError::InvalidArgument)
}

fn offset(args: &[(u32, Value)], id: u32) -> Result<usize, ActionError> {
    let offset = arg(args, id)?
        .as_u64()
        .ok_or(ActionError::InvalidArgument)?;
    usize::try_from(offset).map_err(|_| ActionError::InvalidArgument)
}

// A number that is no slot of any build is a refused index, not a malformed argument.
fn index(args: &[(u32, Value)]) -> Result<u8, ActionError> {
    let index = arg(args, action_field::INDEX)?
        .as_u64()
        .ok_or(ActionError::InvalidArgument)?;
    u8::try_from(index).map_err(|_| ActionError::Rejected("invalid_index"))
}

fn selection(args: &[(u32, Value)], id: u32) -> Result<DataSelection, ActionError> {
    let bytes = arg(args, id)?
        .as_bytes()
        .ok_or(ActionError::InvalidArgument)?;
    Selection::from_slice(bytes)
        .map(DataSelection::local)
        .ok_or(ActionError::InvalidArgument)
}

fn recipient(args: &[(u32, Value)], id: u32) -> Result<RecipientAddress, ActionError> {
    let bytes = arg(args, id)?
        .as_bytes()
        .ok_or(ActionError::InvalidArgument)?;
    <[u8; 32]>::try_from(bytes)
        .map(RecipientAddress)
        .map_err(|_| ActionError::InvalidArgument)
}

const INDEX: FieldDescriptor = FieldDescriptor::new(action_field::INDEX, "index", ValueKind::U64);
const OFFSET: FieldDescriptor =
    FieldDescriptor::new(action_field::OFFSET, "offset", ValueKind::U64);
const fn selection_field(id: u32, name: &'static str) -> FieldDescriptor {
    FieldDescriptor::new(id, name, ValueKind::Bytes).with_selection(
        32,
        SERVICE_ID,
        action::MEASUREMENTS,
    )
}

// The status schema is written out for the slot count of the build: five fields per slot follow
// the generation, with stable ids and names, as a literal slice like the schema of any other
// service. There is nothing to build or register at run time.
macro_rules! status_schema {
    ($($slot:literal),+) => {
        &[
            FieldDescriptor::new(status_field::GENERATION, "generation", ValueKind::U64),
            $(
                FieldDescriptor::new(
                    status_field::profile($slot, status_field::PUBLIC),
                    concat!("profile", $slot, "_public"),
                    ValueKind::Bytes,
                ),
                FieldDescriptor::new(
                    status_field::profile($slot, status_field::PRIVATE),
                    concat!("profile", $slot, "_private"),
                    ValueKind::Bool,
                ),
                FieldDescriptor::new(
                    status_field::profile($slot, status_field::RECIPIENT),
                    concat!("profile", $slot, "_recipient"),
                    ValueKind::Address,
                ),
                FieldDescriptor::new(
                    status_field::profile($slot, status_field::PRIVATE_SELECTION),
                    concat!("profile", $slot, "_priv_selection"),
                    ValueKind::Bytes,
                ),
                FieldDescriptor::new(
                    status_field::profile($slot, status_field::REVISION),
                    concat!("profile", $slot, "_revision"),
                    ValueKind::U64,
                ),
            )+
        ]
    };
}

/// The profile slot count of a build, as a type: the status schema of a service with `N`
/// slots. [`ProfileSlots`] is implemented for 1, 2 and 3 slots (at most 16 status fields, under
/// the limit of 20); any other count does not compile, so no schema exists for a count the
/// build does not use.
pub struct Slots<const N: usize>;

/// The static status schema of [`Slots`] of one size.
pub trait ProfileSlots {
    /// The generation and five fields per profile slot.
    const SCHEMA: &'static [FieldDescriptor];
}

impl ProfileSlots for Slots<1> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0);
}

impl ProfileSlots for Slots<2> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0, 1);
}

impl ProfileSlots for Slots<3> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0, 1, 2);
}

impl<M: RawMutex, S: TelemetryStore<P>, C: FormatCapabilities, const P: usize> Status
    for TelemetryService<M, S, C, P>
where
    Slots<P>: ProfileSlots,
{
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = <Slots<P> as ProfileSlots>::SCHEMA;
    const ISSUES: &'static [IssueDescriptor] = &[IssueDescriptor::new(
        issue::STORAGE_UNAVAILABLE,
        "storage_unavailable",
    )];

    async fn summary(&self) -> ServiceSummary {
        match self.store.load().await {
            Ok(_) => ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true),
            Err(_) => ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false),
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if self.store.load().await.is_err() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Error,
                code: issue::STORAGE_UNAVAILABLE,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let Ok(config) = self.store.load().await else {
            return Ok(());
        };
        out.u64(status_field::GENERATION, u64::from(config.generation))?;
        for (slot, profile) in config.profiles.iter().enumerate().take(P) {
            let field = |n| status_field::profile(slot, n);
            out.bytes(
                field(status_field::PUBLIC),
                profile.public.measurements.as_bytes(),
            )?;
            out.bool(field(status_field::PRIVATE), profile.private.is_some())?;
            let (selection, recipient) = match &profile.private {
                Some(section) => (section.selection.measurements, Some(section.recipient)),
                None => (Selection::NONE, None),
            };
            if let Some(recipient) = recipient {
                out.bytes(field(status_field::RECIPIENT), &recipient.0)?;
            }
            out.bytes(field(status_field::PRIVATE_SELECTION), selection.as_bytes())?;
            out.u64(field(status_field::REVISION), u64::from(profile.revision))?;
        }
        Ok(())
    }
}

impl<M: RawMutex, S: TelemetryStore<P>, C: FormatCapabilities, const P: usize> Action
    for TelemetryService<M, S, C, P>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::SET_PUBLIC, "set_public", ActionAccess::Mutate)
            .args(&[INDEX, selection_field(action_field::SELECTION, "selection")]),
        ActionDescriptor::new(action::SET_PRIVATE, "set_private", ActionAccess::Mutate).args(&[
            INDEX,
            FieldDescriptor::new(action_field::RECIPIENT, "recipient", ValueKind::Address),
            selection_field(action_field::PRIVATE_SELECTION, "selection"),
        ]),
        ActionDescriptor::new(action::MEASUREMENTS, "measurements", ActionAccess::Read)
            .args(&[OFFSET])
            .results(&[
                FieldDescriptor::new(action_field::BIT_INDEX, "bit_index", ValueKind::U64),
                FieldDescriptor::new(action_field::NAME, "name", ValueKind::String),
            ]),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::SET_PUBLIC => {
                let index = index(args)?;
                let selection = selection(args, action_field::SELECTION)?;
                self.edit(|profiles, caps| profiles.set_public(index, selection, caps))
                    .await
            }
            action::SET_PRIVATE => {
                let index = index(args)?;
                let recipient = recipient(args, action_field::RECIPIENT)?;
                let selection = selection(args, action_field::PRIVATE_SELECTION)?;
                self.edit(|profiles, caps| profiles.set_private(index, recipient, selection, caps))
                    .await
            }
            action::MEASUREMENTS => {
                let offset = offset(args, action_field::OFFSET)?;
                // The catalog of what the format can encode now.
                let supported = self.caps.supported();
                for entry in CATALOG
                    .iter()
                    .filter(|entry| supported.contains(entry.id))
                    .skip(offset)
                    .take(MAX_RECORDS)
                {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    write(out.u64(action_field::BIT_INDEX, u64::from(entry.id.0)))?;
                    write(out.text(action_field::NAME, entry.name))?;
                }
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

fn write(result: Result<(), WriteError>) -> Result<(), ActionError> {
    result.map_err(|_| ActionError::Internal)
}

#[cfg(test)]
mod tests;
