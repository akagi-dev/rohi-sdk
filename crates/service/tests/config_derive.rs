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

//! `#[derive(Config)]`: the field metadata and the generic field access
//! generated from one declaration, end to end through a [`Section`].

extern crate std;

use std::cell::RefCell;
use std::format;
use std::string::String;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_config::{Codec, Variant, VariantValue};
use rohi_service::{
    Bounds, Config, ConfigError, ConfigField, ConfigSectionOps, ConfigUpdate, EnumDescriptor,
    EnumVariant, FieldError, Mutation, MutationOp, Section, SectionStore, StoreError, Unit, Value,
    ValueKind,
};

// --- codecs ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, minicbor::Encode, minicbor::Decode, minicbor::CborLen)]
#[cbor(index_only)]
enum Mode {
    #[n(0)]
    Fast,
    #[n(1)]
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

/// A domain type: a token is at least four bytes, which no integer or string
/// codec could tell.
#[derive(Clone, PartialEq, minicbor::Encode, minicbor::Decode, minicbor::CborLen)]
#[cbor(transparent)]
struct Token(#[n(0)] std::vec::Vec<u8>);

impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // A type that prints its content: the generated overrides must not.
        write!(f, "Token({:?})", self.0)
    }
}

thread_local! {
    static ENCODED_SECRETS: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

struct TokenCodec;

impl Codec<Token> for TokenCodec {
    const KIND: ValueKind = ValueKind::Bytes;

    fn encode(_: &Token) -> Result<Option<Value>, FieldError> {
        ENCODED_SECRETS.with(|count| count.set(count.get() + 1));
        Err(FieldError::Secret)
    }

    fn decode(value: &Value) -> Result<Token, FieldError> {
        let bytes = value.as_bytes().ok_or(FieldError::WrongKind)?;
        if bytes.len() < 4 {
            return Err(FieldError::Invalid);
        }
        Ok(Token(bytes.to_vec()))
    }
}

/// A label is at most eight bytes.
struct Label;

impl Codec<String> for Label {
    const KIND: ValueKind = ValueKind::String;

    fn encode(value: &String) -> Result<Option<Value>, FieldError> {
        Value::text(value).map(Some).ok_or(FieldError::Invalid)
    }

    fn decode(value: &Value) -> Result<String, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        if text.len() > 8 {
            return Err(FieldError::OutOfRange);
        }
        Ok(text.to_owned())
    }
}

/// Not given is NaN: no value.
struct Coordinate;

impl Codec<f64> for Coordinate {
    const KIND: ValueKind = ValueKind::F32;

    fn encode(value: &f64) -> Result<Option<Value>, FieldError> {
        Ok((!value.is_nan()).then(|| Value::F32(*value as f32)))
    }

    fn decode(value: &Value) -> Result<f64, FieldError> {
        Ok(f64::from(value.as_f32().ok_or(FieldError::WrongKind)?))
    }
}

// --- the config --------------------------------------------------------------

fn check(gadget: &Gadget) -> Result<(), FieldError> {
    if gadget.enabled && gadget.label.is_empty() {
        return Err(FieldError::Invalid);
    }
    Ok(())
}

/// Every kind of field there is.
#[derive(Clone, Debug, Config)]
#[config(name = "gadget", overrides = GadgetOverrides, version = 3, validate = check, enums = MODES)]
struct Gadget {
    #[config(id = 1)]
    enabled: bool,
    #[config(id = 2, with = Label)]
    label: String,
    #[config(id = 3, name = "level", unit = Percent, min = 0, max = 100)]
    level_percent: u8,
    #[config(id = 4)]
    offset: Option<i32>,
    #[config(id = 5, with = Variant, enum_id = 1)]
    mode: Mode,
    #[config(id = 6, secret, with = TokenCodec)]
    token: Option<Token>,
    #[config(id = 7, with = Coordinate, unit = Degrees, min = -90.0, max = 90.0)]
    latitude: f64,
    #[config(id = 8, required)]
    primary: Option<u16>,
    #[config(id = 9, read_only)]
    serial: u32,
}

impl Default for Gadget {
    fn default() -> Self {
        Self {
            enabled: false,
            label: String::new(),
            level_percent: 50,
            offset: None,
            mode: Mode::Fast,
            token: None,
            latitude: f64::NAN,
            primary: Some(1),
            serial: 4711,
        }
    }
}

fn text(value: &str) -> Value {
    Value::text(value).unwrap()
}

fn field(id: u32) -> &'static ConfigField {
    Gadget::FIELDS.iter().find(|f| f.field.id == id).unwrap()
}

#[test]
fn the_metadata_is_generated_from_the_declaration() {
    assert_eq!(Gadget::NAME, "gadget");
    let ids: std::vec::Vec<_> = Gadget::FIELDS.iter().map(|f| f.field.id).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5, 6, 7, 8, 9]);

    let level = field(3);
    // The public name replaces the identifier.
    assert_eq!(level.field.name, "level");
    assert_eq!(level.field.kind, ValueKind::U64);
    assert_eq!(level.field.unit, Unit::Percent);
    assert_eq!(level.bounds, Some(Bounds::U64(0, 100)));
    assert!(level.writable && !level.secret && !level.optional);

    assert_eq!(field(1).field.kind, ValueKind::Bool);
    assert_eq!(field(2).field.kind, ValueKind::String);
    assert_eq!(field(2).field.name, "label");
    assert_eq!(field(4).field.kind, ValueKind::I64);
    assert!(field(4).optional);
    assert_eq!(field(5).field.kind, ValueKind::Enum);
    assert_eq!(field(5).field.enum_id, 1);
    assert_eq!(Gadget::ENUMS, MODES);
    assert!(field(6).secret && field(6).optional);
    assert_eq!(field(7).bounds, Some(Bounds::F32(-90.0, 90.0)));
    assert_eq!(field(7).field.unit, Unit::Degrees);
    // An `Option` that is `required`.
    assert!(!field(8).optional);
    assert!(!field(9).writable);
}

#[test]
fn primitive_values_are_converted_and_checked() {
    let mut gadget = Gadget::default();
    assert_eq!(gadget.read_field(1), Ok(Some(Value::Bool(false))));
    gadget.set_field(1, Value::Bool(true)).unwrap();
    // The invalid whole is only caught by the validation.
    assert_eq!(gadget.validate(), Err(FieldError::Invalid));
    gadget.set_field(2, text("abc")).unwrap();
    assert_eq!(gadget.read_field(2), Ok(Some(text("abc"))));
    assert_eq!(gadget.validate(), Ok(()));

    gadget.set_field(3, Value::U64(80)).unwrap();
    assert_eq!(gadget.level_percent, 80);
    assert_eq!(gadget.read_field(3), Ok(Some(Value::U64(80))));
    // A value the type cannot hold is refused, never truncated.
    assert_eq!(
        gadget.set_field(3, Value::U64(300)),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(gadget.level_percent, 80);
    assert_eq!(
        gadget.set_field(2, text("too long for it")),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(gadget.set_field(3, text("1")), Err(FieldError::WrongKind));

    gadget.set_field(4, Value::I64(-5)).unwrap();
    assert_eq!(gadget.offset, Some(-5));
    assert_eq!(
        gadget.set_field(4, Value::I64(i64::MAX)),
        Err(FieldError::OutOfRange)
    );
}

#[test]
fn an_unknown_field_is_unknown() {
    let mut gadget = Gadget::default();
    assert_eq!(gadget.read_field(99), Err(FieldError::UnknownField));
    assert_eq!(gadget.field_is_set(99), Err(FieldError::UnknownField));
    assert_eq!(
        gadget.set_field(99, Value::Bool(true)),
        Err(FieldError::UnknownField)
    );
    assert_eq!(gadget.set_none(99), Err(FieldError::UnknownField));
}

#[test]
fn optional_fields_can_be_unset_and_required_ones_cannot() {
    let mut gadget = Gadget::default();
    assert_eq!(gadget.read_field(4), Ok(None));
    assert_eq!(gadget.field_is_set(4), Ok(false));
    gadget.set_field(4, Value::I64(3)).unwrap();
    assert_eq!(gadget.field_is_set(4), Ok(true));
    gadget.set_none(4).unwrap();
    assert_eq!(gadget.offset, None);

    // A plain field and a `required` `Option` have no "no value".
    assert_eq!(gadget.set_none(1), Err(FieldError::NotOptional));
    assert_eq!(gadget.set_none(2), Err(FieldError::NotOptional));
    assert_eq!(gadget.set_none(8), Err(FieldError::NotOptional));
    assert_eq!(gadget.primary, Some(1));
    gadget.set_field(8, Value::U64(7)).unwrap();
    assert_eq!(gadget.read_field(8), Ok(Some(Value::U64(7))));
}

#[test]
fn a_custom_codec_decides_what_a_type_means() {
    let mut gadget = Gadget::default();
    gadget.set_field(5, Value::Enum(1)).unwrap();
    assert_eq!(gadget.mode, Mode::Slow);
    assert_eq!(gadget.read_field(5), Ok(Some(Value::Enum(1))));
    assert_eq!(
        gadget.set_field(5, Value::Enum(7)),
        Err(FieldError::OutOfRange)
    );
    assert_eq!(gadget.mode, Mode::Slow);

    // The codec may say "no value" for a field that is not an `Option`.
    assert_eq!(gadget.read_field(7), Ok(None));
    assert_eq!(gadget.field_is_set(7), Ok(false));
    gadget.set_field(7, Value::F32(52.5)).unwrap();
    assert_eq!(gadget.read_field(7), Ok(Some(Value::F32(52.5))));
    assert_eq!(gadget.field_is_set(7), Ok(true));
}

#[test]
fn a_read_only_field_is_shown_and_never_written() {
    let mut gadget = Gadget::default();
    assert_eq!(gadget.read_field(9), Ok(Some(Value::U64(4711))));
    assert_eq!(
        gadget.set_field(9, Value::U64(1)),
        Err(FieldError::ReadOnly)
    );
    assert_eq!(gadget.set_none(9), Err(FieldError::ReadOnly));
    assert_eq!(gadget.serial, 4711);
    // It is not persisted either.
    assert_eq!(
        <<Gadget as rohi_config::HasOverrides>::Overrides as rohi_config::ConfigOverrides<
            Gadget,
        >>::FIELD_IDS,
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
}

// --- secrets -----------------------------------------------------------------

#[test]
fn a_secret_is_never_converted_to_a_value() {
    let mut gadget = Gadget::default();
    ENCODED_SECRETS.with(|count| count.set(0));
    assert_eq!(gadget.field_is_set(6), Ok(false));
    gadget
        .set_field(6, Value::bytes(&[1, 2, 3, 4]).unwrap())
        .unwrap();
    assert_eq!(gadget.field_is_set(6), Ok(true));
    // Neither reading nor asking whether it is set produces its value.
    assert_eq!(gadget.read_field(6), Err(FieldError::Secret));
    assert_eq!(ENCODED_SECRETS.with(core::cell::Cell::get), 0);
    // A value the type rejects does not replace the secret.
    assert_eq!(
        gadget.set_field(6, Value::bytes(&[9]).unwrap()),
        Err(FieldError::Invalid)
    );
    assert_eq!(gadget.token, Some(Token(std::vec![1, 2, 3, 4])));
    gadget.set_none(6).unwrap();
    assert_eq!(gadget.field_is_set(6), Ok(false));
}

#[test]
fn the_generated_debug_of_the_overrides_hides_a_secret() {
    let overrides = GadgetOverrides {
        token: rohi_config::Override::Value(Some(Token(std::vec![7, 7, 7, 7]))),
        label: rohi_config::Override::Value("shown".to_owned()),
        ..GadgetOverrides::default()
    };
    let shown = format!("{overrides:?}");
    assert!(shown.contains("GadgetOverrides"), "{shown}");
    assert!(shown.contains("shown"), "{shown}");
    assert!(shown.contains("token: Value(<redacted>)"), "{shown}");
    assert!(!shown.contains("7, 7"), "{shown}");
    let pretty = format!("{overrides:#?}");
    assert!(!pretty.contains("Token"), "{pretty}");
}

// --- sections ----------------------------------------------------------------

#[derive(Default)]
struct Store {
    value: RefCell<Option<GadgetOverrides>>,
    published: RefCell<Option<Gadget>>,
    firmware: RefCell<GadgetOverrides>,
}

impl SectionStore<Gadget> for &Store {
    fn firmware_overrides(&self) -> GadgetOverrides {
        self.firmware.borrow().clone()
    }

    async fn load(&self) -> Result<Option<GadgetOverrides>, StoreError> {
        Ok(self.value.borrow().clone())
    }

    async fn store(
        &self,
        overrides: &GadgetOverrides,
        effective: &Gadget,
    ) -> Result<(), StoreError> {
        *self.value.borrow_mut() = Some(overrides.clone());
        *self.published.borrow_mut() = Some(effective.clone());
        Ok(())
    }

    async fn remove(&self, effective: &Gadget) -> Result<(), StoreError> {
        *self.value.borrow_mut() = None;
        *self.published.borrow_mut() = Some(effective.clone());
        Ok(())
    }
}

fn set(id: u32, value: Value) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::Set(value),
    }
}

fn op(id: u32, op: MutationOp) -> Mutation {
    Mutation { field_id: id, op }
}

#[test]
fn a_section_reads_patches_and_resets_the_generated_config() {
    let store = Store::default();
    let section = Section::<Gadget, _, NoopRawMutex>::new(&store);

    let read = block_on(section.get()).unwrap();
    assert_eq!(read.section.name, "gadget");
    assert_eq!(read.section.version, 3);
    // The effective configuration: the defaults. The unset latitude and the
    // unset secret have no entry.
    let ids: std::vec::Vec<_> = read.entries.iter().map(|e| e.field_id).collect();
    assert_eq!(ids, [1, 2, 3, 5, 8, 9]);
    assert!(read.overridden.is_empty());

    block_on(section.patch(
        &[
            set(2, text("lamp")),
            set(1, Value::Bool(true)),
            set(6, Value::bytes(&[1, 2, 3, 4]).unwrap()),
            set(7, Value::F32(10.5)),
        ],
        false,
    ))
    .unwrap();
    let read = block_on(section.get()).unwrap();
    assert_eq!(read.overridden.as_slice(), [1, 2, 6, 7]);
    // The secret is redacted, the rest is readable.
    let entry = |id| read.entries.iter().find(|e| e.field_id == id).unwrap();
    assert_eq!(entry(6).state, rohi_service::EntryState::Redacted);
    assert_eq!(
        entry(2).state,
        rohi_service::EntryState::Value(text("lamp"))
    );
    // The runtime got the validated effective value.
    let published = store.published.borrow().clone().unwrap();
    assert_eq!(published.label.as_str(), "lamp");
    assert!(published.enabled);
    assert_eq!(published.latitude, 10.5);

    // Only what the user set is persisted.
    let stored = store.value.borrow().clone().unwrap();
    assert!(stored.level_percent.as_value().is_none());

    // The whole effective result is validated: disabling nothing but
    // clearing the label while enabled is refused and changes nothing.
    assert_eq!(
        block_on(section.patch(&[op(2, MutationOp::Clear)], false)),
        Err(ConfigError::Field(FieldError::Invalid))
    );
    assert!(block_on(section.get()).unwrap().overridden.contains(&2));

    // Explicit "no value" differs from clearing the override.
    block_on(section.patch(&[op(4, MutationOp::None)], false)).unwrap();
    assert!(block_on(section.get()).unwrap().overridden.contains(&4));
    block_on(section.patch(&[op(4, MutationOp::Clear)], false)).unwrap();
    assert!(!block_on(section.get()).unwrap().overridden.contains(&4));

    // Writes the schema does not allow.
    assert_eq!(
        block_on(section.patch(&[set(9, Value::U64(1))], false)),
        Err(ConfigError::Field(FieldError::ReadOnly))
    );
    assert_eq!(
        block_on(section.patch(&[op(8, MutationOp::None)], false)),
        Err(ConfigError::Field(FieldError::NotOptional))
    );
    assert_eq!(
        block_on(section.patch(&[set(3, Value::U64(101))], false)),
        Err(ConfigError::Field(FieldError::OutOfRange))
    );

    block_on(section.reset(false)).unwrap();
    assert!(store.value.borrow().is_none());
    assert!(block_on(section.get()).unwrap().overridden.is_empty());
    assert!(!store.published.borrow().clone().unwrap().enabled);
}

#[test]
fn a_service_updates_the_same_section_as_the_client() {
    let store = Store::default();
    let section = Section::<Gadget, _, NoopRawMutex>::new(&store);
    let done = block_on(section.update(|overrides| {
        overrides.level_percent = rohi_config::Override::Value(10);
    }))
    .unwrap();
    assert!(!done.restart_required);
    block_on(section.patch(&[set(1, Value::Bool(false))], false)).unwrap();
    let stored = store.value.borrow().clone().unwrap();
    assert_eq!(stored.level_percent.as_value(), Some(&10));
}

#[test]
fn firmware_overrides_sit_between_the_defaults_and_the_user() {
    let store = Store::default();
    store.firmware.borrow_mut().level_percent = rohi_config::Override::Value(70);
    let section = Section::<Gadget, _, NoopRawMutex>::new(&store);
    let level = |section: &Section<Gadget, &Store, NoopRawMutex>| {
        block_on(section.get())
            .unwrap()
            .entries
            .iter()
            .find(|e| e.field_id == 3)
            .unwrap()
            .state
            .clone()
    };
    assert_eq!(
        level(&section),
        rohi_service::EntryState::Value(Value::U64(70))
    );
    block_on(section.patch(&[set(3, Value::U64(20))], false)).unwrap();
    assert_eq!(
        level(&section),
        rohi_service::EntryState::Value(Value::U64(20))
    );
    block_on(section.reset(false)).unwrap();
    assert_eq!(
        level(&section),
        rohi_service::EntryState::Value(Value::U64(70))
    );
}

#[test]
fn the_persisted_encoding_is_keyed_by_the_field_ids() {
    let overrides = GadgetOverrides {
        level_percent: rohi_config::Override::Value(10),
        offset: rohi_config::Override::Value(None),
        mode: rohi_config::Override::Value(Mode::Slow),
        ..GadgetOverrides::default()
    };
    let mut buffer = [0u8; 32];
    let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
    // {3: 10, 4: null, 5: ...}: the mode type has to be encodable itself.
    assert_eq!(buffer[0], 0xa3);
    assert_eq!(&buffer[1..5], &[0x03, 0x0a, 0x04, 0xf6]);
    assert_eq!(buffer[5], 0x05);
    assert!(used > 6);
    assert_eq!(<GadgetOverrides as rohi_config::ConfigSection>::VERSION, 3);
    assert_eq!(
        <GadgetOverrides as rohi_config::ConfigSection>::ID,
        rohi_config::SectionId::from_name("Gadget")
    );
}
