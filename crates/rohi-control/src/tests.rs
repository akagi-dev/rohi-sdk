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
    ActionDescriptor, ActionError, ActionOutput, ConfigField, FieldDescriptor, FieldError, Issue,
    IssueSeverity, ManagedConfig, ManagedSection, ManagedService, SectionStore, ServiceHealth,
    ServiceState, ServiceSummary, StatusWriter, StoreError, Tracked, Value, ValueKind, WriteError,
};

use crate::*;

#[derive(Clone, Debug, PartialEq)]
struct Net {
    enabled: bool,
    port: u64,
    host: std::string::String,
    token: Option<std::string::String>,
}

impl ManagedConfig for Net {
    const NAME: &'static str = "net";
    const VERSION: u16 = 2;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(1, "enabled", ValueKind::Bool),
        ConfigField::new(2, "port", ValueKind::U64).range(1.0, 65535.0),
        ConfigField::new(3, "host", ValueKind::String),
        ConfigField::new(4, "token", ValueKind::String)
            .secret()
            .optional(),
    ];

    fn defaults() -> Self {
        Self {
            enabled: false,
            port: 80,
            host: std::string::String::new(),
            token: None,
        }
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        Ok(match id {
            1 => Some(Value::Bool(self.enabled)),
            2 => Some(Value::U64(self.port)),
            3 => Value::text(&self.host),
            4 => self.token.as_deref().and_then(Value::text),
            _ => return Err(FieldError::UnknownField),
        })
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            1 => self.enabled = value.as_bool().unwrap(),
            2 => self.port = value.as_u64().unwrap(),
            3 => self.host = value.as_str().unwrap().into(),
            4 => self.token = Some(value.as_str().unwrap().into()),
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        assert_eq!(id, 4);
        self.token = None;
        Ok(())
    }

    fn validate(&self) -> Result<(), FieldError> {
        if self.enabled && self.host.is_empty() {
            return Err(FieldError::Invalid);
        }
        Ok(())
    }
}

#[derive(Default)]
struct Memory(RefCell<Option<Net>>);

impl SectionStore<Net> for &Memory {
    async fn load(&self) -> Result<Option<Net>, StoreError> {
        Ok(self.0.borrow().clone())
    }

    async fn store(&self, config: &Net) -> Result<(), StoreError> {
        *self.0.borrow_mut() = Some(config.clone());
        Ok(())
    }

    async fn remove(&self) -> Result<(), StoreError> {
        *self.0.borrow_mut() = None;
        Ok(())
    }
}

struct Radio {
    degraded: RefCell<bool>,
    log: RefCell<std::vec::Vec<std::string::String>>,
}

impl ManagedService for &Radio {
    const NAME: &'static str = "radio";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "ssid", ValueKind::String),
        FieldDescriptor::new(2, "rssi", ValueKind::I64).unit("dBm"),
    ];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new("scan")
            .describe("Scan for networks")
            .results(&[FieldDescriptor::new(1, "ssid", ValueKind::String)]),
        ActionDescriptor::new("reconnect").privileged(),
        ActionDescriptor::new("provision")
            .args(&[FieldDescriptor::new(1, "secret", ValueKind::Bytes)])
            .secret(),
    ];

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
                code: "link_lost",
                message: "The link was lost",
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.text(1, "office")?;
        out.i64(2, -61)
    }

    async fn invoke(
        &self,
        action: &str,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        self.log.borrow_mut().push(action.into());
        match action {
            "scan" => {
                for name in ["a", "b"] {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    out.text(1, name).map_err(|_| ActionError::Internal)?;
                }
                Ok(())
            }
            "reconnect" => Err(ActionError::Rejected("not_configured")),
            "provision" => {
                assert_eq!(args[0].1.as_bytes().unwrap().len(), 4);
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

type Section<'a> = ManagedSection<Net, &'a Memory, NoopRawMutex>;
type TestDevice<'a> = Device<(Section<'a>,), (Tracked<&'a Radio, NoopRawMutex>,)>;

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
        (Tracked::new(radio),),
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

fn string_value(value: &str) -> pb::Value {
    pb::Value {
        value: Some(pb::Value_::Value::StringValue(text(value))),
    }
}

fn mutation(id: u32, value: pb::Value) -> pb::ConfigMutation {
    pb::ConfigMutation {
        field_id: id,
        operation: Some(pb::ConfigMutation_::Operation::Set(value)),
    }
}

fn patch(section: &str, mutations: &[pb::ConfigMutation]) -> pb::PatchConfigSectionRequest {
    let mut list = Vec::new();
    for mutation in mutations {
        list.push(mutation.clone()).ok().unwrap();
    }
    pb::PatchConfigSectionRequest {
        section: text(section),
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
    let controller = Controller::new(Device::new(INFO, uptime, (), ()), OpenPolicy);
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
    let get = Call::GetServiceStatus(pb::GetServiceStatusRequest {
        service: text("radio"),
        ..Default::default()
    });
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
        Call::GetConfigSchema(pb::GetConfigSchemaRequest {
            section: text("net"),
            ..Default::default()
        }),
    ) else {
        panic!()
    };
    let schema = schema.schema().unwrap();
    assert_eq!(schema.fields.len(), 4);
    let port = &schema.fields[1];
    assert_eq!(port.field().unwrap().name.as_str(), "port");
    assert_eq!(port.min(), Some(&1.0));
    assert_eq!(port.max(), Some(&65535.0));
    assert!(schema.fields[3].secret && schema.fields[3].optional);

    let missing = Call::GetConfigSchema(pb::GetConfigSchemaRequest {
        section: text("missing"),
        ..Default::default()
    });
    assert_eq!(
        error_code(&call(&controller, &PLAIN, missing)),
        pb::ErrorCode::NotFound
    );
}

#[test]
fn a_patch_is_atomic_and_secrets_are_never_returned() {
    controller!(controller, OpenPolicy);
    let ok = patch(
        "net",
        &[
            mutation(
                1,
                pb::Value {
                    value: Some(pb::Value_::Value::BoolValue(true)),
                },
            ),
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
        Call::GetConfigSection(pb::GetConfigSectionRequest {
            section: text("net"),
        }),
    ) else {
        panic!()
    };
    assert!(section.configured);
    assert_eq!(section.revision, 1);
    let token = section.entries.iter().find(|e| e.field_id == 4).unwrap();
    assert!(matches!(
        token.state,
        Some(pb::ConfigEntry_::State::Redacted(_))
    ));
    let host = section.entries.iter().find(|e| e.field_id == 3).unwrap();
    assert!(matches!(
        &host.state,
        Some(pb::ConfigEntry_::State::Value(pb::Value {
            value: Some(pb::Value_::Value::StringValue(v))
        })) if v.as_str() == "ingress.example.org"
    ));

    // One bad mutation rejects the whole patch.
    let bad = patch(
        "net",
        &[
            mutation(3, string_value("other.example.org")),
            mutation(
                2,
                pb::Value {
                    value: Some(pb::Value_::Value::U64Value(0)),
                },
            ),
        ],
    );
    assert_eq!(
        error_code(&call(&controller, &PLAIN, Call::PatchConfigSection(bad))),
        pb::ErrorCode::InvalidArgument
    );
    let Reply::GetConfigSection(section) = call(
        &controller,
        &PLAIN,
        Call::GetConfigSection(pb::GetConfigSectionRequest {
            section: text("net"),
        }),
    ) else {
        panic!()
    };
    assert_eq!(section.revision, 1);
}

#[test]
fn patch_conflicts_dry_runs_unset_and_reset() {
    controller!(controller, OpenPolicy);
    let mut stale = patch("net", &[mutation(3, string_value("a.io"))]);
    stale.set_expected_revision(5);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, Call::PatchConfigSection(stale))),
        pb::ErrorCode::Conflict
    );

    let mut dry = patch("net", &[mutation(3, string_value("a.io"))]);
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
            Call::GetConfigSection(pb::GetConfigSectionRequest {
                section: text("net"),
            }),
        )
    };
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert!(!section.configured);

    let set = patch(
        "net",
        &[
            mutation(3, string_value("a.io")),
            mutation(4, string_value("t")),
        ],
    );
    call(&controller, &PLAIN, Call::PatchConfigSection(set));
    let unset = pb::ConfigMutation {
        field_id: 4,
        operation: Some(pb::ConfigMutation_::Operation::Unset(pb::Empty {})),
    };
    call(
        &controller,
        &PLAIN,
        Call::PatchConfigSection(patch("net", &[unset])),
    );
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert!(section.entries.iter().all(|e| e.field_id != 4));

    let Reply::ResetConfigSection(done) = call(
        &controller,
        &PLAIN,
        Call::ResetConfigSection(pb::ResetConfigSectionRequest {
            section: text("net"),
            dry_run: false,
        }),
    ) else {
        panic!()
    };
    assert_eq!(done.revision, 3);
    let Reply::GetConfigSection(section) = get() else {
        panic!()
    };
    assert!(!section.configured);
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
        Call::PatchConfigSection(patch("net", &[mutation(3, string_value("a.io"))])),
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
        Call::GetConfigSection(pb::GetConfigSectionRequest {
            section: text("net"),
        }),
    ) else {
        panic!()
    };
    assert!(!section.configured);
}

#[test]
fn reboot_needs_the_hook_and_a_secure_link() {
    std::thread_local!(static REBOOTS: Cell<u32> = const { Cell::new(0) });
    fn reboot() {
        REBOOTS.with(|count| count.set(count.get() + 1));
    }
    let request = || Call::Reboot(pb::RebootRequest {});

    let controller = Controller::new(Device::new(INFO, uptime, (), ()), OpenPolicy);
    assert_eq!(
        error_code(&call(&controller, &PLAIN, request())),
        pb::ErrorCode::NotSupported
    );

    let device = Device::new(INFO, uptime, (), ()).with_reboot(reboot);
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
    let empty = mutation(3, pb::Value { value: None });
    assert_eq!(
        error_code(&call(
            &controller,
            &PLAIN,
            Call::PatchConfigSection(patch("net", &[empty]))
        )),
        pb::ErrorCode::InvalidArgument
    );
}

#[test]
fn service_status_is_compact_in_the_summary_and_detailed_per_service() {
    controller!(controller, OpenPolicy);
    let Reply::ListServices(list) = call(
        &controller,
        &PLAIN,
        Call::ListServices(pb::ListServicesRequest {}),
    ) else {
        panic!()
    };
    assert_eq!(list.services[0].name.as_str(), "radio");

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
    assert_eq!(summary.service.as_str(), "radio");
    assert_eq!(summary.state, pb::ServiceState::Running);
    assert!(summary.ready);
    assert_eq!(summary.revision, 1);

    let Reply::GetServiceSchema(schema) = call(
        &controller,
        &PLAIN,
        Call::GetServiceSchema(pb::GetServiceSchemaRequest {
            service: text("radio"),
            ..Default::default()
        }),
    ) else {
        panic!()
    };
    assert_eq!(schema.fields.len(), 2);
    assert_eq!(schema.fields[1].unit.as_str(), "dBm");

    let Reply::GetServiceStatus(status) = call(
        &controller,
        &PLAIN,
        Call::GetServiceStatus(pb::GetServiceStatusRequest {
            service: text("radio"),
            ..Default::default()
        }),
    ) else {
        panic!()
    };
    assert_eq!(status.fields.len(), 2);
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
    assert_eq!(
        all.services[0].reason().map(|reason| reason.as_str()),
        Some("link_lost")
    );
    let Reply::GetServiceStatus(status) = call(
        &controller,
        &PLAIN,
        Call::GetServiceStatus(pb::GetServiceStatusRequest {
            service: text("radio"),
            ..Default::default()
        }),
    ) else {
        panic!()
    };
    assert_eq!(status.issues[0].code.as_str(), "link_lost");
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
        .map(|a| (a.service.as_str(), a.name.as_str(), a.privileged))
        .collect();
    assert_eq!(
        names,
        [
            ("radio", "scan", false),
            ("radio", "reconnect", true),
            ("radio", "provision", true)
        ]
    );

    let Reply::GetActionSchema(schema) = call(
        &controller,
        &PLAIN,
        Call::GetActionSchema(pb::GetActionSchemaRequest {
            service: text("radio"),
            action: text("provision"),
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
            service: text("radio"),
            action: text("scan"),
            args: Vec::new(),
        }),
    ) else {
        panic!()
    };
    assert_eq!(done.records.len(), 2);
    assert!(matches!(
        done.records[1].fields[0].value().unwrap().value,
        Some(pb::Value_::Value::StringValue(ref s)) if s.as_str() == "b"
    ));
}

#[test]
fn action_failures_are_typed() {
    controller!(controller, OpenPolicy);
    let invoke = |service: &str, action: &str, args: Vec<pb::FieldValue, 3>| {
        call(
            &controller,
            &PLAIN,
            Call::InvokeAction(pb::InvokeActionRequest {
                service: text(service),
                action: text(action),
                args,
            }),
        )
    };
    assert_eq!(
        error_code(&invoke("radio", "missing", Vec::new())),
        pb::ErrorCode::NotFound
    );
    assert_eq!(
        error_code(&invoke("missing", "scan", Vec::new())),
        pb::ErrorCode::NotFound
    );
    // `provision` needs its argument.
    assert_eq!(
        error_code(&invoke("radio", "provision", Vec::new())),
        pb::ErrorCode::InvalidArgument
    );
    let Reply::Error(rejected) = invoke("radio", "reconnect", Vec::new()) else {
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

    let write = || Call::PatchConfigSection(patch("net", &[mutation(3, string_value("a.io"))]));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, write())),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &encrypted, write()),
        Reply::PatchConfigSection(_)
    ));

    let reset = Call::ResetConfigSection(pb::ResetConfigSectionRequest {
        section: text("net"),
        dry_run: true,
    });
    assert_eq!(
        error_code(&call(&controller, &PLAIN, reset)),
        pb::ErrorCode::PermissionDenied
    );

    let action = |name: &str, args: Vec<pb::FieldValue, 3>| {
        Call::InvokeAction(pb::InvokeActionRequest {
            service: text("radio"),
            action: text(name),
            args,
        })
    };
    // An unprivileged action is open; a privileged one needs encryption.
    assert!(matches!(
        call(&controller, &PLAIN, action("scan", Vec::new())),
        Reply::InvokeAction(_)
    ));
    assert_eq!(
        error_code(&call(&controller, &PLAIN, action("reconnect", Vec::new()))),
        pb::ErrorCode::PermissionDenied
    );

    // Secret arguments need the strongest link, and the policy runs before
    // the action does.
    let mut args = Vec::new();
    let mut secret = pb::FieldValue {
        field_id: 1,
        ..Default::default()
    };
    secret.set_value(pb::Value {
        value: Some(pb::Value_::Value::BytesValue(
            Vec::from_slice(&[1, 2, 3, 4]).unwrap(),
        )),
    });
    args.push(secret).ok().unwrap();
    assert_eq!(
        error_code(&call(
            &controller,
            &encrypted,
            action("provision", args.clone())
        )),
        pb::ErrorCode::PermissionDenied
    );
    assert!(matches!(
        call(&controller, &strong, action("provision", args)),
        Reply::InvokeAction(_)
    ));
}

#[test]
fn policy_rejections_do_not_reach_the_service() {
    let memory = Memory::default();
    let radio = radio();
    let controller = Controller::new(device(&memory, &radio), RequireEncryption);
    let reconnect = Call::InvokeAction(pb::InvokeActionRequest {
        service: text("radio"),
        action: text("reconnect"),
        args: Vec::new(),
    });
    call(&controller, &PLAIN, reconnect);
    assert!(radio.log.borrow().is_empty());
}

struct Many;

impl ManagedService for Many {
    const NAME: &'static str = "many";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "f1", ValueKind::U64),
        FieldDescriptor::new(2, "f2", ValueKind::U64),
        FieldDescriptor::new(3, "f3", ValueKind::U64),
        FieldDescriptor::new(4, "f4", ValueKind::U64),
        FieldDescriptor::new(5, "f5", ValueKind::U64),
        FieldDescriptor::new(6, "f6", ValueKind::U64),
        FieldDescriptor::new(7, "f7", ValueKind::U64),
        FieldDescriptor::new(8, "f8", ValueKind::U64),
        FieldDescriptor::new(9, "f9", ValueKind::U64),
        FieldDescriptor::new(10, "f10", ValueKind::U64),
    ];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new("a"),
        ActionDescriptor::new("b"),
        ActionDescriptor::new("c"),
        ActionDescriptor::new("d"),
        ActionDescriptor::new("e"),
    ];

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        for id in 1..=10 {
            out.u64(id, u64::from(id) * 100)?;
        }
        Ok(())
    }
}

#[test]
fn long_schemas_status_and_action_lists_are_paged() {
    let controller = Controller::new(
        Device::new(
            INFO,
            uptime,
            (),
            (
                Tracked::<_, NoopRawMutex>::new(Many),
                Tracked::<_, NoopRawMutex>::new(Many2),
            ),
        ),
        OpenPolicy,
    );
    let schema = |offset| {
        let Reply::GetServiceSchema(page) = call(
            &controller,
            &PLAIN,
            Call::GetServiceSchema(pb::GetServiceSchemaRequest {
                service: text("many"),
                offset,
            }),
        ) else {
            panic!()
        };
        page
    };
    let first = schema(0);
    assert_eq!(first.total, 10);
    assert_eq!(first.fields.len(), limits::MAX_SCHEMA_PAGE);
    let mut names: std::vec::Vec<std::string::String> = first
        .fields
        .iter()
        .map(|f| f.name.as_str().into())
        .collect();
    let mut offset = names.len() as u32;
    while offset < first.total {
        let page = schema(offset);
        names.extend(
            page.fields
                .iter()
                .map(|f| std::string::String::from(f.name.as_str())),
        );
        offset = names.len() as u32;
    }
    assert_eq!(names.len(), 10);
    assert_eq!(names[9], "f10");

    let status = |offset| {
        let Reply::GetServiceStatus(page) = call(
            &controller,
            &PLAIN,
            Call::GetServiceStatus(pb::GetServiceStatusRequest {
                service: text("many"),
                offset,
            }),
        ) else {
            panic!()
        };
        page
    };
    let first = status(0);
    assert_eq!(first.total, 10);
    assert_eq!(first.fields.len(), limits::MAX_STATUS_PAGE);
    let second = status(first.fields.len() as u32);
    assert_eq!(second.fields.len(), 2);
    assert_eq!(second.fields[1].field_id, 10);
    // Every page repeats the compact status.
    assert_eq!(second.summary().unwrap().service.as_str(), "many");

    let actions = |offset| {
        let Reply::ListActions(page) = call(
            &controller,
            &PLAIN,
            Call::ListActions(pb::ListActionsRequest { offset }),
        ) else {
            panic!()
        };
        page
    };
    let first = actions(0);
    assert_eq!(first.total, 10);
    assert_eq!(first.actions.len(), limits::MAX_ACTIONS_PAGE);
    let rest = actions(limits::MAX_ACTIONS_PAGE as u32);
    assert_eq!(rest.actions.len(), 2);
    assert_eq!(rest.actions[1].service.as_str(), "many2");
}

struct Many2;

impl ManagedService for Many2 {
    const NAME: &'static str = "many2";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new("a"),
        ActionDescriptor::new("b"),
        ActionDescriptor::new("c"),
        ActionDescriptor::new("d"),
        ActionDescriptor::new("e"),
    ];

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, _: &mut impl StatusWriter) -> Result<(), WriteError> {
        Ok(())
    }
}
