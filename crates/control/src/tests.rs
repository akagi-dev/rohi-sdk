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
use core::cell::{Cell, RefCell};

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use heapless::{String, Vec};
use rohi_config::{Text, Variant};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, Config, EnumDescriptor,
    EnumVariant, FIRST_SERVICE_ISSUE, FeedInfo, FieldDescriptor, FieldError, Issue,
    IssueDescriptor, IssueSeverity, ResultFormat, Section, SectionStore, Service, ServiceHealth,
    ServiceSummary, Status, StatusWriter, Statuses, StoreError, Unit, Value, ValueKind, WriteError,
};

use crate::*;

const MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    1,
    &[EnumVariant::new(0, "auto"), EnumVariant::new(5, "manual")],
)];

fn net_is_valid(net: &Net) -> Result<(), FieldError> {
    if net.enabled && net.host.is_empty() {
        return Err(FieldError::Invalid);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Config)]
#[config(name = "net", overrides = NetOverrides, version = 2, id = 1, validate = net_is_valid, enums = MODES)]
struct Net {
    #[config(id = 1)]
    enabled: bool,
    #[config(id = 2, min = 1, max = 65535)]
    port: u64,
    #[config(id = 3, with = Text)]
    host: std::string::String,
    #[config(id = 4, secret, with = Text)]
    token: Option<std::string::String>,
    #[config(id = 5, with = Variant, enum_id = 1)]
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
            ServiceSummary::new(ServiceHealth::Degraded)
        } else {
            ServiceSummary::new(ServiceHealth::Ok)
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
        ActionDescriptor::new(1, "scan", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(&[FieldDescriptor::new(1, "ssid", ValueKind::String)]),
        ActionDescriptor::new(2, "reconnect", ActionAccess::Mutate),
        ActionDescriptor::new(3, "provision", ActionAccess::Provision)
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

type NetSection<'a> = Section<Net, &'a Memory, NoopRawMutex>;
type TestDevice<'a> = Device<(NetSection<'a>,), Statuses<NoopRawMutex, (&'a Radio,)>, (&'a Radio,)>;

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
        (Section::new(memory),),
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
    session: 0,
    transport: Transport::Ble,
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
    assert!(!done.restart_required);

    let Reply::GetConfigSection(section) = call(
        &controller,
        &PLAIN,
        Call::GetConfigSection(pb::GetConfigSectionRequest { section_id: 1 }),
    ) else {
        panic!()
    };
    assert_eq!(section.overridden.as_slice(), [1, 3, 4]);
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
    assert_eq!(section.overridden.as_slice(), [1, 3, 4]);
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
fn patch_dry_runs_clear_and_reset() {
    controller!(controller, OpenPolicy);
    let mut dry = patch(1, &[mutation(3, string_value("a.io"))]);
    dry.dry_run = true;
    let Reply::PatchConfigSection(done) = call(&controller, &PLAIN, Call::PatchConfigSection(dry))
    else {
        panic!()
    };
    assert!(!done.restart_required);
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

    let current = pb::ResetConfigSectionRequest {
        section_id: 1,
        ..Default::default()
    };
    let Reply::ResetConfigSection(done) =
        call(&controller, &PLAIN, Call::ResetConfigSection(current))
    else {
        panic!()
    };
    assert!(!done.restart_required);
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert!(section.overridden.is_empty());
}

#[derive(Service)]
struct Maintenance {
    calls: Cell<u32>,
}

impl Action for Maintenance {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "restart", ActionAccess::Mutate),
        ActionDescriptor::new(2, "wipe", ActionAccess::Provision),
    ];

    async fn invoke(
        &self,
        action: u32,
        _args: &[(u32, Value)],
        _out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        self.calls.set(self.calls.get() + 1);
        match action {
            1 | 2 => Ok(()),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

// Allows reads and mutations, never provisioning.
struct OnlyReadsAndMutations;

impl AuthorizationPolicy for OnlyReadsAndMutations {
    fn allows(&self, privilege: Privilege, _link: &LinkSecurity) -> bool {
        privilege != Privilege::Provision
    }
}

// Allows only provisioning.
struct OnlyProvisioning;

impl AuthorizationPolicy for OnlyProvisioning {
    fn allows(&self, privilege: Privilege, _link: &LinkSecurity) -> bool {
        privilege != Privilege::Mutate
    }
}

#[test]
fn the_declared_access_decides_the_privilege_not_the_secret_flag() {
    let invoke = |action_id| {
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: Maintenance::ID,
            action_id,
            args: Vec::new(),
        })
    };
    let maintenance = Maintenance {
        calls: Cell::new(0),
    };
    let device = || Device::new(INFO, uptime, (), (), (&maintenance,));

    // The access is on the wire.
    let controller = Controller::new(device(), OpenPolicy);
    let Reply::ListActions(list) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest::default()),
    ) else {
        panic!()
    };
    let access: std::vec::Vec<_> = list
        .actions
        .iter()
        .map(|a| (a.name.as_str(), a.access))
        .collect();
    assert_eq!(
        access,
        [
            ("restart", pb::ActionAccess::Mutate),
            ("wipe", pb::ActionAccess::Provision)
        ]
    );
    assert!(list.actions.iter().all(|action| !action.secret_args));

    // A policy that distinguishes the two levels.
    let controller = Controller::new(device(), OnlyReadsAndMutations);
    assert!(matches!(
        call(&controller, &PLAIN, invoke(1)),
        Reply::ActionEmpty(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, invoke(2))),
        pb::ErrorCode::PermissionDenied
    );
    assert_eq!(maintenance.calls.get(), 1);

    let controller = Controller::new(device(), OnlyProvisioning);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, invoke(1))),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &PLAIN, invoke(2)),
        Reply::ActionEmpty(_)
    ));
    assert_eq!(maintenance.calls.get(), 2);
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
    assert_eq!(summary.health, pb::ServiceHealth::Ok);
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
        .map(|a| (a.service_id, a.id, a.name.as_str(), a.access))
        .collect();
    assert_eq!(
        names,
        [
            (RADIO, 1, "scan", pb::ActionAccess::Read),
            (RADIO, 2, "reconnect", pb::ActionAccess::Mutate),
            (RADIO, 3, "provision", pb::ActionAccess::Provision)
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

    let Reply::RecordsPage(done) = call(
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
        session: 0,
        transport: Transport::Ble,
    };
    let strong = LinkSecurity {
        encrypted: true,
        authenticated: true,
        session: 0,
        transport: Transport::Ble,
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
        Reply::RecordsPage(_)
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
        Reply::ActionEmpty(_)
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
        ServiceSummary::new(ServiceHealth::Ok)
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
        ActionDescriptor::new(1, "a", ActionAccess::Read),
        ActionDescriptor::new(2, "b", ActionAccess::Read),
        ActionDescriptor::new(3, "c", ActionAccess::Read),
        ActionDescriptor::new(4, "d", ActionAccess::Read),
        ActionDescriptor::new(5, "e", ActionAccess::Read),
        ActionDescriptor::new(6, "f", ActionAccess::Read),
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
        Call::ListActions(pb::ListActionsRequest::default()),
    ) else {
        panic!()
    };
    assert_eq!(list.actions.len(), limits::MAX_ACTIONS);
    assert_eq!(list.actions[0].service_id, Many::ID);
    assert_eq!(list.actions[11].service_id, Many2::ID);
    assert_eq!(list.actions[11].name.as_str(), "f");
    assert_eq!(list.total, 12);

    let Reply::ListActions(page) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest { offset: 10 }),
    ) else {
        panic!()
    };
    assert_eq!(page.actions.len(), 2);
    assert_eq!(page.actions[0].name.as_str(), "e");
    assert_eq!(page.total, 12);

    let Reply::ListActions(end) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest { offset: 12 }),
    ) else {
        panic!()
    };
    assert!(end.actions.is_empty());
}

#[derive(Service)]
struct Many2;

impl Action for Many2 {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "a", ActionAccess::Read),
        ActionDescriptor::new(2, "b", ActionAccess::Read),
        ActionDescriptor::new(3, "c", ActionAccess::Read),
        ActionDescriptor::new(4, "d", ActionAccess::Read),
        ActionDescriptor::new(5, "e", ActionAccess::Read),
        ActionDescriptor::new(6, "f", ActionAccess::Read),
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

#[derive(Service)]
pub(super) struct Files {
    pub(super) open: Cell<bool>,
    lost: Cell<u32>,
}

const FILE_BYTES: &[u8] = b"0123456789abcdef";

impl Action for Files {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "export", ActionAccess::Read).format(ResultFormat::Stream),
        ActionDescriptor::new(2, "list", ActionAccess::Read),
        ActionDescriptor::new(3, "export_private", ActionAccess::Mutate)
            .format(ResultFormat::Stream),
    ];

    async fn invoke(
        &self,
        action: u32,
        _: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            1 | 3 if self.open.get() => Err(ActionError::Busy),
            1 | 3 => {
                self.open.set(true);
                let mut files = Vec::new();
                files
                    .push(rohi_service::StreamFile {
                        id: 3,
                        name: String::try_from("rohi1.log.lz4").unwrap(),
                        size_bytes: FILE_BYTES.len() as u64,
                    })
                    .unwrap();
                out.stream(rohi_service::StreamInfo { id: 9, files })
                    .map_err(|_| ActionError::Internal)
            }
            // Declared without a stream, so the output refuses one.
            2 => out
                .stream(rohi_service::StreamInfo {
                    id: 1,
                    files: Vec::new(),
                })
                .map_err(|_| ActionError::Internal),
            _ => Err(ActionError::UnknownAction),
        }
    }

    async fn read_stream(
        &self,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<rohi_service::StreamChunk, ActionError> {
        if !self.open.get() || stream != 9 || file != 3 {
            return Err(ActionError::UnknownStream);
        }
        let offset = usize::try_from(offset).map_err(|_| ActionError::InvalidArgument)?;
        let rest = FILE_BYTES
            .get(offset..)
            .ok_or(ActionError::InvalidArgument)?;
        let len = rest.len().min(5).min(buf.len());
        buf[..len].copy_from_slice(&rest[..len]);
        Ok(rohi_service::StreamChunk {
            len,
            eof: len == rest.len(),
        })
    }

    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        if self.open.replace(false) && stream == 9 {
            Ok(())
        } else {
            Err(ActionError::UnknownStream)
        }
    }

    fn link_lost(&self) {
        self.lost.set(self.lost.get() + 1);
        self.open.set(false);
    }
}

pub(super) fn files_controller(files: &Files) -> Controller<Device<(), (), (&Files,)>, OpenPolicy> {
    Controller::new(Device::new(INFO, uptime, (), (), (files,)), OpenPolicy)
}

pub(super) fn files() -> Files {
    Files {
        open: Cell::new(false),
        lost: Cell::new(0),
    }
}

fn read_stream(offset: u64) -> Call {
    Call::ReadStream(pb::ReadStreamRequest {
        service_id: Files::ID,
        stream_id: 9,
        file_id: 3,
        offset,
    })
}

#[test]
fn an_action_returns_a_stream_that_is_read_by_offset_and_closed() {
    let files = files();
    let controller = files_controller(&files);

    let Reply::ActionStream(info) = call(
        &controller,
        &PLAIN,
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: Files::ID,
            action_id: 1,
            args: Vec::new(),
        }),
    ) else {
        panic!("not a stream");
    };
    assert_eq!((info.service_id, info.id), (Files::ID, 9));
    assert_eq!(info.files.len(), 1);
    assert_eq!(info.files[0].name.as_str(), "rohi1.log.lz4");
    assert_eq!(info.files[0].size_bytes, FILE_BYTES.len() as u64);

    let mut data = std::vec::Vec::new();
    loop {
        let Reply::ReadStream(chunk) = call(&controller, &PLAIN, read_stream(data.len() as u64))
        else {
            panic!("not a chunk");
        };
        assert_eq!(chunk.offset, data.len() as u64);
        data.extend_from_slice(&chunk.data);
        if chunk.eof {
            break;
        }
    }
    assert_eq!(data, FILE_BYTES);

    // A repeated read of the same offset gives the same bytes.
    let (Reply::ReadStream(first), Reply::ReadStream(again)) = (
        call(&controller, &PLAIN, read_stream(5)),
        call(&controller, &PLAIN, read_stream(5)),
    ) else {
        panic!("not a chunk");
    };
    assert_eq!(first.data, again.data);

    assert_eq!(
        error_code(&call(&controller, &PLAIN, read_stream(99))),
        pb::ErrorCode::InvalidArgument
    );
    let close = || {
        Call::CloseStream(pb::CloseStreamRequest {
            service_id: Files::ID,
            stream_id: 9,
        })
    };
    assert!(matches!(
        call(&controller, &PLAIN, close()),
        Reply::CloseStream(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, read_stream(0))),
        pb::ErrorCode::NotFound
    );
    assert_eq!(
        error_code(&call(&controller, &PLAIN, close())),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_stream_is_refused_when_busy_unknown_or_undeclared() {
    let files = files();
    let controller = files_controller(&files);
    let invoke = |action| {
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: Files::ID,
            action_id: action,
            args: Vec::new(),
        })
    };
    assert!(matches!(
        call(&controller, &PLAIN, invoke(1)),
        Reply::ActionStream(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, invoke(1))),
        pb::ErrorCode::Busy
    );
    assert_eq!(
        error_code(&call(&controller, &PLAIN, invoke(2))),
        pb::ErrorCode::Internal
    );
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::ReadStream(pb::ReadStreamRequest {
                service_id: 12345,
                ..Default::default()
            }),
        )),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn losing_the_link_releases_the_streams() {
    let files = files();
    let controller = files_controller(&files);
    assert!(matches!(
        call(
            &controller,
            &PLAIN,
            Call::InvokeAction(pb::InvokeActionRequest {
                service_id: Files::ID,
                action_id: 1,
                args: Vec::new(),
            }),
        ),
        Reply::ActionStream(_)
    ));
    controller.link_lost();
    assert_eq!(files.lost.get(), 1);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, read_stream(0))),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn the_action_listing_tells_which_actions_return_a_stream() {
    let files = files();
    let controller = files_controller(&files);
    let Reply::ListActions(list) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest::default()),
    ) else {
        panic!("not a list");
    };
    let formats: std::vec::Vec<_> = list.actions.iter().map(|a| a.result_format).collect();
    assert_eq!(
        formats,
        [
            pb::ResultFormat::Stream,
            pb::ResultFormat::Empty,
            pb::ResultFormat::Stream
        ]
    );
}

const OTHER: LinkSecurity = LinkSecurity {
    encrypted: false,
    authenticated: false,
    session: 2,
    transport: Transport::Ble,
};

pub(super) fn export(action: u32) -> Call {
    Call::InvokeAction(pb::InvokeActionRequest {
        service_id: Files::ID,
        action_id: action,
        args: Vec::new(),
    })
}

fn close_stream() -> Call {
    Call::CloseStream(pb::CloseStreamRequest {
        service_id: Files::ID,
        stream_id: 9,
    })
}

#[test]
fn a_stream_cannot_be_read_or_closed_by_another_session() {
    let files = files();
    let controller = files_controller(&files);
    assert!(matches!(
        call(&controller, &PLAIN, export(1)),
        Reply::ActionStream(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &OTHER, read_stream(0))),
        pb::ErrorCode::NotFound
    );
    assert_eq!(
        error_code(&call(&controller, &OTHER, close_stream())),
        pb::ErrorCode::NotFound
    );
    // The owner still has it.
    assert!(matches!(
        call(&controller, &PLAIN, read_stream(0)),
        Reply::ReadStream(_)
    ));
    assert!(matches!(
        call(&controller, &PLAIN, close_stream()),
        Reply::CloseStream(_)
    ));
}

#[test]
fn a_stream_needs_the_privilege_of_the_action_that_created_it() {
    let files = files();
    let controller = Controller::new(
        Device::new(INFO, uptime, (), (), (&files,)),
        RequireEncryption,
    );
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
        session: 1,
        transport: Transport::Ble,
    };
    assert_eq!(
        error_code(&call(&controller, &PLAIN, export(3))),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &encrypted, export(3)),
        Reply::ActionStream(_)
    ));
    assert!(matches!(
        call(&controller, &encrypted, read_stream(0)),
        Reply::ReadStream(_)
    ));
    // The same session on a link that lost its encryption is refused.
    let downgraded = LinkSecurity {
        encrypted: false,
        ..encrypted
    };
    assert_eq!(
        error_code(&call(&controller, &downgraded, read_stream(0))),
        pb::ErrorCode::PermissionDenied
    );
    assert_eq!(
        error_code(&call(&controller, &downgraded, close_stream())),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &encrypted, close_stream()),
        Reply::CloseStream(_)
    ));
}

#[test]
fn a_new_session_cannot_use_the_streams_of_a_lost_link() {
    let files = files();
    let controller = files_controller(&files);
    assert!(matches!(
        call(&controller, &PLAIN, export(1)),
        Reply::ActionStream(_)
    ));
    controller.link_lost();
    assert_eq!(
        error_code(&call(&controller, &PLAIN, read_stream(0))),
        pb::ErrorCode::NotFound
    );
    // The id is free again for the next client.
    assert!(matches!(
        call(&controller, &OTHER, export(1)),
        Reply::ActionStream(_)
    ));
    assert!(matches!(
        call(&controller, &OTHER, read_stream(0)),
        Reply::ReadStream(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, read_stream(0))),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_bitset_argument_refers_to_its_catalog_through_the_selection_table() {
    let fields = [
        FieldDescriptor::new(1, "owner", ValueKind::Address),
        FieldDescriptor::new(2, "public", ValueKind::Bytes).with_selection(32, 7, 9),
        FieldDescriptor::new(3, "private", ValueKind::Bytes).with_selection(32, 7, 9),
    ];
    let wire: Vec<pb::FieldDescriptor, 3> = mapping::fields(&fields);
    assert_eq!(wire[0].kind, pb::ValueKind::Address);
    assert_eq!(
        [wire[0].enum_id, wire[1].enum_id, wire[2].enum_id],
        [0, 1, 2]
    );
    let table: Vec<pb::SelectionDescriptor, { limits::MAX_SELECTIONS }> =
        mapping::selections(&fields);
    assert_eq!(table.len(), 2);
    assert_eq!(table[1].id, 2);
    assert_eq!(
        (
            table[1].size_bytes,
            table[1].catalog_service_id,
            table[1].catalog_action_id
        ),
        (32, 7, 9)
    );
}

#[derive(Service)]
#[service(name = "tailing")]
struct Tailing {
    served: Cell<u32>,
}

const TAIL_TEXT: &[u8] = b"1 5 - INFO t: one\n2 6 - INFO t: two\n";
const TAIL_ARGS: &[FieldDescriptor] = &[
    FieldDescriptor::new(1, "session", ValueKind::U64),
    FieldDescriptor::new(2, "after", ValueKind::U64),
];

mod tail_action {
    pub const TAIL: u32 = 1;
    pub const ADMIN: u32 = 2;
    pub const OVERSIZED: u32 = 3;
}

impl Action for Tailing {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(tail_action::TAIL, "tail", ActionAccess::Read)
            .args(TAIL_ARGS)
            .format(ResultFormat::Feed),
        ActionDescriptor::new(tail_action::ADMIN, "admin", ActionAccess::Mutate)
            .format(ResultFormat::Feed),
        ActionDescriptor::new(tail_action::OVERSIZED, "oversized", ActionAccess::Read)
            .format(ResultFormat::Feed),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        self.served.set(self.served.get() + 1);
        let number = |id| {
            args.iter().find_map(|(field, value)| match value {
                Value::U64(number) if *field == id => Some(*number),
                _ => None,
            })
        };
        let (session, after) = (number(1).unwrap_or(0), number(2).unwrap_or(0));
        let info = FeedInfo {
            session: if session == 0 { 7 } else { session as u32 },
            cursor: after + 2,
            oldest_sequence: 1,
            newest_sequence: after + 2,
            reset: session == 99,
            gap: 3,
        };
        let page = match action {
            tail_action::OVERSIZED => out.feed(info, &[b'x'; limits::MAX_FEED_CHUNK + 1]),
            _ => out.feed(info, TAIL_TEXT),
        };
        page.map_err(|_| ActionError::Internal)
    }
}

fn tail_call(service_id: u32, action_id: u32, session: u64, after: u64) -> Call {
    let arg = |field_id, value| pb::ActionArg {
        field_id,
        value: Some(pb::ActionArg_::Value::U64Value(value)),
    };
    let mut args = Vec::new();
    if action_id == tail_action::TAIL {
        args.push(arg(1, session)).ok().unwrap();
        args.push(arg(2, after)).ok().unwrap();
    }
    Call::InvokeAction(pb::InvokeActionRequest {
        service_id,
        action_id,
        args,
    })
}

#[test]
fn a_page_is_one_invoke_and_one_reply_without_a_stream() {
    let tailing = Tailing {
        served: Cell::new(0),
    };
    let controller = Controller::new(
        Device::new(INFO, uptime, (), (), (&tailing,)),
        RequireEncryption,
    );
    // Reads are open on a plain link: a page of a `Read` action needs no encryption.
    let Reply::FeedPage(page) = call(
        &controller,
        &PLAIN,
        tail_call(Tailing::ID, tail_action::TAIL, 0, 10),
    ) else {
        panic!("not a page");
    };
    assert_eq!(page.data.as_slice(), TAIL_TEXT);
    assert_eq!(
        (
            page.session,
            page.cursor,
            page.oldest_sequence,
            page.newest_sequence
        ),
        (7, 12, 1, 12)
    );
    assert_eq!((page.reset, page.gap), (false, 3));
    // The reply of another session is flagged by the service, as it set it.
    let Reply::FeedPage(page) = call(
        &controller,
        &PLAIN,
        tail_call(Tailing::ID, tail_action::TAIL, 99, 0),
    ) else {
        panic!("not a page");
    };
    assert!(page.reset);
    assert_eq!(tailing.served.get(), 2);
    // Nothing was registered: no stream is open, whatever the client does next.
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::ReadStream(pb::ReadStreamRequest {
                service_id: Tailing::ID,
                stream_id: 1,
                file_id: 1,
                offset: 0,
            })
        )),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_page_action_is_discovered_and_follows_its_declared_access() {
    let tailing = Tailing {
        served: Cell::new(0),
    };
    let controller = Controller::new(
        Device::new(INFO, uptime, (), (), (&tailing,)),
        RequireEncryption,
    );
    let Reply::ListActions(list) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest::default()),
    ) else {
        panic!("not a list");
    };
    let formats: std::vec::Vec<_> = list
        .actions
        .iter()
        .map(|action| action.result_format)
        .collect();
    assert_eq!(formats, [pb::ResultFormat::Feed; 3]);
    // A page of an action that mutates needs the privilege of a mutation.
    let admin = tail_call(Tailing::ID, tail_action::ADMIN, 0, 0);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, admin.clone())),
        pb::ErrorCode::PermissionDenied
    );
    assert_eq!(tailing.served.get(), 0);
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
        session: 0,
        transport: Transport::Ble,
    };
    assert!(matches!(
        call(&controller, &encrypted, admin),
        Reply::FeedPage(_)
    ));
}

#[test]
fn a_page_beyond_its_bound_is_never_sent() {
    let tailing = Tailing {
        served: Cell::new(0),
    };
    let controller = Controller::new(Device::new(INFO, uptime, (), (), (&tailing,)), OpenPolicy);
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            tail_call(Tailing::ID, tail_action::OVERSIZED, 0, 0)
        )),
        pb::ErrorCode::Internal
    );
}

#[test]
fn a_page_of_a_missing_service_or_action_is_not_found_and_the_old_rpc_is_gone() {
    let tailing = Tailing {
        served: Cell::new(0),
    };
    let controller = Controller::new(Device::new(INFO, uptime, (), (), (&tailing,)), OpenPolicy);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, tail_call(12345, 1, 0, 0))),
        pb::ErrorCode::NotFound
    );
    assert_eq!(
        error_code(&call(&controller, &PLAIN, tail_call(Tailing::ID, 99, 0, 0))),
        pb::ErrorCode::NotFound
    );
    // Arguments that do not fit the action are refused before it runs.
    let bad = Call::InvokeAction(pb::InvokeActionRequest {
        service_id: Tailing::ID,
        action_id: tail_action::TAIL,
        args: Vec::new(),
    });
    assert_eq!(
        error_code(&call(&controller, &PLAIN, bad)),
        pb::ErrorCode::InvalidArgument
    );
    assert_eq!(tailing.served.get(), 0);
}

#[test]
fn closing_a_session_releases_only_its_streams() {
    let files = files();
    let controller = files_controller(&files);
    assert!(matches!(
        call(&controller, &PLAIN, export(1)),
        Reply::ActionStream(_)
    ));
    // Another session going away neither closes the stream nor forgets its owner.
    block_on(controller.session_closed(OTHER.session));
    assert!(files.open.get());
    assert!(matches!(
        call(&controller, &PLAIN, read_stream(0)),
        Reply::ReadStream(_)
    ));
    block_on(controller.session_closed(PLAIN.session));
    assert!(!files.open.get());
    // The services were not told that every link is gone.
    assert_eq!(files.lost.get(), 0);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, read_stream(0))),
        pb::ErrorCode::NotFound
    );
}

// What the access policy takes from a link: only what the transport established.
mod link_security {
    use super::*;

    const TCP: LinkSecurity = LinkSecurity {
        encrypted: false,
        authenticated: false,
        session: 11,
        transport: Transport::Tcp,
    };
    const BLE: LinkSecurity = LinkSecurity {
        session: 12,
        transport: Transport::Ble,
        ..TCP
    };
    // A TCP connection that completed the handshake of `rohi-control-tcp`.
    const TCP_SECURE: LinkSecurity = LinkSecurity {
        encrypted: true,
        authenticated: true,
        ..TCP
    };

    fn entropy(out: &mut [u8]) -> bool {
        out.fill(0xA5);
        true
    }

    // A mutation that reaches the registry and is answered NotFound there.
    fn mutate() -> Call {
        Call::ResetConfigSection(pb::ResetConfigSectionRequest::default())
    }

    // The provisioning action (with a secret argument) of the test radio.
    fn provision() -> Call {
        Call::InvokeAction(pb::InvokeActionRequest {
            service_id: RADIO,
            action_id: 3,
            ..Default::default()
        })
    }

    fn reason(reply: &Reply) -> (pb::ErrorCode, &str) {
        match reply {
            Reply::Error(error) => (error.code, error.reason().map_or("", |r| r.as_str())),
            _ => panic!("not an error: {reply:?}"),
        }
    }

    macro_rules! restricted {
        ($controller:ident, $access:ident) => {
            let $access = ControlAccess::new(entropy);
            let memory = Memory::default();
            let radio = radio();
            let $controller = Controller::new(device(&memory, &radio), AccessPolicy::new(&$access));
        };
    }

    #[test]
    fn reads_are_open_and_privileged_requests_need_a_secured_link_on_restricted_tcp() {
        restricted!(controller, access);
        assert!(matches!(
            call(&controller, &TCP, Call::GetInfo(pb::GetInfoRequest {})),
            Reply::GetInfo(_)
        ));
        // No key is provisioned: the reason says so.
        assert_eq!(
            reason(&call(&controller, &TCP, mutate())),
            (pb::ErrorCode::PermissionDenied, "no_psk")
        );
        access.set_tcp_psk(true);
        assert_eq!(
            reason(&call(&controller, &TCP, mutate())),
            (pb::ErrorCode::PermissionDenied, "encryption_required")
        );
        // Encrypted alone is not enough on TCP: the client must be authenticated too.
        let unauthenticated = LinkSecurity {
            encrypted: true,
            ..TCP
        };
        assert_eq!(
            reason(&call(&controller, &unauthenticated, mutate())).1,
            "encryption_required"
        );
        assert_eq!(
            error_code(&call(&controller, &TCP_SECURE, mutate())),
            pb::ErrorCode::NotFound
        );
        // Every decoded request is counted once, the refused ones too, and a refusal is no error.
        assert_eq!(access.requests(), 5);
        assert_eq!(access.errors(), 0);
    }

    #[test]
    fn the_psk_state_is_a_flag_and_a_generation_never_a_key() {
        let access = ControlAccess::new(entropy);
        assert!(!access.tcp_psk_enrolled());
        let before = access.tcp_psk_epoch();
        access.set_tcp_psk(true);
        assert!(access.tcp_psk_enrolled());
        assert_ne!(access.tcp_psk_epoch(), before);
        let before = access.tcp_psk_epoch();
        access.set_tcp_psk(false);
        assert!(!access.tcp_psk_enrolled());
        assert_ne!(access.tcp_psk_epoch(), before);
        let mut random = [0u8; 4];
        assert!(access.fill_random(&mut random));
        assert_eq!(random, [0xA5; 4]);
    }

    #[test]
    fn ble_is_open_and_never_reported_as_protected() {
        restricted!(controller, access);
        assert_eq!(
            error_code(&call(&controller, &BLE, mutate())),
            pb::ErrorCode::NotFound
        );
        assert_ne!(
            error_code(&call(&controller, &BLE, provision())),
            pb::ErrorCode::PermissionDenied
        );
        // BLE has no mode to change; the link keeps the properties the transport reported.
        access.set_mode(Transport::Ble, TransportAccessMode::Private);
        assert_eq!(access.mode(Transport::Ble), TransportAccessMode::Open);
        assert_eq!(
            error_code(&call(&controller, &BLE, mutate())),
            pb::ErrorCode::NotFound
        );
        const { assert!(!BLE.encrypted && !BLE.authenticated) };
        // TCP stays restricted.
        assert_eq!(reason(&call(&controller, &TCP, mutate())).1, "no_psk");
    }

    // A service with one action that carries the key material of a transport.
    struct Keys;

    impl Service for Keys {
        const ID: u32 = rohi_service::service_id("keys");
        const NAME: &'static str = "keys";
    }

    impl Action for Keys {
        const ACTIONS: &'static [ActionDescriptor] = &[
            ActionDescriptor::new(1, "set_key", ActionAccess::Provision)
                .secret()
                .secure_link(),
            ActionDescriptor::new(2, "plain", ActionAccess::Provision),
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
    fn a_secure_link_action_needs_a_protected_link_in_every_mode() {
        let access = ControlAccess::new(entropy);
        let keys = Keys;
        let device = Device::new(INFO, uptime, (), (), (&keys,));
        let controller = Controller::new(device, AccessPolicy::new(&access));
        let invoke = |link: &LinkSecurity, action: u32| {
            call(
                &controller,
                link,
                Call::InvokeAction(pb::InvokeActionRequest {
                    service_id: Keys::ID,
                    action_id: action,
                    args: Vec::new(),
                }),
            )
        };
        let encrypted_only = LinkSecurity {
            encrypted: true,
            ..TCP
        };
        for mode in [
            TransportAccessMode::Open,
            TransportAccessMode::Protected,
            TransportAccessMode::Private,
        ] {
            access.set_mode(Transport::Tcp, mode);
            // Never over an unprotected link, whatever the mode says (the open modes allow the
            // plain action, which proves the refusal is the action's own).
            for link in [&TCP, &encrypted_only] {
                let reply = invoke(link, 1);
                let (code, why) = reason(&reply);
                assert_eq!(code, pb::ErrorCode::PermissionDenied, "{mode:?} {link:?}");
                if mode == TransportAccessMode::Open {
                    // The mode allows it; the action does not.
                    assert_eq!(why, "encryption_required");
                }
            }
            // On a protected TCP link it runs. Open BLE is the accepted insecure state of this phase.
            assert!(
                matches!(invoke(&TCP_SECURE, 1), Reply::ActionEmpty(_)),
                "{mode:?}"
            );
            assert!(matches!(invoke(&BLE, 1), Reply::ActionEmpty(_)), "{mode:?}");
        }
        // An action that is not marked is as open as its mode.
        access.set_mode(Transport::Tcp, TransportAccessMode::Open);
        assert!(matches!(invoke(&TCP, 2), Reply::ActionEmpty(_)));
    }

    #[test]
    fn the_modes_decide_what_a_client_without_proof_may_do() {
        restricted!(controller, access);
        let read = || Call::GetInfo(pb::GetInfoRequest {});
        let allowed = |reply: &Reply| {
            !matches!(
                reply,
                Reply::Error(error) if error.code == pb::ErrorCode::PermissionDenied
            )
        };
        // (mode, read, mutate, provision) for a client without proof, then after the handshake.
        let table = [
            (TransportAccessMode::Open, true, true, true),
            (TransportAccessMode::Protected, true, false, false),
            (TransportAccessMode::Private, false, false, false),
        ];
        for (mode, can_read, can_mutate, can_provision) in table {
            access.set_mode(Transport::Tcp, mode);
            assert_eq!(
                allowed(&call(&controller, &TCP, read())),
                can_read,
                "{mode:?}"
            );
            assert_eq!(
                allowed(&call(&controller, &TCP, mutate())),
                can_mutate,
                "{mode:?}"
            );
            assert_eq!(
                allowed(&call(&controller, &TCP, provision())),
                can_provision,
                "{mode:?}"
            );
            // A connection that completed the handshake may do all three in every mode.
            assert!(allowed(&call(&controller, &TCP_SECURE, read())), "{mode:?}");
            assert!(
                allowed(&call(&controller, &TCP_SECURE, mutate())),
                "{mode:?}"
            );
            assert!(
                allowed(&call(&controller, &TCP_SECURE, provision())),
                "{mode:?}"
            );
            // Encrypted without being authenticated proves nothing.
            let half = LinkSecurity {
                encrypted: true,
                ..TCP
            };
            assert_eq!(
                allowed(&call(&controller, &half, mutate())),
                can_mutate,
                "{mode:?}"
            );
            // BLE is untouched by the mode of TCP.
            assert!(allowed(&call(&controller, &BLE, mutate())), "{mode:?}");
        }
    }

    #[test]
    fn requests_are_counted_once_and_a_refusal_is_not_an_error() {
        restricted!(controller, access);
        let mut out = [0u8; limits::MAX_RESPONSE_LEN];
        // Not a request: not counted.
        block_on(controller.handle(&TCP, &[0xFF, 0xFF], &mut out));
        assert_eq!(access.requests(), 0);
        let _ = call(&controller, &TCP, Call::GetInfo(pb::GetInfoRequest {}));
        let _ = call(&controller, &TCP, mutate());
        let _ = call(&controller, &BLE, mutate());
        assert_eq!(access.requests(), 3);
        assert_eq!(access.errors(), 0);
        access.transport_error(Transport::Tcp, 1);
        access.transport_error(Transport::Ble, 2);
        assert_eq!(access.errors(), 2);
        assert_eq!(access.stats(Transport::Tcp).errors, 1);
    }

    #[test]
    fn the_availability_follows_the_expected_endpoints_only() {
        use crate::Availability::*;
        let access = ControlAccess::new(entropy);
        assert_eq!(access.availability(), None);
        // Absent by build is not a failure, whatever the state says.
        access.set_state(Transport::Ble, EndpointState::Failed);
        assert_eq!(access.availability(), None);
        access.expect(Transport::Tcp);
        assert_eq!(access.availability(), Starting);
        access.set_state(Transport::Tcp, EndpointState::Running);
        assert_eq!(access.availability(), All);
        access.expect(Transport::Ble);
        assert_eq!(access.availability(), Some);
        access.set_state(Transport::Ble, EndpointState::Running);
        assert_eq!(access.availability(), All);
        access.set_state(Transport::Ble, EndpointState::Disabled);
        assert_eq!(access.availability(), Starting);
        access.set_state(Transport::Tcp, EndpointState::Failed);
        access.set_state(Transport::Ble, EndpointState::Failed);
        assert_eq!(access.availability(), Failed);
        // Sessions, counters, mode and key do not move it.
        access.session_opened(Transport::Tcp);
        access.transport_error(Transport::Tcp, 1);
        access.set_tcp_psk(true);
        assert_eq!(access.availability(), Failed);
    }

    #[test]
    fn an_open_tcp_endpoint_allows_privileged_requests() {
        restricted!(controller, access);
        access.set_mode(Transport::Tcp, TransportAccessMode::Open);
        assert_eq!(
            error_code(&call(&controller, &TCP, mutate())),
            pb::ErrorCode::NotFound
        );
    }

    #[test]
    fn nothing_in_a_request_can_raise_the_security_of_a_link() {
        restricted!(controller, access);
        access.set_tcp_psk(true);
        // Bytes that look like some other wrapper are not a request at all: refused as such,
        // and a plain mutation stays refused on the same link.
        let mut wire = [0u8; 16];
        let request = pb::Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: 5,
            request: Some(mutate()),
        };
        let len = encode(&request, &mut wire).unwrap();
        let mut out = [0u8; limits::MAX_RESPONSE_LEN];
        for bytes in [
            &wire[..len],
            &[0xDA, 0x01, 0x04, 0x0A, 0x02, 0x08, 0x01][..],
        ] {
            let n = block_on(controller.handle(&TCP, bytes, &mut out));
            let reply = decode::<pb::Response>(&out[..n]).unwrap().response.unwrap();
            assert!(matches!(
                error_code(&reply),
                pb::ErrorCode::PermissionDenied
                    | pb::ErrorCode::NotSupported
                    | pb::ErrorCode::InvalidArgument
            ));
        }
    }
}

// --- results of the five formats ---------------------------------------------

#[derive(Service)]
struct Library {
    // The snapshot held, 0 when none.
    held: Cell<u32>,
    runs: Cell<u32>,
    reads: Cell<u32>,
    closed: Cell<u32>,
}

mod library {
    pub const LIST: u32 = 1;
    pub const LIST_PRIVATE: u32 = 2;
    pub const SHORT: u32 = 3;
    pub const NONE: u32 = 4;
    pub const INLINE: u32 = 5;
    pub const EMPTY: u32 = 6;
    pub const STUCK: u32 = 7;
    pub const VIEW: u32 = 8;
    pub const TOTAL: u16 = 10;
    pub const SNAPSHOT: u32 = 41;
}

const BOOKS: &[FieldDescriptor] = &[FieldDescriptor::new(1, "title", ValueKind::String)];

impl Library {
    fn write(out: &mut impl ActionOutput, from: u32, total: u32) -> Result<(), ActionError> {
        for index in from..total {
            if out.next_record().is_err() {
                break;
            }
            let mut title = String::<16>::new();
            core::fmt::write(&mut title, format_args!("book{index}"))
                .map_err(|_| ActionError::Internal)?;
            out.text(1, &title).map_err(|_| ActionError::Internal)?;
        }
        Ok(())
    }
}

impl Action for Library {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(library::LIST, "list", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(BOOKS),
        ActionDescriptor::new(library::LIST_PRIVATE, "list_private", ActionAccess::Mutate)
            .format(ResultFormat::Records)
            .results(BOOKS),
        ActionDescriptor::new(library::SHORT, "short", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(BOOKS),
        ActionDescriptor::new(library::NONE, "none", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(BOOKS),
        ActionDescriptor::new(library::INLINE, "newest", ActionAccess::Read)
            .format(ResultFormat::Inline)
            .results(BOOKS),
        ActionDescriptor::new(library::EMPTY, "tidy", ActionAccess::Mutate),
        ActionDescriptor::new(library::STUCK, "stuck", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(BOOKS),
        ActionDescriptor::new(library::VIEW, "view", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(BOOKS),
    ];

    async fn invoke(
        &self,
        action: u32,
        _: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        self.runs.set(self.runs.get() + 1);
        let snapshot = |out: &mut dyn FnMut(rohi_service::SnapshotInfo) -> Result<(), _>,
                        total: u16| {
            if self.held.get() != 0 {
                return Err(ActionError::Busy);
            }
            self.held.set(library::SNAPSHOT);
            out(rohi_service::SnapshotInfo {
                id: library::SNAPSHOT,
                total,
            })
            .map_err(|_: WriteError| ActionError::Internal)
        };
        match action {
            library::LIST | library::LIST_PRIVATE | library::STUCK => {
                snapshot(&mut |info| out.snapshot(info), library::TOTAL)
            }
            library::SHORT => snapshot(&mut |info| out.snapshot(info), 4),
            // A view holds nothing: every session gets the same id.
            library::VIEW => out
                .snapshot(rohi_service::SnapshotInfo {
                    id: library::SNAPSHOT,
                    total: library::TOTAL,
                })
                .map_err(|_| ActionError::Internal),
            library::NONE | library::EMPTY => Ok(()),
            library::INLINE => out.text(1, "book9").map_err(|_| ActionError::Internal),
            _ => Err(ActionError::UnknownAction),
        }
    }

    async fn read_records(
        &self,
        action: u32,
        snapshot: u32,
        from: u32,
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        if action != library::VIEW && (self.held.get() != snapshot || snapshot == 0) {
            return Err(ActionError::UnknownSnapshot);
        }
        self.reads.set(self.reads.get() + 1);
        match action {
            library::STUCK => Ok(()),
            library::SHORT => Self::write(out, from, 4),
            _ => Self::write(out, from, u32::from(library::TOTAL)),
        }
    }

    async fn close_records(&self, snapshot: u32) -> Result<(), ActionError> {
        if snapshot == 0 {
            return Err(ActionError::UnknownSnapshot);
        }
        if self.held.get() != snapshot {
            // A view, or a snapshot already gone.
            return Ok(());
        }
        self.held.set(0);
        self.closed.set(self.closed.get() + 1);
        Ok(())
    }

    fn link_lost(&self) {
        self.held.set(0);
    }
}

fn library() -> Library {
    Library {
        held: Cell::new(0),
        runs: Cell::new(0),
        reads: Cell::new(0),
        closed: Cell::new(0),
    }
}

fn library_controller(library: &Library) -> Controller<Device<(), (), (&Library,)>, OpenPolicy> {
    Controller::new(Device::new(INFO, uptime, (), (), (library,)), OpenPolicy)
}

fn run(action: u32) -> Call {
    Call::InvokeAction(pb::InvokeActionRequest {
        service_id: Library::ID,
        action_id: action,
        args: Vec::new(),
    })
}

fn page_of(action: u32, token: &[u8]) -> Call {
    Call::ReadResultPage(pb::ReadResultPageRequest {
        service_id: Library::ID,
        action_id: action,
        page_token: Vec::from_slice(token).unwrap(),
    })
}

fn close_page_of(action: u32, token: &[u8]) -> Call {
    Call::CloseResultPage(pb::CloseResultPageRequest {
        service_id: Library::ID,
        action_id: action,
        page_token: Vec::from_slice(token).unwrap(),
    })
}

fn titles(page: &pb::RecordsPage) -> std::vec::Vec<std::string::String> {
    page.records
        .iter()
        .map(|record| match &record.fields[0].value {
            Some(pb::FieldValue_::Value::StringValue(title)) => title.as_str().into(),
            _ => panic!("not a title"),
        })
        .collect()
}

#[test]
fn empty_inline_and_an_empty_collection_are_three_distinct_replies() {
    let library = library();
    let controller = library_controller(&library);
    assert!(matches!(
        call(&controller, &PLAIN, run(library::EMPTY)),
        Reply::ActionEmpty(_)
    ));
    let Reply::ActionInline(inline) = call(&controller, &PLAIN, run(library::INLINE)) else {
        panic!("not inline");
    };
    assert_eq!(inline.fields.len(), 1);
    let Reply::RecordsPage(none) = call(&controller, &PLAIN, run(library::NONE)) else {
        panic!("not records");
    };
    assert!(none.records.is_empty() && none.page_token.is_empty());
    assert_eq!(library.held.get(), 0);
}

#[test]
fn the_format_of_every_action_is_discoverable() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::ListActions(list) = call(
        &controller,
        &PLAIN,
        Call::ListActions(pb::ListActionsRequest::default()),
    ) else {
        panic!("not a list");
    };
    let formats: std::vec::Vec<_> = list.actions.iter().map(|a| a.result_format).collect();
    assert_eq!(
        formats,
        [
            pb::ResultFormat::Records,
            pb::ResultFormat::Records,
            pb::ResultFormat::Records,
            pb::ResultFormat::Records,
            pb::ResultFormat::Inline,
            pb::ResultFormat::Empty,
            pb::ResultFormat::Records,
            pb::ResultFormat::Records,
        ]
    );
}

#[test]
fn a_collection_is_read_page_by_page_from_one_run() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::LIST)) else {
        panic!("not records");
    };
    assert_eq!(titles(&first).len(), limits::MAX_RECORDS);
    assert_eq!(titles(&first)[0], "book0");
    assert!(!first.page_token.is_empty());

    let read = |token: &[u8]| match call(&controller, &PLAIN, page_of(library::LIST, token)) {
        Reply::RecordsPage(page) => page,
        other => panic!("{other:?}"),
    };
    let second = read(&first.page_token);
    assert_eq!(titles(&second), ["book6", "book7", "book8", "book9"]);
    // The last page has no token.
    assert!(second.page_token.is_empty());
    // A repeated read of the same token returns the same page.
    assert_eq!(read(&first.page_token), second);
    // The action ran once: later pages never run it again.
    assert_eq!(library.runs.get(), 1);

    // The snapshot is held until it is closed, also after the last page.
    assert_eq!(library.held.get(), library::SNAPSHOT);
    assert!(matches!(
        call(
            &controller,
            &PLAIN,
            close_page_of(library::LIST, &first.page_token)
        ),
        Reply::CloseResultPage(_)
    ));
    assert_eq!((library.held.get(), library.closed.get()), (0, 1));
    // A closed snapshot returns nothing, and closing twice is refused.
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            page_of(library::LIST, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            close_page_of(library::LIST, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_collection_that_fits_the_first_page_holds_nothing() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(page) = call(&controller, &PLAIN, run(library::SHORT)) else {
        panic!("not records");
    };
    assert_eq!(titles(&page).len(), 4);
    assert!(page.page_token.is_empty());
    // Released at once: the client has everything and nothing to close.
    assert_eq!((library.held.get(), library.closed.get()), (0, 1));
}

#[test]
fn one_snapshot_is_held_at_a_time() {
    let library = library();
    let controller = library_controller(&library);
    assert!(matches!(
        call(&controller, &PLAIN, run(library::LIST)),
        Reply::RecordsPage(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &OTHER, run(library::LIST))),
        pb::ErrorCode::Busy
    );
}

#[test]
fn a_page_cannot_be_read_or_closed_by_another_session() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::LIST)) else {
        panic!("not records");
    };
    let token = first.page_token.as_slice();
    // Another session learns nothing, not even that the snapshot exists.
    for request in [
        page_of(library::LIST, token),
        close_page_of(library::LIST, token),
    ] {
        assert_eq!(
            error_code(&call(&controller, &OTHER, request)),
            pb::ErrorCode::NotFound
        );
    }
    assert_eq!(library.reads.get(), 1);
    assert_eq!(library.held.get(), library::SNAPSHOT);
    // The owner still has it.
    assert!(matches!(
        call(&controller, &PLAIN, page_of(library::LIST, token)),
        Reply::RecordsPage(_)
    ));
}

#[test]
fn invalid_foreign_and_out_of_range_tokens_return_nothing() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::LIST)) else {
        panic!("not records");
    };
    let good = first.page_token.to_vec();
    let with = |snapshot: u32, next: u16| {
        let mut token = snapshot.to_le_bytes().to_vec();
        token.extend_from_slice(&next.to_le_bytes());
        token
    };
    let bad: std::vec::Vec<(u32, std::vec::Vec<u8>)> = std::vec![
        (library::LIST, std::vec::Vec::new()),
        (library::LIST, good[..5].to_vec()),
        // Another snapshot, a position at or beyond the end.
        (library::LIST, with(library::SNAPSHOT + 1, 6)),
        (library::LIST, with(library::SNAPSHOT, library::TOTAL)),
        (library::LIST, with(library::SNAPSHOT, u16::MAX)),
        // The right token of another action.
        (library::LIST_PRIVATE, good.clone()),
        (library::NONE, good.clone()),
    ];
    for (action, token) in &bad {
        for request in [page_of(*action, token), close_page_of(*action, token)] {
            assert_eq!(
                error_code(&call(&controller, &PLAIN, request)),
                pb::ErrorCode::NotFound,
                "{action} {token:?}"
            );
        }
    }
    // Nothing was read or released by any of them.
    assert_eq!(library.reads.get(), 1);
    assert_eq!(library.held.get(), library::SNAPSHOT);
    // A position inside the result is valid: a client may resume anywhere.
    let Reply::RecordsPage(middle) = call(
        &controller,
        &PLAIN,
        page_of(library::LIST, &with(library::SNAPSHOT, 8)),
    ) else {
        panic!("not records");
    };
    assert_eq!(titles(&middle), ["book8", "book9"]);
}

#[test]
fn a_page_needs_the_privilege_of_its_action() {
    let library = library();
    let controller = Controller::new(
        Device::new(INFO, uptime, (), (), (&library,)),
        RequireEncryption,
    );
    let encrypted = LinkSecurity {
        encrypted: true,
        authenticated: false,
        session: 1,
        transport: Transport::Ble,
    };
    assert_eq!(
        error_code(&call(&controller, &PLAIN, run(library::LIST_PRIVATE))),
        pb::ErrorCode::PermissionDenied
    );
    let Reply::RecordsPage(first) = call(&controller, &encrypted, run(library::LIST_PRIVATE))
    else {
        panic!("not records");
    };
    let token = first.page_token.as_slice();
    // The same session on a link that lost its encryption is refused.
    let downgraded = LinkSecurity {
        encrypted: false,
        ..encrypted
    };
    for request in [
        page_of(library::LIST_PRIVATE, token),
        close_page_of(library::LIST_PRIVATE, token),
    ] {
        assert_eq!(
            error_code(&call(&controller, &downgraded, request)),
            pb::ErrorCode::PermissionDenied
        );
    }
    assert_eq!(library.reads.get(), 1);
    assert!(matches!(
        call(
            &controller,
            &encrypted,
            page_of(library::LIST_PRIVATE, token)
        ),
        Reply::RecordsPage(_)
    ));
}

#[test]
fn a_closed_connection_and_a_lost_link_release_the_snapshot() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::LIST)) else {
        panic!("not records");
    };
    // Another connection ending changes nothing.
    block_on(controller.session_closed(OTHER.session));
    assert_eq!(library.held.get(), library::SNAPSHOT);
    block_on(controller.session_closed(PLAIN.session));
    assert_eq!((library.held.get(), library.closed.get()), (0, 1));
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            page_of(library::LIST, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );

    // A lost link releases it in the service and forgets the owner.
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::LIST)) else {
        panic!("not records");
    };
    controller.link_lost();
    assert_eq!(library.held.get(), 0);
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            page_of(library::LIST, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_new_session_cannot_use_a_snapshot_the_service_forgot() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::LIST)) else {
        panic!("not records");
    };
    // The service times it out by itself; the next read finds it gone.
    library.held.set(0);
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            page_of(library::LIST, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );
    // Its id is free again and the old token is not that snapshot.
    assert!(matches!(
        call(&controller, &OTHER, run(library::LIST)),
        Reply::RecordsPage(_)
    ));
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            page_of(library::LIST, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_page_that_cannot_advance_is_an_error_and_releases_the_snapshot() {
    let library = library();
    let controller = library_controller(&library);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, run(library::STUCK))),
        pb::ErrorCode::Internal
    );
    assert_eq!((library.held.get(), library.closed.get()), (0, 1));
}

#[test]
fn a_page_holds_the_records_that_fit_and_never_splits_one() {
    let record = |len: usize| {
        let mut record = rohi_service::Record::new();
        record
            .push((1, Value::text(&"x".repeat(len)).unwrap()))
            .ok();
        record
    };
    let mut records = rohi_service::Records::new();
    for _ in 0..limits::MAX_RECORDS {
        records.push(record(40)).ok().unwrap();
    }
    // Everything fits the response, with the longest token.
    assert_eq!(
        controller::fit_within(&records, limits::MAX_RESPONSE_LEN),
        6
    );
    // A smaller room takes a prefix of whole records: the encoded size of the
    // page decides.
    let room = |count| controller::records_size(&records, count) + controller::ENVELOPE;
    assert!(room(2) < room(3));
    assert_eq!(controller::fit_within(&records, room(3)), 3);
    assert_eq!(controller::fit_within(&records, room(3) - 1), 2);
    // Not even one record fits: no page, the caller fails instead of looping.
    assert_eq!(controller::fit_within(&records, 10), 0);
}

#[test]
fn two_sessions_may_hold_a_view_with_the_same_id_and_neither_loses_its_pages() {
    let library = library();
    let controller = library_controller(&library);
    let Reply::RecordsPage(first) = call(&controller, &PLAIN, run(library::VIEW)) else {
        panic!("not records");
    };
    let Reply::RecordsPage(second) = call(&controller, &OTHER, run(library::VIEW)) else {
        panic!("not records");
    };
    assert_eq!(first.page_token, second.page_token);
    // The second session did not take the first one's entry over.
    for link in [&PLAIN, &OTHER] {
        assert!(matches!(
            call(&controller, link, page_of(library::VIEW, &first.page_token)),
            Reply::RecordsPage(_)
        ));
    }
    // Closing is per session too.
    assert!(matches!(
        call(
            &controller,
            &PLAIN,
            close_page_of(library::VIEW, &first.page_token)
        ),
        Reply::CloseResultPage(_)
    ));
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            page_of(library::VIEW, &first.page_token)
        )),
        pb::ErrorCode::NotFound
    );
    assert!(matches!(
        call(
            &controller,
            &OTHER,
            page_of(library::VIEW, &first.page_token)
        ),
        Reply::RecordsPage(_)
    ));
}

#[test]
fn a_link_always_names_its_transport_and_starts_unprotected() {
    for transport in [Transport::Ble, Transport::Tcp] {
        let link = LinkSecurity::new(transport);
        assert_eq!(
            (
                link.transport,
                link.encrypted,
                link.authenticated,
                link.session
            ),
            (transport, false, false, 0)
        );
    }
}
