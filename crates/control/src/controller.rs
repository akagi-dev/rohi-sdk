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
use rohi_control_proto::{MessageEncode, PROTOCOL_VERSION, decode, encode, limits};
use rohi_service::{
    ActionError, ActionRegistry, ActionResult, ConfigError, ConfigRegistry, FeedPage,
    MAX_ACTION_ARGS, MAX_MUTATIONS, Mutation, Record, Records, RecordsPage, ResultFormat,
    ServiceStatusSummary, StatusError, StatusRegistry, Value,
};

use crate::{AuthorizationPolicy, ControlBackend, LinkSecurity, Privilege, Transport};
use crate::{diag, mapping};
use rohi_service::isolated;

/// The most streams and result snapshots the controller tracks at once, over
/// all services.
pub const MAX_OPEN_STREAMS: usize = 16;

/// What a client holds open in a service.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
    /// A stream of files.
    Stream,
    /// A snapshot of the records of `action`, `total` records long.
    Records { action: u32, total: u16 },
}

/// A stream or snapshot the controller handed to a client: who may use it and
/// with what privilege.
#[derive(Clone, Copy)]
struct Owned {
    service: u32,
    id: u32,
    session: u64,
    privilege: Privilege,
    held: Held,
}

impl Owned {
    fn is_stream(&self) -> bool {
        self.held == Held::Stream
    }
}

// A stream or snapshot to close: the service, its id and whether it is a stream.
type Closing = (u32, u32, bool);

// The opaque token of a page of a snapshot: the snapshot and the index of the
// first record of the page. It names a position and grants nothing: who may
// use it is decided by the table of owners.
const TOKEN_LEN: usize = 6;
const _: () = assert!(TOKEN_LEN <= limits::MAX_PAGE_TOKEN_LEN);

fn page_token(snapshot: u32, next: u16) -> Vec<u8, { limits::MAX_PAGE_TOKEN_LEN }> {
    let mut token = Vec::new();
    let _ = token.extend_from_slice(&snapshot.to_le_bytes());
    let _ = token.extend_from_slice(&next.to_le_bytes());
    token
}

fn parse_page_token(token: &[u8]) -> Option<(u32, u16)> {
    let token: &[u8; TOKEN_LEN] = token.try_into().ok()?;
    Some((
        u32::from_le_bytes([token[0], token[1], token[2], token[3]]),
        u16::from_le_bytes([token[4], token[5]]),
    ))
}

/// Dispatches control requests to a [`ControlBackend`], gated by an
/// [`AuthorizationPolicy`].
///
/// It owns the backend and the policy. The only state it keeps between
/// requests is who owns each open stream and result snapshot (see
/// [`LinkSecurity::session`]), so one controller serves every connection; the per-connection
/// part is the [`LinkSecurity`] passed to [`handle`](Self::handle). See the
/// [crate documentation](crate) for the operations, privileges and errors.
pub struct Controller<B, P> {
    backend: B,
    policy: P,
    streams: Mutex<CriticalSectionRawMutex, RefCell<Vec<Owned, MAX_OPEN_STREAMS>>>,
    // Streams and snapshots to close in their services once the reply of the
    // request is encoded: an eviction, or a result that fitted its first page.
    // Keeping them out of the request future keeps the big reply out of it.
    pending: Mutex<CriticalSectionRawMutex, RefCell<Vec<Closing, 2>>>,
}

impl<B: ControlBackend, P: AuthorizationPolicy> Controller<B, P> {
    /// A dispatcher over `backend`, gated by `policy`.
    pub const fn new(backend: B, policy: P) -> Self {
        Self {
            backend,
            policy,
            streams: Mutex::new(RefCell::new(Vec::new())),
            pending: Mutex::new(RefCell::new(Vec::new())),
        }
    }

    /// The wrapped backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Every control link was lost or replaced: services release the streams
    /// and snapshots the previous clients left open, and the controller
    /// forgets who owned them. Prefer [`session_closed`](Self::session_closed)
    /// when one of several connections ends.
    pub fn link_lost(&self) {
        let released = self.streams.lock(|streams| {
            let mut streams = streams.borrow_mut();
            let count = streams.len();
            streams.clear();
            count
        });
        diag::link_lost(released);
        self.backend.actions().link_lost();
    }

    /// The connection `session` ended: only its own streams and snapshots are
    /// closed in their services. Those of other sessions are untouched.
    pub async fn session_closed(&self, session: u64) {
        let mut owned: Vec<Owned, MAX_OPEN_STREAMS> = Vec::new();
        self.streams.lock(|streams| {
            streams.borrow_mut().retain(|held| {
                if held.session != session {
                    return true;
                }
                let _ = owned.push(*held);
                false
            });
        });
        for held in owned.iter() {
            let result = self.close_held(held).await;
            diag::session_held_closed(session, held.service, held.id, held.is_stream(), result);
        }
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
        self.close_pending().await;
        let (request_id, reply) = self.dispatch(link, request).await;
        diag::reply(request_id, &reply);
        let len = self.encode_reply(request_id, reply, response);
        self.close_pending().await;
        len
    }

    fn encode_reply(&self, request_id: u32, reply: Reply, response: &mut [u8]) -> usize {
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
        let len = encode(&fallback, response).unwrap_or(0);
        if len == 0 {
            diag::response_lost(request_id);
        } else {
            diag::response_fallback(request_id);
        }
        len
    }

    async fn dispatch(&self, link: &LinkSecurity, bytes: &[u8]) -> (u32, Reply) {
        if bytes.len() > limits::MAX_REQUEST_LEN {
            diag::request_too_long(bytes.len());
            return (0, error(pb::ErrorCode::InvalidArgument));
        }
        let request = match decode::<pb::Request>(bytes) {
            Ok(request) => request,
            Err(e) => {
                diag::decode_failed(bytes.len(), e);
                return (0, error(pb::ErrorCode::InvalidArgument));
            }
        };
        self.policy.request_decoded();
        let id = request.request_id;
        if request.protocol_version != PROTOCOL_VERSION {
            diag::unsupported_version(id, request.protocol_version);
            return (id, error(pb::ErrorCode::NotSupported));
        }
        match request.request {
            Some(call) => isolated(self.dispatch_call(link, id, call)).await,
            None => {
                diag::empty_request(id);
                (id, error(pb::ErrorCode::NotSupported))
            }
        }
    }

    async fn dispatch_call(&self, link: &LinkSecurity, id: u32, call: Call) -> (u32, Reply) {
        diag::request(id, &call);

        let mut secure_link = false;
        let privilege = match &call {
            Call::ReadStream(request) => {
                match self.owned(link, request.service_id, request.stream_id, None) {
                    Some(owned) => owned.privilege,
                    None => {
                        diag::unknown_held(id, &call, link);
                        return (id, action_error(ActionError::UnknownStream));
                    }
                }
            }
            Call::CloseStream(request) => {
                match self.owned(link, request.service_id, request.stream_id, None) {
                    Some(owned) => owned.privilege,
                    None => {
                        diag::unknown_held(id, &call, link);
                        return (id, action_error(ActionError::UnknownStream));
                    }
                }
            }
            Call::ReadResultPage(request) => {
                match self.owned_page(
                    link,
                    request.service_id,
                    request.action_id,
                    &request.page_token,
                ) {
                    Some((owned, _)) => owned.privilege,
                    None => {
                        diag::unknown_held(id, &call, link);
                        return (id, action_error(ActionError::UnknownSnapshot));
                    }
                }
            }
            Call::CloseResultPage(request) => {
                match self.owned_page(
                    link,
                    request.service_id,
                    request.action_id,
                    &request.page_token,
                ) {
                    Some((owned, _)) => owned.privilege,
                    None => {
                        diag::unknown_held(id, &call, link);
                        return (id, action_error(ActionError::UnknownSnapshot));
                    }
                }
            }
            Call::PatchConfigSection(_) | Call::ResetConfigSection(_) => Privilege::Mutate,
            Call::InvokeAction(invoke) => {
                match self
                    .backend
                    .actions()
                    .action(invoke.service_id, invoke.action_id)
                {
                    Ok(action) => {
                        secure_link = action.secure_link;
                        Privilege::of(action.access)
                    }
                    Err(e) => return (id, self.action_failure(e, invoke.service_id)),
                }
            }
            _ => Privilege::Read,
        };
        if !self.policy.allows(privilege, link) {
            diag::denied(id, &call, privilege, link);
            return (id, self.refusal(privilege, link));
        }
        if secure_link && !self.policy.protects_secrets(link) {
            diag::denied(id, &call, privilege, link);
            let reason = match link.transport {
                Transport::Ble => "pairing_required",
                _ => "encryption_required",
            };
            return (id, denied(reason));
        }
        (id, self.execute(link, privilege, call).await)
    }

    fn refusal(&self, privilege: Privilege, link: &LinkSecurity) -> Reply {
        match self.policy.denial_reason(privilege, link) {
            Some(reason) => denied(reason),
            None => error(pb::ErrorCode::PermissionDenied),
        }
    }

    // What `link` holds open: a stream (`records_of` is `None`) or a snapshot
    // of the records of the action, when `link` is the session that created
    // it. One of another session is reported as unknown.
    fn owned(
        &self,
        link: &LinkSecurity,
        service: u32,
        id: u32,
        records_of: Option<u32>,
    ) -> Option<Owned> {
        self.streams.lock(|streams| {
            streams
                .borrow()
                .iter()
                .find(|owned| {
                    owned.service == service
                        && owned.id == id
                        && owned.session == link.session
                        && match (owned.held, records_of) {
                            (Held::Stream, None) => true,
                            (Held::Records { action, .. }, Some(wanted)) => action == wanted,
                            _ => false,
                        }
                })
                .copied()
        })
    }

    // The snapshot a page token of `link` names, with the index of the
    // first record of the page.
    fn owned_page(
        &self,
        link: &LinkSecurity,
        service: u32,
        action: u32,
        token: &[u8],
    ) -> Option<(Owned, u16)> {
        let (snapshot, next) = parse_page_token(token)?;
        let owned = self.owned(link, service, snapshot, Some(action))?;
        match owned.held {
            Held::Records { total, .. } if next < total => Some((owned, next)),
            _ => None,
        }
    }

    fn forget(&self, session: u64, service: u32, id: u32, stream: bool) {
        self.streams.lock(|streams| {
            streams.borrow_mut().retain(|owned| {
                owned.session != session
                    || owned.service != service
                    || owned.id != id
                    || owned.is_stream() != stream
            });
        });
    }

    async fn close_held(&self, held: &Owned) -> Result<(), ActionError> {
        self.close_one(held.service, held.id, held.is_stream())
            .await
    }

    async fn close_one(&self, service: u32, id: u32, stream: bool) -> Result<(), ActionError> {
        if stream {
            self.backend.actions().close_stream(service, id).await
        } else {
            self.backend.actions().close_records(service, id).await
        }
    }

    // Records the owner of a new stream or snapshot. When the table is full the
    // oldest one is closed to make room: it is the likeliest to be abandoned.
    fn own(&self, owned: Owned) {
        let evicted = self.streams.lock(|streams| {
            let mut streams = streams.borrow_mut();
            // A view that holds nothing may give every session the same id.
            streams.retain(|old| {
                old.service != owned.service
                    || old.id != owned.id
                    || old.session != owned.session
                    || old.is_stream() != owned.is_stream()
            });
            let evicted = streams.is_full().then(|| streams.remove(0));
            let _ = streams.push(owned);
            evicted
        });
        if let Some(old) = evicted {
            diag::held_evicted(old.service, old.id, old.is_stream());
            self.close_later(old.service, old.id, old.is_stream());
        }
    }

    // `stream` tells a stream from a snapshot.
    fn close_later(&self, service: u32, id: u32, stream: bool) {
        self.pending.lock(|pending| {
            let _ = pending.borrow_mut().push((service, id, stream));
        });
    }

    // Closes in their services what `close_later` queued.
    async fn close_pending(&self) {
        while let Some((service, id, stream)) =
            self.pending.lock(|pending| pending.borrow_mut().pop())
        {
            let result = self.close_one(service, id, stream).await;
            diag::held_closed(service, id, stream, result);
        }
    }

    // Every request is built by a method of its own. The response types keep
    // their bounded fields inline (a few KiB each), so a method that inlined
    // them all would reserve their sum on the stack; as separate functions they
    // cost the largest one.
    async fn execute(&self, link: &LinkSecurity, privilege: Privilege, call: Call) -> Reply {
        match call {
            Call::GetInfo(_) => self.get_info(),
            Call::ListConfigSections(_) => self.list_config_sections(),
            Call::GetConfigSchema(request) => self.get_config_schema(request.section_id),
            Call::GetConfigSection(request) => {
                isolated(self.get_config_section(request.section_id)).await
            }
            Call::PatchConfigSection(request) => isolated(self.patch(request)).await,
            Call::ResetConfigSection(request) => isolated(self.reset_config_section(request)).await,
            Call::GetServicesStatus(_) => isolated(self.get_services_status()).await,
            Call::GetServiceSchema(request) => self.get_service_schema(request.service_id),
            Call::GetServiceStatus(request) => {
                isolated(self.get_service_status(request.service_id)).await
            }
            Call::ListActions(request) => self.list_actions(request.offset),
            Call::GetActionSchema(request) => {
                self.get_action_schema(request.service_id, request.action_id)
            }
            Call::InvokeAction(request) => isolated(self.invoke(link, privilege, request)).await,
            Call::ReadStream(request) => isolated(self.read_stream(link, request)).await,
            Call::CloseStream(request) => isolated(self.close_stream(link, request)).await,
            Call::ReadResultPage(request) => isolated(self.read_result_page(link, request)).await,
            Call::CloseResultPage(request) => isolated(self.close_result_page(link, request)).await,
        }
    }

    #[inline(never)]
    fn get_info(&self) -> Reply {
        let info = self.backend.device_info();
        Reply::GetInfo(pb::GetInfoResponse {
            firmware: mapping::text(info.firmware),
            board: mapping::text(info.board),
        })
    }

    #[inline(never)]
    fn list_config_sections(&self) -> Reply {
        let config = self.backend.config();
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

    #[inline(never)]
    fn get_config_schema(&self, section_id: u64) -> Reply {
        match self.backend.config().schema(section_id) {
            None => {
                diag::config_failed("get_config_schema", section_id, ConfigError::UnknownSection);
                error(pb::ErrorCode::NotFound)
            }
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
        }
    }

    async fn get_config_section(&self, section_id: u64) -> Reply {
        match self.backend.config().get(section_id).await {
            Err(e) => config_error("get_config_section", section_id, e),
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

    async fn reset_config_section(&self, request: pb::ResetConfigSectionRequest) -> Reply {
        match self
            .backend
            .config()
            .reset(request.section_id, request.dry_run)
            .await
        {
            Ok(done) => {
                diag::config_applied(
                    "reset_config_section",
                    request.section_id,
                    request.dry_run,
                    done.restart_required,
                );
                Reply::ResetConfigSection(pb::ResetConfigSectionResponse {
                    restart_required: done.restart_required,
                })
            }
            Err(e) => config_error("reset_config_section", request.section_id, e),
        }
    }

    async fn get_services_status(&self) -> Reply {
        let services = self.backend.statuses();
        let now = self.backend.uptime_ms();
        let mut summaries: Vec<ServiceStatusSummary, { rohi_service::MAX_REGISTRY_ENTRIES }> =
            Vec::new();
        for index in 0..services.service_count() {
            if let Some(summary) = services.summary(index, now).await
                && summaries.push(summary).is_err()
            {
                break;
            }
        }
        self.services_status_reply(now, &summaries)
    }

    #[inline(never)]
    fn services_status_reply(&self, now: u64, summaries: &[ServiceStatusSummary]) -> Reply {
        let mut wire = Vec::new();
        for summary in summaries {
            if wire.push(mapping::summary(summary)).is_err() {
                break;
            }
        }
        Reply::GetServicesStatus(pb::GetServicesStatusResponse {
            uptime_ms: now,
            health: mapping::device_health(self.backend.device_health(summaries)),
            services: wire,
        })
    }

    #[inline(never)]
    fn get_service_schema(&self, service_id: u32) -> Reply {
        match self.backend.statuses().schema(service_id) {
            None => self.missing_status(service_id),
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

    async fn get_service_status(&self, service_id: u32) -> Reply {
        match self
            .backend
            .statuses()
            .status(service_id, self.backend.uptime_ms())
            .await
        {
            Err(StatusError::UnknownService) => {
                diag::status_failed(service_id);
                self.missing_status(service_id)
            }
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

    #[inline(never)]
    fn list_actions(&self, offset: u32) -> Reply {
        let services = self.backend.actions();
        let mut actions = Vec::new();
        let mut ordinal = 0u32;
        for index in 0..services.service_count() {
            let Some(info) = services.service_identity(index) else {
                continue;
            };
            for action in services.service_actions(index) {
                if ordinal >= offset && !actions.is_full() {
                    let _ = actions.push(mapping::action_info(info.id, info.name, action));
                }
                ordinal += 1;
            }
        }
        Reply::ListActions(pb::ListActionsResponse {
            actions,
            total: ordinal,
        })
    }

    #[inline(never)]
    fn get_action_schema(&self, service_id: u32, action_id: u32) -> Reply {
        match self.backend.actions().action(service_id, action_id) {
            Err(e) => self.action_failure(e, service_id),
            Ok(action) => {
                let mut out = pb::GetActionSchemaResponse {
                    args: mapping::fields(action.args),
                    results: mapping::fields(action.results),
                    enums: mapping::enums(action.enums),
                    selections: mapping::selections(action.args),
                    ..Default::default()
                };
                out.set_action(mapping::action_info(
                    service_id,
                    self.action_service_name(service_id),
                    action,
                ));
                Reply::GetActionSchema(out)
            }
        }
    }

    async fn close_stream(&self, link: &LinkSecurity, request: pb::CloseStreamRequest) -> Reply {
        let result = self
            .backend
            .actions()
            .close_stream(request.service_id, request.stream_id)
            .await;
        diag::stream_closed(request.service_id, request.stream_id, result);
        if matches!(result, Ok(()) | Err(ActionError::UnknownStream)) {
            self.forget(link.session, request.service_id, request.stream_id, true);
        }
        match result {
            Ok(()) => Reply::CloseStream(pb::CloseStreamResponse {}),
            Err(e) => self.action_failure(e, request.service_id),
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
        diag::action_failed("action", service, e);
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
            diag::invalid_values(
                "patch_config_section",
                request.section_id,
                request.mutations.len(),
            );
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
            Ok(done) => {
                diag::config_applied(
                    "patch_config_section",
                    request.section_id,
                    request.dry_run,
                    done.restart_required,
                );
                Reply::PatchConfigSection(pb::PatchConfigSectionResponse {
                    restart_required: done.restart_required,
                })
            }
            Err(e) => config_error("patch_config_section", request.section_id, e),
        }
    }

    async fn read_stream(&self, link: &LinkSecurity, request: pb::ReadStreamRequest) -> Reply {
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
                Ok(data) => {
                    diag::stream_read(
                        request.service_id,
                        request.stream_id,
                        request.file_id,
                        request.offset,
                        chunk.len,
                        chunk.eof,
                    );
                    Reply::ReadStream(pb::ReadStreamResponse {
                        stream_id: request.stream_id,
                        file_id: request.file_id,
                        offset: request.offset,
                        data,
                        eof: chunk.eof,
                    })
                }
                Err(_) => error(pb::ErrorCode::Internal),
            },
            Ok(chunk) => {
                diag::stream_read_oversized(request.service_id, request.stream_id, chunk.len);
                error(pb::ErrorCode::Internal)
            }
            Err(e) => {
                if e == ActionError::UnknownStream {
                    // Closed or timed out in the service.
                    diag::stream_gone(request.service_id, request.stream_id);
                    self.forget(link.session, request.service_id, request.stream_id, true);
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
            let quiet = self
                .backend
                .actions()
                .action(request.service_id, request.action_id)
                .is_ok_and(|action| action.format == ResultFormat::Feed);
            diag::invoking(request.service_id, request.action_id, args.len(), quiet);
            self.backend
                .actions()
                .invoke(request.service_id, request.action_id, &args)
                .await
        } else {
            diag::invalid_values(
                "invoke_action",
                request.action_id.into(),
                request.args.len(),
            );
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
            Ok(ActionResult::Empty) => {
                diag::invoked(request.service_id, request.action_id, privilege, "empty");
                Reply::ActionEmpty(pb::Empty {})
            }
            Ok(ActionResult::Inline(record)) => {
                diag::invoked(request.service_id, request.action_id, privilege, "inline");
                inline_reply(&record)
            }
            Ok(ActionResult::Stream(stream)) => {
                diag::invoked(request.service_id, request.action_id, privilege, "stream");
                diag::stream_opened(
                    request.service_id,
                    stream.id,
                    link.session,
                    stream.files.len(),
                );
                self.own(Owned {
                    service: request.service_id,
                    id: stream.id,
                    session: link.session,
                    privilege,
                    held: Held::Stream,
                });
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
            Ok(ActionResult::Feed(page)) => {
                diag::invoked(request.service_id, request.action_id, privilege, "feed");
                feed_reply(&page)
            }
            Ok(ActionResult::Records(page)) => {
                diag::invoked(request.service_id, request.action_id, privilege, "records");
                self.first_page(link, privilege, request.service_id, request.action_id, page)
            }
        }
    }

    // The first page of a `Records` result. A collection that fits one page is
    // complete: its snapshot is released at once. Otherwise the connection owns
    // the snapshot and the reply carries the token of the next page.
    fn first_page(
        &self,
        link: &LinkSecurity,
        privilege: Privilege,
        service: u32,
        action: u32,
        page: RecordsPage,
    ) -> Reply {
        let RecordsPage { records, snapshot } = page;
        let included = fit_records(&records);
        let Some(info) = snapshot else {
            // Without a snapshot there is nothing to continue in: all records
            // must fit.
            if included < records.len() {
                diag::page_overflow(service, action, included, records.len());
                return error(pb::ErrorCode::Internal);
            }
            return records_reply(&records, included, None);
        };
        if included == 0 || included > usize::from(info.total) {
            diag::page_overflow(service, action, included, records.len());
            self.release_later(service, info.id);
            return error(pb::ErrorCode::Internal);
        }
        if included == usize::from(info.total) {
            self.release_later(service, info.id);
            return records_reply(&records, included, None);
        }
        self.own(Owned {
            service,
            id: info.id,
            session: link.session,
            privilege,
            held: Held::Records {
                action,
                total: info.total,
            },
        });
        diag::snapshot_opened(service, info.id, link.session, info.total);
        let next = included as u16;
        records_reply(&records, included, Some(page_token(info.id, next)))
    }

    // A result that was complete in its first page, or could not be used.
    fn release_later(&self, service: u32, snapshot: u32) {
        self.close_later(service, snapshot, false);
    }

    async fn read_result_page(
        &self,
        link: &LinkSecurity,
        request: pb::ReadResultPageRequest,
    ) -> Reply {
        let Some((owned, next)) = self.owned_page(
            link,
            request.service_id,
            request.action_id,
            &request.page_token,
        ) else {
            return action_error(ActionError::UnknownSnapshot);
        };
        let Held::Records { total, .. } = owned.held else {
            return action_error(ActionError::UnknownSnapshot);
        };
        // The records stay where the read left them: one copy of a page.
        let read = self
            .backend
            .actions()
            .read_records(
                request.service_id,
                request.action_id,
                owned.id,
                u32::from(next),
            )
            .await;
        match &read {
            Ok(records) => page_after(&owned, request.action_id, next, total, records),
            Err(e) => {
                if *e == ActionError::UnknownSnapshot {
                    // Closed or expired in the service.
                    diag::snapshot_gone(request.service_id, owned.id);
                    self.forget(link.session, request.service_id, owned.id, false);
                }
                self.action_failure(*e, request.service_id)
            }
        }
    }

    async fn close_result_page(
        &self,
        link: &LinkSecurity,
        request: pb::CloseResultPageRequest,
    ) -> Reply {
        let Some((owned, _)) = self.owned_page(
            link,
            request.service_id,
            request.action_id,
            &request.page_token,
        ) else {
            return action_error(ActionError::UnknownSnapshot);
        };
        let result = self
            .backend
            .actions()
            .close_records(request.service_id, owned.id)
            .await;
        diag::snapshot_closed(request.service_id, owned.id, result);
        if matches!(result, Ok(()) | Err(ActionError::UnknownSnapshot)) {
            self.forget(link.session, request.service_id, owned.id, false);
        }
        match result {
            Ok(()) => Reply::CloseResultPage(pb::CloseResultPageResponse {}),
            Err(e) => self.action_failure(e, request.service_id),
        }
    }
}

// The page read from `next` on: it advances, holds a record and never more than
// remain, and carries the token of the following page when there is one.
#[inline(never)]
fn page_after(owned: &Owned, action: u32, next: u16, total: u16, records: &Records) -> Reply {
    let included = fit_records(records);
    let remaining = usize::from(total - next);
    if included == 0 || included > remaining {
        diag::page_overflow(owned.service, action, included, records.len());
        return error(pb::ErrorCode::Internal);
    }
    diag::page_read(owned.service, owned.id, next, included);
    let following = usize::from(next) + included;
    let token = (following < usize::from(total)).then(|| page_token(owned.id, following as u16));
    records_reply(records, included, token)
}

// An `Inline` result: the fields of one record.
#[inline(never)]
fn inline_reply(record: &Record) -> Reply {
    let mut out = pb::InlineResult::default();
    for (id, value) in record {
        if out.fields.push(mapping::field_value(*id, value)).is_err() {
            return error(pb::ErrorCode::Internal);
        }
    }
    Reply::ActionInline(out)
}

// What the envelope of a response (version, request id, the case tag and its
// length) takes around a `RecordsPage`.
pub(crate) const ENVELOPE: usize = 16;

fn records_page(records: &Records, count: usize) -> pb::RecordsPage {
    let mut page = pb::RecordsPage::default();
    for record in records.iter().take(count) {
        let mut fields = Vec::new();
        for (id, value) in record {
            if fields.push(mapping::field_value(*id, value)).is_err() {
                break;
            }
        }
        if page.records.push(pb::ActionRecord { fields }).is_err() {
            break;
        }
    }
    page
}

// The encoded size of a page of the first `count` records, with the longest token.
#[inline(never)]
pub(crate) fn records_size(records: &Records, count: usize) -> usize {
    let mut page = records_page(records, count);
    let _ = page.page_token.extend_from_slice(&[0; TOKEN_LEN]);
    page.compute_size()
}

// How many whole records of `records` (a prefix) fit a response of
// `response_len` bytes, with the longest token. A record is never split.
pub(crate) fn fit_within(records: &Records, response_len: usize) -> usize {
    let room = response_len.saturating_sub(ENVELOPE);
    let mut count = records.len();
    while count > 0 && records_size(records, count) > room {
        count -= 1;
    }
    count
}

fn fit_records(records: &Records) -> usize {
    fit_within(records, limits::MAX_RESPONSE_LEN)
}

// The page of the first `included` records, with the token of the next page
// when more follow.
#[inline(never)]
fn records_reply(
    records: &Records,
    included: usize,
    token: Option<Vec<u8, { limits::MAX_PAGE_TOKEN_LEN }>>,
) -> Reply {
    let mut page = records_page(records, included);
    if let Some(token) = token {
        page.page_token = token;
    }
    Reply::RecordsPage(page)
}

// One bounded feed page: nothing is kept for the client.
#[inline(never)]
fn feed_reply(page: &FeedPage) -> Reply {
    let mut reply = pb::FeedPage {
        session: page.info.session,
        cursor: page.info.cursor,
        oldest_sequence: page.info.oldest_sequence,
        newest_sequence: page.info.newest_sequence,
        reset: page.info.reset,
        gap: page.info.gap,
        ..Default::default()
    };
    if reply.data.extend_from_slice(&page.data).is_err() {
        return error(pb::ErrorCode::Internal);
    }
    Reply::FeedPage(reply)
}

fn denied(reason: &'static str) -> Reply {
    let mut out = pb::Error {
        code: pb::ErrorCode::PermissionDenied,
        ..Default::default()
    };
    out.set_reason(mapping::text(reason));
    Reply::Error(out)
}

fn error(code: pb::ErrorCode) -> Reply {
    Reply::Error(pb::Error {
        code,
        ..Default::default()
    })
}

fn config_error(op: &'static str, section: u64, e: ConfigError) -> Reply {
    diag::config_failed(op, section, e);
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
        ActionError::UnknownService
        | ActionError::UnknownAction
        | ActionError::UnknownStream
        | ActionError::UnknownSnapshot => error(pb::ErrorCode::NotFound),
        ActionError::InvalidArgument => error(pb::ErrorCode::InvalidArgument),
        ActionError::Busy => error(pb::ErrorCode::Busy),
        ActionError::Unavailable => error(pb::ErrorCode::Unavailable),
        ActionError::Internal => error(pb::ErrorCode::Internal),
    }
}
