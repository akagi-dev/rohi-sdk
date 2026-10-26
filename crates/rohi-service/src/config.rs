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
//! [`SectionStore`] that persists it and applies runtime effects. A
//! [`ManagedSection`] ties both together and implements the generic
//! operations; a tuple of sections is a [`ConfigRegistry`].
//!
//! Patch semantics: load the current section (or the module defaults), apply
//! all mutations to a copy, validate the complete result, persist atomically,
//! increment the revision. If anything is invalid nothing is persisted. The
//! whole transaction (revision check, load, validate, store, bump) runs under
//! a per-section lock, so concurrent patches and resets are serialized.

use core::cell::Cell;
use core::marker::PhantomData;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex as AsyncMutex;
use heapless::Vec;

use crate::field::ConfigField;
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
    /// The field is required and cannot be unset.
    NotOptional,
    /// The value or the resulting section is rejected by the typed
    /// validation.
    Invalid,
}

/// A typed config section exposed through the generic config interface.
///
/// The module owns the typed struct, the field metadata, the conversion
/// between [`Value`] and typed fields, and the validation. Field ids are
/// stable and only ever added.
pub trait ManagedConfig: Clone {
    /// Stable public section name, for example `connectivity`.
    const NAME: &'static str;
    /// Version of the section schema.
    const VERSION: u16;
    /// Field metadata, at most [`MAX_CONFIG_FIELDS`] entries.
    const FIELDS: &'static [ConfigField];

    /// The module defaults: the base of a patch when nothing is persisted.
    fn defaults() -> Self;

    /// The current value of a field, `None` when it is unset. Called for
    /// secret fields too; the registry never returns their value.
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError>;

    /// Sets a field. The registry has already checked that the field exists,
    /// is writable and that the value matches kind, variants and range.
    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError>;

    /// Unsets an optional field.
    fn unset_field(&mut self, id: u32) -> Result<(), FieldError>;

    /// Validates the complete section, including rules across fields.
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

/// Persistence and runtime effects of one section.
///
/// `store` and `remove` commit atomically and hand the effective value to the
/// runtime (the owner of the store publishes it in the same step), so a
/// cancelled caller can never leave a stored value unknown to the runtime.
#[allow(async_fn_in_trait)]
pub trait SectionStore<C> {
    /// The persisted section, `None` when nothing is persisted.
    async fn load(&self) -> Result<Option<C>, StoreError>;

    /// Persists `config` and applies it.
    async fn store(&self, config: &C) -> Result<(), StoreError>;

    /// Removes the persisted section and restores the module defaults.
    async fn remove(&self) -> Result<(), StoreError>;

    /// Whether a change only takes effect after a restart.
    fn restart_required(&self) -> bool {
        false
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

/// Name, version and revision of a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionInfo {
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
}

/// State of one field in [`SectionValues`].
#[derive(Clone, Debug, PartialEq)]
pub enum EntryState {
    /// The current value.
    Value(Value),
    /// A set secret; its value is not readable.
    Redacted,
}

/// One set field.
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
    /// Whether anything is persisted for the section.
    pub configured: bool,
    /// The fields that are set. Unset fields are absent.
    pub entries: Vec<ConfigEntry, MAX_CONFIG_FIELDS>,
}

/// What a mutation does to a field.
#[derive(Clone, Debug, PartialEq)]
pub enum MutationOp {
    /// Sets the field.
    Set(Value),
    /// Unsets the (optional) field.
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

/// Outcome of a successful patch or reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patched {
    /// The revision after the change (unchanged for a dry run).
    pub revision: u32,
    /// Whether the change only takes effect after a restart.
    pub restart_required: bool,
}

/// The generic operations of one section.
#[allow(async_fn_in_trait)]
pub trait ConfigSectionOps {
    /// Stable section name.
    fn name(&self) -> &'static str;
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
    /// Removes the persisted section and restores the defaults.
    async fn reset(&self, dry_run: bool) -> Result<Patched, ConfigError>;
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
        const { assert!(C::FIELDS.len() <= MAX_CONFIG_FIELDS) };
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

    fn field(id: u32) -> Result<&'static ConfigField, FieldError> {
        C::FIELDS
            .iter()
            .find(|field| field.field.id == id)
            .ok_or(FieldError::UnknownField)
    }

    fn apply(config: &mut C, mutation: &Mutation) -> Result<(), FieldError> {
        let field = Self::field(mutation.field_id)?;
        if !field.writable {
            return Err(FieldError::ReadOnly);
        }
        match &mutation.op {
            MutationOp::Set(value) => {
                field.check(value)?;
                config.set_field(field.field.id, value.clone())
            }
            MutationOp::Unset if field.optional => config.unset_field(field.field.id),
            MutationOp::Unset => Err(FieldError::NotOptional),
        }
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
    fn name(&self) -> &'static str {
        C::NAME
    }

    fn info(&self) -> SectionInfo {
        SectionInfo {
            name: C::NAME,
            version: u32::from(C::VERSION),
            revision: self.revision(),
        }
    }

    fn schema(&self) -> SectionSchema {
        SectionSchema {
            section: self.info(),
            fields: C::FIELDS,
        }
    }

    async fn get(&self) -> Result<SectionValues, ConfigError> {
        let _transaction = self.transaction.lock().await;
        let stored = self.store.load().await?;
        let mut entries = Vec::new();
        if let Some(config) = &stored {
            for field in C::FIELDS {
                // A value the generic model cannot express (for example a
                // non-UTF-8 SSID) is simply not reported.
                let Ok(Some(value)) = config.read_field(field.field.id) else {
                    continue;
                };
                let state = if field.secret {
                    EntryState::Redacted
                } else {
                    EntryState::Value(value)
                };
                // At most `FIELDS.len() <= MAX_CONFIG_FIELDS` entries.
                let _ = entries.push(ConfigEntry {
                    field_id: field.field.id,
                    state,
                });
            }
        }
        Ok(SectionValues {
            section: self.info(),
            configured: stored.is_some(),
            entries,
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
        let mut next = self.store.load().await?.unwrap_or_else(C::defaults);
        for mutation in mutations {
            Self::apply(&mut next, mutation)?;
        }
        next.validate()?;
        let restart_required = self.store.restart_required();
        if dry_run {
            return Ok(Patched {
                revision: self.revision(),
                restart_required,
            });
        }
        self.store.store(&next).await?;
        Ok(Patched {
            revision: self.bump(),
            restart_required,
        })
    }

    async fn reset(&self, dry_run: bool) -> Result<Patched, ConfigError> {
        let _transaction = self.transaction.lock().await;
        let restart_required = self.store.restart_required();
        if dry_run {
            return Ok(Patched {
                revision: self.revision(),
                restart_required,
            });
        }
        self.store.remove().await?;
        Ok(Patched {
            revision: self.bump(),
            restart_required,
        })
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
    /// Schema of the named section.
    fn schema(&self, section: &str) -> Option<SectionSchema>;
    /// Readable content of the named section.
    async fn get(&self, section: &str) -> Result<SectionValues, ConfigError>;
    /// Applies a patch to the named section.
    async fn patch(
        &self,
        section: &str,
        mutations: &[Mutation],
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Patched, ConfigError>;
    /// Resets the named section.
    async fn reset(&self, section: &str, dry_run: bool) -> Result<Patched, ConfigError>;
}

impl ConfigRegistry for () {
    fn section_count(&self) -> usize {
        0
    }

    fn section_info(&self, _: usize) -> Option<SectionInfo> {
        None
    }

    fn schema(&self, _: &str) -> Option<SectionSchema> {
        None
    }

    async fn get(&self, _: &str) -> Result<SectionValues, ConfigError> {
        Err(ConfigError::UnknownSection)
    }

    async fn patch(
        &self,
        _: &str,
        _: &[Mutation],
        _: Option<u32>,
        _: bool,
    ) -> Result<Patched, ConfigError> {
        Err(ConfigError::UnknownSection)
    }

    async fn reset(&self, _: &str, _: bool) -> Result<Patched, ConfigError> {
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

            fn schema(&self, section: &str) -> Option<SectionSchema> {
                $(if self.$index.name() == section {
                    return Some(self.$index.schema());
                })+
                None
            }

            async fn get(&self, section: &str) -> Result<SectionValues, ConfigError> {
                $(if self.$index.name() == section {
                    return self.$index.get().await;
                })+
                Err(ConfigError::UnknownSection)
            }

            async fn patch(
                &self,
                section: &str,
                mutations: &[Mutation],
                expected_revision: Option<u32>,
                dry_run: bool,
            ) -> Result<Patched, ConfigError> {
                $(if self.$index.name() == section {
                    return self.$index.patch(mutations, expected_revision, dry_run).await;
                })+
                Err(ConfigError::UnknownSection)
            }

            async fn reset(&self, section: &str, dry_run: bool) -> Result<Patched, ConfigError> {
                $(if self.$index.name() == section {
                    return self.$index.reset(dry_run).await;
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
