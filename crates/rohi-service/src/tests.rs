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
use core::cell::RefCell;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use crate::*;

#[derive(Clone, Debug, PartialEq)]
struct Demo {
    enabled: bool,
    interval_s: u64,
    host: heapless::String<16>,
    token: Option<heapless::String<16>>,
}

const DEMO_FIELDS: &[ConfigField] = &[
    ConfigField::new(1, "enabled", ValueKind::Bool),
    ConfigField::new(2, "interval_s", ValueKind::U64)
        .range(10.0, 100.0)
        .unit("s"),
    ConfigField::new(3, "host", ValueKind::String),
    ConfigField::new(4, "token", ValueKind::String)
        .secret()
        .optional(),
    ConfigField::new(5, "mode", ValueKind::Enum)
        .variants(&["fast", "slow"])
        .read_only(),
];

impl ManagedConfig for Demo {
    const NAME: &'static str = "demo";
    const VERSION: u16 = 3;
    const FIELDS: &'static [ConfigField] = DEMO_FIELDS;

    fn defaults() -> Self {
        Self {
            enabled: false,
            interval_s: 60,
            host: heapless::String::new(),
            token: None,
        }
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        Ok(match id {
            1 => Some(Value::Bool(self.enabled)),
            2 => Some(Value::U64(self.interval_s)),
            3 => Some(Value::text(&self.host).unwrap()),
            4 => self.token.as_deref().and_then(Value::text),
            5 => Some(Value::variant("fast").unwrap()),
            _ => return Err(FieldError::UnknownField),
        })
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match (id, value) {
            (1, Value::Bool(v)) => self.enabled = v,
            (2, Value::U64(v)) => self.interval_s = v,
            (3, Value::String(v)) => {
                self.host =
                    heapless::String::try_from(v.as_str()).map_err(|_| FieldError::Invalid)?
            }
            (4, Value::String(v)) => {
                self.token =
                    Some(heapless::String::try_from(v.as_str()).map_err(|_| FieldError::Invalid)?)
            }
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            4 => {
                self.token = None;
                Ok(())
            }
            _ => Err(FieldError::NotOptional),
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        if self.enabled && self.host.is_empty() {
            return Err(FieldError::Invalid);
        }
        Ok(())
    }
}

#[derive(Default)]
struct MemoryStore {
    value: RefCell<Option<Demo>>,
    writes: RefCell<u32>,
    unavailable: RefCell<bool>,
}

impl SectionStore<Demo> for &MemoryStore {
    async fn load(&self) -> Result<Option<Demo>, StoreError> {
        // Lets another task run between a load and the following store.
        embassy_futures::yield_now().await;
        if *self.unavailable.borrow() {
            return Err(StoreError::Unavailable);
        }
        Ok(self.value.borrow().clone())
    }

    async fn store(&self, config: &Demo) -> Result<(), StoreError> {
        *self.value.borrow_mut() = Some(config.clone());
        *self.writes.borrow_mut() += 1;
        Ok(())
    }

    async fn remove(&self) -> Result<(), StoreError> {
        *self.value.borrow_mut() = None;
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

fn unset(id: u32) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::Unset,
    }
}

fn section(store: &MemoryStore) -> ManagedSection<Demo, &MemoryStore, NoopRawMutex> {
    ManagedSection::new(store)
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
    let stored = store.value.borrow().clone().unwrap();
    assert!(stored.enabled);
    assert_eq!(stored.host.as_str(), "a.io");
    assert_eq!(stored.interval_s, 30);
}

#[test]
fn a_patch_keeps_untouched_fields() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io")), set(2, Value::U64(20))], None, false)).unwrap();
    block_on(section.patch(&[set(3, text("b.io"))], None, false)).unwrap();
    let stored = store.value.borrow().clone().unwrap();
    assert_eq!(stored.host.as_str(), "b.io");
    assert_eq!(stored.interval_s, 20);
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
fn the_complete_result_is_validated() {
    let store = MemoryStore::default();
    let section = section(&store);
    // `enabled` needs a host: a cross-field rule of the typed section.
    assert_eq!(
        block_on(section.patch(&[set(1, Value::Bool(true))], None, false)),
        Err(ConfigError::Field(FieldError::Invalid))
    );
    assert!(store.value.borrow().is_none());
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
        patch(set(5, Value::variant("fast").unwrap())),
        Err(FieldError::ReadOnly.into())
    );
    assert_eq!(patch(unset(3)), Err(FieldError::NotOptional.into()));
    assert_eq!(patch(unset(99)), Err(FieldError::UnknownField.into()));
    assert!(store.value.borrow().is_none());
}

#[test]
fn unset_is_distinct_from_an_empty_value() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io")), set(4, text(""))], None, false)).unwrap();
    assert_eq!(
        store.value.borrow().clone().unwrap().token.as_deref(),
        Some("")
    );
    block_on(section.patch(&[unset(4)], None, false)).unwrap();
    assert_eq!(store.value.borrow().clone().unwrap().token, None);
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
    block_on(section.reset(true)).unwrap();
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
    assert_eq!(store.value.borrow().clone().unwrap().host.as_str(), "a.io");
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
    let stored = store.value.borrow().clone().unwrap();
    assert_eq!(stored.host.as_str(), "b.io");
    assert_eq!(stored.token.as_deref(), Some("t0ken"));
}

#[test]
fn get_redacts_secrets_and_omits_unset_fields() {
    let store = MemoryStore::default();
    let section = section(&store);
    let empty = block_on(section.get()).unwrap();
    assert!(!empty.configured);
    assert!(empty.entries.is_empty());

    block_on(section.patch(&[set(3, text("a.io")), set(4, text("s3cret"))], None, false)).unwrap();
    let values = block_on(section.get()).unwrap();
    assert!(values.configured);
    let state = |id| {
        values
            .entries
            .iter()
            .find(|entry| entry.field_id == id)
            .map(|entry| entry.state.clone())
    };
    assert_eq!(state(3), Some(EntryState::Value(text("a.io"))));
    assert_eq!(state(4), Some(EntryState::Redacted));
    assert!(!std::format!("{values:?}").contains("s3cret"));

    block_on(section.patch(&[unset(4)], None, false)).unwrap();
    assert_eq!(
        block_on(section.get())
            .unwrap()
            .entries
            .iter()
            .find(|e| e.field_id == 4),
        None
    );
}

#[test]
fn reset_removes_the_persisted_section() {
    let store = MemoryStore::default();
    let section = section(&store);
    block_on(section.patch(&[set(3, text("a.io"))], None, false)).unwrap();
    let patched = block_on(section.reset(false)).unwrap();
    assert_eq!(patched.revision, 2);
    assert!(store.value.borrow().is_none());
    assert!(!block_on(section.get()).unwrap().configured);
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
    let b = MemoryStore::default();

    #[derive(Clone)]
    struct Other(Demo);
    // A second section type with another name.
    impl ManagedConfig for Other {
        const NAME: &'static str = "other";
        const VERSION: u16 = 1;
        const FIELDS: &'static [ConfigField] = DEMO_FIELDS;
        fn defaults() -> Self {
            Self(Demo::defaults())
        }
        fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
            self.0.read_field(id)
        }
        fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
            self.0.set_field(id, value)
        }
        fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
            self.0.unset_field(id)
        }
        fn validate(&self) -> Result<(), FieldError> {
            self.0.validate()
        }
    }
    #[derive(Default)]
    struct OtherStore(RefCell<Option<Demo>>);
    impl SectionStore<Other> for &OtherStore {
        async fn load(&self) -> Result<Option<Other>, StoreError> {
            Ok(self.0.borrow().clone().map(Other))
        }
        async fn store(&self, config: &Other) -> Result<(), StoreError> {
            *self.0.borrow_mut() = Some(config.0.clone());
            Ok(())
        }
        async fn remove(&self) -> Result<(), StoreError> {
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
        ManagedSection::<Other, _, NoopRawMutex>::new(&other_store),
    );
    let _ = &b;
    assert_eq!(registry.section_count(), 2);
    assert_eq!(registry.section_info(0).unwrap().name, "demo");
    assert_eq!(registry.section_info(1).unwrap().name, "other");
    assert_eq!(registry.section_info(2), None);
    assert!(registry.schema("other").is_some());
    assert!(registry.schema("missing").is_none());

    let patched = block_on(registry.patch("other", &[set(3, text("x.io"))], None, false)).unwrap();
    assert!(patched.restart_required);
    assert!(other_store.0.borrow().is_some());
    assert!(a.value.borrow().is_none());
    assert_eq!(
        block_on(registry.get("missing")),
        Err(ConfigError::UnknownSection)
    );
    assert_eq!(
        block_on(().get("anything")),
        Err(ConfigError::UnknownSection)
    );
    block_on(registry.reset("other", false)).unwrap();
    assert!(other_store.0.borrow().is_none());
}

#[test]
fn check_enforces_kind_variants_and_range() {
    let field = ConfigField::new(1, "n", ValueKind::U64).range(1.0, 10.0);
    assert_eq!(field.check(&Value::U64(5)), Ok(()));
    assert_eq!(field.check(&Value::U64(0)), Err(FieldError::OutOfRange));
    assert_eq!(field.check(&Value::U64(11)), Err(FieldError::OutOfRange));
    assert_eq!(field.check(&Value::I64(5)), Err(FieldError::WrongKind));
    let float = ConfigField::new(1, "f", ValueKind::F64).range(0.0, 1.0);
    assert_eq!(
        float.check(&Value::F64(f64::NAN)),
        Err(FieldError::OutOfRange)
    );
    let variant = ConfigField::new(1, "e", ValueKind::Enum).variants(&["a", "b"]);
    assert_eq!(variant.check(&Value::variant("a").unwrap()), Ok(()));
    assert_eq!(
        variant.check(&Value::variant("c").unwrap()),
        Err(FieldError::OutOfRange)
    );
}

#[test]
fn value_debug_never_shows_the_content() {
    assert_eq!(std::format!("{:?}", text("hunter2")), "Value(String)");
}

// --- status -----------------------------------------------------------------

struct Probe {
    state: RefCell<ServiceState>,
    health: RefCell<ServiceHealth>,
    issue: RefCell<Option<&'static str>>,
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

impl ManagedService for &Probe {
    const NAME: &'static str = "probe";
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "endpoint", ValueKind::String),
        FieldDescriptor::new(2, "queue_depth", ValueKind::U64),
        FieldDescriptor::new(3, "mode", ValueKind::Enum).variants(&["a", "b"]),
    ];

    async fn summary(&self) -> ServiceSummary {
        let health = *self.health.borrow();
        ServiceSummary::new(*self.state.borrow(), health, health == ServiceHealth::Ok)
    }

    async fn issues(&self, out: &mut heapless::Vec<Issue, MAX_ISSUES>) {
        if let Some(code) = *self.issue.borrow() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code,
                message: "something is wrong",
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.text(1, "ingress.example.org:443")?;
        out.u64(2, *self.queue.borrow())?;
        out.variant(3, "b")
    }
}

#[test]
fn the_status_revision_follows_observable_changes_only() {
    let probe = Probe::new();
    let service = Tracked::<_, NoopRawMutex>::new(&probe);

    let first = block_on(service.summary(1_000));
    assert_eq!(first.revision, 1);
    assert_eq!(first.name, "probe");
    assert!(first.essential);

    // A service-specific value is not observable state.
    *probe.queue.borrow_mut() = 17;
    assert_eq!(block_on(service.summary(2_000)).revision, 1);

    *probe.health.borrow_mut() = ServiceHealth::Degraded;
    *probe.issue.borrow_mut() = Some("connection_timeout");
    let degraded = block_on(service.summary(3_000));
    assert_eq!(degraded.revision, 2);
    assert_eq!(degraded.reason, Some("connection_timeout"));

    assert_eq!(block_on(service.summary(4_000)).revision, 2);
    *probe.issue.borrow_mut() = None;
    *probe.health.borrow_mut() = ServiceHealth::Ok;
    assert_eq!(block_on(service.summary(5_000)).revision, 3);
}

#[test]
fn state_age_counts_from_the_last_state_change() {
    let probe = Probe::new();
    let service = Tracked::<_, NoopRawMutex>::new(&probe);
    assert_eq!(block_on(service.summary(1_000)).state_age_ms, 0);
    assert_eq!(block_on(service.summary(4_000)).state_age_ms, 3_000);
    *probe.state.borrow_mut() = ServiceState::Failed;
    assert_eq!(block_on(service.summary(5_000)).state_age_ms, 0);
    assert_eq!(block_on(service.summary(6_500)).state_age_ms, 1_500);
}

#[test]
fn detailed_status_has_fields_and_active_issues_with_age() {
    let probe = Probe::new();
    *probe.queue.borrow_mut() = 17;
    let registry = (Tracked::<_, NoopRawMutex>::new(&probe),);
    *probe.issue.borrow_mut() = Some("connection_timeout");
    let _ = block_on(registry.summary(0, 1_000));
    let status = block_on(registry.status("probe", 35_000)).unwrap();
    assert_eq!(status.issues.len(), 1);
    assert_eq!(status.issues[0].code, "connection_timeout");
    assert_eq!(status.issues[0].age_ms, Some(34_000));
    assert_eq!(status.fields.len(), 3);
    assert_eq!(status.fields[0], (1, text("ingress.example.org:443")));
    assert_eq!(status.fields[1], (2, Value::U64(17)));
    assert_eq!(status.fields[2], (3, Value::variant("b").unwrap()));
    assert_eq!(
        block_on(registry.status("missing", 0)),
        Err(StatusError::UnknownService)
    );
    let (info, schema) = registry.schema("probe").unwrap();
    assert_eq!((info.name, info.version), ("probe", 2));
    assert_eq!(schema.len(), 3);
}

#[test]
fn the_writer_checks_the_schema() {
    let mut writer = status::Collector {
        schema: <&Probe as ManagedService>::STATUS_SCHEMA,
        fields: heapless::Vec::new(),
    };
    assert_eq!(writer.u64(99, 1), Err(WriteError::UnknownField));
    assert_eq!(writer.u64(1, 1), Err(WriteError::WrongKind));
    assert_eq!(writer.variant(3, "c"), Err(WriteError::WrongKind));
    assert_eq!(writer.bytes(1, &[0; 65]), Err(WriteError::TooLong));
    // Text is cut at a character boundary.
    let long = "é".repeat(40);
    writer.text(1, &long).unwrap();
    assert_eq!(writer.fields[0].1.as_str().unwrap().len(), 64);
}

fn summary(
    state: ServiceState,
    health: ServiceHealth,
    ready: bool,
    essential: bool,
) -> ServiceStatusSummary {
    ServiceStatusSummary {
        name: "s",
        state,
        health,
        ready,
        revision: 1,
        state_age_ms: 0,
        reason: None,
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

struct Scanner;

impl ManagedService for Scanner {
    const NAME: &'static str = "scanner";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new("scan")
            .args(&[FieldDescriptor::new(1, "limit", ValueKind::U64)])
            .results(&[
                FieldDescriptor::new(1, "name", ValueKind::String),
                FieldDescriptor::new(2, "rssi", ValueKind::I64).unit("dBm"),
            ]),
        ActionDescriptor::new("reset"),
        ActionDescriptor::new("provision")
            .args(&[FieldDescriptor::new(1, "secret", ValueKind::Bytes)])
            .secret(),
    ];

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, _: &mut impl StatusWriter) -> Result<(), WriteError> {
        Ok(())
    }

    async fn invoke(
        &self,
        action: &str,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            "scan" => {
                let limit = args[0].1.as_u64().ok_or(ActionError::InvalidArgument)?;
                for index in 0..limit {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    out.text(1, "net").map_err(|_| ActionError::Internal)?;
                    out.i64(2, -(index as i64))
                        .map_err(|_| ActionError::Internal)?;
                }
                Ok(())
            }
            "reset" => Err(ActionError::Rejected("busy_resetting")),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

fn scanner() -> (Tracked<Scanner, NoopRawMutex>,) {
    (Tracked::new(Scanner),)
}

#[test]
fn an_action_returns_records() {
    let registry = scanner();
    let records = block_on(registry.invoke("scanner", "scan", &[(1, Value::U64(3))])).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1][0], (1, text("net")));
    assert_eq!(records[2][1], (2, Value::I64(-2)));

    let none = block_on(registry.invoke("scanner", "scan", &[(1, Value::U64(0))])).unwrap();
    assert!(none.is_empty());
}

#[test]
fn too_many_records_fail_instead_of_truncating() {
    let registry = scanner();
    let result = block_on(registry.invoke("scanner", "scan", &[(1, Value::U64(7))]));
    assert_eq!(result, Err(ActionError::Internal));
}

#[test]
fn arguments_are_checked_against_the_descriptor() {
    let registry = scanner();
    let call = |args: &[(u32, Value)]| block_on(registry.invoke("scanner", "scan", args));
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
        block_on(registry.invoke("missing", "scan", &[])),
        Err(ActionError::UnknownService)
    );
    assert_eq!(
        block_on(registry.invoke("scanner", "missing", &[])),
        Err(ActionError::UnknownAction)
    );
    assert_eq!(
        block_on(registry.invoke("scanner", "reset", &[])),
        Err(ActionError::Rejected("busy_resetting"))
    );
}

#[test]
fn action_metadata_is_exposed() {
    let registry = scanner();
    let actions = registry.service_actions(0);
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
