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
//! Typed configuration sections behind a generic, schema-driven interface.
//!
//! A module implements [`Config`] for its **typed** section, the *effective*
//! configuration, and keeps all knowledge about it, and a [`SectionStore`]
//! that persists its overrides and applies runtime effects. A [`Section`]
//! ties both together and implements the generic operations
//! ([`ConfigSectionOps`]); a tuple of sections is a [`ConfigRegistry`]. The
//! [`Section`] documentation has a complete, runnable example.
//!
//! # Effective configuration and overrides
//!
//! The **persisted** part is an overrides struct generated from the config by
//! [`#[derive(Config)]`](rohi_config_derive::Config) (the persisted model is
//! described in [`rohi_config`]) that holds an [`Override<T>`](rohi_config::Override)
//! per field: `Override::Default` (the user did not override it) or
//! `Override::Value(v)` (explicit). Nullable fields use `Override<Option<T>>`:
//! `Value(Some(v))` is an explicit value, `Value(None)` an explicit "no value"
//! and `Default` follows the layer below.
//!
//! The effective configuration is `C::default()` (the portable baseline owned
//! by the module), then the **firmware overrides** compiled into the product,
//! then the **user overrides**: [`materialize`]. Firmware and user overrides
//! are two values of the same generated type ([`HasOverrides::Overrides`]).
//! The firmware supplies its part through
//! [`SectionStore::firmware_overrides`]; it is the same for reads, validation
//! and runtime publication and is never persisted, so a field the user never
//! set follows the firmware that runs. A changed firmware override or default
//! reaches every field the user did not override; an explicit override
//! survives it.
//!
//! The id and the version of a section in the registry are those of its
//! generated overrides as a [`ConfigSection`]; the name is [`Config::NAME`].
//!
//! # Schema
//!
//! The schema ([`Config::FIELDS`]) only expresses simple constraints (kind,
//! enum variants, numeric range, see [`ConfigField`]); everything else stays
//! in the typed [`Config::validate`]. A [`Section`] adds the generic
//! behaviour on top of a [`SectionStore`] (`firmware_overrides`, `load`,
//! `store`, `remove` of the **user overrides**, `restart_required`), which the
//! firmware implements over its configuration owner task. Reads, validation
//! and runtime publication all use `materialize(firmware_overrides(), user)`,
//! and `store` / `remove` receive that same effective value to publish.
//! `#[derive(Config)]` generates the [`Config`] implementation from the field
//! declarations.
//!
//! # Patch semantics
//!
//! A patch is a list of [`Mutation`]s by field id (at most
//! [`MAX_MUTATIONS`](crate::MAX_MUTATIONS)):
//!
//! 1. Load the stored overrides (or an empty set) and apply all mutations to
//!    a temporary copy. [`MutationOp::Set`] checks kind and range, sets the
//!    value on the effective copy and **captures** it as an explicit
//!    override. [`MutationOp::None`] sets an **optional** field to no value
//!    and captures that as an explicit override (a required field answers
//!    [`FieldError::NotOptional`]). [`MutationOp::Clear`] removes the override
//!    of any writable field, required or not, so the field follows the
//!    firmware override, or else the default, again.
//! 2. Validate the **complete effective** config (defaults, firmware
//!    overrides and the candidate user overrides). An unconfigured or
//!    disabled section is a valid state (wifi without an SSID is disabled, a
//!    location without coordinates, connectivity disabled), so every patch is
//!    validated.
//! 3. Store atomically (a result without any override removes the persisted
//!    section) and publish the same effective config.
//!
//! If anything is invalid, nothing is stored. A field that is not mentioned
//! keeps its override or its default. Consequence: fields that belong
//! together are set in one patch (`enabled=true` alone on connectivity fails
//! until `host` is set).
//!
//! A patch addresses one section; no request spans several, so a client that
//! needs that applies them one by one. Success means the configuration is
//! stored **and** handed to its runtime, not that its effect (a network
//! connection, say) was reached: observe the [`status`](crate::status).
//!
//! The whole transaction (load, validate, store) runs under a per-section
//! lock, so concurrent patches and resets are serialized and each applies to
//! the latest persisted overrides; for the same field the last committed
//! value wins. A read ([`ConfigSectionOps::get`]) takes the same lock.
//!
//! * **Secrets** ([`ConfigField::secret`]) are write-only: a read reports
//!   them as [`EntryState::Redacted`], never their value, and the typed
//!   config is only asked [`Config::field_is_set`].
//! * **Dry run** validates a patch exactly like a real one and stores
//!   nothing.
//! * **Service actions** that change the section use [`ConfigUpdate::update`]
//!   of the same [`Section`]: the same lock, validation and publication as a
//!   patch, never a separate load and store.
//! * **Reset** removes every user override (without validating, the result is
//!   the firmware policy over the defaults) and publishes it; compiled
//!   firmware overrides are retained. [`ConfigRegistry::factory_reset`]
//!   returns the whole storage to its factory state.
//! * [`restart_required`](ConfigSectionOps::restart_required) tells the
//!   client that a change needs a restart.
//!
//! # Reading
//!
//! [`ConfigSectionOps::get`] returns the effective values
//! ([`SectionValues::entries`]; a field without a value, such as an optional
//! field whose default is none, is absent) and
//! [`overridden`](SectionValues::overridden), the ids of the explicitly
//! overridden fields. A value the generic model cannot express (for example a
//! non-UTF-8 SSID) is not reported either. Bounds of the schema are not part
//! of what clients receive from the control plane.
//!
//! The three states of a nullable field stay distinct: `FIELD=value` is an
//! explicit value, `FIELD=none` ([`MutationOp::None`]) an explicit "no value"
//! and `FIELD=null` ([`MutationOp::Clear`]) removes only the user override
//! and follows the current firmware override or default (`server2` of the
//! `time` section defaults to `time.cloudflare.com`: `server2=none` disables
//! the second server, `server2=null` restores that default). The `FIELD=...`
//! forms are the syntax of the `rohi` tool.
//!
//! # Commit and cancellation
//!
//! The store commits **and** publishes to the runtime in one step, so a
//! cancelled caller can never leave a stored value unknown to the runtime.
//! The firmware owner publishes the **effective** config (defaults, firmware
//! overrides and user overrides, also after a removal). This is a contract
//! of the [`SectionStore`] implementation: the generic layer only guarantees
//! that the section lock is released when a caller is dropped, and that
//! nothing is stored when validation fails.
//!
//! # Registry
//!
//! A [`ConfigRegistry`] is a **tuple of sections** (up to
//! [`MAX_REGISTRY_ENTRIES`]) and dispatches by
//! the numeric section id; names are for discovery. Ids must be unique within
//! a registry (see [`ConfigRegistry::ids_are_unique`]; `Device::new` of
//! [`rohi-control`](../../rohi_control/index.html) panics in every build,
//! release included, when two sections share an id). `()` is the registry of
//! a device without sections.
//!
//! Adding a section means adding it to the tuple. Nothing else changes: not
//! the protocol, not the client, not the CLI. A section is checked when it is
//! built ([`Section::new`]): name, field and enum tables fit the limits, and
//! every writable field has an override and every override a field, a
//! violation is a build error (see [`registry`](crate::registry) for the
//! shared rules).
//!
//! ```
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_service::{
//!     Config, ConfigError, ConfigRegistry, ConfigSectionOps, Section, SectionStore, StoreError,
//! };
//!
//! #[derive(Clone, Debug, PartialEq, Config)]
//! #[config(name = "alpha", overrides = AlphaOverrides, version = 1)]
//! pub struct AlphaConfig {
//!     #[config(id = 1)]
//!     pub enabled: bool,
//! }
//!
//! #[derive(Clone, Debug, PartialEq, Config)]
//! #[config(name = "beta", overrides = BetaOverrides, version = 1)]
//! pub struct BetaConfig {
//!     #[config(id = 1)]
//!     pub level: u32,
//! }
//!
//! impl Default for AlphaConfig {
//!     fn default() -> Self {
//!         Self { enabled: false }
//!     }
//! }
//!
//! impl Default for BetaConfig {
//!     fn default() -> Self {
//!         Self { level: 3 }
//!     }
//! }
//!
//! // A store without persistence: nothing is ever stored.
//! struct Volatile;
//!
//! impl<C: Config> SectionStore<C> for Volatile {
//!     fn firmware_overrides(&self) -> C::Overrides {
//!         C::Overrides::default()
//!     }
//!     async fn load(&self) -> Result<Option<C::Overrides>, StoreError> {
//!         Ok(None)
//!     }
//!     async fn store(&self, _: &C::Overrides, _: &C) -> Result<(), StoreError> {
//!         Ok(())
//!     }
//!     async fn remove(&self, _: &C) -> Result<(), StoreError> {
//!         Ok(())
//!     }
//! }
//!
//! // The registry is a tuple: adding a section is adding an element.
//! let sections = (
//!     Section::<AlphaConfig, _, NoopRawMutex>::new(Volatile),
//!     Section::<BetaConfig, _, NoopRawMutex>::new(Volatile),
//! );
//! assert_eq!(sections.section_count(), 2);
//! assert!(sections.ids_are_unique());
//!
//! let beta = sections.section_info(1).unwrap();
//! assert_eq!(beta.name, "beta");
//! let values = block_on(sections.get(beta.id)).unwrap();
//! assert_eq!(values.entries.len(), 1);
//! assert_eq!(block_on(sections.get(0)), Err(ConfigError::UnknownSection));
//! ```

use core::marker::PhantomData;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex as AsyncMutex;
use heapless::Vec;
use rohi_config::{ConfigOverrides, ConfigSection, FieldError, HasOverrides, materialize};

use crate::field::{ConfigField, EnumDescriptor};
use crate::isolated::isolated;
use crate::schema::{config_fields_valid, enums_valid, field_ids_match, name_ok};
use crate::{MAX_CONFIG_FIELDS, MAX_REGISTRY_ENTRIES};
use rohi_config::Value;

/// The typed configuration of one managed section.
///
/// The type is the *effective* configuration the runtime consumes: `Default`,
/// the firmware overrides and the user overrides, see [`materialize`]. The
/// persisted state is [`HasOverrides::Overrides`], the fields the user set
/// explicitly.
///
/// A config owns its field metadata, the conversion between [`Value`] and its
/// typed fields (see [`rohi_config::Codec`]), which fields are optional or
/// secret, its validation and its link to the generated persisted overrides.
/// It owns no storage, no transaction and no runtime task: those
/// belong to the [`Section`] that manages it and to the [`SectionStore`] behind
/// it. It is not a service either: a service may share a section through
/// [`ConfigUpdate`], and neither needs the other's identity.
///
/// Declare a config with [`#[derive(Config)]`](rohi_config_derive::Config),
/// which generates this implementation and the overrides from the one
/// declaration of the fields (the derive documentation lists every attribute):
///
/// ```
/// use rohi_config::{ConfigSection, SectionId};
/// use rohi_service::{Config, FieldError, Value, ValueKind};
///
/// fn validate_wifi(config: &WifiConfig) -> Result<(), FieldError> {
///     // A disabled section (no SSID) is valid; a password needs an SSID.
///     if config.ssid.is_none() && config.password.is_some() {
///         return Err(FieldError::Invalid);
///     }
///     Ok(())
/// }
///
/// #[derive(Clone, Debug, Default, PartialEq, Config)]
/// #[config(name = "wifi", overrides = WifiOverrides, version = 1, validate = validate_wifi)]
/// pub struct WifiConfig {
///     #[config(id = 1)]
///     pub ssid: Option<u32>,
///     #[config(id = 2, secret)]
///     pub password: Option<u32>,
///     #[config(id = 3, name = "scan_timeout", unit = Ms, min = 1_000, max = 120_000)]
///     pub scan_timeout_ms: u32,
/// }
///
/// assert_eq!(WifiConfig::NAME, "wifi");
/// assert_eq!(WifiConfig::FIELDS[2].field.name, "scan_timeout");
/// // The id of the stored section is the hash of the config type name.
/// assert_eq!(<WifiOverrides as ConfigSection>::ID, SectionId::from_name("WifiConfig"));
///
/// let mut config = WifiConfig::default();
/// assert_eq!(config.validate(), Ok(()));
/// assert_eq!(config.set_field(2, Value::U64(7)), Ok(()));
/// assert_eq!(config.validate(), Err(FieldError::Invalid));
/// assert_eq!(config.field_is_set(2), Ok(true));
/// // The value of a secret is never produced.
/// assert_eq!(config.read_field(2), Err(FieldError::Secret));
/// assert_eq!(WifiConfig::FIELDS[0].field.kind, ValueKind::U64);
/// ```
///
/// Field ids are stable and only ever added. The id and version of the section
/// in the registry are those of the persisted overrides: the hash of the name
/// of the config type and the `version` of the derive. The trait can still be
/// implemented by hand; the derive is the one source of truth for the common
/// case.
pub trait Config: HasOverrides + Clone + Default {
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
pub trait SectionStore<C: Config> {
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

impl<C: Config, S: SectionStore<C>> SectionStore<C> for RestartRequired<S> {
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
    /// No section with this id.
    UnknownSection,
    /// A mutation or the resulting section is invalid; nothing was changed.
    Field(FieldError),
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

/// Id, name and version of a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    /// Stable numeric id.
    pub id: u64,
    /// Stable section name.
    pub name: &'static str,
    /// Version of the section schema.
    pub version: u32,
}

/// A section with its field metadata.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionSchema {
    /// Id, name and version.
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
    /// Id, name and version.
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
    /// Id, name and version.
    fn info(&self) -> SectionInfo;
    /// Field metadata.
    fn schema(&self) -> SectionSchema;
    /// Readable content; secrets are redacted.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unavailable`] or [`ConfigError::Internal`] when the store
    /// fails to load the persisted overrides.
    async fn get(&self) -> Result<SectionValues, ConfigError>;
    /// Applies `mutations` atomically; with `dry_run` it only validates.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Field`] with the [`FieldError`] of the first mutation that
    /// is invalid (unknown or read-only field, wrong kind, out of range, not
    /// optional, ...) or of the failed validation of the effective section;
    /// nothing is stored then. [`ConfigError::Unavailable`] and
    /// [`ConfigError::Internal`] come from the [`SectionStore`].
    async fn patch(&self, mutations: &[Mutation], dry_run: bool) -> Result<Patched, ConfigError>;
    /// Removes every user override: the firmware overrides and the defaults apply.
    /// With `dry_run` nothing is changed.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unavailable`] or [`ConfigError::Internal`] from the
    /// [`SectionStore`].
    async fn reset(&self, dry_run: bool) -> Result<Patched, ConfigError>;
    /// Whether a change only takes effect after a restart.
    fn restart_required(&self) -> bool;
}

/// A managed section: a typed [`Config`] with its store and the transaction
/// lock that serializes every change.
///
/// It implements the generic `get`, `patch` and `reset` of
/// [`ConfigSectionOps`] and the transactional [`ConfigUpdate`] once, for every
/// config. Control requests and services that change their own section must
/// share the one instance, so that there is one lock.
///
/// [`Section::new`] checks the schema of `C` at build time. The store is the
/// firmware's [`SectionStore`]; the lock type `M` is the firmware's raw mutex
/// (`NoopRawMutex` for a single executor, as in the example).
///
/// # Example
///
/// A section over a store kept in memory, patched like a control request does.
///
/// ```
/// use core::cell::RefCell;
///
/// use embassy_futures::block_on;
/// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
/// use rohi_config::Value;
/// use rohi_service::{
///     Config, ConfigError, ConfigSectionOps, EntryState, FieldError, Mutation, MutationOp,
///     Section, SectionStore, StoreError,
/// };
///
/// #[derive(Clone, Debug, PartialEq, Config)]
/// #[config(name = "pump", overrides = PumpOverrides, version = 1)]
/// pub struct PumpConfig {
///     #[config(id = 1, name = "interval", unit = S, min = 10, max = 3_600)]
///     pub interval_s: u32,
///     #[config(id = 2, secret)]
///     pub pin: Option<u32>,
/// }
///
/// impl Default for PumpConfig {
///     fn default() -> Self {
///         Self { interval_s: 120, pin: None }
///     }
/// }
///
/// // Persists the user overrides in memory. A real store commits to flash and
/// // publishes `effective` to the owner of the runtime in the same step.
/// #[derive(Default)]
/// struct MemoryStore {
///     overrides: RefCell<Option<PumpOverrides>>,
///     published: RefCell<Option<PumpConfig>>,
/// }
///
/// impl SectionStore<PumpConfig> for &MemoryStore {
///     fn firmware_overrides(&self) -> PumpOverrides {
///         PumpOverrides::default()
///     }
///     async fn load(&self) -> Result<Option<PumpOverrides>, StoreError> {
///         Ok(self.overrides.borrow().clone())
///     }
///     async fn store(&self, overrides: &PumpOverrides, effective: &PumpConfig) -> Result<(), StoreError> {
///         *self.overrides.borrow_mut() = Some(overrides.clone());
///         *self.published.borrow_mut() = Some(effective.clone());
///         Ok(())
///     }
///     async fn remove(&self, effective: &PumpConfig) -> Result<(), StoreError> {
///         *self.overrides.borrow_mut() = None;
///         *self.published.borrow_mut() = Some(effective.clone());
///         Ok(())
///     }
/// }
///
/// let store = MemoryStore::default();
/// let section = Section::<PumpConfig, _, NoopRawMutex>::new(&store);
///
/// // Out of range: rejected before anything is stored.
/// let too_fast = [Mutation { field_id: 1, op: MutationOp::Set(Value::U64(1)) }];
/// assert_eq!(
///     block_on(section.patch(&too_fast, false)),
///     Err(ConfigError::Field(FieldError::OutOfRange))
/// );
/// assert!(store.overrides.borrow().is_none());
///
/// // A dry run validates like a real patch and stores nothing.
/// let patch = [
///     Mutation { field_id: 1, op: MutationOp::Set(Value::U64(30)) },
///     Mutation { field_id: 2, op: MutationOp::Set(Value::U64(1234)) },
/// ];
/// block_on(section.patch(&patch, true)).unwrap();
/// assert!(store.overrides.borrow().is_none());
///
/// block_on(section.patch(&patch, false)).unwrap();
/// assert_eq!(store.published.borrow().as_ref().unwrap().interval_s, 30);
///
/// // A read reports the effective values; the secret is only marked as set.
/// let values = block_on(section.get()).unwrap();
/// assert_eq!(values.entries[0].state, EntryState::Value(Value::U64(30)));
/// assert_eq!(values.entries[1].state, EntryState::Redacted);
/// assert_eq!(values.overridden.as_slice(), &[1, 2]);
///
/// // Resetting removes the user overrides: the defaults apply again.
/// block_on(section.reset(false)).unwrap();
/// assert!(store.overrides.borrow().is_none());
/// assert_eq!(store.published.borrow().as_ref().unwrap().interval_s, 120);
/// ```
pub struct Section<C, S, M: RawMutex> {
    store: S,
    // Serializes transactions on the store.
    transaction: AsyncMutex<M, ()>,
    _config: PhantomData<fn() -> C>,
}

impl<C: Config, S: SectionStore<C>, M: RawMutex> Section<C, S, M> {
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
            transaction: AsyncMutex::new(()),
            _config: PhantomData,
        }
    }

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
    /// value that was validated. The one commit step of every mutation; callers
    /// hold the transaction lock.
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
            return Ok(Patched { restart_required });
        }
        if !next.is_empty() {
            self.store.store(next, &effective).await?;
        } else if persisted {
            // The last user override is gone: only the firmware policy applies.
            self.store.remove(&effective).await?;
        }
        Ok(Patched { restart_required })
    }
}

impl<C: Config, S: SectionStore<C>, M: RawMutex> ConfigSectionOps for Section<C, S, M> {
    fn id(&self) -> u64 {
        Self::SECTION_ID
    }

    fn info(&self) -> SectionInfo {
        SectionInfo {
            id: Self::SECTION_ID,
            name: C::NAME,
            version: u32::from(Self::VERSION),
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

    async fn patch(&self, mutations: &[Mutation], dry_run: bool) -> Result<Patched, ConfigError> {
        let _transaction = self.transaction.lock().await;
        let stored = self.store.load().await?;
        let persisted = stored.is_some();
        let mut next = stored.unwrap_or_default();
        for mutation in mutations {
            self.apply(&mut next, mutation)?;
        }
        self.write(&next, persisted, dry_run).await
    }

    async fn reset(&self, dry_run: bool) -> Result<Patched, ConfigError> {
        let _transaction = self.transaction.lock().await;
        let restart_required = self.store.restart_required();
        if dry_run {
            return Ok(Patched { restart_required });
        }
        let effective = self.effective(&C::Overrides::default());
        self.store.remove(&effective).await?;
        Ok(Patched { restart_required })
    }

    fn restart_required(&self) -> bool {
        self.store.restart_required()
    }
}

/// Changes the user overrides of one section from a service, through the same
/// transaction as a client's patch: under the section lock the persisted
/// overrides are loaded, changed, the complete effective result is validated,
/// persisted and published. A service never
/// reads and writes the store on its own, which would lose a concurrent change.
#[allow(async_fn_in_trait)]
pub trait ConfigUpdate<C: Config> {
    /// Applies `change` to the persisted user overrides. Nothing is changed
    /// when the effective result is invalid.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Field`] when the effective result fails validation,
    /// [`ConfigError::Unavailable`] or [`ConfigError::Internal`] from the
    /// [`SectionStore`].
    async fn update<F: FnOnce(&mut C::Overrides)>(&self, change: F)
    -> Result<Patched, ConfigError>;
}

impl<C: Config, S: SectionStore<C>, M: RawMutex> ConfigUpdate<C> for Section<C, S, M> {
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

impl<C: Config, T: ConfigUpdate<C>> ConfigUpdate<C> for &T {
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
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownSection`] when no section has this id, otherwise as
    /// [`ConfigSectionOps::get`].
    async fn get(&self, section: u64) -> Result<SectionValues, ConfigError>;
    /// Applies a patch to the section with the given id.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownSection`] when no section has this id, otherwise as
    /// [`ConfigSectionOps::patch`].
    async fn patch(
        &self,
        section: u64,
        mutations: &[Mutation],
        dry_run: bool,
    ) -> Result<Patched, ConfigError>;
    /// Resets the section with the given id.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownSection`] when no section has this id, otherwise as
    /// [`ConfigSectionOps::reset`].
    async fn reset(&self, section: u64, dry_run: bool) -> Result<Patched, ConfigError>;
    /// Whether a change of any section only takes effect after a restart.
    fn restart_required(&self) -> bool;

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
    ///
    /// # Errors
    ///
    /// The first error of a section reset; the sections before it stay reset.
    async fn factory_reset(&self) -> Result<FactoryReset, ConfigError> {
        let mut restart_required = false;
        for index in 0..self.section_count() {
            let Some(info) = self.section_info(index) else {
                continue;
            };
            restart_required |= self.reset(info.id, false).await?.restart_required;
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

    async fn patch(&self, _: u64, _: &[Mutation], _: bool) -> Result<Patched, ConfigError> {
        Err(ConfigError::UnknownSection)
    }

    async fn reset(&self, _: u64, _: bool) -> Result<Patched, ConfigError> {
        Err(ConfigError::UnknownSection)
    }

    fn restart_required(&self) -> bool {
        false
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
                    return isolated(self.$index.get()).await;
                })+
                Err(ConfigError::UnknownSection)
            }

            async fn patch(
                &self,
                section: u64,
                mutations: &[Mutation],
                        dry_run: bool,
            ) -> Result<Patched, ConfigError> {
                $(if self.$index.id() == section {
                    return isolated(self.$index.patch(mutations, dry_run)).await;
                })+
                Err(ConfigError::UnknownSection)
            }

            async fn reset(
                &self,
                section: u64,
                        dry_run: bool,
            ) -> Result<Patched, ConfigError> {
                $(if self.$index.id() == section {
                    return isolated(self.$index.reset(dry_run)).await;
                })+
                Err(ConfigError::UnknownSection)
            }

            fn restart_required(&self) -> bool {
                false $(|| self.$index.restart_required())+
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
tuple_config_registry!(9; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8);
tuple_config_registry!(10; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9);
tuple_config_registry!(11; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9, K.10);
tuple_config_registry!(12; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9, K.10, L.11);

const _: () = assert!(MAX_REGISTRY_ENTRIES == 12);
