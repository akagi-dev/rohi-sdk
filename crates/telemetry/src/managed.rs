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
//! The managed `telemetry` service: profile Actions, the measurement catalog
//! and status.
//!
//! The persisted state is one [`TelemetrySection`], a typed bounded value
//! kept by the firmware's configuration owner like the identity: it is not a
//! generic config section, clients change it only through the Actions below.
//! Every mutation loads the committed value, applies the change to a
//! candidate, validates it (including the 1 024-byte section budget) and
//! asks the store to commit it atomically; the runtime sees the new value
//! only after a successful commit. A refused or failed edit changes nothing.
//!
//! # Actions
//!
//! | Action | Arguments | Access | Result |
//! |--------|-----------|--------|--------|
//! | `profile_add` | `profile_id`, `format` | Mutate | |
//! | `profile_remove` | `profile_id` | Mutate | |
//! | `set_public` | `profile_id`, `selection` | Mutate | |
//! | `add_private` | `profile_id`, `recipient`, `selection` | Mutate | |
//! | `remove_private` | `profile_id`, `recipient` | Mutate | |
//! | `profile_list` | `offset` | Read | `profile_id`, `format`, `public`, `revision` |
//! | `profile_recipients` | `profile_id`, `offset` | Read | `recipient`, `selection` |
//! | `measurements` | `offset` | Read | `bit_index`, `name` |
//!
//! `selection` arguments are 32-byte bitsets over the `measurements`
//! catalog (a generic selection descriptor), `recipient` is an address.
//! Offsets are zero-based ordinals; an empty page ends the listing. Mutations
//! never reveal anything by themselves: a profile is created with an empty
//! public selection and no private section.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use heapless::Vec;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{ConfigSection, MAX_SECTION_PAYLOAD};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, MAX_RECORDS, Service,
    ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter, Value, ValueKind,
    WriteError,
};

use crate::catalog::CATALOG;
use crate::profile::{
    FormatCapabilities, PacketFormatId, ProfileError, RecipientAddress, TelemetryConfig,
};
use crate::section::ProfileStamp;
use crate::selection::{DataSelection, Selection};

/// Profiles the device keeps. A budget goal, checked by tests against the
/// section limit; raise it only with measurements on both chips.
pub const MAX_PROFILES: usize = 3;
/// Private recipients per profile, see [`MAX_PROFILES`].
pub const MAX_RECIPIENTS: usize = 2;

/// The effective packet profiles of the device.
pub type ProfileSet = TelemetryConfig<MAX_PROFILES, MAX_RECIPIENTS>;

/// The persisted section: all packet profiles. Bumping `version` is a storage
/// ABI change and an unreadable or obsolete section is never defaulted to
/// publishing anything.
#[derive(Clone, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen, ConfigSection)]
#[cbor(transparent)]
#[config(version = 1)]
pub struct TelemetrySection(#[n(0)] pub ProfileSet);

/// Ids of the Actions of the `telemetry` service.
pub mod action {
    /// Creates an empty profile.
    pub const PROFILE_ADD: u32 = 1;
    /// Deletes a profile.
    pub const PROFILE_REMOVE: u32 = 2;
    /// Replaces the public selection.
    pub const SET_PUBLIC: u32 = 3;
    /// Adds or replaces a private section.
    pub const ADD_PRIVATE: u32 = 4;
    /// Removes a private section.
    pub const REMOVE_PRIVATE: u32 = 5;
    /// Pages the profiles.
    pub const PROFILE_LIST: u32 = 6;
    /// Pages the private sections of a profile.
    pub const PROFILE_RECIPIENTS: u32 = 7;
    /// Pages the measurement catalog.
    pub const MEASUREMENTS: u32 = 8;
}

/// Field ids of the Actions.
pub mod action_field {
    /// Argument: profile id.
    pub const PROFILE_ID: u32 = 1;
    /// Argument of `profile_add`: the format.
    pub const FORMAT: u32 = 2;
    /// Argument of the selection Actions: the bitset.
    pub const SELECTION: u32 = 2;
    /// Argument of the recipient Actions: the address.
    pub const RECIPIENT: u32 = 2;
    /// Argument of `add_private`: the bitset (the recipient takes id 2).
    pub const PRIVATE_SELECTION: u32 = 3;
    /// Argument of the paged Actions: the zero-based ordinal of the first record.
    pub const OFFSET: u32 = 1;
    /// Argument of `profile_recipients`: the offset (the profile takes id 1).
    pub const RECIPIENTS_OFFSET: u32 = 2;
    /// Result of `profile_list`: profile id.
    pub const LIST_ID: u32 = 1;
    /// Result of `profile_list`: format.
    pub const LIST_FORMAT: u32 = 2;
    /// Result of `profile_list`: public selection.
    pub const LIST_PUBLIC: u32 = 3;
    /// Result of `profile_list`: policy revision.
    pub const LIST_REVISION: u32 = 4;
    /// Result of `profile_recipients`: recipient.
    pub const RECIPIENT_ADDRESS: u32 = 1;
    /// Result of `profile_recipients`: private selection.
    pub const RECIPIENT_SELECTION: u32 = 2;
    /// Result of `measurements`: stable bit index.
    pub const BIT_INDEX: u32 = 1;
    /// Result of `measurements`: canonical name.
    pub const NAME: u32 = 2;
}

/// Field ids of the status of the service.
pub mod status_field {
    /// Number of profiles.
    pub const PROFILES: u32 = 1;
    /// The generation counter of the persisted policy.
    pub const GENERATION: u32 = 2;
}

/// Issue codes of the service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE;

    /// The profiles cannot be read from storage.
    pub const STORAGE_UNAVAILABLE: u32 = FIRST_SERVICE_ISSUE;
}

const SERVICE_ID: u32 = rohi_service::service_id("telemetry");
const FORMAT_ENUM: u32 = 1;
const FORMATS: &[EnumDescriptor] = &[EnumDescriptor::new(
    FORMAT_ENUM,
    &[EnumVariant::new(1, "robonomics")],
)];

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
pub trait TelemetryStore {
    /// The committed profiles; no profile when none were stored.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; an unreadable section is an error, not an empty set.
    async fn load(&self) -> Result<ProfileSet, StoreError>;

    /// Atomically persists `candidate` and publishes it.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; nothing changed.
    async fn commit(&self, candidate: &ProfileSet) -> Result<(), StoreError>;
}

/// The effective profiles as the runtime sees them, with a change signal.
///
/// The store publishes here after a successful commit. Consumers (the packet
/// builder, connectivity) resolve profiles to a [`ProfileStamp`], check that a
/// stamp is still current before they start work, and wait for changes to
/// invalidate queued packets. Nothing here is held across I/O.
pub struct TelemetryState<M: RawMutex> {
    effective: BlockingMutex<M, RefCell<ProfileSet>>,
    changed: Signal<M, u32>,
}

impl<M: RawMutex> Default for TelemetryState<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex> TelemetryState<M> {
    /// No profile: nothing is packaged.
    pub const fn new() -> Self {
        Self {
            effective: BlockingMutex::new(RefCell::new(TelemetryConfig {
                generation: 0,
                profiles: Vec::new(),
            })),
            changed: Signal::new(),
        }
    }

    /// Makes `profiles` the effective policy and wakes the waiter. Called
    /// synchronously in the commit step, so no packet can be admitted under
    /// the old policy after the commit completed.
    pub fn publish(&self, profiles: &ProfileSet) {
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

    /// The stamp of profile `id`, `None` if it does not exist.
    pub fn resolve(&self, id: &str) -> Option<ProfileStamp> {
        self.effective.lock(|effective| {
            effective.borrow().get(id).map(|profile| ProfileStamp {
                id: profile.id.clone(),
                incarnation: profile.incarnation,
                revision: profile.revision,
            })
        })
    }

    /// Whether `stamp` is still the current policy of its profile.
    pub fn is_current(&self, stamp: &ProfileStamp) -> bool {
        self.resolve(stamp.id.as_str()).as_ref() == Some(stamp)
    }

    /// Runs `f` on profile `id` of the effective policy.
    pub fn with_profile<T>(
        &self,
        id: &str,
        f: impl FnOnce(&crate::profile::PacketProfile<MAX_RECIPIENTS>) -> T,
    ) -> Option<T> {
        self.effective
            .lock(|effective| effective.borrow().get(id).map(f))
    }

    /// Waits for the next change of the policy and returns its generation.
    pub async fn changed(&self) -> u32 {
        self.changed.wait().await
    }
}

/// The `telemetry` service over a [`TelemetryStore`] and the capabilities of
/// the packet formats compiled in.
pub struct TelemetryService<M: RawMutex, S, C> {
    store: S,
    caps: C,
    // Serializes read-modify-write: no lost updates between Actions.
    edit: Mutex<M, ()>,
}

impl<M: RawMutex, S, C> Service for TelemetryService<M, S, C> {
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "telemetry";
}

impl<M: RawMutex, S: TelemetryStore, C: FormatCapabilities> TelemetryService<M, S, C> {
    /// A service over `store`; `caps` says what the formats can encode.
    pub const fn new(store: S, caps: C) -> Self {
        Self {
            store,
            caps,
            edit: Mutex::new(()),
        }
    }

    async fn edit(
        &self,
        change: impl FnOnce(&mut ProfileSet, &C) -> Result<(), ProfileError>,
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
struct TelemetrySectionRef<'a>(&'a ProfileSet);

impl<C> Encode<C> for TelemetrySectionRef<'_> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }
}

impl<C> CborLen<C> for TelemetrySectionRef<'_> {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        self.0.cbor_len(ctx)
    }
}

fn reject(error: ProfileError) -> ActionError {
    ActionError::Rejected(match error {
        ProfileError::InvalidId => "invalid_id",
        ProfileError::Duplicate => "already_exists",
        ProfileError::NotFound => "profile_not_found",
        ProfileError::RecipientNotFound => "recipient_not_found",
        ProfileError::Selection(_) => "unsupported_measurement",
        ProfileError::EmptyPrivateSelection => "empty_selection",
        ProfileError::InvalidRecipient => "invalid_recipient",
        ProfileError::TooManyProfiles => "too_many_profiles",
        ProfileError::TooManyRecipients => "too_many_recipients",
    })
}

fn arg(args: &[(u32, Value)], id: u32) -> Result<&Value, ActionError> {
    args.iter()
        .find(|(field, _)| *field == id)
        .map(|(_, value)| value)
        .ok_or(ActionError::InvalidArgument)
}

fn text(args: &[(u32, Value)], id: u32) -> Result<&str, ActionError> {
    arg(args, id)?.as_str().ok_or(ActionError::InvalidArgument)
}

fn offset(args: &[(u32, Value)], id: u32) -> Result<usize, ActionError> {
    let offset = arg(args, id)?
        .as_u64()
        .ok_or(ActionError::InvalidArgument)?;
    usize::try_from(offset).map_err(|_| ActionError::InvalidArgument)
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

fn format(args: &[(u32, Value)]) -> Result<PacketFormatId, ActionError> {
    match arg(args, action_field::FORMAT)?.as_enum() {
        Some(1) => Ok(PacketFormatId::Robonomics),
        _ => Err(ActionError::InvalidArgument),
    }
}

const PROFILE_ID: FieldDescriptor =
    FieldDescriptor::new(action_field::PROFILE_ID, "profile_id", ValueKind::String);
const OFFSET: FieldDescriptor =
    FieldDescriptor::new(action_field::OFFSET, "offset", ValueKind::U64);
const fn selection_field(id: u32, name: &'static str) -> FieldDescriptor {
    FieldDescriptor::new(id, name, ValueKind::Bytes).with_selection(
        32,
        SERVICE_ID,
        action::MEASUREMENTS,
    )
}

impl<M: RawMutex, S: TelemetryStore, C: FormatCapabilities> Status for TelemetryService<M, S, C> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::PROFILES, "profiles", ValueKind::U64),
        FieldDescriptor::new(status_field::GENERATION, "generation", ValueKind::U64),
    ];
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
        if let Ok(profiles) = self.store.load().await {
            out.u64(status_field::PROFILES, profiles.profiles.len() as u64)?;
            out.u64(status_field::GENERATION, u64::from(profiles.generation))?;
        }
        Ok(())
    }
}

impl<M: RawMutex, S: TelemetryStore, C: FormatCapabilities> Action for TelemetryService<M, S, C> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::PROFILE_ADD, "profile_add", ActionAccess::Mutate)
            .args(&[
                PROFILE_ID,
                FieldDescriptor::new(action_field::FORMAT, "format", ValueKind::Enum)
                    .with_enum(FORMAT_ENUM),
            ])
            .enums(FORMATS),
        ActionDescriptor::new(
            action::PROFILE_REMOVE,
            "profile_remove",
            ActionAccess::Mutate,
        )
        .args(&[PROFILE_ID]),
        ActionDescriptor::new(action::SET_PUBLIC, "set_public", ActionAccess::Mutate).args(&[
            PROFILE_ID,
            selection_field(action_field::SELECTION, "selection"),
        ]),
        ActionDescriptor::new(action::ADD_PRIVATE, "add_private", ActionAccess::Mutate).args(&[
            PROFILE_ID,
            FieldDescriptor::new(action_field::RECIPIENT, "recipient", ValueKind::Address),
            selection_field(action_field::PRIVATE_SELECTION, "selection"),
        ]),
        ActionDescriptor::new(
            action::REMOVE_PRIVATE,
            "remove_private",
            ActionAccess::Mutate,
        )
        .args(&[
            PROFILE_ID,
            FieldDescriptor::new(action_field::RECIPIENT, "recipient", ValueKind::Address),
        ]),
        ActionDescriptor::new(action::PROFILE_LIST, "profile_list", ActionAccess::Read)
            .args(&[OFFSET])
            .results(&[
                FieldDescriptor::new(action_field::LIST_ID, "profile_id", ValueKind::String),
                FieldDescriptor::new(action_field::LIST_FORMAT, "format", ValueKind::Enum)
                    .with_enum(FORMAT_ENUM),
                FieldDescriptor::new(action_field::LIST_PUBLIC, "public", ValueKind::Bytes),
                FieldDescriptor::new(action_field::LIST_REVISION, "revision", ValueKind::U64),
            ])
            .enums(FORMATS),
        ActionDescriptor::new(
            action::PROFILE_RECIPIENTS,
            "profile_recipients",
            ActionAccess::Read,
        )
        .args(&[
            PROFILE_ID,
            FieldDescriptor::new(action_field::RECIPIENTS_OFFSET, "offset", ValueKind::U64),
        ])
        .results(&[
            FieldDescriptor::new(
                action_field::RECIPIENT_ADDRESS,
                "recipient",
                ValueKind::Address,
            ),
            FieldDescriptor::new(
                action_field::RECIPIENT_SELECTION,
                "selection",
                ValueKind::Bytes,
            ),
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
            action::PROFILE_ADD => {
                let id = text(args, action_field::PROFILE_ID)?;
                let format = format(args)?;
                self.edit(|profiles, _| profiles.add_profile(id, format))
                    .await
            }
            action::PROFILE_REMOVE => {
                let id = text(args, action_field::PROFILE_ID)?;
                self.edit(|profiles, _| profiles.remove_profile(id)).await
            }
            action::SET_PUBLIC => {
                let id = text(args, action_field::PROFILE_ID)?;
                let selection = selection(args, action_field::SELECTION)?;
                self.edit(|profiles, caps| profiles.set_public(id, selection, caps))
                    .await
            }
            action::ADD_PRIVATE => {
                let id = text(args, action_field::PROFILE_ID)?;
                let recipient = recipient(args, action_field::RECIPIENT)?;
                let selection = selection(args, action_field::PRIVATE_SELECTION)?;
                self.edit(|profiles, caps| profiles.add_private(id, recipient, selection, caps))
                    .await
            }
            action::REMOVE_PRIVATE => {
                let id = text(args, action_field::PROFILE_ID)?;
                let recipient = recipient(args, action_field::RECIPIENT)?;
                self.edit(|profiles, _| profiles.remove_private(id, &recipient))
                    .await
            }
            action::PROFILE_LIST => {
                let offset = offset(args, action_field::OFFSET)?;
                let profiles = self.store.load().await?;
                for profile in profiles.profiles.iter().skip(offset).take(MAX_RECORDS) {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    write(out.text(action_field::LIST_ID, profile.id.as_str()))?;
                    write(out.variant(action_field::LIST_FORMAT, profile.format.number()))?;
                    write(out.bytes(
                        action_field::LIST_PUBLIC,
                        profile.public.measurements.as_bytes(),
                    ))?;
                    write(out.u64(action_field::LIST_REVISION, u64::from(profile.revision)))?;
                }
                Ok(())
            }
            action::PROFILE_RECIPIENTS => {
                let id = text(args, action_field::PROFILE_ID)?;
                let offset = offset(args, action_field::RECIPIENTS_OFFSET)?;
                let profiles = self.store.load().await?;
                let profile = profiles
                    .get(id)
                    .ok_or(ActionError::Rejected("profile_not_found"))?;
                for section in profile.private.iter().skip(offset).take(MAX_RECORDS) {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    write(out.bytes(action_field::RECIPIENT_ADDRESS, &section.recipient.0))?;
                    write(out.bytes(
                        action_field::RECIPIENT_SELECTION,
                        section.selection.measurements.as_bytes(),
                    ))?;
                }
                Ok(())
            }
            action::MEASUREMENTS => {
                let offset = offset(args, action_field::OFFSET)?;
                // The catalog of what the (only) format can encode now.
                let supported = self.caps.supported(PacketFormatId::Robonomics);
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
