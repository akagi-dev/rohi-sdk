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
use heapless::Vec;
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_service::{
    ActionError, ActionRegistry, ConfigError, ConfigRegistry, MAX_ACTION_ARGS, MAX_MUTATIONS,
    Mutation, ServiceStatusSummary, StatusError, StatusRegistry, Value,
};

use crate::mapping;
use crate::{AuthorizationPolicy, ControlBackend, LinkSecurity, Privilege};

/// Dispatches control requests to a [`ControlBackend`].
pub struct Controller<B, P> {
    backend: B,
    policy: P,
}

impl<B: ControlBackend, P: AuthorizationPolicy> Controller<B, P> {
    /// A dispatcher over `backend`, gated by `policy`.
    pub const fn new(backend: B, policy: P) -> Self {
        Self { backend, policy }
    }

    /// The wrapped backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Handles one encoded request received on a link with the given
    /// security and writes exactly one encoded response to `response`.
    ///
    /// Returns the response length. Every failure, including malformed
    /// requests and an undersized `response` buffer, is answered with a typed
    /// error response; the transport is never expected to disconnect. `0` is
    /// returned only if not even an error fits into `response`.
    pub async fn handle(&self, link: &LinkSecurity, request: &[u8], response: &mut [u8]) -> usize {
        let (request_id, reply) = self.dispatch(link, request).await;
        let message = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Some(reply),
        };
        if let Ok(len) = encode(&message, response) {
            return len;
        }
        let fallback = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Some(error(pb::ErrorCode::Internal)),
        };
        encode(&fallback, response).unwrap_or(0)
    }

    async fn dispatch(&self, link: &LinkSecurity, bytes: &[u8]) -> (u32, Reply) {
        if bytes.len() > limits::MAX_REQUEST_LEN {
            return (0, error(pb::ErrorCode::InvalidArgument));
        }
        let Ok(request) = decode::<pb::Request>(bytes) else {
            return (0, error(pb::ErrorCode::InvalidArgument));
        };
        let id = request.request_id;
        if request.protocol_version != PROTOCOL_VERSION {
            return (id, error(pb::ErrorCode::NotSupported));
        }
        let Some(call) = request.request else {
            return (id, error(pb::ErrorCode::NotSupported));
        };

        let privilege = match &call {
            Call::PatchConfigSection(_) | Call::ResetConfigSection(_) => Privilege::Mutate,
            Call::FactoryReset(_) => Privilege::Provision,
            Call::Reboot(_) => Privilege::Mutate,
            Call::InvokeAction(invoke) => {
                match self
                    .backend
                    .actions()
                    .action(invoke.service_id, invoke.action_id)
                {
                    Ok(action) if action.secret_args => Privilege::Provision,
                    Ok(action) if action.privileged => Privilege::Mutate,
                    Ok(_) => Privilege::Read,
                    Err(e) => return (id, self.action_failure(e, invoke.service_id)),
                }
            }
            _ => Privilege::Read,
        };
        if !self.policy.allows(privilege, link) {
            return (id, error(pb::ErrorCode::PermissionDenied));
        }
        (id, self.execute(call).await)
    }

    async fn execute(&self, call: Call) -> Reply {
        let backend = &self.backend;
        match call {
            Call::GetInfo(_) => {
                let info = backend.device_info();
                Reply::GetInfo(pb::GetInfoResponse {
                    firmware: mapping::text(info.firmware),
                    board: mapping::text(info.board),
                })
            }
            Call::ListConfigSections(_) => {
                let config = backend.config();
                let mut sections = Vec::new();
                for index in 0..config.section_count() {
                    if let Some(info) = config.section_info(index)
                        && sections.push(mapping::section_info(&info)).is_err()
                    {
                        break;
                    }
                }
                Reply::ListConfigSections(pb::ListConfigSectionsResponse { sections })
            }
            Call::GetConfigSchema(request) => match backend.config().schema(request.section_id) {
                None => error(pb::ErrorCode::NotFound),
                Some(schema) => {
                    let mut fields = Vec::new();
                    for field in schema.fields {
                        if fields.push(mapping::config_field(field)).is_err() {
                            break;
                        }
                    }
                    let mut response = pb::GetConfigSchemaResponse {
                        fields,
                        enums: mapping::enums(schema.enums),
                        ..Default::default()
                    };
                    response.set_section(mapping::section_info(&schema.section));
                    Reply::GetConfigSchema(response)
                }
            },
            Call::GetConfigSection(request) => {
                match backend.config().get(request.section_id).await {
                    Err(e) => config_error(e),
                    Ok(values) => {
                        let mut entries = Vec::new();
                        for entry in &values.entries {
                            if entries.push(mapping::config_entry(entry)).is_err() {
                                break;
                            }
                        }
                        let mut overridden = Vec::new();
                        for id in &values.overridden {
                            if overridden.push(*id).is_err() {
                                break;
                            }
                        }
                        Reply::GetConfigSection(pb::GetConfigSectionResponse {
                            section_id: values.section.id,
                            version: values.section.version,
                            revision: values.section.revision,
                            effective: entries,
                            overridden,
                        })
                    }
                }
            }
            Call::PatchConfigSection(request) => self.patch(request).await,
            Call::ResetConfigSection(request) => {
                match backend
                    .config()
                    .reset(
                        request.section_id,
                        request.expected_revision().copied(),
                        request.dry_run,
                    )
                    .await
                {
                    Ok(done) => Reply::ResetConfigSection(pb::ResetConfigSectionResponse {
                        revision: done.revision,
                        restart_required: done.restart_required,
                    }),
                    Err(e) => config_error(e),
                }
            }
            Call::FactoryReset(_) => match backend.config().factory_reset().await {
                Ok(done) => Reply::FactoryReset(pb::FactoryResetResponse {
                    restart_required: done.restart_required,
                }),
                Err(e) => config_error(e),
            },
            Call::Reboot(_) if backend.reboot() => Reply::Reboot(pb::RebootResponse {}),
            Call::Reboot(_) => error(pb::ErrorCode::NotSupported),
            Call::GetServicesStatus(_) => {
                let services = backend.statuses();
                let now = backend.uptime_ms();
                let mut summaries: Vec<
                    ServiceStatusSummary,
                    { rohi_service::MAX_REGISTRY_ENTRIES },
                > = Vec::new();
                for index in 0..services.service_count() {
                    if let Some(summary) = services.summary(index, now).await
                        && summaries.push(summary).is_err()
                    {
                        break;
                    }
                }
                let mut wire = Vec::new();
                for summary in &summaries {
                    if wire.push(mapping::summary(summary)).is_err() {
                        break;
                    }
                }
                Reply::GetServicesStatus(pb::GetServicesStatusResponse {
                    uptime_ms: now,
                    health: mapping::device_health(backend.device_health(&summaries)),
                    services: wire,
                })
            }
            Call::GetServiceSchema(request) => {
                match backend.statuses().schema(request.service_id) {
                    None => self.missing_status(request.service_id),
                    Some(schema) => Reply::GetServiceSchema(pb::GetServiceSchemaResponse {
                        service_id: schema.info.id,
                        name: mapping::text(schema.info.name),
                        version: schema.info.version,
                        fields: mapping::fields(schema.fields),
                        enums: mapping::enums(schema.enums),
                        issues: mapping::issue_descriptors(schema.issues),
                    }),
                }
            }
            Call::GetServiceStatus(request) => {
                match backend
                    .statuses()
                    .status(request.service_id, backend.uptime_ms())
                    .await
                {
                    Err(StatusError::UnknownService) => self.missing_status(request.service_id),
                    Ok(status) => {
                        let mut fields = Vec::new();
                        for (id, value) in &status.fields {
                            if fields.push(mapping::field_value(*id, value)).is_err() {
                                break;
                            }
                        }
                        let mut issues = Vec::new();
                        for issue in &status.issues {
                            if issues.push(mapping::issue(issue)).is_err() {
                                break;
                            }
                        }
                        Reply::GetServiceStatus(pb::GetServiceStatusResponse {
                            service_id: status.summary.id,
                            revision: status.summary.revision,
                            fields,
                            issues,
                        })
                    }
                }
            }
            Call::ListActions(_) => {
                let services = backend.actions();
                let mut actions = Vec::new();
                'services: for index in 0..services.service_count() {
                    let Some(info) = services.service_identity(index) else {
                        continue;
                    };
                    for action in services.service_actions(index) {
                        if actions
                            .push(mapping::action_info(info.id, info.name, action))
                            .is_err()
                        {
                            break 'services;
                        }
                    }
                }
                Reply::ListActions(pb::ListActionsResponse { actions })
            }
            Call::GetActionSchema(request) => {
                match backend
                    .actions()
                    .action(request.service_id, request.action_id)
                {
                    Err(e) => self.action_failure(e, request.service_id),
                    Ok(action) => {
                        let mut out = pb::GetActionSchemaResponse {
                            args: mapping::fields(action.args),
                            results: mapping::fields(action.results),
                            enums: mapping::enums(action.enums),
                            ..Default::default()
                        };
                        out.set_action(mapping::action_info(
                            request.service_id,
                            self.action_service_name(request.service_id),
                            action,
                        ));
                        Reply::GetActionSchema(out)
                    }
                }
            }
            Call::InvokeAction(request) => self.invoke(request).await,
        }
    }

    /// The name of an action service the registry has just resolved.
    fn action_service_name(&self, service: u32) -> &'static str {
        let actions = self.backend.actions();
        (0..actions.service_count())
            .filter_map(|index| actions.service_identity(index))
            .find(|identity| identity.id == service)
            .map_or("", |identity| identity.name)
    }

    /// A status request for a service that has no status: "not supported" when
    /// the service exists with other capabilities, "not found" otherwise.
    fn missing_status(&self, service: u32) -> Reply {
        if self.backend.actions().contains(service) {
            error(pb::ErrorCode::NotSupported)
        } else {
            error(pb::ErrorCode::NotFound)
        }
    }

    /// Maps an action failure; an action request for a service that only
    /// reports status is "not supported", not "not found".
    fn action_failure(&self, e: ActionError, service: u32) -> Reply {
        if e == ActionError::UnknownService && self.backend.statuses().contains(service) {
            error(pb::ErrorCode::NotSupported)
        } else {
            action_error(e)
        }
    }

    /// Converts, applies and wipes: the values of a patch may be secrets.
    async fn patch(&self, mut request: pb::PatchConfigSectionRequest) -> Reply {
        let mut mutations: Vec<Mutation, MAX_MUTATIONS> = Vec::new();
        let mut valid = true;
        for mutation in &request.mutations {
            match mapping::mutation(mutation) {
                Some(mutation) => valid &= mutations.push(mutation).is_ok(),
                None => valid = false,
            }
        }
        let result = if valid {
            self.backend
                .config()
                .patch(
                    request.section_id,
                    &mutations,
                    request.expected_revision().copied(),
                    request.dry_run,
                )
                .await
        } else {
            Err(ConfigError::Field(rohi_service::FieldError::Invalid))
        };
        for mutation in &mut mutations {
            if let rohi_service::MutationOp::Set(value) = &mut mutation.op {
                value.wipe();
            }
        }
        for mutation in &mut request.mutations {
            mapping::wipe_mutation(mutation);
        }
        match result {
            Ok(done) => Reply::PatchConfigSection(pb::PatchConfigSectionResponse {
                revision: done.revision,
                restart_required: done.restart_required,
            }),
            Err(e) => config_error(e),
        }
    }

    async fn invoke(&self, mut request: pb::InvokeActionRequest) -> Reply {
        let mut args: Vec<(u32, Value), MAX_ACTION_ARGS> = Vec::new();
        let mut valid = true;
        for arg in &request.args {
            match mapping::from_arg(arg) {
                Some(arg) => valid &= args.push(arg).is_ok(),
                None => valid = false,
            }
        }
        let result = if valid {
            self.backend
                .actions()
                .invoke(request.service_id, request.action_id, &args)
                .await
        } else {
            Err(ActionError::InvalidArgument)
        };
        for (_, value) in &mut args {
            value.wipe();
        }
        for arg in &mut request.args {
            mapping::wipe_arg(arg);
        }
        match result {
            Err(e) => self.action_failure(e, request.service_id),
            Ok(records) => {
                let mut out = Vec::new();
                for record in &records {
                    let mut fields = Vec::new();
                    for (id, value) in record {
                        if fields.push(mapping::field_value(*id, value)).is_err() {
                            break;
                        }
                    }
                    if out.push(pb::ActionRecord { fields }).is_err() {
                        break;
                    }
                }
                Reply::InvokeAction(pb::InvokeActionResponse { records: out })
            }
        }
    }
}

fn error(code: pb::ErrorCode) -> Reply {
    Reply::Error(pb::Error {
        code,
        ..Default::default()
    })
}

fn config_error(e: ConfigError) -> Reply {
    error(match e {
        ConfigError::UnknownSection => pb::ErrorCode::NotFound,
        ConfigError::Field(_) => pb::ErrorCode::InvalidArgument,
        ConfigError::Conflict => pb::ErrorCode::Conflict,
        ConfigError::Unavailable => pb::ErrorCode::Unavailable,
        ConfigError::Internal => pb::ErrorCode::Internal,
    })
}

fn action_error(e: ActionError) -> Reply {
    match e {
        ActionError::Rejected(reason) => {
            let mut out = pb::Error {
                code: pb::ErrorCode::Rejected,
                ..Default::default()
            };
            out.set_reason(mapping::text(reason));
            Reply::Error(out)
        }
        ActionError::UnknownService | ActionError::UnknownAction => error(pb::ErrorCode::NotFound),
        ActionError::InvalidArgument => error(pb::ErrorCode::InvalidArgument),
        ActionError::Busy => error(pb::ErrorCode::Busy),
        ActionError::Unavailable => error(pb::ErrorCode::Unavailable),
        ActionError::Internal => error(pb::ErrorCode::Internal),
    }
}
