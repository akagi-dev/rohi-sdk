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
use core::cell::{Cell, RefCell};

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use heapless::{String, Vec};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_service::{
    Action, ActionDescriptor, ActionError, ActionOutput, Bounds, ConfigField, EnumDescriptor,
    EnumVariant, FIRST_SERVICE_ISSUE, FieldDescriptor, FieldError, Issue, IssueDescriptor,
    IssueSeverity, ManagedConfig, ManagedSection, SectionStore, Service, ServiceHealth,
    ServiceState, ServiceSummary, Status, StatusWriter, Statuses, StoreError, Unit, Value,
    ValueKind, WriteError,
};

use crate::*;

#[derive(Clone, Debug, PartialEq, rohi_config::ConfigOverrides)]
#[config(overrides = NetOverrides, version = 2, id = 1)]
struct Net {
    #[config(id = 1)]
    enabled: bool,
    #[config(id = 2)]
    port: u64,
    #[config(id = 3)]
    host: std::string::String,
    #[config(id = 4)]
    token: Option<std::string::String>,
    #[config(id = 5)]
    mode: Option<u32>,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 80,
            host: std::string::String::new(),
            token: None,
            mode: None,
        }
    }
}

impl ManagedConfig for Net {
    const NAME: &'static str = "net";
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(1, "enabled", ValueKind::Bool),
        ConfigField::new(2, "port", ValueKind::U64).bounds(Bounds::U64(1, 65535)),
        ConfigField::new(3, "host", ValueKind::String),
        ConfigField::new(4, "token", ValueKind::String)
            .secret()
            .optional(),
        ConfigField::new(5, "mode", ValueKind::Enum)
            .with_enum(1)
            .optional(),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        1,
        &[EnumVariant::new(0, "auto"), EnumVariant::new(5, "manual")],
    )];

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        Ok(match id {
            1 => Some(Value::Bool(self.enabled)),
            2 => Some(Value::U64(self.port)),
            3 => Value::text(&self.host),
            4 => self.token.as_deref().and_then(Value::text),
            5 => self.mode.map(Value::Enum),
            _ => return Err(FieldError::UnknownField),
        })
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            1 => self.enabled = value.as_bool().unwrap(),
            2 => self.port = value.as_u64().unwrap(),
            3 => self.host = value.as_str().unwrap().into(),
            4 => self.token = Some(value.as_str().unwrap().into()),
            5 => self.mode = value.as_enum(),
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn set_none(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            4 => self.token = None,
            5 => self.mode = None,
            _ => return Err(FieldError::NotOptional),
        }
        Ok(())
    }

    fn field_is_set(&self, id: u32) -> Result<bool, FieldError> {
        match id {
            4 => Ok(self.token.is_some()),
            _ => Ok(self.read_field(id)?.is_some()),
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
struct Memory(RefCell<Option<NetOverrides>>);

impl SectionStore<Net> for &Memory {
    fn firmware_overrides(&self) -> NetOverrides {
        NetOverrides::default()
    }

    async fn load(&self) -> Result<Option<NetOverrides>, StoreError> {
        Ok(self.0.borrow().clone())
    }

    async fn store(&self, overrides: &NetOverrides, _: &Net) -> Result<(), StoreError> {
        *self.0.borrow_mut() = Some(overrides.clone());
        Ok(())
    }

    async fn remove(&self, _: &Net) -> Result<(), StoreError> {
        *self.0.borrow_mut() = None;
        Ok(())
    }
}

#[derive(Service)]
struct Radio {
    degraded: RefCell<bool>,
    log: RefCell<std::vec::Vec<std::string::String>>,
}

impl Status for Radio {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "ssid", ValueKind::String),
        FieldDescriptor::new(2, "rssi", ValueKind::I64).unit(Unit::Dbm),
        FieldDescriptor::new(3, "band", ValueKind::Enum).with_enum(1),
        FieldDescriptor::new(4, "gain", ValueKind::F32),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        1,
        &[EnumVariant::new(2, "2.4ghz"), EnumVariant::new(5, "5ghz")],
    )];
    const ISSUES: &'static [IssueDescriptor] =
        &[IssueDescriptor::new(FIRST_SERVICE_ISSUE, "link_lost")];
    async fn summary(&self) -> ServiceSummary {
        if *self.degraded.borrow() {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Degraded, false)
        } else {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, { rohi_service::MAX_ISSUES }>) {
        if *self.degraded.borrow() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code: FIRST_SERVICE_ISSUE,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.text(1, "office")?;
        out.i64(2, -61)?;
        out.variant(3, 5)?;
        out.f32(4, 1.5)
    }
}

impl Action for Radio {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "scan").results(&[FieldDescriptor::new(
            1,
            "ssid",
            ValueKind::String,
        )]),
        ActionDescriptor::new(2, "reconnect").privileged(),
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
        self.log.borrow_mut().push(std::format!("{action}"));
        match action {
            1 => {
                for name in ["a", "b"] {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    out.text(1, name).map_err(|_| ActionError::Internal)?;
                }
                Ok(())
            }
            2 => Err(ActionError::Rejected("not_configured")),
            3 => {
                assert_eq!(args[0].1.as_bytes().unwrap().len(), 4);
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

type Section<'a> = ManagedSection<Net, &'a Memory, NoopRawMutex>;
type TestDevice<'a> = Device<(Section<'a>,), Statuses<NoopRawMutex, (&'a Radio,)>, (&'a Radio,)>;

const RADIO: u32 = Radio::ID;

const INFO: DeviceInfo = DeviceInfo {
    firmware: "test-0.1.0",
    board: "test-board",
};

fn uptime() -> u64 {
    5_000
}

fn device<'a>(memory: &'a Memory, radio: &'a Radio) -> TestDevice<'a> {
    Device::new(
        INFO,
        uptime,
        (ManagedSection::new(memory),),
        Statuses::new((radio,)),
        (radio,),
    )
}

fn radio() -> Radio {
    Radio {
        degraded: RefCell::new(false),
        log: RefCell::new(std::vec::Vec::new()),
    }
}

const PLAIN: LinkSecurity = LinkSecurity {
    encrypted: false,
    authenticated: false,
};

fn text<const N: usize>(value: &str) -> String<N> {
    String::try_from(value).unwrap()
}

fn call<B: ControlBackend, P: AuthorizationPolicy>(
    controller: &Controller<B, P>,
    link: &LinkSecurity,
    request: Call,
) -> Reply {
    let request = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 42,
        request: Some(request),
    };
    let mut wire = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&request, &mut wire).unwrap();
    let mut out = [0u8; limits::MAX_RESPONSE_LEN];
    let n = block_on(controller.handle(link, &wire[..len], &mut out));
    let response: pb::Response = decode(&out[..n]).unwrap();
    assert_eq!(response.protocol_version, PROTOCOL_VERSION);
    assert_eq!(response.request_id, 42);
    response.response.unwrap()
}

fn error_code(reply: &Reply) -> pb::ErrorCode {
    match reply {
        Reply::Error(error) => error.code,
        _ => panic!("not an error"),
    }
}

fn string_value(value: &str) -> pb::ConfigMutation_::Operation {
    pb::ConfigMutation_::Operation::StringValue(text(value))
}

fn mutation(id: u32, value: pb::ConfigMutation_::Operation) -> pb::ConfigMutation {
    pb::ConfigMutation {
        field_id: id,
        operation: Some(value),
    }
}

fn patch(section: u64, mutations: &[pb::ConfigMutation]) -> pb::PatchConfigSectionRequest {
    let mut list = Vec::new();
    for mutation in mutations {
        list.push(mutation.clone()).ok().unwrap();
    }
    pb::PatchConfigSectionRequest {
        section_id: section,
        mutations: list,
        ..Default::default()
    }
}

macro_rules! controller {
    ($name:ident, $policy:expr) => {
        let memory = Memory::default();
        let radio = radio();
        let $name = Controller::new(device(&memory, &radio), $policy);
    };
}

#[test]
fn info_is_served_without_any_module() {
    let controller = Controller::new(Device::new(INFO, uptime, (), (), ()), OpenPolicy);
    let Reply::GetInfo(info) = call(&controller, &PLAIN, Call::GetInfo(pb::GetInfoRequest {}))
    else {
        panic!()
    };
    assert_eq!(info.firmware.as_str(), "test-0.1.0");
    assert_eq!(info.board.as_str(), "test-board");
    // An empty device lists nothing and knows no section, service or action.
    let Reply::ListConfigSections(sections) = call(
        &controller,
        &PLAIN,
        Call::ListConfigSections(pb::ListConfigSectionsRequest {}),
    ) else {
        panic!()
    };
    assert!(sections.sections.is_empty());
    let get = Call::GetServiceStatus(pb::GetServiceStatusRequest { service_id: RADIO });
    assert_eq!(
        error_code(&call(&controller, &PLAIN, get)),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_malformed_or_foreign_request_is_a_typed_error() {
    controller!(controller, OpenPolicy);
    let mut out = [0u8; 64];
    let n = block_on(controller.handle(&PLAIN, &[0xff, 0xff, 0xff], &mut out));
    let response: pb::Response = decode(&out[..n]).unwrap();
    assert_eq!(response.request_id, 0);
    assert_eq!(
        error_code(&response.response.unwrap()),
        pb::ErrorCode::InvalidArgument
    );

    let wrong_version = pb::Request {
        protocol_version: 99,
        request_id: 3,
        request: Some(Call::GetInfo(pb::GetInfoRequest {})),
    };
    let mut wire = [0u8; 16];
    let len = encode(&wrong_version, &mut wire).unwrap();
    let n = block_on(controller.handle(&PLAIN, &wire[..len], &mut out));
    let response: pb::Response = decode(&out[..n]).unwrap();
    assert_eq!(
        error_code(&response.response.unwrap()),
        pb::ErrorCode::NotSupported
    );
}

#[test]
fn an_undersized_response_buffer_is_answered_with_an_error_that_fits() {
    controller!(controller, OpenPolicy);
    let request = pb::Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 7,
        request: Some(Call::GetInfo(pb::GetInfoRequest {})),
    };
    let mut wire = [0u8; 16];
    let len = encode(&request, &mut wire).unwrap();
    let mut tiny = [0u8; 8];
    let n = block_on(controller.handle(&PLAIN, &wire[..len], &mut tiny));
    let response: pb::Response = decode(&tiny[..n]).unwrap();
    assert_eq!(
        error_code(&response.response.unwrap()),
        pb::ErrorCode::Internal
    );
    let mut none = [0u8; 2];
    assert_eq!(
        block_on(controller.handle(&PLAIN, &wire[..len], &mut none)),
        0
    );
}

#[test]
fn config_sections_are_discovered_and_described() {
    controller!(controller, OpenPolicy);
    let Reply::ListConfigSections(list) = call(
        &controller,
        &PLAIN,
        Call::ListConfigSections(pb::ListConfigSectionsRequest {}),
    ) else {
        panic!()
    };
    assert_eq!(list.sections.len(), 1);
    assert_eq!(list.sections[0].name.as_str(), "net");
    assert_eq!(list.sections[0].version, 2);

    let Reply::GetConfigSchema(schema) = call(
        &controller,
        &PLAIN,
        Call::GetConfigSchema(pb::GetConfigSchemaRequest { section_id: 1 }),
    ) else {
        panic!()
    };
    assert_eq!(schema.section().unwrap().id, 1);
    assert_eq!(schema.fields.len(), 5);
    let port = &schema.fields[1];
    assert_eq!(port.field().unwrap().name.as_str(), "port");
    assert_eq!(port.field().unwrap().kind, pb::ValueKind::U64);
    assert!(schema.fields[3].secret && schema.fields[3].optional);
    // Enums are a table; only enum fields refer to it.
    let mode = schema.fields[4].field().unwrap();
    assert_eq!((mode.kind, mode.enum_id), (pb::ValueKind::Enum, 1));
    assert_eq!(port.field().unwrap().enum_id, 0);
    assert_eq!(schema.enums.len(), 1);
    assert_eq!(schema.enums[0].id, 1);
    assert_eq!(schema.enums[0].variants[1].value, 5);
    assert_eq!(schema.enums[0].variants[1].name.as_str(), "manual");

    let missing = Call::GetConfigSchema(pb::GetConfigSchemaRequest { section_id: 99 });
    assert_eq!(
        error_code(&call(&controller, &PLAIN, missing)),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_patch_is_atomic_and_secrets_are_never_returned() {
    controller!(controller, OpenPolicy);
    let ok = patch(
        1,
        &[
            mutation(1, pb::ConfigMutation_::Operation::BoolValue(true)),
            mutation(3, string_value("ingress.example.org")),
            mutation(4, string_value("s3cret-token")),
        ],
    );
    let Reply::PatchConfigSection(done) = call(&controller, &PLAIN, Call::PatchConfigSection(ok))
    else {
        panic!()
    };
    assert_eq!(done.revision, 1);
    assert!(!done.restart_required);

    let Reply::GetConfigSection(section) = call(
        &controller,
        &PLAIN,
        Call::GetConfigSection(pb::GetConfigSectionRequest { section_id: 1 }),
    ) else {
        panic!()
    };
    assert_eq!(section.overridden.as_slice(), [1, 3, 4]);
    assert_eq!(section.revision, 1);
    // The effective values include the defaults, such as the port.
    let port = section.effective.iter().find(|e| e.field_id == 2).unwrap();
    assert!(matches!(
        &port.state,
        Some(pb::ConfigEntry_::State::U64Value(80))
    ));
    let token = section.effective.iter().find(|e| e.field_id == 4).unwrap();
    assert!(matches!(
        token.state,
        Some(pb::ConfigEntry_::State::Redacted(_))
    ));
    let host = section.effective.iter().find(|e| e.field_id == 3).unwrap();
    assert!(matches!(
        &host.state,
        Some(pb::ConfigEntry_::State::StringValue(v)) if v.as_str() == "ingress.example.org"
    ));

    // One bad mutation rejects the whole patch.
    let bad = patch(
        1,
        &[
            mutation(3, string_value("other.example.org")),
            mutation(2, pb::ConfigMutation_::Operation::U64Value(0)),
        ],
    );
    assert_eq!(
        error_code(&call(&controller, &PLAIN, Call::PatchConfigSection(bad))),
        pb::ErrorCode::InvalidArgument
    );
    let Reply::GetConfigSection(section) = call(
        &controller,
        &PLAIN,
        Call::GetConfigSection(pb::GetConfigSectionRequest { section_id: 1 }),
    ) else {
        panic!()
    };
    assert_eq!(section.revision, 1);
}

#[test]
fn none_overrides_an_optional_field_and_is_refused_for_a_required_one() {
    controller!(controller, OpenPolicy);
    let none = |field_id| pb::ConfigMutation {
        field_id,
        operation: Some(pb::ConfigMutation_::Operation::None(pb::Empty {})),
    };
    let get = || {
        let Reply::GetConfigSection(section) = call(
            &controller,
            &PLAIN,
            Call::GetConfigSection(pb::GetConfigSectionRequest { section_id: 1 }),
        ) else {
            panic!()
        };
        section
    };
    // Port is required.
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::PatchConfigSection(patch(1, &[none(2)]))
        )),
        pb::ErrorCode::InvalidArgument
    );
    assert!(get().overridden.is_empty());

    // The mode is optional: overridden, without a value.
    assert!(matches!(
        call(
            &controller,
            &PLAIN,
            Call::PatchConfigSection(patch(1, &[none(5)]))
        ),
        Reply::PatchConfigSection(_)
    ));
    let section = get();
    assert_eq!(section.overridden.as_slice(), [5]);
    assert!(section.effective.iter().all(|e| e.field_id != 5));

    // Clearing removes the override again.
    let clear = pb::ConfigMutation {
        field_id: 5,
        operation: Some(pb::ConfigMutation_::Operation::Clear(pb::Empty {})),
    };
    call(
        &controller,
        &PLAIN,
        Call::PatchConfigSection(patch(1, &[clear])),
    );
    assert!(get().overridden.is_empty());
}

#[test]
fn patch_conflicts_dry_runs_clear_and_reset() {
    controller!(controller, OpenPolicy);
    let mut stale = patch(1, &[mutation(3, string_value("a.io"))]);
    stale.set_expected_revision(5);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, Call::PatchConfigSection(stale))),
        pb::ErrorCode::Conflict
    );

    let mut dry = patch(1, &[mutation(3, string_value("a.io"))]);
    dry.dry_run = true;
    let Reply::PatchConfigSection(done) = call(&controller, &PLAIN, Call::PatchConfigSection(dry))
    else {
        panic!()
    };
    assert_eq!(done.revision, 0);
    let get = || {
        call(
            &controller,
            &PLAIN,
            Call::GetConfigSection(pb::GetConfigSectionRequest { section_id: 1 }),
        )
    };
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert!(section.overridden.is_empty());

    let set = patch(
        1,
        &[
            mutation(3, string_value("a.io")),
            mutation(4, string_value("t")),
        ],
    );
    call(&controller, &PLAIN, Call::PatchConfigSection(set));
    let clear = pb::ConfigMutation {
        field_id: 4,
        operation: Some(pb::ConfigMutation_::Operation::Clear(pb::Empty {})),
    };
    call(
        &controller,
        &PLAIN,
        Call::PatchConfigSection(patch(1, &[clear])),
    );
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    // The override is gone, so the token has its default again: none.
    assert!(section.effective.iter().all(|e| e.field_id != 4));
    assert_eq!(section.overridden.as_slice(), [3]);

    // A reset of a revision the section has moved past is a conflict.
    let mut stale = pb::ResetConfigSectionRequest {
        section_id: 1,
        ..Default::default()
    };
    stale.set_expected_revision(1);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, Call::ResetConfigSection(stale))),
        pb::ErrorCode::Conflict
    );
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert_eq!(section.overridden.as_slice(), [3]);

    let mut current = pb::ResetConfigSectionRequest {
        section_id: 1,
        ..Default::default()
    };
    current.set_expected_revision(2);
    let Reply::ResetConfigSection(done) =
        call(&controller, &PLAIN, Call::ResetConfigSection(current))
    else {
        panic!()
    };
    assert_eq!(done.revision, 3);
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert!(section.overridden.is_empty());
}

#[test]
fn factory_reset_resets_every_section_and_needs_a_secure_link() {
    controller!(controller, RequireEncryption);
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
    };
    call(
        &controller,
        &encrypted,
        Call::PatchConfigSection(patch(1, &[mutation(3, string_value("a.io"))])),
    );
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::FactoryReset(pb::FactoryResetRequest {})
        )),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(
            &controller,
            &encrypted,
            Call::FactoryReset(pb::FactoryResetRequest {})
        ),
        Reply::FactoryReset(_)
    ));
    let Reply::GetConfigSection(section) = call(
        &controller,
        &PLAIN,
        Call::GetConfigSection(pb::GetConfigSectionRequest { section_id: 1 }),
    ) else {
        panic!()
    };
    assert!(section.overridden.is_empty());
}

#[test]
fn reboot_needs_the_hook_and_a_secure_link() {
    std::thread_local!(static REBOOTS: Cell<u32> = const { Cell::new(0) });
    fn reboot() {
        REBOOTS.with(|count| count.set(count.get() + 1));
    }
    let request = || Call::Reboot(pb::RebootRequest {});

    let controller = Controller::new(Device::new(INFO, uptime, (), (), ()), OpenPolicy);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, request())),
        pb::ErrorCode::NotSupported
    );

    let device = Device::new(INFO, uptime, (), (), ()).with_reboot(reboot);
    let controller = Controller::new(device, RequireEncryption);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, request())),
        pb::ErrorCode::PermissionDenied
    );
    assert_eq!(REBOOTS.with(Cell::get), 0);
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
    };
    assert!(matches!(
        call(&controller, &encrypted, request()),
        Reply::Reboot(_)
    ));
    assert_eq!(REBOOTS.with(Cell::get), 1);
}

#[test]
fn a_value_without_content_is_invalid() {
    controller!(controller, OpenPolicy);
    let empty = pb::ConfigMutation {
        field_id: 3,
        operation: None,
    };
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::PatchConfigSection(patch(1, &[empty]))
        )),
        pb::ErrorCode::InvalidArgument
    );
}

#[test]
fn service_status_is_compact_in_the_summary_and_detailed_per_service() {
    controller!(controller, OpenPolicy);
    let Reply::GetServicesStatus(all) = call(
        &controller,
        &PLAIN,
        Call::GetServicesStatus(pb::GetServicesStatusRequest {}),
    ) else {
        panic!()
    };
    assert_eq!(all.uptime_ms, 5_000);
    assert_eq!(all.health, pb::DeviceHealth::Ok);
    assert_eq!(all.services.len(), 1);
    let summary = &all.services[0];
    // Discovery and compact status in one message.
    assert_eq!(summary.service_id, RADIO);
    assert_eq!(summary.name.as_str(), "radio");
    assert_eq!(summary.version, 1);
    assert_eq!(summary.state, pb::ServiceState::Running);
    assert!(summary.ready);
    assert_eq!(summary.revision, 1);

    let Reply::GetServiceSchema(schema) = call(
        &controller,
        &PLAIN,
        Call::GetServiceSchema(pb::GetServiceSchemaRequest { service_id: RADIO }),
    ) else {
        panic!()
    };
    assert_eq!(schema.service_id, RADIO);
    assert_eq!(schema.name.as_str(), "radio");
    assert_eq!(schema.fields.len(), 4);
    assert_eq!(schema.fields[1].unit, pb::Unit::Dbm);
    assert_eq!(schema.fields[3].kind, pb::ValueKind::F32);
    assert_eq!(schema.fields[2].enum_id, 1);
    assert_eq!(schema.enums[0].variants[1].name.as_str(), "5ghz");
    assert_eq!(schema.issues[0].code, FIRST_SERVICE_ISSUE);
    assert_eq!(schema.issues[0].name.as_str(), "link_lost");

    let Reply::GetServiceStatus(status) = call(
        &controller,
        &PLAIN,
        Call::GetServiceStatus(pb::GetServiceStatusRequest { service_id: RADIO }),
    ) else {
        panic!()
    };
    // The detailed status does not repeat the summary.
    assert_eq!(status.service_id, RADIO);
    assert_eq!(status.revision, summary.revision);
    assert_eq!(status.fields.len(), 4);
    assert!(matches!(
        status.fields[2].value,
        Some(pb::FieldValue_::Value::EnumValue(5))
    ));
    assert!(matches!(
        status.fields[3].value,
        Some(pb::FieldValue_::Value::F32Value(v)) if v == 1.5
    ));
    assert!(status.issues.is_empty());
}

#[test]
fn device_health_is_decided_by_the_firmware_and_issues_have_codes() {
    let memory = Memory::default();
    let radio = radio();
    *radio.degraded.borrow_mut() = true;
    let controller = Controller::new(device(&memory, &radio), OpenPolicy);
    let Reply::GetServicesStatus(all) = call(
        &controller,
        &PLAIN,
        Call::GetServicesStatus(pb::GetServicesStatusRequest {}),
    ) else {
        panic!()
    };
    assert_eq!(all.health, pb::DeviceHealth::Degraded);
    assert_eq!(all.services[0].health, pb::ServiceHealth::Degraded);
    let Reply::GetServiceStatus(status) = call(
        &controller,
        &PLAIN,
        Call::GetServiceStatus(pb::GetServiceStatusRequest { service_id: RADIO }),
    ) else {
        panic!()
    };
    assert_eq!(status.issues[0].code, FIRST_SERVICE_ISSUE);
    assert_eq!(status.issues[0].age_ms, 0);
    assert_eq!(status.issues[0].severity, pb::IssueSeverity::Warning);
}

#[test]
fn actions_are_discovered_described_and_invoked() {
    controller!(controller, OpenPolicy);
    let Reply::ListActions(list) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest::default()),
    ) else {
        panic!()
    };
    let names: std::vec::Vec<_> = list
        .actions
        .iter()
        .map(|a| (a.service_id, a.id, a.name.as_str(), a.privileged))
        .collect();
    assert_eq!(
        names,
        [
            (RADIO, 1, "scan", false),
            (RADIO, 2, "reconnect", true),
            (RADIO, 3, "provision", true)
        ]
    );

    let Reply::GetActionSchema(schema) = call(
        &controller,
        &PLAIN,
        Call::GetActionSchema(pb::GetActionSchemaRequest {
            service_id: RADIO,
            action_id: 3,
        }),
    ) else {
        panic!()
    };
    assert_eq!(schema.args.len(), 1);
    assert_eq!(schema.args[0].kind, pb::ValueKind::Bytes);
    assert!(schema.action().unwrap().secret_args);

    let Reply::InvokeAction(done) = call(
        &controller,
        &PLAIN,
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: RADIO,
            action_id: 1,
            args: Vec::new(),
        }),
    ) else {
        panic!()
    };
    assert_eq!(done.records.len(), 2);
    assert!(matches!(
        done.records[1].fields[0].value,
        Some(pb::FieldValue_::Value::StringValue(ref s)) if s.as_str() == "b"
    ));
}

#[test]
fn action_failures_are_typed() {
    controller!(controller, OpenPolicy);
    let invoke = |service: u32, action: u32, args: Vec<pb::ActionArg, 3>| {
        call(
            &controller,
            &PLAIN,
            Call::InvokeAction(pb::InvokeActionRequest {
                service_id: service,
                action_id: action,
                args,
            }),
        )
    };
    assert_eq!(
        error_code(&invoke(RADIO, 99, Vec::new())),
        pb::ErrorCode::NotFound
    );
    assert_eq!(
        error_code(&invoke(99, 1, Vec::new())),
        pb::ErrorCode::NotFound
    );
    // `provision` needs its argument.
    assert_eq!(
        error_code(&invoke(RADIO, 3, Vec::new())),
        pb::ErrorCode::InvalidArgument
    );
    let Reply::Error(rejected) = invoke(RADIO, 2, Vec::new()) else {
        panic!()
    };
    assert_eq!(rejected.code, pb::ErrorCode::Rejected);
    assert_eq!(
        rejected.reason().map(|r| r.as_str()),
        Some("not_configured")
    );
}

struct Gate;

impl AuthorizationPolicy for Gate {
    fn allows(&self, privilege: Privilege, link: &LinkSecurity) -> bool {
        match privilege {
            Privilege::Read => true,
            Privilege::Mutate => link.encrypted,
            Privilege::Provision => link.encrypted && link.authenticated,
        }
    }
}

#[test]
fn the_policy_gates_writes_and_privileged_actions() {
    controller!(controller, Gate);
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
    };
    let strong = LinkSecurity {
        encrypted: true,
        authenticated: true,
    };

    let write = || Call::PatchConfigSection(patch(1, &[mutation(3, string_value("a.io"))]));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, write())),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &encrypted, write()),
        Reply::PatchConfigSection(_)
    ));

    let reset = Call::ResetConfigSection(pb::ResetConfigSectionRequest {
        section_id: 1,
        dry_run: true,
        ..Default::default()
    });
    assert_eq!(
        error_code(&call(&controller, &PLAIN, reset)),
        pb::ErrorCode::PermissionDenied
    );

    let action = |id: u32, args: Vec<pb::ActionArg, 3>| {
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: RADIO,
            action_id: id,
            args,
        })
    };
    // An unprivileged action is open; a privileged one needs encryption.
    assert!(matches!(
        call(&controller, &PLAIN, action(1, Vec::new())),
        Reply::InvokeAction(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, action(2, Vec::new()))),
        pb::ErrorCode::PermissionDenied
    );

    // Secret arguments need the strongest link, and the policy runs before
    // the action does.
    let mut args = Vec::new();
    let secret = pb::ActionArg {
        field_id: 1,
        value: Some(pb::ActionArg_::Value::BytesValue(
            Vec::from_slice(&[1, 2, 3, 4]).unwrap(),
        )),
    };
    args.push(secret).ok().unwrap();
    assert_eq!(
        error_code(&call(&controller, &encrypted, action(3, args.clone()))),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &strong, action(3, args)),
        Reply::InvokeAction(_)
    ));
}

#[test]
fn policy_rejections_do_not_reach_the_service() {
    let memory = Memory::default();
    let radio = radio();
    let controller = Controller::new(device(&memory, &radio), RequireEncryption);
    let reconnect = Call::InvokeAction(pb::InvokeActionRequest {
        service_id: RADIO,
        action_id: 2,
        args: Vec::new(),
    });
    call(&controller, &PLAIN, reconnect);
    assert!(radio.log.borrow().is_empty());
}

#[derive(Service)]
struct Many;

macro_rules! counters {
    ($($id:expr, $name:expr;)+) => {
        &[$(FieldDescriptor::new($id, $name, ValueKind::U64)),+]
    };
}

impl Status for Many {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = counters! {
        1, "f1"; 2, "f2"; 3, "f3"; 4, "f4"; 5, "f5"; 6, "f6"; 7, "f7"; 8, "f8"; 9, "f9"; 10, "f10";
        11, "f11"; 12, "f12"; 13, "f13"; 14, "f14"; 15, "f15"; 16, "f16"; 17, "f17"; 18, "f18";
        19, "f19"; 20, "f20";
    };

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        for id in 1..=20 {
            out.u64(id, u64::from(id) * 100)?;
        }
        Ok(())
    }
}

impl Action for Many {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "a"),
        ActionDescriptor::new(2, "b"),
        ActionDescriptor::new(3, "c"),
        ActionDescriptor::new(4, "d"),
        ActionDescriptor::new(5, "e"),
        ActionDescriptor::new(6, "f"),
    ];

    async fn invoke(
        &self,
        _: u32,
        _: &[(u32, Value)],
        _: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        Ok(())
    }
}

#[test]
fn full_schemas_status_and_action_lists_fit_one_response() {
    let controller = Controller::new(
        Device::new(
            INFO,
            uptime,
            (),
            Statuses::<NoopRawMutex, _>::new((&Many,)),
            (&Many, &Many2),
        ),
        OpenPolicy,
    );
    let Reply::GetServiceSchema(schema) = call(
        &controller,
        &PLAIN,
        Call::GetServiceSchema(pb::GetServiceSchemaRequest {
            service_id: Many::ID,
        }),
    ) else {
        panic!()
    };
    assert_eq!(schema.fields.len(), rohi_service::MAX_FIELDS);
    assert_eq!(schema.fields[19].name.as_str(), "f20");

    let Reply::GetServiceStatus(status) = call(
        &controller,
        &PLAIN,
        Call::GetServiceStatus(pb::GetServiceStatusRequest {
            service_id: Many::ID,
        }),
    ) else {
        panic!()
    };
    assert_eq!(status.fields.len(), rohi_service::MAX_FIELDS);
    assert_eq!(status.fields[19].field_id, 20);

    let Reply::ListActions(list) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest {}),
    ) else {
        panic!()
    };
    assert_eq!(list.actions.len(), limits::MAX_ACTIONS);
    assert_eq!(list.actions[0].service_id, Many::ID);
    assert_eq!(list.actions[11].service_id, Many2::ID);
    assert_eq!(list.actions[11].name.as_str(), "f");
}

#[derive(Service)]
struct Many2;

impl Action for Many2 {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "a"),
        ActionDescriptor::new(2, "b"),
        ActionDescriptor::new(3, "c"),
        ActionDescriptor::new(4, "d"),
        ActionDescriptor::new(5, "e"),
        ActionDescriptor::new(6, "f"),
    ];

    async fn invoke(
        &self,
        _: u32,
        _: &[(u32, Value)],
        _: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        Ok(())
    }
}

// The only way two different services can share an id is a hash collision of
// two names; it is caught when the device is built.
struct Impostor;

impl Service for Impostor {
    const ID: u32 = Radio::ID;
    const NAME: &'static str = "impostor";
}

impl Action for Impostor {
    const ACTIONS: &'static [ActionDescriptor] = &[];

    async fn invoke(
        &self,
        _: u32,
        _: &[(u32, Value)],
        _: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        Ok(())
    }
}

#[test]
#[should_panic(expected = "two different services share an id")]
fn a_device_with_a_colliding_service_id_does_not_start_in_any_build() {
    let radio = radio();
    let _ = Device::new(
        INFO,
        uptime,
        (),
        Statuses::<NoopRawMutex, _>::new((&radio,)),
        (&Impostor,),
    );
}

#[test]
fn a_service_with_one_capability_is_unsupported_for_the_other() {
    let radio = radio();
    let controller = Controller::new(
        Device::new(
            INFO,
            uptime,
            (),
            Statuses::<NoopRawMutex, _>::new((&radio,)),
            (&Many2,),
        ),
        OpenPolicy,
    );
    // `radio` has status only here, `many2` actions only.
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::InvokeAction(pb::InvokeActionRequest {
                service_id: RADIO,
                action_id: 1,
                args: Vec::new(),
            }),
        )),
        pb::ErrorCode::NotSupported
    );
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::GetServiceStatus(pb::GetServiceStatusRequest {
                service_id: Many2::ID,
            }),
        )),
        pb::ErrorCode::NotSupported
    );
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::GetServiceSchema(pb::GetServiceSchemaRequest {
                service_id: Many2::ID,
            }),
        )),
        pb::ErrorCode::NotSupported
    );
    // A service nobody registered is unknown, whatever the capability.
    for service_id in [1, Many::ID] {
        assert_eq!(
            error_code(&call(
                &controller,
                &PLAIN,
                Call::GetServiceStatus(pb::GetServiceStatusRequest { service_id }),
            )),
            pb::ErrorCode::NotFound
        );
        assert_eq!(
            error_code(&call(
                &controller,
                &PLAIN,
                Call::InvokeAction(pb::InvokeActionRequest {
                    service_id,
                    action_id: 1,
                    args: Vec::new(),
                }),
            )),
            pb::ErrorCode::NotFound
        );
    }
}
