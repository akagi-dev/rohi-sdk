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

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use heapless::Vec;
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_service::{
    ActionError, ActionRegistry, ActionResult, ConfigError, ConfigRegistry, MAX_ACTION_ARGS,
    MAX_MUTATIONS, Mutation, ServiceStatusSummary, StatusError, StatusRegistry, Value,
};

use crate::mapping;
use crate::{AuthorizationPolicy, ControlBackend, LinkSecurity, Privilege};

/// The most streams the controller tracks at once, over all services.
pub const MAX_OPEN_STREAMS: usize = 16;

/// A stream the controller handed to a client: who may use it and with what
/// privilege.
#[derive(Clone, Copy)]
struct OwnedStream {
    service: u32,
    stream: u32,
    session: u32,
    privilege: Privilege,
}

/// Dispatches control requests to a [`ControlBackend`], gated by an
/// [`AuthorizationPolicy`].
///
/// It owns the backend and the policy. The only state it keeps between
/// requests is who owns each open stream (see
/// [`LinkSecurity::session`]), so one controller serves every connection; the
/// per-connection part is the [`LinkSecurity`] passed to
/// [`handle`](Self::handle). See the [crate documentation](crate) for the
/// operations, privileges and errors.
pub struct Controller<B, P> {
    backend: B,
    policy: P,
    streams: Mutex<CriticalSectionRawMutex, RefCell<Vec<OwnedStream, MAX_OPEN_STREAMS>>>,
}

impl<B: ControlBackend, P: AuthorizationPolicy> Controller<B, P> {
    /// A dispatcher over `backend`, gated by `policy`.
    pub const fn new(backend: B, policy: P) -> Self {
        Self {
            backend,
            policy,
            streams: Mutex::new(RefCell::new(Vec::new())),
        }
    }

    /// The wrapped backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The control link was lost or replaced: services release the streams
    /// the previous client left open and the controller forgets who owned
    /// them.
    pub fn link_lost(&self) {
        self.streams.lock(|streams| streams.borrow_mut().clear());
        self.backend.actions().link_lost();
    }

    /// Handles one encoded request received on a link with the given
    /// security and writes exactly one encoded response to `response`.
    ///
    /// Returns the response length. Every failure, including malformed
    /// requests and an undersized `response` buffer, is answered with a typed
    /// error response; the transport is never expected to disconnect. `0` is
    /// returned only if not even an error fits into `response`.
    ///
    /// A request that is too long or cannot be decoded is answered with
    /// `InvalidArgument` and `request_id` 0, since the id cannot be trusted.
    /// A response too large for `response` is replaced by an `Internal`
    /// error. The future holds no state: dropping it abandons the request, but
    /// anything a registry already committed stays committed.
    ///
    /// Secret values in the decoded request are wiped before this returns; the
    /// bytes of `request` stay with the caller.
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
            Call::ReadStream(request) => {
                match self.owned_privilege(link, request.service_id, request.stream_id) {
                    Some(privilege) => privilege,
                    None => return (id, action_error(ActionError::UnknownStream)),
                }
            }
            Call::CloseStream(request) => {
                match self.owned_privilege(link, request.service_id, request.stream_id) {
                    Some(privilege) => privilege,
                    None => return (id, action_error(ActionError::UnknownStream)),
                }
            }
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
        (id, self.execute(link, privilege, call).await)
    }

    // The privilege a stream was created with, when `link` is the session
    // that created it. A stream of another session is reported as unknown.
    fn owned_privilege(&self, link: &LinkSecurity, service: u32, stream: u32) -> Option<Privilege> {
        self.streams.lock(|streams| {
            streams
                .borrow()
                .iter()
                .find(|owned| {
                    owned.service == service
                        && owned.stream == stream
                        && owned.session == link.session
                })
                .map(|owned| owned.privilege)
        })
    }

    fn forget_stream(&self, service: u32, stream: u32) {
        self.streams.lock(|streams| {
            streams
                .borrow_mut()
                .retain(|owned| owned.service != service || owned.stream != stream);
        });
    }

    // Records the owner of a new stream. When the table is full the oldest
    // stream is closed to make room: it is the likeliest to be abandoned.
    async fn own_stream(&self, owned: OwnedStream) {
        let evicted = self.streams.lock(|streams| {
            let mut streams = streams.borrow_mut();
            streams.retain(|old| old.service != owned.service || old.stream != owned.stream);
            let evicted = streams.is_full().then(|| streams.remove(0));
            let _ = streams.push(owned);
            evicted
        });
        if let Some(old) = evicted {
            let _ = self
                .backend
                .actions()
                .close_stream(old.service, old.stream)
                .await;
        }
    }

    async fn execute(&self, link: &LinkSecurity, privilege: Privilege, call: Call) -> Reply {
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
                    .reset(request.section_id, request.dry_run)
                    .await
                {
                    Ok(done) => Reply::ResetConfigSection(pb::ResetConfigSectionResponse {
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
            Call::InvokeAction(request) => self.invoke(link, privilege, request).await,
            Call::ReadStream(request) => self.read_stream(request).await,
            Call::CloseStream(request) => {
                let result = self
                    .backend
                    .actions()
                    .close_stream(request.service_id, request.stream_id)
                    .await;
                if matches!(result, Ok(()) | Err(ActionError::UnknownStream)) {
                    self.forget_stream(request.service_id, request.stream_id);
                }
                match result {
                    Ok(()) => Reply::CloseStream(pb::CloseStreamResponse {}),
                    Err(e) => self.action_failure(e, request.service_id),
                }
            }
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
                .patch(request.section_id, &mutations, request.dry_run)
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
                restart_required: done.restart_required,
            }),
            Err(e) => config_error(e),
        }
    }

    async fn read_stream(&self, request: pb::ReadStreamRequest) -> Reply {
        let mut buf = [0u8; limits::MAX_STREAM_CHUNK];
        match self
            .backend
            .actions()
            .read_stream(
                request.service_id,
                request.stream_id,
                request.file_id,
                request.offset,
                &mut buf,
            )
            .await
        {
            Ok(chunk) if chunk.len <= buf.len() => match Vec::from_slice(&buf[..chunk.len]) {
                Ok(data) => Reply::ReadStream(pb::ReadStreamResponse {
                    stream_id: request.stream_id,
                    file_id: request.file_id,
                    offset: request.offset,
                    data,
                    eof: chunk.eof,
                }),
                Err(_) => error(pb::ErrorCode::Internal),
            },
            Ok(_) => error(pb::ErrorCode::Internal),
            Err(e) => {
                if e == ActionError::UnknownStream {
                    // Closed or timed out in the service.
                    self.forget_stream(request.service_id, request.stream_id);
                }
                self.action_failure(e, request.service_id)
            }
        }
    }

    async fn invoke(
        &self,
        link: &LinkSecurity,
        privilege: Privilege,
        mut request: pb::InvokeActionRequest,
    ) -> Reply {
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
            Ok(ActionResult::Stream(stream)) => {
                self.own_stream(OwnedStream {
                    service: request.service_id,
                    stream: stream.id,
                    session: link.session,
                    privilege,
                })
                .await;
                let mut files = Vec::new();
                for file in &stream.files {
                    let file = pb::StreamFile {
                        id: file.id,
                        name: mapping::text(&file.name),
                        size_bytes: file.size_bytes,
                    };
                    if files.push(file).is_err() {
                        break;
                    }
                }
                Reply::ActionStream(pb::StreamInfo {
                    service_id: request.service_id,
                    id: stream.id,
                    files,
                })
            }
            Ok(ActionResult::Records(records)) => {
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
        ActionError::UnknownService | ActionError::UnknownAction | ActionError::UnknownStream => {
            error(pb::ErrorCode::NotFound)
        }
        ActionError::InvalidArgument => error(pb::ErrorCode::InvalidArgument),
        ActionError::Busy => error(pb::ErrorCode::Busy),
        ActionError::Unavailable => error(pb::ErrorCode::Unavailable),
        ActionError::Internal => error(pb::ErrorCode::Internal),
    }
}
