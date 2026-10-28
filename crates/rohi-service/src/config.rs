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
//! Typed configuration sections behind a generic, schema-driven interface.
//!
//! A module implements [`ManagedConfig`] for its typed section and a
//! [`SectionStore`] that persists its overrides and applies runtime effects.
//! A [`ManagedSection`] ties both together and implements the generic
//! operations; a tuple of sections is a [`ConfigRegistry`].
//!
//! The effective configuration is `C::default()`, plus the firmware overrides
//! compiled into the product, plus the explicit overrides of the user, see
//! [`materialize`]. Firmware and user overrides are two values of the same
//! generated type ([`HasOverrides::Overrides`]). The firmware supplies its part
//! through [`SectionStore::firmware_overrides`]; it is the same for reads,
//! validation and runtime publication and is never persisted, so a field the
//! user never set follows the firmware that runs.
//!
//! Patch semantics: load the persisted overrides, apply all mutations to a
//! copy (`Set` creates or replaces an override with a value, `None` overrides an
//! optional field with an explicit "no value", `Clear` removes the override),
//! validate the complete effective result, persist atomically, increment the
//! revision. If anything is invalid nothing is persisted. The same effective
//! value is validated, stored and handed to the runtime. The whole transaction (revision check,
//! load, validate, store, bump) runs under a per-section lock, so concurrent
//! patches and resets are serialized.

use core::cell::Cell;
use core::marker::PhantomData;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex as AsyncMutex;
use heapless::Vec;
use rohi_config::{ConfigOverrides, ConfigSection, HasOverrides, materialize};

use crate::field::{ConfigField, EnumDescriptor};
use crate::schema::{config_fields_valid, enums_valid, field_ids_match, name_ok};
use crate::value::Value;
use crate::{MAX_CONFIG_FIELDS, MAX_REGISTRY_ENTRIES};

/// Why a field access or a whole-section validation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// The section has no field with this id.
    UnknownField,
    /// The field cannot be changed by clients.
    ReadOnly,
    /// The value has the wrong kind.
    WrongKind,
    /// The value is outside the range or variants of the field.
    OutOfRange,
    /// The value or the resulting section is rejected by the typed
    /// validation.
    Invalid,
    /// The field is required: it cannot be set to "no value".
    NotOptional,
}

/// A typed config section exposed through the generic config interface.
///
/// The type is the *effective* configuration the runtime consumes: `Default`,
/// the firmware overrides and the user overrides, see [`materialize`]. The
/// persisted state is [`HasOverrides::Overrides`], the fields the user set
/// explicitly.
///
/// The module owns the typed struct (which derives its persisted overrides with
/// `#[derive(ConfigOverrides)]`, see [`HasOverrides`]), the field metadata, the
/// conversion between [`Value`] and typed fields, and the validation. Field ids
/// are stable and only ever added. The id and version of the section in the
/// registry are those of the persisted overrides: the hash of the name of the
/// config type and the `version` of the derive.
pub trait ManagedConfig: HasOverrides + Clone + Default {
    /// Stable public section name, for example `connectivity`.
    const NAME: &'static str;
    /// Field metadata, at most [`MAX_CONFIG_FIELDS`] entries.
    const FIELDS: &'static [ConfigField];
    /// The enums the fields refer to, see [`EnumDescriptor`].
    const ENUMS: &'static [EnumDescriptor] = &[];

    /// The current value of a field, `None` when it has none (an optional
    /// field that is not set). Never called for secret fields: the registry
    /// only asks whether they are set.
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError>;

    /// Whether a field has a value. A secret field must answer without
    /// producing its value, so that a generic read never copies a secret into a
    /// [`Value`] only to discard it. For any other field it is
    /// `read_field(id)?.is_some()`.
    fn field_is_set(&self, id: u32) -> Result<bool, FieldError>;

    /// Sets a field. The registry has already checked that the field exists,
    /// is writable and that the value matches kind, variants and range.
    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError>;

    /// Sets an optional field to "no value". The registry has already checked
    /// that the field exists, is writable and optional. This is a domain state
    /// distinct from clearing the override: the field does not follow the
    /// firmware default afterwards.
    fn set_none(&mut self, id: u32) -> Result<(), FieldError> {
        let _ = id;
        Err(FieldError::NotOptional)
    }

    /// Validates the complete effective section, including rules across
    /// fields. It is called for every patch, so an unconfigured or disabled
    /// section must be a valid state.
    fn validate(&self) -> Result<(), FieldError>;
}

/// Failure of the persistence behind a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The storage is currently unavailable.
    Unavailable,
    /// Unexpected failure.
    Internal,
}

/// Persistence, firmware overrides and runtime effects of one section `C`.
///
/// [`firmware_overrides`](Self::firmware_overrides) is the product policy of
/// the section: the generic layer materializes `C::default()`, these and the
/// persisted user overrides for reads, validation and publication. `store` and
/// `remove` commit atomically and hand the `effective` value, the one that was
/// validated, to the runtime (the
/// owner of the store publishes it in the same step), so a cancelled caller
/// can never leave a stored value unknown to the runtime.
#[allow(async_fn_in_trait)]
pub trait SectionStore<C: ManagedConfig> {
    /// The overrides compiled into the firmware, all `Default` when the
    /// product has no policy for the section. Deterministic for the lifetime of
    /// the running firmware and never persisted.
    fn firmware_overrides(&self) -> C::Overrides;

    /// The persisted overrides, `None` when nothing is persisted.
    async fn load(&self) -> Result<Option<C::Overrides>, StoreError>;

    /// Persists the user `overrides` and publishes `effective`, the
    /// materialization of the firmware overrides and `overrides`.
    async fn store(&self, overrides: &C::Overrides, effective: &C) -> Result<(), StoreError>;

    /// Removes the persisted user overrides and publishes `effective`, the
    /// materialization of the firmware overrides alone.
    async fn remove(&self, effective: &C) -> Result<(), StoreError>;

    /// Whether a change only takes effect after a restart.
    fn restart_required(&self) -> bool {
        false
    }
}

/// A [`SectionStore`] whose section is read once, when its owner starts: every
/// change, a reset included, takes effect only after a restart.
pub struct RestartRequired<S>(pub S);

impl<C: ManagedConfig, S: SectionStore<C>> SectionStore<C> for RestartRequired<S> {
    fn firmware_overrides(&self) -> C::Overrides {
        self.0.firmware_overrides()
    }

    async fn load(&self) -> Result<Option<C::Overrides>, StoreError> {
        self.0.load().await
    }

    async fn store(&self, overrides: &C::Overrides, effective: &C) -> Result<(), StoreError> {
        self.0.store(overrides, effective).await
    }

    async fn remove(&self, effective: &C) -> Result<(), StoreError> {
        self.0.remove(effective).await
    }

    fn restart_required(&self) -> bool {
        true
    }
}

/// Failure of a generic config operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// No section with this name.
    UnknownSection,
    /// A mutation is invalid; nothing was changed.
    Field(FieldError),
    /// The section changed since the expected revision.
    Conflict,
    /// The storage is currently unavailable.
    Unavailable,
    /// Unexpected failure.
    Internal,
}

impl From<FieldError> for ConfigError {
    fn from(error: FieldError) -> Self {
        Self::Field(error)
    }
}

impl From<StoreError> for ConfigError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Unavailable => Self::Unavailable,
            StoreError::Internal => Self::Internal,
        }
    }
}

/// Id, name, version and revision of a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    /// Stable numeric id.
    pub id: u64,
    /// Stable section name.
    pub name: &'static str,
    /// Version of the section schema.
    pub version: u32,
    /// Changes after every successful write or reset; restarts from 0 at boot.
    pub revision: u32,
}

/// A section with its field metadata.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionSchema {
    /// Name, version and current revision.
    pub section: SectionInfo,
    /// Field metadata.
    pub fields: &'static [ConfigField],
    /// The enums the fields refer to.
    pub enums: &'static [EnumDescriptor],
}

/// State of one field in [`SectionValues`].
#[derive(Clone, Debug, PartialEq)]
pub enum EntryState {
    /// The effective value.
    Value(Value),
    /// A set secret; its value is not readable.
    Redacted,
}

/// One field that has an effective value.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigEntry {
    /// Field id.
    pub field_id: u32,
    /// Value or redaction marker.
    pub state: EntryState,
}

/// The readable content of a section.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionValues {
    /// Name, version and current revision.
    pub section: SectionInfo,
    /// The effective configuration: defaults, firmware and user overrides.
    /// A field without a value (an optional field that is not set) is absent.
    pub entries: Vec<ConfigEntry, MAX_CONFIG_FIELDS>,
    /// The ids of the fields the user overrides explicitly.
    pub overridden: Vec<u32, MAX_CONFIG_FIELDS>,
}

/// What a mutation does to a field.
#[derive(Clone, Debug, PartialEq)]
pub enum MutationOp {
    /// Creates or replaces the override of the field with a value.
    Set(Value),
    /// Overrides an optional field with an explicit "no value". It differs
    /// from [`Clear`](Self::Clear): the firmware default no longer applies.
    None,
    /// Removes the override of the field: the firmware default applies again.
    Clear,
}

/// One change of a patch.
#[derive(Clone, Debug, PartialEq)]
pub struct Mutation {
    /// Field id.
    pub field_id: u32,
    /// The change.
    pub op: MutationOp,
}

/// Outcome of a successful patch or reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patched {
    /// The revision after the change (unchanged for a dry run).
    pub revision: u32,
    /// Whether the change only takes effect after a restart.
    pub restart_required: bool,
}

/// Outcome of a successful factory reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FactoryReset {
    /// Whether the change only takes effect after a restart.
    pub restart_required: bool,
}

/// The generic operations of one section.
#[allow(async_fn_in_trait)]
pub trait ConfigSectionOps {
    /// Stable section id.
    fn id(&self) -> u64;
    /// Name, version and current revision.
    fn info(&self) -> SectionInfo;
    /// Field metadata.
    fn schema(&self) -> SectionSchema;
    /// Readable content; secrets are redacted.
    async fn get(&self) -> Result<SectionValues, ConfigError>;
    /// Applies `mutations` atomically.
    async fn patch(
        &self,
        mutations: &[Mutation],
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError>;
    /// Removes every user override: the firmware overrides and the defaults apply.
    async fn reset(
        &self,
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError>;
}

/// A typed section with its store and revision counter.
///
/// The revision lives in RAM: it starts at 0 at boot and is meant for
/// optimistic concurrency within one boot.
pub struct ManagedSection<C, S, M: RawMutex> {
    store: S,
    revision: Mutex<M, Cell<u32>>,
    // Serializes transactions on the store.
    transaction: AsyncMutex<M, ()>,
    _config: PhantomData<fn() -> C>,
}

impl<C: ManagedConfig, S: SectionStore<C>, M: RawMutex> ManagedSection<C, S, M> {
    /// A section persisted through `store`.
    pub const fn new(store: S) -> Self {
        const {
            assert!(name_ok(C::NAME), "section name is empty or too long");
            assert!(
                C::FIELDS.len() <= MAX_CONFIG_FIELDS,
                "too many config fields"
            );
            assert!(enums_valid(C::ENUMS), "invalid section enums");
            assert!(
                config_fields_valid(C::FIELDS, C::ENUMS),
                "invalid config fields: ids and names must be unique and fit"
            );
            assert!(
                field_ids_match(C::FIELDS, <C::Overrides as ConfigOverrides<C>>::FIELD_IDS),
                "every writable config field needs an override and every override a config field"
            );
        };
        Self {
            store,
            revision: Mutex::new(Cell::new(0)),
            transaction: AsyncMutex::new(()),
            _config: PhantomData,
        }
    }

    fn revision(&self) -> u32 {
        self.revision.lock(Cell::get)
    }

    fn bump(&self) -> u32 {
        self.revision.lock(|revision| {
            let next = revision.get().wrapping_add(1);
            revision.set(next);
            next
        })
    }

    /// The id of the section: that of its persisted overrides.
    const SECTION_ID: u64 = <C::Overrides as ConfigSection>::ID.0;
    const VERSION: u16 = <C::Overrides as ConfigSection>::VERSION;

    fn field(id: u32) -> Result<&'static ConfigField, FieldError> {
        C::FIELDS
            .iter()
            .find(|field| field.field.id == id)
            .ok_or(FieldError::UnknownField)
    }

    /// The effective configuration: `C::default()`, the firmware overrides and
    /// the user `overrides`. The one place that decides it.
    fn effective(&self, overrides: &C::Overrides) -> C {
        materialize(&self.store.firmware_overrides(), overrides)
    }

    fn apply(&self, overrides: &mut C::Overrides, mutation: &Mutation) -> Result<(), FieldError> {
        let field = Self::field(mutation.field_id)?;
        if !field.writable {
            return Err(FieldError::ReadOnly);
        }
        match &mutation.op {
            MutationOp::Set(value) => {
                field.check(value, C::ENUMS)?;
                let mut config = self.effective(overrides);
                config.set_field(field.field.id, value.clone())?;
                Self::capture(overrides, &config, field.field.id)?;
            }
            MutationOp::None => {
                if !field.optional {
                    return Err(FieldError::NotOptional);
                }
                let mut config = self.effective(overrides);
                config.set_none(field.field.id)?;
                Self::capture(overrides, &config, field.field.id)?;
            }
            MutationOp::Clear => {
                if !overrides.clear(field.field.id) {
                    return Err(FieldError::UnknownField);
                }
            }
        }
        Ok(())
    }

    /// Overrides the field with its value in `config`. The schema and the
    /// overrides are checked to agree when the section is built, so a field
    /// the overrides do not know is a bug that must not be ignored.
    fn capture(overrides: &mut C::Overrides, config: &C, id: u32) -> Result<(), FieldError> {
        if overrides.capture(config, id) {
            Ok(())
        } else {
            Err(FieldError::UnknownField)
        }
    }

    /// Validates the effective result of `next` and persists it (or removes
    /// the persisted section when no user override is left), publishing the
    /// value that was validated, then bumps the revision. The one commit step
    /// of every mutation; callers hold the transaction lock.
    async fn write(
        &self,
        next: &C::Overrides,
        persisted: bool,
        dry_run: bool,
    ) -> Result<Patched, ConfigError> {
        // The value validated here is the one stored and published.
        let effective = self.effective(next);
        effective.validate()?;
        let restart_required = self.store.restart_required();
        if dry_run {
            return Ok(Patched {
                revision: self.revision(),
                restart_required,
            });
        }
        if !next.is_empty() {
            self.store.store(next, &effective).await?;
        } else if persisted {
            // The last user override is gone: only the firmware policy applies.
            self.store.remove(&effective).await?;
        }
        Ok(Patched {
            revision: self.bump(),
            restart_required,
        })
    }

    fn check_revision(&self, expected: Option<u32>) -> Result<(), ConfigError> {
        match expected {
            Some(expected) if expected != self.revision() => Err(ConfigError::Conflict),
            _ => Ok(()),
        }
    }
}

impl<C: ManagedConfig, S: SectionStore<C>, M: RawMutex> ConfigSectionOps
    for ManagedSection<C, S, M>
{
    fn id(&self) -> u64 {
        Self::SECTION_ID
    }

    fn info(&self) -> SectionInfo {
        SectionInfo {
            id: Self::SECTION_ID,
            name: C::NAME,
            version: u32::from(Self::VERSION),
            revision: self.revision(),
        }
    }

    fn schema(&self) -> SectionSchema {
        SectionSchema {
            section: self.info(),
            fields: C::FIELDS,
            enums: C::ENUMS,
        }
    }

    async fn get(&self) -> Result<SectionValues, ConfigError> {
        let _transaction = self.transaction.lock().await;
        let overrides = self.store.load().await?.unwrap_or_default();
        let config = self.effective(&overrides);
        let mut entries = Vec::new();
        let mut overridden = Vec::new();
        for field in C::FIELDS {
            let id = field.field.id;
            if overrides.is_overridden(id) == Some(true) {
                // At most `FIELDS.len() <= MAX_CONFIG_FIELDS` ids.
                let _ = overridden.push(id);
            }
            // A value the generic model cannot express (for example a
            // non-UTF-8 SSID) is simply not reported. A secret is only
            // asked whether it is set, its value is never read.
            let state = if field.secret {
                match config.field_is_set(id) {
                    Ok(true) => EntryState::Redacted,
                    _ => continue,
                }
            } else {
                match config.read_field(id) {
                    Ok(Some(value)) => EntryState::Value(value),
                    _ => continue,
                }
            };
            // At most `FIELDS.len() <= MAX_CONFIG_FIELDS` entries.
            let _ = entries.push(ConfigEntry {
                field_id: id,
                state,
            });
        }
        Ok(SectionValues {
            section: self.info(),
            entries,
            overridden,
        })
    }

    async fn patch(
        &self,
        mutations: &[Mutation],
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError> {
        let _transaction = self.transaction.lock().await;
        self.check_revision(expected_revision)?;
        let stored = self.store.load().await?;
        let persisted = stored.is_some();
        let mut next = stored.unwrap_or_default();
        for mutation in mutations {
            self.apply(&mut next, mutation)?;
        }
        self.write(&next, persisted, dry_run).await
    }

    async fn reset(
        &self,
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError> {
        let _transaction = self.transaction.lock().await;
        self.check_revision(expected_revision)?;
        let restart_required = self.store.restart_required();
        if dry_run {
            return Ok(Patched {
                revision: self.revision(),
                restart_required,
            });
        }
        let effective = self.effective(&C::Overrides::default());
        self.store.remove(&effective).await?;
        Ok(Patched {
            revision: self.bump(),
            restart_required,
        })
    }
}

/// Changes the user overrides of one section from a service, through the same
/// transaction as a client's patch: under the section lock the persisted
/// overrides are loaded, changed, the complete effective result is validated,
/// persisted and published, and the revision is incremented. A service never
/// reads and writes the store on its own, which would lose a concurrent change.
#[allow(async_fn_in_trait)]
pub trait ConfigUpdate<C: ManagedConfig> {
    /// Applies `change` to the persisted user overrides. Nothing is changed
    /// when the effective result is invalid.
    async fn update<F: FnOnce(&mut C::Overrides)>(&self, change: F)
    -> Result<Patched, ConfigError>;
}

impl<C: ManagedConfig, S: SectionStore<C>, M: RawMutex> ConfigUpdate<C>
    for ManagedSection<C, S, M>
{
    async fn update<F: FnOnce(&mut C::Overrides)>(
        &self,
        change: F,
    ) -> Result<Patched, ConfigError> {
        let _transaction = self.transaction.lock().await;
        let stored = self.store.load().await?;
        let persisted = stored.is_some();
        let mut next = stored.unwrap_or_default();
        change(&mut next);
        self.write(&next, persisted, false).await
    }
}

impl<C: ManagedConfig, T: ConfigUpdate<C>> ConfigUpdate<C> for &T {
    async fn update<F: FnOnce(&mut C::Overrides)>(
        &self,
        change: F,
    ) -> Result<Patched, ConfigError> {
        (**self).update(change).await
    }
}

/// All config sections compiled into the firmware.
///
/// Implemented for tuples of [`ConfigSectionOps`] (up to
/// [`MAX_REGISTRY_ENTRIES`]) and for `()`; adding a section means adding it to
/// the tuple, nothing else changes.
#[allow(async_fn_in_trait)]
pub trait ConfigRegistry {
    /// Number of sections.
    fn section_count(&self) -> usize;
    /// Description of the section at `index`.
    fn section_info(&self, index: usize) -> Option<SectionInfo>;
    /// Schema of the section with the given id.
    fn schema(&self, section: u64) -> Option<SectionSchema>;
    /// Readable content of the section with the given id.
    async fn get(&self, section: u64) -> Result<SectionValues, ConfigError>;
    /// Applies a patch to the section with the given id.
    async fn patch(
        &self,
        section: u64,
        mutations: &[Mutation],
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError>;
    /// Resets the section with the given id.
    async fn reset(
        &self,
        section: u64,
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError>;

    /// Whether no two sections share an id. Firmware checks it once at
    /// start, a clash would make requests reach the wrong section.
    fn ids_are_unique(&self) -> bool {
        (0..self.section_count()).all(|index| {
            let id = self.section_info(index).map(|info| info.id);
            (0..index).all(|other| self.section_info(other).map(|info| info.id) != id)
        })
    }

    /// Returns the whole configuration storage to its factory state.
    ///
    /// The default resets every section of the registry one by one. A
    /// registry that owns the storage overrides it to format the storage,
    /// which also removes what no section of this registry knows about (stale
    /// sections of older firmware, the device identity).
    async fn factory_reset(&self) -> Result<FactoryReset, ConfigError> {
        let mut restart_required = false;
        for index in 0..self.section_count() {
            let Some(info) = self.section_info(index) else {
                continue;
            };
            restart_required |= self.reset(info.id, None, false).await?.restart_required;
        }
        Ok(FactoryReset { restart_required })
    }
}

impl ConfigRegistry for () {
    fn section_count(&self) -> usize {
        0
    }

    fn section_info(&self, _: usize) -> Option<SectionInfo> {
        None
    }

    fn schema(&self, _: u64) -> Option<SectionSchema> {
        None
    }

    async fn get(&self, _: u64) -> Result<SectionValues, ConfigError> {
        Err(ConfigError::UnknownSection)
    }

    async fn patch(
        &self,
        _: u64,
        _: &[Mutation],
        _: Option<u32>,
        _: bool,
    ) -> Result<Patched, ConfigError> {
        Err(ConfigError::UnknownSection)
    }

    async fn reset(&self, _: u64, _: Option<u32>, _: bool) -> Result<Patched, ConfigError> {
        Err(ConfigError::UnknownSection)
    }
}

macro_rules! tuple_config_registry {
    ($count:expr; $($name:ident . $index:tt),+) => {
        impl<$($name: ConfigSectionOps),+> ConfigRegistry for ($($name,)+) {
            fn section_count(&self) -> usize {
                $count
            }

            fn section_info(&self, index: usize) -> Option<SectionInfo> {
                match index {
                    $($index => Some(self.$index.info()),)+
                    _ => None,
                }
            }

            fn schema(&self, section: u64) -> Option<SectionSchema> {
                $(if self.$index.id() == section {
                    return Some(self.$index.schema());
                })+
                None
            }

            async fn get(&self, section: u64) -> Result<SectionValues, ConfigError> {
                $(if self.$index.id() == section {
                    return self.$index.get().await;
                })+
                Err(ConfigError::UnknownSection)
            }

            async fn patch(
                &self,
                section: u64,
                mutations: &[Mutation],
                expected_revision: Option<u32>,
                dry_run: bool,
            ) -> Result<Patched, ConfigError> {
                $(if self.$index.id() == section {
                    return self.$index.patch(mutations, expected_revision, dry_run).await;
                })+
                Err(ConfigError::UnknownSection)
            }

            async fn reset(
                &self,
                section: u64,
                expected_revision: Option<u32>,
                dry_run: bool,
            ) -> Result<Patched, ConfigError> {
                $(if self.$index.id() == section {
                    return self.$index.reset(expected_revision, dry_run).await;
                })+
                Err(ConfigError::UnknownSection)
            }
        }
    };
}

tuple_config_registry!(1; A.0);
tuple_config_registry!(2; A.0, B.1);
tuple_config_registry!(3; A.0, B.1, C.2);
tuple_config_registry!(4; A.0, B.1, C.2, D.3);
tuple_config_registry!(5; A.0, B.1, C.2, D.3, E.4);
tuple_config_registry!(6; A.0, B.1, C.2, D.3, E.4, F.5);
tuple_config_registry!(7; A.0, B.1, C.2, D.3, E.4, F.5, G.6);
tuple_config_registry!(8; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7);

const _: () = assert!(MAX_REGISTRY_ENTRIES == 8);
