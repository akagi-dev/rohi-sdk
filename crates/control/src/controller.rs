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
    MAX_MUTATIONS, Mutation, ServiceStatusSummary, StatusError, StatusRegistry, TailRequest, Value,
};

use crate::{AuthorizationPolicy, ControlBackend, LinkSecurity, Privilege, RequestAuthorization};
use crate::{diag, mapping};
use rohi_identity::{PublicKey, Signature};
use rohi_service::isolated;
use zeroize::Zeroize;

/// The most streams the controller tracks at once, over all services.
pub const MAX_OPEN_STREAMS: usize = 16;

/// A stream the controller handed to a client: who may use it and with what
/// privilege.
#[derive(Clone, Copy)]
struct OwnedStream {
    service: u32,
    stream: u32,
    session: u64,
    privilege: Privilege,
}

/// The most connections that can hold a challenge at once.
pub const MAX_AUTH_SESSIONS: usize = 4;

/// Signed requests of one connection that fail before it is refused without
/// even checking a signature.
const MAX_AUTH_FAILURES: u8 = 8;

const AUTH_DOMAIN: &[u8] = b"rohi-control-auth-v1";

// The signed preimage: domain, nonce, sequence, inner request.
const PREIMAGE_LEN: usize = AUTH_DOMAIN.len() + 32 + 8 + limits::MAX_INNER_REQUEST_LEN;

/// The challenge state of one connection. RAM only.
struct SessionAuth {
    session: u64,
    nonce: [u8; 32],
    epoch: u32,
    last: Option<u64>,
    failures: u8,
}

impl Drop for SessionAuth {
    fn drop(&mut self) {
        self.nonce.zeroize();
    }
}

/// What the signed envelope of a request came to.
// Both payloads are a message that the caller moves straight on.
#[allow(clippy::large_enum_variant)]
enum Unsigned {
    /// The bytes are no signed request.
    NotSigned,
    /// The request was refused or malformed: this is the answer.
    Reply(Reply),
    /// The verified inner request.
    Call(Call),
}

/// Dispatches control requests to a [`ControlBackend`], gated by an
/// [`AuthorizationPolicy`].
///
/// It owns the backend and the policy. The only state it keeps between
/// requests is who owns each open stream (see
/// [`LinkSecurity::session`]) and the challenge of each connection that asked
/// for one, so one controller serves every connection; the per-connection
/// part is the [`LinkSecurity`] passed to [`handle`](Self::handle). See the
/// [crate documentation](crate) for the operations, privileges and errors.
pub struct Controller<B, P> {
    backend: B,
    policy: P,
    streams: Mutex<CriticalSectionRawMutex, RefCell<Vec<OwnedStream, MAX_OPEN_STREAMS>>>,
    auth: Mutex<CriticalSectionRawMutex, RefCell<Vec<SessionAuth, MAX_AUTH_SESSIONS>>>,
}

impl<B: ControlBackend, P: AuthorizationPolicy> Controller<B, P> {
    /// A dispatcher over `backend`, gated by `policy`.
    pub const fn new(backend: B, policy: P) -> Self {
        Self {
            backend,
            policy,
            streams: Mutex::new(RefCell::new(Vec::new())),
            auth: Mutex::new(RefCell::new(Vec::new())),
        }
    }

    /// The wrapped backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Every control link was lost or replaced: services release the streams
    /// the previous clients left open, and the controller forgets who owned
    /// them and every challenge. Prefer [`session_closed`](Self::session_closed)
    /// when one of several connections ends.
    pub fn link_lost(&self) {
        let released = self.streams.lock(|streams| {
            let mut streams = streams.borrow_mut();
            let count = streams.len();
            streams.clear();
            count
        });
        self.auth.lock(|auth| auth.borrow_mut().clear());
        diag::link_lost(released);
        self.backend.actions().link_lost();
    }

    /// The connection `session` ended: its challenge is forgotten and only
    /// its own streams are closed in their services. Streams and challenges
    /// of other sessions are untouched.
    pub async fn session_closed(&self, session: u64) {
        self.auth
            .lock(|auth| auth.borrow_mut().retain(|entry| entry.session != session));
        let mut owned: Vec<(u32, u32), MAX_OPEN_STREAMS> = Vec::new();
        self.streams.lock(|streams| {
            streams.borrow_mut().retain(|stream| {
                if stream.session != session {
                    return true;
                }
                let _ = owned.push((stream.service, stream.stream));
                false
            });
        });
        for (service, stream) in owned.iter().copied() {
            let result = self.backend.actions().close_stream(service, stream).await;
            diag::session_stream_closed(session, service, stream, result);
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
        let (request_id, reply) = self.dispatch(link, request).await;
        diag::reply(request_id, &reply);
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
        // Only this controller decides who proved what.
        let anonymous = LinkSecurity {
            authorization: RequestAuthorization::Anonymous,
            ..*link
        };
        let link = &anonymous;
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
        let id = request.request_id;
        if request.protocol_version != PROTOCOL_VERSION {
            diag::unsupported_version(id, request.protocol_version);
            return (id, error(pb::ErrorCode::NotSupported));
        }
        match request.request {
            Some(call) => self.dispatch_call(link, id, call).await,
            None => match isolated(self.unsign(link, id, bytes)).await {
                Unsigned::Call(call) => {
                    let verified = LinkSecurity {
                        authorization: RequestAuthorization::VerifiedAdmin,
                        ..*link
                    };
                    self.dispatch_call(&verified, id, call).await
                }
                Unsigned::Reply(reply) => (id, reply),
                Unsigned::NotSigned => {
                    diag::empty_request(id);
                    (id, error(pb::ErrorCode::NotSupported))
                }
            },
        }
    }

    async fn dispatch_call(&self, link: &LinkSecurity, id: u32, call: Call) -> (u32, Reply) {
        diag::request(id, &call);

        let privilege = match &call {
            Call::ReadStream(request) => {
                match self.owned_privilege(link, request.service_id, request.stream_id) {
                    Some(privilege) => privilege,
                    None => {
                        diag::unknown_stream(id, &call, link);
                        return (id, action_error(ActionError::UnknownStream));
                    }
                }
            }
            Call::CloseStream(request) => {
                match self.owned_privilege(link, request.service_id, request.stream_id) {
                    Some(privilege) => privilege,
                    None => {
                        diag::unknown_stream(id, &call, link);
                        return (id, action_error(ActionError::UnknownStream));
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
                    Ok(action) => Privilege::of(action.access),
                    Err(e) => return (id, self.action_failure(e, invoke.service_id)),
                }
            }
            _ => Privilege::Read,
        };
        if !self.policy.allows(privilege, link) {
            diag::denied(id, &call, privilege, link);
            return (id, self.refusal(privilege, link));
        }
        (id, self.execute(link, privilege, call).await)
    }

    fn refusal(&self, privilege: Privilege, link: &LinkSecurity) -> Reply {
        match self.policy.denial_reason(privilege, link) {
            Some(reason) => denied(reason),
            None => error(pb::ErrorCode::PermissionDenied),
        }
    }

    // The request bytes were not a plain request: unwraps and verifies a
    // signed one. The replay state advances before the inner request runs.
    async fn unsign(&self, link: &LinkSecurity, id: u32, bytes: &[u8]) -> Unsigned {
        let Ok(envelope) = decode::<pb::SignedEnvelope>(bytes) else {
            return Unsigned::NotSigned;
        };
        let Some(signed) = envelope.signed_request() else {
            return Unsigned::NotSigned;
        };
        if !self.policy.accepts_signed(link) {
            diag::auth_refused(id, link, "not_supported");
            return Unsigned::Reply(denied("not_supported"));
        }
        if let Err(reason) = self.verify(link, signed) {
            self.policy.auth_failed(link);
            diag::auth_refused(id, link, reason);
            return Unsigned::Reply(denied(reason));
        }
        let inner = match decode::<pb::Request>(&signed.request_bytes) {
            Ok(inner) => inner,
            Err(e) => {
                diag::decode_failed(signed.request_bytes.len(), e);
                return Unsigned::Reply(error(pb::ErrorCode::InvalidArgument));
            }
        };
        if inner.request_id != id || inner.protocol_version != PROTOCOL_VERSION {
            return Unsigned::Reply(error(pb::ErrorCode::InvalidArgument));
        }
        match inner.request {
            Some(Call::GetAuthChallenge(_)) => Unsigned::Reply(denied("nested")),
            Some(call) => Unsigned::Call(call),
            None if decode::<pb::SignedEnvelope>(&signed.request_bytes)
                .is_ok_and(|nested| nested.signed_request().is_some()) =>
            {
                Unsigned::Reply(denied("nested"))
            }
            None => Unsigned::Reply(error(pb::ErrorCode::NotSupported)),
        }
    }

    // Checks the challenge, the sequence and the signature, in that order,
    // and records the sequence on success.
    #[inline(never)]
    fn verify(&self, link: &LinkSecurity, signed: &pb::SignedRequest) -> Result<(), &'static str> {
        let Some(admin) = self.policy.admin_key() else {
            return Err("no_admin_key");
        };
        let state = self.auth.lock(|auth| {
            auth.borrow()
                .iter()
                .find(|entry| entry.session == link.session)
                .map(|entry| (entry.nonce, entry.epoch, entry.last, entry.failures))
        });
        let Some((mut nonce, epoch, last, failures)) = state else {
            return Err("no_challenge");
        };
        let result = (|| {
            if epoch != admin.epoch {
                return Err("no_challenge");
            }
            if failures >= MAX_AUTH_FAILURES {
                return Err("bad_signature");
            }
            if last.is_some_and(|last| signed.sequence <= last) {
                return Err("stale_sequence");
            }
            let signature = Signature::from_slice(&signed.signature).ok_or("bad_signature")?;
            let key = PublicKey::from_slice(&admin.key).ok_or("bad_signature")?;
            let mut preimage: Vec<u8, PREIMAGE_LEN> = Vec::new();
            let built = preimage.extend_from_slice(AUTH_DOMAIN).is_ok()
                && preimage.extend_from_slice(&nonce).is_ok()
                && preimage
                    .extend_from_slice(&signed.sequence.to_le_bytes())
                    .is_ok()
                && preimage.extend_from_slice(&signed.request_bytes).is_ok();
            let verified = built && key.verify(&preimage, &signature).is_ok();
            preimage.as_mut_slice().zeroize();
            if verified {
                Ok(())
            } else {
                Err("bad_signature")
            }
        })();
        nonce.zeroize();
        self.auth.lock(|auth| {
            if let Some(entry) = auth
                .borrow_mut()
                .iter_mut()
                .find(|entry| entry.session == link.session)
            {
                match result {
                    Ok(()) => {
                        entry.last = Some(signed.sequence);
                        entry.failures = 0;
                    }
                    Err("bad_signature") => entry.failures = entry.failures.saturating_add(1),
                    Err(_) => {}
                }
            }
        });
        result
    }

    #[inline(never)]
    fn get_auth_challenge(&self, link: &LinkSecurity) -> Reply {
        if !self.policy.accepts_signed(link) {
            return error(pb::ErrorCode::NotSupported);
        }
        let mut nonce = [0u8; 32];
        if !self.policy.fill_nonce(&mut nonce) {
            diag::challenge_unavailable(link);
            nonce.zeroize();
            return error(pb::ErrorCode::Unavailable);
        }
        let admin = self.policy.admin_key();
        let mut entry = SessionAuth {
            session: link.session,
            nonce,
            epoch: admin.map_or(0, |admin| admin.epoch),
            last: None,
            failures: 0,
        };
        nonce.zeroize();
        self.auth.lock(|auth| {
            let mut auth = auth.borrow_mut();
            if let Some(old) = auth.iter_mut().find(|old| old.session == link.session) {
                entry.failures = old.failures;
                *old = entry;
                return;
            }
            if auth.is_full() {
                auth.remove(0);
            }
            let _ = auth.push(entry);
        });
        let mut reply = pb::GetAuthChallengeResponse {
            admin_key_enrolled: admin.is_some(),
            ..Default::default()
        };
        self.auth.lock(|auth| {
            if let Some(entry) = auth.borrow().iter().find(|e| e.session == link.session) {
                let _ = reply.session_nonce.extend_from_slice(&entry.nonce);
            }
        });
        Reply::GetAuthChallenge(reply)
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
            let result = self
                .backend
                .actions()
                .close_stream(old.service, old.stream)
                .await;
            diag::stream_evicted(old.service, old.stream, result);
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
            Call::ReadStream(request) => isolated(self.read_stream(request)).await,
            Call::CloseStream(request) => isolated(self.close_stream(request)).await,
            Call::TailLogs(request) => self.tail_logs(request),
            Call::GetAuthChallenge(_) => self.get_auth_challenge(link),
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

    async fn close_stream(&self, request: pb::CloseStreamRequest) -> Reply {
        let result = self
            .backend
            .actions()
            .close_stream(request.service_id, request.stream_id)
            .await;
        diag::stream_closed(request.service_id, request.stream_id, result);
        if matches!(result, Ok(()) | Err(ActionError::UnknownStream)) {
            self.forget_stream(request.service_id, request.stream_id);
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
                    self.forget_stream(request.service_id, request.stream_id);
                }
                self.action_failure(e, request.service_id)
            }
        }
    }

    // One bounded page of text straight into the data field of the reply: no
    // buffer of its own, no stream and nothing kept for the client.
    #[inline(never)]
    fn tail_logs(&self, request: pb::TailLogsRequest) -> Reply {
        let mut reply = pb::TailLogsResponse::default();
        if reply.data.resize_default(limits::MAX_TAIL_CHUNK).is_err() {
            return error(pb::ErrorCode::Internal);
        }
        let page = self.backend.actions().tail_logs(
            request.service_id,
            TailRequest {
                session: request.session,
                after: request.after,
            },
            reply.data.as_mut_slice(),
        );
        match page {
            Ok(page) if page.len <= limits::MAX_TAIL_CHUNK => {
                reply.data.truncate(page.len);
                reply.session = page.session;
                reply.cursor = page.cursor;
                reply.oldest_sequence = page.oldest_sequence;
                reply.newest_sequence = page.newest_sequence;
                reply.reset = page.reset;
                reply.gap = page.gap;
                Reply::TailLogs(reply)
            }
            Ok(page) => {
                diag::tail_oversized(request.service_id, page.len);
                error(pb::ErrorCode::Internal)
            }
            Err(e) => self.action_failure(e, request.service_id),
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
            diag::invoking(request.service_id, request.action_id, args.len());
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
            Ok(ActionResult::Stream(stream)) => {
                diag::invoked(request.service_id, request.action_id, privilege, "stream");
                diag::stream_opened(
                    request.service_id,
                    stream.id,
                    link.session,
                    stream.files.len(),
                );
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
                diag::invoked(request.service_id, request.action_id, privilege, "records");
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
        ActionError::UnknownService | ActionError::UnknownAction | ActionError::UnknownStream => {
            error(pb::ErrorCode::NotFound)
        }
        ActionError::InvalidArgument => error(pb::ErrorCode::InvalidArgument),
        ActionError::Busy => error(pb::ErrorCode::Busy),
        ActionError::Unavailable => error(pb::ErrorCode::Unavailable),
        ActionError::Internal => error(pb::ErrorCode::Internal),
    }
}
