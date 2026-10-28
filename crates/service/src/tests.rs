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
use core::cell::RefCell;
use std::string::String;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_config::{Codec, ConfigOverrides, Override, Text, Variant, VariantValue};

use crate::*;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Fast,
    Slow,
}

const MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    1,
    &[EnumVariant::new(0, "fast"), EnumVariant::new(1, "slow")],
)];

impl VariantValue for Mode {
    fn variant(&self) -> u32 {
        match self {
            Mode::Fast => 0,
            Mode::Slow => 1,
        }
    }

    fn from_variant(variant: u32) -> Option<Self> {
        match variant {
            0 => Some(Mode::Fast),
            1 => Some(Mode::Slow),
            _ => None,
        }
    }
}

/// An enabled section needs a host.
fn host_is_set(enabled: bool, host: &str) -> Result<(), FieldError> {
    if enabled && host.is_empty() {
        return Err(FieldError::Invalid);
    }
    Ok(())
}

fn demo_is_valid(demo: &Demo) -> Result<(), FieldError> {
    host_is_set(demo.enabled, &demo.host)
}

#[derive(Clone, Debug, PartialEq, Config)]
#[config(
    name = "demo",
    overrides = DemoOverrides,
    version = 3,
    id = 1,
    validate = demo_is_valid,
    enums = MODES
)]
struct Demo {
    #[config(id = 1)]
    enabled: bool,
    #[config(id = 2, unit = S, min = 10, max = 100)]
    interval_s: u64,
    #[config(id = 3, with = Text)]
    host: String,
    #[config(id = 4, secret, with = Text)]
    token: Option<String>,
    #[config(id = 5, read_only, with = Variant, enum_id = 1)]
    mode: Mode,
}

std::thread_local! {
    /// The default interval of the "firmware": a test changes it to model a
    /// firmware update between two reads.
    static DEFAULT_INTERVAL_S: core::cell::Cell<u64> = const { core::cell::Cell::new(60) };
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_s: DEFAULT_INTERVAL_S.get(),
            host: String::new(),
            token: None,
            mode: Mode::Fast,
        }
    }
}

#[derive(Default)]
struct MemoryStore {
    value: RefCell<Option<DemoOverrides>>,
    writes: RefCell<u32>,
    unavailable: RefCell<bool>,
    /// The firmware policy: every field of this config is a firmware override.
    firmware: RefCell<Option<Demo>>,
    /// What the store handed to the runtime last.
    published: RefCell<Option<Demo>>,
}

impl MemoryStore {
    fn firmware_overrides(&self) -> DemoOverrides {
        let mut overrides = DemoOverrides::default();
        if let Some(demo) = self.firmware.borrow().as_ref() {
            for id in 1..=4 {
                assert!(overrides.capture(demo, id));
            }
        }
        overrides
    }

    /// The effective configuration behind the persisted overrides.
    fn effective(&self) -> Demo {
        let user = self.value.borrow().clone().unwrap_or_default();
        rohi_config::materialize(&self.firmware_overrides(), &user)
    }
}

impl SectionStore<Demo> for &MemoryStore {
    fn firmware_overrides(&self) -> DemoOverrides {
        MemoryStore::firmware_overrides(self)
    }

    async fn load(&self) -> Result<Option<DemoOverrides>, StoreError> {
        // Lets another task run between a load and the following store.
        embassy_futures::yield_now().await;
        if *self.unavailable.borrow() {
            return Err(StoreError::Unavailable);
        }
        Ok(self.value.borrow().clone())
    }

    async fn store(&self, overrides: &DemoOverrides, effective: &Demo) -> Result<(), StoreError> {
        *self.value.borrow_mut() = Some(overrides.clone());
        *self.published.borrow_mut() = Some(effective.clone());
        *self.writes.borrow_mut() += 1;
        Ok(())
    }

    async fn remove(&self, effective: &Demo) -> Result<(), StoreError> {
        *self.value.borrow_mut() = None;
        *self.published.borrow_mut() = Some(effective.clone());
        *self.writes.borrow_mut() += 1;
        Ok(())
    }
}

fn set(id: u32, value: Value) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::Set(value),
    }
}

fn none(id: u32) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::None,
    }
}

fn clear(id: u32) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::Clear,
    }
}

fn section(store: &MemoryStore) -> Section<Demo, &MemoryStore, NoopRawMutex> {
    Section::new(store)
}

fn text(value: &str) -> Value {
    Value::text(value).unwrap()
}

#[test]
fn patch_applies_all_mutations_and_bumps_the_revision() {
    let store = MemoryStore::default();
    let section = section(&store);
    let patched = block_on(section.patch(
        &[
            set(1, Value::Bool(true)),
            set(3, text("a.io")),
            set(2, Value::U64(30)),
        ],
        Some(0),
        false,
    ))
    .unwrap();
    assert_eq!(patched.revision, 1);
    assert!(!patched.restart_required);
    let stored = store.effective();
    assert!(stored.enabled);
    assert_eq!(stored.host.as_str(), "a.io");
    assert_eq!(stored.interval_s, 30);
}

#[test]
fn a_patch_keeps_untouched_overrides() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io")), set(2, Value::U64(20))], None, false)).unwrap();
    block_on(section.patch(&[set(3, text("b.io"))], None, false)).unwrap();
    let stored = store.effective();
    assert_eq!(stored.host.as_str(), "b.io");
    assert_eq!(stored.interval_s, 20);
}

#[test]
fn only_the_fields_that_were_set_are_persisted() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    let overrides = store.value.borrow().clone().unwrap();
    assert_eq!(overrides.host, rohi_config::Override::Value("a.io".into()));
    assert_eq!(overrides.interval_s, rohi_config::Override::Default);
    assert_eq!(overrides.enabled, rohi_config::Override::Default);
}

#[test]
fn a_value_equal_to_the_default_is_still_an_override() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(2, Value::U64(60))], None, false)).unwrap();
    assert_eq!(
        store.value.borrow().clone().unwrap().interval_s,
        rohi_config::Override::Value(60)
    );
    assert_eq!(block_on(section.get()).unwrap().overridden.as_slice(), [2]);
}

#[test]
fn an_invalid_mutation_changes_nothing() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    let before = store.value.borrow().clone();
    let result = block_on(section.patch(
        &[set(3, text("b.io")), set(2, Value::U64(1_000))],
        None,
        false,
    ));
    assert_eq!(result, Err(ConfigError::Field(FieldError::OutOfRange)));
    assert_eq!(*store.value.borrow(), before);
    assert_eq!(*store.writes.borrow(), 1);
    assert_eq!(section.info().revision, 1);
}

#[test]
fn the_complete_effective_result_is_validated() {
    let store = MemoryStore::default();
    let section = section(&store);
    // `enabled` needs a host: a cross-field rule of the typed section.
    assert_eq!(
        block_on(section.patch(&[set(1, Value::Bool(true))], None, false)),
        Err(ConfigError::Field(FieldError::Invalid))
    );
    assert!(store.value.borrow().is_none());
    // The host can come from the same patch.
    block_on(section.patch(
        &[set(1, Value::Bool(true)), set(3, text("a.io"))],
        None,
        false,
    ))
    .unwrap();
    // Clearing the host leaves `enabled` without one.
    assert_eq!(
        block_on(section.patch(&[clear(3)], None, false)),
        Err(ConfigError::Field(FieldError::Invalid))
    );
    assert_eq!(store.effective().host.as_str(), "a.io");
}

#[test]
fn field_errors_are_distinguished() {
    let store = MemoryStore::default();
    let section = section(&store);
    let patch = |mutation| block_on(section.patch(&[mutation], None, false));
    assert_eq!(
        patch(set(99, Value::Bool(true))),
        Err(FieldError::UnknownField.into())
    );
    assert_eq!(
        patch(set(1, text("yes"))),
        Err(FieldError::WrongKind.into())
    );
    assert_eq!(
        patch(set(5, Value::Enum(0))),
        Err(FieldError::ReadOnly.into())
    );
    assert_eq!(patch(clear(5)), Err(FieldError::ReadOnly.into()));
    assert_eq!(patch(clear(99)), Err(FieldError::UnknownField.into()));
    assert!(store.value.borrow().is_none());
}

#[test]
fn clearing_an_override_restores_the_default_of_any_field() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(
        &[
            set(3, text("a.io")),
            set(2, Value::U64(20)),
            set(4, text("")),
        ],
        None,
        false,
    ))
    .unwrap();
    assert_eq!(store.effective().interval_s, 20);
    // A required field can be cleared too: the default applies again.
    block_on(section.patch(&[clear(2)], None, false)).unwrap();
    assert_eq!(store.effective().interval_s, 60);
    assert_eq!(store.effective().host.as_str(), "a.io");
    // An empty value is an override, clearing is not the same.
    assert_eq!(store.effective().token.as_deref(), Some(""));
    block_on(section.patch(&[clear(4)], None, false)).unwrap();
    assert_eq!(store.effective().token, None);
}

#[test]
fn clearing_the_last_override_removes_the_persisted_section() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    block_on(section.patch(&[clear(3)], None, false)).unwrap();
    assert!(store.value.borrow().is_none());
    assert_eq!(store.effective(), Demo::default());
    assert!(block_on(section.get()).unwrap().overridden.is_empty());
}

#[test]
fn clearing_a_field_that_is_not_overridden_is_a_no_op() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[clear(2)], None, false)).unwrap();
    assert!(store.value.borrow().is_none());
    // Nothing was persisted, so there was nothing to write either.
    assert_eq!(*store.writes.borrow(), 0);
}

#[test]
fn dry_run_validates_without_persisting() {
    let store = MemoryStore::default();
    let section = section(&store);
    let patched = block_on(section.patch(&[set(3, text("a.io"))], None, true)).unwrap();
    assert_eq!(patched.revision, 0);
    assert!(store.value.borrow().is_none());
    assert_eq!(*store.writes.borrow(), 0);
    assert_eq!(
        block_on(section.patch(&[set(1, Value::Bool(true))], None, true)),
        Err(FieldError::Invalid.into())
    );
    block_on(section.reset(None, true)).unwrap();
    assert_eq!(*store.writes.borrow(), 0);
}

#[test]
fn a_stale_revision_is_a_conflict() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], Some(0), false)).unwrap();
    assert_eq!(
        block_on(section.patch(&[set(3, text("b.io"))], Some(0), false)),
        Err(ConfigError::Conflict)
    );
    assert_eq!(store.effective().host.as_str(), "a.io");
    block_on(section.patch(&[set(3, text("b.io"))], Some(1), false)).unwrap();
}

#[test]
fn concurrent_patches_are_serialized() {
    let store = MemoryStore::default();
    let section = section(&store);
    let (a, b) = block_on(embassy_futures::join::join(
        section.patch(&[set(3, text("a.io"))], Some(0), false),
        section.patch(&[set(4, text("s3cret"))], Some(0), false),
    ));
    assert_eq!(a.map(|patched| patched.revision), Ok(1));
    assert_eq!(b, Err(ConfigError::Conflict));

    let (a, b) = block_on(embassy_futures::join::join(
        section.patch(&[set(3, text("b.io"))], None, false),
        section.patch(&[set(4, text("t0ken"))], None, false),
    ));
    assert!(a.is_ok() && b.is_ok());
    let stored = store.effective();
    assert_eq!(stored.host.as_str(), "b.io");
    assert_eq!(stored.token.as_deref(), Some("t0ken"));
}

#[test]
fn a_service_update_shares_the_transaction_and_the_revision() {
    let store = MemoryStore::default();
    let section = section(&store);
    // A patch and a service update at once: neither loses the other's change.
    let (a, b) = block_on(embassy_futures::join::join(
        section.patch(&[set(3, text("a.io"))], None, false),
        section.update(|overrides| overrides.interval_s = Override::Value(20)),
    ));
    assert!(a.is_ok() && b.is_ok());
    let stored = store.effective();
    assert_eq!(stored.host.as_str(), "a.io");
    assert_eq!(stored.interval_s, 20);
    // Both writes moved the revision of the section.
    assert_eq!(section.info().revision, 2);
    assert_eq!(
        block_on(section.patch(&[set(3, text("b.io"))], Some(1), false)),
        Err(ConfigError::Conflict)
    );
}

#[test]
fn an_update_with_an_invalid_effective_result_changes_nothing() {
    let store = MemoryStore::default();
    let section = section(&store);
    let result = block_on(section.update(|overrides| overrides.enabled = Override::Value(true)));
    // Enabled without a host is invalid on its own.
    assert_eq!(
        result.map(|_| ()),
        Err(ConfigError::Field(FieldError::Invalid))
    );
    assert!(store.value.borrow().is_none());
    assert_eq!(section.info().revision, 0);
}

#[test]
fn get_reports_the_effective_values_and_redacts_secrets() {
    let store = MemoryStore::default();
    let section = section(&store);
    // Nothing is overridden: the defaults are the effective values.
    let defaults = block_on(section.get()).unwrap();
    assert!(defaults.overridden.is_empty());
    let value = |values: &SectionValues, id| {
        values
            .entries
            .iter()
            .find(|entry| entry.field_id == id)
            .map(|entry| entry.state.clone())
    };
    assert_eq!(
        value(&defaults, 1),
        Some(EntryState::Value(Value::Bool(false)))
    );
    assert_eq!(value(&defaults, 2), Some(EntryState::Value(Value::U64(60))));
    assert_eq!(value(&defaults, 3), Some(EntryState::Value(text(""))));
    // An optional field without a value is absent.
    assert_eq!(value(&defaults, 4), None);

    block_on(section.patch(&[set(3, text("a.io")), set(4, text("s3cret"))], None, false)).unwrap();
    let values = block_on(section.get()).unwrap();
    assert_eq!(values.overridden.as_slice(), [3, 4]);
    assert_eq!(value(&values, 2), Some(EntryState::Value(Value::U64(60))));
    assert_eq!(value(&values, 3), Some(EntryState::Value(text("a.io"))));
    assert_eq!(value(&values, 4), Some(EntryState::Redacted));
    assert!(!std::format!("{values:?}").contains("s3cret"));

    block_on(section.patch(&[clear(4)], None, false)).unwrap();
    let values = block_on(section.get()).unwrap();
    assert_eq!(value(&values, 4), None);
    assert_eq!(values.overridden.as_slice(), [3]);
}

fn interval(section: &Section<Demo, &MemoryStore, NoopRawMutex>) -> Option<EntryState> {
    block_on(section.get())
        .unwrap()
        .entries
        .iter()
        .find(|entry| entry.field_id == 2)
        .map(|entry| entry.state.clone())
}

#[test]
fn a_changed_firmware_default_reaches_the_fields_without_an_override() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    assert_eq!(interval(&section), Some(EntryState::Value(Value::U64(60))));
    // A firmware update changes the default; the persisted data stays.
    DEFAULT_INTERVAL_S.set(90);
    assert_eq!(interval(&section), Some(EntryState::Value(Value::U64(90))));
    assert_eq!(store.effective().host.as_str(), "a.io");
}

#[test]
fn an_explicit_override_survives_a_changed_default() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(2, Value::U64(30))], None, false)).unwrap();
    DEFAULT_INTERVAL_S.set(90);
    assert_eq!(interval(&section), Some(EntryState::Value(Value::U64(30))));
    // Clearing the override falls back to the current default.
    block_on(section.patch(&[clear(2)], None, false)).unwrap();
    assert_eq!(interval(&section), Some(EntryState::Value(Value::U64(90))));
}

#[test]
fn reset_clears_every_override() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io")), set(2, Value::U64(20))], None, false)).unwrap();
    let patched = block_on(section.reset(None, false)).unwrap();
    assert_eq!(patched.revision, 2);
    assert!(store.value.borrow().is_none());
    let values = block_on(section.get()).unwrap();
    assert!(values.overridden.is_empty());
    assert_eq!(store.effective(), Demo::default());
}

fn value_of(values: &SectionValues, id: u32) -> Option<EntryState> {
    values
        .entries
        .iter()
        .find(|entry| entry.field_id == id)
        .map(|entry| entry.state.clone())
}

fn firmware(host: &str, interval_s: u64, token: Option<&str>) -> Demo {
    Demo {
        enabled: false,
        interval_s,
        host: host.into(),
        token: token.map(Into::into),
        mode: Mode::Fast,
    }
}

#[test]
fn get_reports_the_firmware_defaults_when_nothing_is_overridden() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, Some("t")));
    let values = block_on(section(&store).get()).unwrap();
    assert!(values.overridden.is_empty());
    assert_eq!(
        value_of(&values, 2),
        Some(EntryState::Value(Value::U64(30)))
    );
    assert_eq!(value_of(&values, 3), Some(EntryState::Value(text("fw.io"))));
    assert_eq!(value_of(&values, 4), Some(EntryState::Redacted));
}

#[test]
fn an_override_is_applied_on_top_of_the_firmware_defaults() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, None));
    let section = section(&store);
    block_on(section.patch(&[set(2, Value::U64(20))], None, false)).unwrap();
    // The other fields keep the firmware defaults, not `Demo::default()`.
    assert_eq!(store.effective(), firmware("fw.io", 20, None));
    assert_eq!(store.published.borrow().clone(), Some(store.effective()));
    let values = block_on(section.get()).unwrap();
    assert_eq!(value_of(&values, 3), Some(EntryState::Value(text("fw.io"))));
    assert_eq!(values.overridden.as_slice(), [2]);
}

#[test]
fn clearing_an_override_exposes_the_current_firmware_default() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, None));
    let section = section(&store);
    block_on(section.patch(&[set(2, Value::U64(20)), set(3, text("a.io"))], None, false)).unwrap();
    *store.firmware.borrow_mut() = Some(firmware("fw2.io", 45, None));
    block_on(section.patch(&[clear(2)], None, false)).unwrap();
    // The cleared field follows the new default, the override survives.
    assert_eq!(store.effective(), firmware("a.io", 45, None));
    assert_eq!(store.published.borrow().clone(), Some(store.effective()));
}

#[test]
fn none_replaces_a_firmware_value_and_clear_reveals_it_again() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, Some("t")));
    let section = section(&store);
    block_on(section.patch(&[none(4)], None, false)).unwrap();
    // An explicit "no value" is a user override of a non-empty firmware value.
    assert_eq!(store.effective().token, None);
    assert_eq!(
        store.value.borrow().as_ref().unwrap().token,
        Override::Value(None)
    );
    block_on(section.patch(&[clear(4)], None, false)).unwrap();
    assert_eq!(store.effective().token.as_deref(), Some("t"));
    // Nothing is left to persist: only the firmware policy applies.
    assert!(store.value.borrow().is_none());
}

#[test]
fn the_firmware_overrides_are_never_persisted() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, Some("t")));
    let section = section(&store);
    block_on(section.patch(&[set(2, Value::U64(20))], None, false)).unwrap();
    let stored = store.value.borrow().clone().unwrap();
    assert_eq!(stored.interval_s, Override::Value(20));
    assert_eq!(stored.host, Override::Default);
    assert_eq!(stored.token, Override::Default);
}

#[test]
fn reset_publishes_the_firmware_defaults() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, Some("t")));
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    block_on(section.reset(None, false)).unwrap();
    assert!(store.value.borrow().is_none());
    assert_eq!(
        store.published.borrow().clone(),
        Some(firmware("fw.io", 30, Some("t")))
    );
}

#[test]
fn the_validated_value_is_the_one_published() {
    let store = MemoryStore::default();
    let mut base = firmware("", 30, None);
    base.enabled = true;
    *store.firmware.borrow_mut() = Some(base);
    let section = section(&store);
    // The firmware default is enabled without a host: invalid on its own, so
    // an unrelated change is rejected, a host override makes it valid.
    assert_eq!(
        block_on(section.patch(&[set(2, Value::U64(20))], None, false)),
        Err(ConfigError::Field(FieldError::Invalid))
    );
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    let published = store.published.borrow().clone().unwrap();
    assert_eq!(published.validate(), Ok(()));
    assert!(published.enabled);
    assert_eq!(published.host, "a.io");
}

#[test]
fn none_is_an_explicit_override_distinct_from_clear() {
    let store = MemoryStore::default();
    *store.firmware.borrow_mut() = Some(firmware("fw.io", 30, Some("default")));
    let section = section(&store);
    block_on(section.patch(&[none(4)], None, false)).unwrap();
    let values = block_on(section.get()).unwrap();
    // Overridden, and without a value.
    assert_eq!(values.overridden.as_slice(), [4]);
    assert_eq!(value_of(&values, 4), None);
    assert_eq!(
        store.value.borrow().as_ref().unwrap().token,
        rohi_config::Override::Value(None)
    );
    assert_eq!(store.published.borrow().clone().unwrap().token, None);

    // An explicit value replaces it.
    block_on(section.patch(&[set(4, text("v"))], None, false)).unwrap();
    assert_eq!(
        store.value.borrow().as_ref().unwrap().token,
        rohi_config::Override::Value(Some("v".into()))
    );

    // Clearing follows the firmware default again.
    block_on(section.patch(&[none(4)], None, false)).unwrap();
    block_on(section.patch(&[clear(4)], None, false)).unwrap();
    let values = block_on(section.get()).unwrap();
    assert!(values.overridden.is_empty());
    assert_eq!(value_of(&values, 4), Some(EntryState::Redacted));
    assert_eq!(
        store.published.borrow().clone().unwrap().token.as_deref(),
        Some("default")
    );
}

#[test]
fn none_is_refused_for_a_required_field() {
    let store = MemoryStore::default();
    let section = section(&store);
    assert_eq!(
        block_on(section.patch(&[none(3)], None, false)),
        Err(ConfigError::Field(FieldError::NotOptional))
    );
    // A read-only field stays read-only.
    assert_eq!(
        block_on(section.patch(&[none(5)], None, false)),
        Err(ConfigError::Field(FieldError::ReadOnly))
    );
    assert_eq!(*store.writes.borrow(), 0);
}

#[test]
fn an_unavailable_store_is_reported() {
    let store = MemoryStore::default();
    *store.unavailable.borrow_mut() = true;
    let section = section(&store);
    assert_eq!(block_on(section.get()), Err(ConfigError::Unavailable));
    assert_eq!(
        block_on(section.patch(&[], None, false)),
        Err(ConfigError::Unavailable)
    );
}

#[test]
fn the_schema_describes_the_fields() {
    let store = MemoryStore::default();
    let section = section(&store);
    let schema = section.schema();
    assert_eq!(schema.section.name, "demo");
    assert_eq!(schema.section.version, 3);
    assert_eq!(schema.fields.len(), 5);
    assert!(schema.fields[3].secret && schema.fields[3].optional);
}

#[test]
fn a_tuple_of_sections_is_a_registry() {
    let a = MemoryStore::default();

    fn other_is_valid(other: &Other) -> Result<(), FieldError> {
        host_is_set(other.enabled, &other.host)
    }

    // A second section type with another name.
    #[derive(Clone, Debug, Config)]
    #[config(
        name = "other",
        overrides = OtherOverrides,
        version = 1,
        id = 2,
        validate = other_is_valid,
        enums = MODES
    )]
    struct Other {
        #[config(id = 1)]
        enabled: bool,
        #[config(id = 2, unit = S, min = 10, max = 100)]
        interval_s: u64,
        #[config(id = 3, with = Text)]
        host: String,
        #[config(id = 4, secret, with = Text)]
        token: Option<String>,
        #[config(id = 5, read_only, with = Variant, enum_id = 1)]
        mode: Mode,
    }
    impl Default for Other {
        fn default() -> Self {
            Demo::default().into()
        }
    }
    impl From<Demo> for Other {
        fn from(demo: Demo) -> Self {
            Self {
                enabled: demo.enabled,
                interval_s: demo.interval_s,
                host: demo.host,
                token: demo.token,
                mode: demo.mode,
            }
        }
    }
    #[derive(Default)]
    struct OtherStore(RefCell<Option<OtherOverrides>>);
    impl SectionStore<Other> for &OtherStore {
        fn firmware_overrides(&self) -> OtherOverrides {
            OtherOverrides::default()
        }
        async fn load(&self) -> Result<Option<OtherOverrides>, StoreError> {
            Ok(self.0.borrow().clone())
        }
        async fn store(&self, overrides: &OtherOverrides, _: &Other) -> Result<(), StoreError> {
            *self.0.borrow_mut() = Some(overrides.clone());
            Ok(())
        }
        async fn remove(&self, _: &Other) -> Result<(), StoreError> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
        fn restart_required(&self) -> bool {
            true
        }
    }
    let other_store = OtherStore::default();

    let registry = (
        section(&a),
        Section::<Other, _, NoopRawMutex>::new(&other_store),
    );
    assert_eq!(registry.section_count(), 2);
    assert_eq!(registry.section_info(0).unwrap().name, "demo");
    assert_eq!(registry.section_info(1).unwrap().name, "other");
    assert_eq!(registry.section_info(2), None);
    assert!(registry.schema(2).is_some());
    assert!(registry.schema(99).is_none());

    let patched = block_on(registry.patch(2, &[set(3, text("x.io"))], None, false)).unwrap();
    assert!(patched.restart_required);
    assert!(other_store.0.borrow().is_some());
    assert!(a.value.borrow().is_none());
    assert_eq!(block_on(registry.get(99)), Err(ConfigError::UnknownSection));
    assert_eq!(block_on(().get(1)), Err(ConfigError::UnknownSection));
    block_on(registry.reset(2, None, false)).unwrap();
    assert!(other_store.0.borrow().is_none());
}

#[test]
fn check_enforces_kind_variants_and_range() {
    let field = ConfigField::new(1, "n", ValueKind::U64).bounds(Bounds::U64(1, 10));
    assert_eq!(field.check(&Value::U64(5), &[]), Ok(()));
    assert_eq!(
        field.check(&Value::U64(0), &[]),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(
        field.check(&Value::U64(11), &[]),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(field.check(&Value::I64(5), &[]), Err(FieldError::WrongKind));
    let float = ConfigField::new(1, "f", ValueKind::F32).bounds(Bounds::F32(0.0, 1.0));
    assert_eq!(float.check(&Value::F32(0.5), &[]), Ok(()));
    assert_eq!(
        float.check(&Value::F32(f32::NAN), &[]),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(
        float.check(&Value::F32(1.5), &[]),
        Err(FieldError::OutOfRange)
    );
    let variant = ConfigField::new(1, "e", ValueKind::Enum).with_enum(1);
    assert_eq!(variant.check(&Value::Enum(0), MODES), Ok(()));
    assert_eq!(
        variant.check(&Value::Enum(7), MODES),
        Err(FieldError::OutOfRange)
    );
}

#[test]
fn value_debug_never_shows_the_content() {
    assert_eq!(std::format!("{:?}", text("hunter2")), "Value(String)");
}

// --- status -----------------------------------------------------------------

#[derive(Service)]
struct Probe {
    state: RefCell<ServiceState>,
    health: RefCell<ServiceHealth>,
    issue: RefCell<Option<u32>>,
    queue: RefCell<u64>,
}

impl Probe {
    fn new() -> Self {
        Self {
            state: RefCell::new(ServiceState::Running),
            health: RefCell::new(ServiceHealth::Ok),
            issue: RefCell::new(None),
            queue: RefCell::new(0),
        }
    }
}

impl Status for Probe {
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "endpoint", ValueKind::String),
        FieldDescriptor::new(2, "queue_depth", ValueKind::U64),
        FieldDescriptor::new(3, "mode", ValueKind::Enum).with_enum(1),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        1,
        &[EnumVariant::new(0, "a"), EnumVariant::new(1, "b")],
    )];
    const ISSUES: &'static [IssueDescriptor] = &[IssueDescriptor::new(
        FIRST_SERVICE_ISSUE,
        "connection_timeout",
    )];

    async fn summary(&self) -> ServiceSummary {
        let health = *self.health.borrow();
        ServiceSummary::new(*self.state.borrow(), health, health == ServiceHealth::Ok)
    }

    async fn issues(&self, out: &mut heapless::Vec<Issue, MAX_ISSUES>) {
        if let Some(code) = *self.issue.borrow() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.text(1, "ingress.example.org:443")?;
        out.u64(2, *self.queue.borrow())?;
        out.variant(3, 1)
    }
}

#[test]
fn the_status_revision_follows_observable_changes_only() {
    let probe = Probe::new();
    let registry = Statuses::<NoopRawMutex, _>::new((&probe,));
    let summary = |now| block_on(registry.summary(0, now)).unwrap();

    let first = summary(1_000);
    assert_eq!(first.revision, 1);
    assert_eq!(first.name, "probe");
    assert!(first.essential);

    // A service-specific value is not observable state.
    *probe.queue.borrow_mut() = 17;
    assert_eq!(summary(2_000).revision, 1);

    *probe.health.borrow_mut() = ServiceHealth::Degraded;
    *probe.issue.borrow_mut() = Some(FIRST_SERVICE_ISSUE);
    let degraded = summary(3_000);
    assert_eq!(degraded.revision, 2);

    assert_eq!(summary(4_000).revision, 2);
    *probe.issue.borrow_mut() = None;
    *probe.health.borrow_mut() = ServiceHealth::Ok;
    assert_eq!(summary(5_000).revision, 3);
}

#[test]
fn state_age_counts_from_the_last_state_change() {
    let probe = Probe::new();
    let registry = Statuses::<NoopRawMutex, _>::new((&probe,));
    let summary = |now| block_on(registry.summary(0, now)).unwrap();
    assert_eq!(summary(1_000).state_age_ms, 0);
    assert_eq!(summary(4_000).state_age_ms, 3_000);
    *probe.state.borrow_mut() = ServiceState::Failed;
    assert_eq!(summary(5_000).state_age_ms, 0);
    assert_eq!(summary(6_500).state_age_ms, 1_500);
}

#[test]
fn detailed_status_has_fields_and_active_issues_with_age() {
    let probe = Probe::new();
    *probe.queue.borrow_mut() = 17;
    let registry = Statuses::<NoopRawMutex, _>::new((&probe,));
    *probe.issue.borrow_mut() = Some(FIRST_SERVICE_ISSUE);
    let _ = block_on(registry.summary(0, 1_000));
    let status = block_on(registry.status(Probe::ID, 35_000)).unwrap();
    assert_eq!(status.issues.len(), 1);
    assert_eq!(status.issues[0].code, FIRST_SERVICE_ISSUE);
    assert_eq!(status.issues[0].age_ms, Some(34_000));
    assert_eq!(status.fields.len(), 3);
    assert_eq!(status.fields[0], (1, text("ingress.example.org:443")));
    assert_eq!(status.fields[1], (2, Value::U64(17)));
    assert_eq!(status.fields[2], (3, Value::Enum(1)));
    assert_eq!(
        block_on(registry.status(99, 0)),
        Err(StatusError::UnknownService)
    );
    let schema = registry.schema(Probe::ID).unwrap();
    assert_eq!(
        (schema.info.id, schema.info.name, schema.info.version),
        (Probe::ID, "probe", 2)
    );
    assert_eq!(schema.fields.len(), 3);
    assert_eq!(schema.enums.len(), 1);
    assert_eq!(schema.issues[0].name, "connection_timeout");
}

#[test]
fn the_writer_checks_the_schema() {
    let mut writer = status::Collector {
        schema: <Probe as Status>::STATUS_SCHEMA,
        enums: <Probe as Status>::ENUMS,
        fields: heapless::Vec::new(),
    };
    assert_eq!(writer.u64(99, 1), Err(WriteError::UnknownField));
    assert_eq!(writer.u64(1, 1), Err(WriteError::WrongKind));
    assert_eq!(writer.variant(3, 9), Err(WriteError::WrongKind));
    assert_eq!(writer.bytes(1, &[0; 33]), Err(WriteError::TooLong));
    // Text is bounded like bytes: it is never cut.
    assert_eq!(writer.text(1, &"é".repeat(17)), Err(WriteError::TooLong));
    assert!(writer.fields.is_empty());
    writer.text(1, &"é".repeat(16)).unwrap();
    assert_eq!(writer.fields[0].1.as_str().unwrap().len(), 32);
}

fn summary(
    state: ServiceState,
    health: ServiceHealth,
    ready: bool,
    essential: bool,
) -> ServiceStatusSummary {
    ServiceStatusSummary {
        id: 1,
        name: "s",
        version: 1,
        state,
        health,
        ready,
        revision: 1,
        state_age_ms: 0,
        essential,
    }
}

#[test]
fn the_default_device_health_policy() {
    use DeviceHealth::*;
    use ServiceHealth as H;
    use ServiceState as S;
    assert_eq!(default_device_health(&[]), Ok);
    assert_eq!(
        default_device_health(&[summary(S::Disabled, H::Unknown, false, true)]),
        Ok
    );
    assert_eq!(
        default_device_health(&[summary(S::Running, H::Ok, true, true)]),
        Ok
    );
    assert_eq!(
        default_device_health(&[summary(S::Running, H::Degraded, false, true)]),
        Degraded
    );
    assert_eq!(
        default_device_health(&[summary(S::Running, H::Ok, false, true)]),
        Degraded
    );
    assert_eq!(
        default_device_health(&[summary(S::Failed, H::Error, false, true)]),
        Error
    );
    assert_eq!(
        default_device_health(&[
            summary(S::Failed, H::Error, false, false),
            summary(S::Running, H::Ok, true, true),
        ]),
        Degraded
    );
}

// --- actions ----------------------------------------------------------------

#[derive(Service)]
struct Scanner;

impl Action for Scanner {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "scan")
            .args(&[FieldDescriptor::new(1, "limit", ValueKind::U64)])
            .results(&[
                FieldDescriptor::new(1, "name", ValueKind::String),
                FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
            ]),
        ActionDescriptor::new(2, "reset"),
        ActionDescriptor::new(3, "provision")
            .args(&[FieldDescriptor::new(1, "secret", ValueKind::Bytes)])
            .secret(),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            1 => {
                let limit = args[0].1.as_u64().ok_or(ActionError::InvalidArgument)?;
                for index in 0..limit {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    out.text(1, "net").map_err(|_| ActionError::Internal)?;
                    out.i64(2, -(index as i64))
                        .map_err(|_| ActionError::Internal)?;
                }
                Ok(())
            }
            2 => Err(ActionError::Rejected("busy_resetting")),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

fn scanner() -> (&'static Scanner,) {
    (&Scanner,)
}

#[test]
fn an_action_returns_records() {
    let registry = scanner();
    let records = block_on(registry.invoke(Scanner::ID, 1, &[(1, Value::U64(3))])).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1][0], (1, text("net")));
    assert_eq!(records[2][1], (2, Value::I64(-2)));

    let none = block_on(registry.invoke(Scanner::ID, 1, &[(1, Value::U64(0))])).unwrap();
    assert!(none.is_empty());
}

#[test]
fn too_many_records_fail_instead_of_truncating() {
    let registry = scanner();
    let result = block_on(registry.invoke(Scanner::ID, 1, &[(1, Value::U64(7))]));
    assert_eq!(result, Err(ActionError::Internal));
}

#[test]
fn arguments_are_checked_against_the_descriptor() {
    let registry = scanner();
    let call = |args: &[(u32, Value)]| block_on(registry.invoke(Scanner::ID, 1, args));
    assert_eq!(call(&[]), Err(ActionError::InvalidArgument));
    assert_eq!(call(&[(1, text("3"))]), Err(ActionError::InvalidArgument));
    assert_eq!(
        call(&[(2, Value::U64(3))]),
        Err(ActionError::InvalidArgument)
    );
    assert_eq!(
        call(&[(1, Value::U64(1)), (1, Value::U64(1))]),
        Err(ActionError::InvalidArgument)
    );
}

#[test]
fn unknown_targets_and_rejections_are_distinguished() {
    let registry = scanner();
    assert_eq!(
        block_on(registry.invoke(99, 1, &[])),
        Err(ActionError::UnknownService)
    );
    assert_eq!(
        block_on(registry.invoke(Scanner::ID, 99, &[])),
        Err(ActionError::UnknownAction)
    );
    assert_eq!(
        block_on(registry.invoke(Scanner::ID, 2, &[])),
        Err(ActionError::Rejected("busy_resetting"))
    );
}

#[test]
fn action_metadata_is_exposed() {
    let registry = scanner();
    let actions = registry.service_actions(0);
    assert_eq!(registry.service_identity(0).unwrap().name, "scanner");
    assert_eq!(actions.len(), 3);
    assert!(!actions[0].privileged);
    assert!(actions[2].privileged && actions[2].secret_args);
    assert!(registry.service_actions(1).is_empty());
}

#[test]
fn wipe_clears_secret_values() {
    let mut value = Value::bytes(&[7; 8]).unwrap();
    value.wipe();
    assert_eq!(value.as_bytes(), Some(&[][..]));
    let mut value = text("hunter2");
    value.wipe();
    assert_eq!(value.as_str(), Some(""));
}

#[test]
fn integer_bounds_hold_over_the_whole_64_bit_range() {
    let upper = ConfigField::new(1, "n", ValueKind::U64).bounds(Bounds::U64(0, u64::MAX - 1));
    assert_eq!(upper.check(&Value::U64(u64::MAX - 1), &[]), Ok(()));
    assert_eq!(
        upper.check(&Value::U64(u64::MAX), &[]),
        Err(FieldError::OutOfRange)
    );
    let lower = ConfigField::new(1, "n", ValueKind::I64).bounds(Bounds::I64(i64::MIN + 1, 0));
    assert_eq!(lower.check(&Value::I64(i64::MIN + 1), &[]), Ok(()));
    assert_eq!(
        lower.check(&Value::I64(i64::MIN), &[]),
        Err(FieldError::OutOfRange)
    );
    // A bound of another kind never matches: integers are not compared as
    // floats.
    let mismatched = ConfigField::new(1, "n", ValueKind::U64).bounds(Bounds::I64(0, 10));
    assert_eq!(
        mismatched.check(&Value::U64(5), &[]),
        Err(FieldError::OutOfRange)
    );
}

#[test]
fn config_registries_detect_clashing_ids() {
    let store = MemoryStore::default();
    assert!((section(&store),).ids_are_unique());
    let (a, b) = (MemoryStore::default(), MemoryStore::default());
    assert!(!(section(&a), section(&b)).ids_are_unique());
}

#[test]
fn enum_values_are_checked_against_the_enum_of_the_field() {
    let descriptor = Scanner::ACTIONS[0];
    assert_eq!(descriptor.enums.len(), 0);
    const VARIANTS: &[EnumVariant] = &[EnumVariant::new(4, "wpa2")];
    let table = EnumDescriptor::new(1, VARIANTS);
    assert!(table.contains(4) && !table.contains(5));
    assert_eq!(table.name_of(4), Some("wpa2"));
    assert_eq!(table.value_of("wpa2"), Some(4));
    assert_eq!(table.value_of("open"), None);
}

#[test]
fn a_restart_only_store_reports_it_for_every_change_and_factory_reset() {
    let store = MemoryStore::default();
    let registry = (Section::<Demo, _, NoopRawMutex>::new(RestartRequired(
        &store,
    )),);

    let patched = block_on(registry.patch(1, &[set(3, text("a.io"))], None, false)).unwrap();
    assert!(patched.restart_required);
    assert!(store.value.borrow().is_some());
    let dry = block_on(registry.patch(1, &[set(3, text("b.io"))], None, true)).unwrap();
    assert!(dry.restart_required);
    assert!(
        block_on(registry.reset(1, None, false))
            .unwrap()
            .restart_required
    );
    assert!(store.value.borrow().is_none());
    assert!(block_on(registry.factory_reset()).unwrap().restart_required);
}

mod schema_invariants {
    use super::*;
    use crate::schema::{
        actions_valid, config_fields_valid, enums_valid, fields_valid, issues_valid, name_ok,
    };

    const MODE: &[EnumVariant] = &[EnumVariant::new(0, "fast"), EnumVariant::new(1, "slow")];

    fn field(id: u32, name: &'static str) -> FieldDescriptor {
        FieldDescriptor::new(id, name, ValueKind::U64)
    }

    #[test]
    fn names_are_never_empty_or_longer_than_the_wire() {
        assert!(name_ok("scan"));
        assert!(name_ok("abcdefghijklmnopqrstuvwx"));
        assert!(!name_ok(""));
        assert!(!name_ok("abcdefghijklmnopqrstuvwxy"));
    }

    #[test]
    fn field_ids_and_names_are_unique() {
        assert!(fields_valid(&[field(1, "a"), field(2, "b")], &[]));
        assert!(!fields_valid(&[field(1, "a"), field(1, "b")], &[]));
        assert!(!fields_valid(&[field(1, "a"), field(2, "a")], &[]));
        assert!(!fields_valid(&[field(1, "")], &[]));
        assert!(!fields_valid(&[field(1, "abcdefghijklmnopqrstuvwxy")], &[]));
        assert!(config_fields_valid(
            &[ConfigField::new(1, "a", ValueKind::U64)],
            &[]
        ));
        assert!(!config_fields_valid(
            &[
                ConfigField::new(1, "a", ValueKind::U64),
                ConfigField::new(1, "b", ValueKind::U64),
            ],
            &[]
        ));
        assert!(!config_fields_valid(
            &[
                ConfigField::new(1, "a", ValueKind::U64),
                ConfigField::new(2, "a", ValueKind::U64),
            ],
            &[]
        ));
    }

    #[test]
    fn an_enum_field_refers_to_a_known_enum_and_nothing_else_does() {
        let enums = [EnumDescriptor::new(1, MODE)];
        let mode = FieldDescriptor::new(1, "mode", ValueKind::Enum).with_enum(1);
        assert!(fields_valid(&[mode], &enums));
        assert!(!fields_valid(&[mode], &[]));
        let unknown = FieldDescriptor::new(1, "mode", ValueKind::Enum).with_enum(2);
        assert!(!fields_valid(&[unknown], &enums));
        let stray = FieldDescriptor::new(1, "n", ValueKind::U64).with_enum(1);
        assert!(!fields_valid(&[stray], &enums));
    }

    #[test]
    fn enums_have_unique_ids_values_and_names_that_fit() {
        assert!(enums_valid(&[EnumDescriptor::new(1, MODE)]));
        assert!(!enums_valid(&[EnumDescriptor::new(0, MODE)]));
        assert!(!enums_valid(&[
            EnumDescriptor::new(1, MODE),
            EnumDescriptor::new(1, MODE)
        ]));
        const SAME_VALUE: &[EnumVariant] =
            &[EnumVariant::new(1, "fast"), EnumVariant::new(1, "slow")];
        assert!(!enums_valid(&[EnumDescriptor::new(1, SAME_VALUE)]));
        const SAME_NAME: &[EnumVariant] =
            &[EnumVariant::new(1, "fast"), EnumVariant::new(2, "fast")];
        assert!(!enums_valid(&[EnumDescriptor::new(1, SAME_NAME)]));
        const EMPTY_NAME: &[EnumVariant] = &[EnumVariant::new(1, "")];
        assert!(!enums_valid(&[EnumDescriptor::new(1, EMPTY_NAME)]));
        const LONG_NAME: &[EnumVariant] = &[EnumVariant::new(1, "abcdefghijklmnopq")];
        assert!(!enums_valid(&[EnumDescriptor::new(1, LONG_NAME)]));
        // One enum per schema is a hard limit of the compact protocol.
        assert!(!enums_valid(&[
            EnumDescriptor::new(1, MODE),
            EnumDescriptor::new(2, MODE)
        ]));
    }

    #[test]
    fn issue_codes_and_names_are_unique_and_outside_the_sdk_range() {
        let first = crate::FIRST_SERVICE_ISSUE;
        assert!(issues_valid(&[
            IssueDescriptor::new(first, "a"),
            IssueDescriptor::new(first + 1, "b"),
            IssueDescriptor::new(crate::ISSUE_STARTUP_FAILED, "startup_failed"),
        ]));
        assert!(!issues_valid(&[
            IssueDescriptor::new(first, "a"),
            IssueDescriptor::new(first, "b"),
        ]));
        assert!(!issues_valid(&[
            IssueDescriptor::new(first, "a"),
            IssueDescriptor::new(first + 1, "a"),
        ]));
        assert!(!issues_valid(&[IssueDescriptor::new(2, "reserved")]));
        assert!(!issues_valid(&[IssueDescriptor::new(0, "zero")]));
    }

    #[test]
    fn actions_have_unique_ids_and_names_and_valid_fields() {
        const ARGS: &[FieldDescriptor] = &[FieldDescriptor::new(1, "n", ValueKind::U64)];
        const DUPLICATE_ARGS: &[FieldDescriptor] = &[
            FieldDescriptor::new(1, "n", ValueKind::U64),
            FieldDescriptor::new(1, "m", ValueKind::U64),
        ];
        assert!(actions_valid(&[
            ActionDescriptor::new(1, "scan").args(ARGS),
            ActionDescriptor::new(2, "stop"),
        ]));
        assert!(!actions_valid(&[
            ActionDescriptor::new(1, "scan"),
            ActionDescriptor::new(1, "stop"),
        ]));
        assert!(!actions_valid(&[
            ActionDescriptor::new(1, "scan"),
            ActionDescriptor::new(2, "scan"),
        ]));
        assert!(!actions_valid(&[ActionDescriptor::new(1, "")]));
        assert!(!actions_valid(&[
            ActionDescriptor::new(1, "scan").args(DUPLICATE_ARGS)
        ]));
        assert!(!actions_valid(&[
            ActionDescriptor::new(1, "scan").results(DUPLICATE_ARGS)
        ]));
    }
}

#[test]
fn issue_age_follows_the_set_of_active_issues_only() {
    let probe = Probe::new();
    let registry = Statuses::<NoopRawMutex, _>::new((&probe,));
    let age = |now| block_on(registry.status(Probe::ID, now)).unwrap().issues[0].age_ms;
    let revision = |now| {
        block_on(registry.status(Probe::ID, now))
            .unwrap()
            .summary
            .revision
    };

    let _ = block_on(registry.summary(0, 1_000));
    *probe.issue.borrow_mut() = Some(FIRST_SERVICE_ISSUE);
    assert_eq!(age(2_000), Some(0));
    let before = revision(2_000);

    // The state, health and readiness change while the same issue stays: a
    // new revision, but the issue is as old as ever.
    *probe.state.borrow_mut() = ServiceState::Failed;
    *probe.health.borrow_mut() = ServiceHealth::Error;
    assert_eq!(age(5_000), Some(3_000));
    assert_eq!(revision(5_000), before + 1);
    // The state age is its own clock.
    let summary = block_on(registry.summary(0, 6_000));
    assert_eq!(summary.unwrap().state_age_ms, 1_000);
    assert_eq!(age(6_000), Some(4_000));

    // Another issue replaces it: the set changed, so the age starts over.
    *probe.issue.borrow_mut() = Some(FIRST_SERVICE_ISSUE + 1);
    assert_eq!(age(9_000), Some(0));
    assert_eq!(age(10_000), Some(1_000));

    // Gone and back: a new issue.
    *probe.issue.borrow_mut() = None;
    let _ = block_on(registry.summary(0, 11_000));
    *probe.issue.borrow_mut() = Some(FIRST_SERVICE_ISSUE + 1);
    assert_eq!(age(12_000), Some(0));
}

/// A codec that fails the test when a secret is converted to a value.
struct Secret;

impl Codec<String> for Secret {
    const KIND: ValueKind = ValueKind::String;

    fn encode(_: &String) -> Result<Option<Value>, FieldError> {
        panic!("a secret must never be read")
    }

    fn decode(value: &Value) -> Result<String, FieldError> {
        value
            .as_str()
            .map(String::from)
            .ok_or(FieldError::WrongKind)
    }
}

#[derive(Clone, Debug, Config)]
#[config(name = "vault", overrides = VaultOverrides, version = 1, id = 9)]
struct Vault {
    #[config(id = 1, with = Text)]
    label: String,
    #[config(id = 2, secret, with = Secret)]
    token: Option<String>,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            label: "main".into(),
            token: None,
        }
    }
}

#[derive(Default)]
struct VaultStore(RefCell<Option<VaultOverrides>>);

impl SectionStore<Vault> for &VaultStore {
    fn firmware_overrides(&self) -> VaultOverrides {
        VaultOverrides::default()
    }
    async fn load(&self) -> Result<Option<VaultOverrides>, StoreError> {
        Ok(self.0.borrow().clone())
    }
    async fn store(&self, overrides: &VaultOverrides, _: &Vault) -> Result<(), StoreError> {
        *self.0.borrow_mut() = Some(overrides.clone());
        Ok(())
    }
    async fn remove(&self, _: &Vault) -> Result<(), StoreError> {
        *self.0.borrow_mut() = None;
        Ok(())
    }
}

#[test]
fn reading_a_section_never_reads_a_secret() {
    let store = VaultStore::default();
    let section = Section::<Vault, _, NoopRawMutex>::new(&store);

    // A secret that is set is reported as redacted.
    *store.0.borrow_mut() = Some(VaultOverrides {
        token: rohi_config::Override::Value(Some("hunter2".into())),
        ..VaultOverrides::default()
    });
    let values = block_on(section.get()).unwrap();
    assert_eq!(values.entries.len(), 2);
    assert_eq!(values.entries[1].state, EntryState::Redacted);
    assert_eq!(values.overridden.as_slice(), [2]);

    // An explicit `None` is not set: the secret is absent.
    *store.0.borrow_mut() = Some(VaultOverrides {
        token: rohi_config::Override::Value(None),
        ..VaultOverrides::default()
    });
    let values = block_on(section.get()).unwrap();
    assert_eq!(values.entries.len(), 1);
    assert_eq!(values.entries[0].field_id, 1);
}

#[test]
fn a_patch_never_reads_a_secret_either() {
    let store = VaultStore::default();
    let section = Section::<Vault, _, NoopRawMutex>::new(&store);
    block_on(section.patch(&[set(2, text("hunter2"))], None, false)).unwrap();
    block_on(section.patch(&[clear(2)], None, false)).unwrap();
    assert!(store.0.borrow().is_none());
}
