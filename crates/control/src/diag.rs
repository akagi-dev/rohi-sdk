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
//! Log records of the controller.
//!
//! The dispatch futures inline every temporary, so each record is emitted by a
//! small out-of-line function that takes plain values: the stack cost of a
//! record stays out of the request path. Request arguments, config values and
//! other secret-bearing data are never logged, only ids and error kinds.

use rohi_control_proto::DecodeError;
use rohi_control_proto::control::{Request_::Request as Call, Response_::Response as Reply};
use rohi_service::{ActionError, ConfigError};

use crate::{LinkSecurity, Privilege};

fn op_name(call: &Call) -> &'static str {
    match call {
        Call::GetInfo(_) => "get_info",
        Call::ListConfigSections(_) => "list_config_sections",
        Call::GetConfigSchema(_) => "get_config_schema",
        Call::GetConfigSection(_) => "get_config_section",
        Call::PatchConfigSection(_) => "patch_config_section",
        Call::ResetConfigSection(_) => "reset_config_section",
        Call::GetServicesStatus(_) => "get_services_status",
        Call::GetServiceSchema(_) => "get_service_schema",
        Call::GetServiceStatus(_) => "get_service_status",
        Call::ListActions(_) => "list_actions",
        Call::GetActionSchema(_) => "get_action_schema",
        Call::InvokeAction(_) => "invoke_action",
        Call::ReadStream(_) => "read_stream",
        Call::CloseStream(_) => "close_stream",
        Call::ReadResultPage(_) => "read_result_page",
        Call::CloseResultPage(_) => "close_result_page",
    }
}

#[inline(never)]
pub(crate) fn request(id: u32, call: &Call) {
    log::trace!(target: "control", "Request received (id={} op={})", id, op_name(call));
}

#[inline(never)]
pub(crate) fn reply(id: u32, reply: &Reply) {
    match reply {
        Reply::Error(error) => {
            log::debug!(target: "control", "Request failed (id={} err={:?})", id, error.code)
        }
        _ => log::trace!(target: "control", "Request completed (id={})", id),
    }
}

#[cold]
#[inline(never)]
pub(crate) fn request_too_long(len: usize) {
    log::warn!(target: "control", "Request rejected: too long (size_b={})", len);
}

#[cold]
#[inline(never)]
pub(crate) fn decode_failed(len: usize, e: DecodeError) {
    log::warn!(target: "control", "Request rejected: cannot decode (size_b={} err={:?})", len, e);
}

#[cold]
#[inline(never)]
pub(crate) fn unsupported_version(id: u32, version: u32) {
    log::warn!(
        target: "control",
        "Request rejected: unsupported protocol version (id={} version={})",
        id,
        version
    );
}

#[cold]
#[inline(never)]
pub(crate) fn empty_request(id: u32) {
    log::warn!(target: "control", "Request rejected: no operation (id={})", id);
}

#[cold]
#[inline(never)]
pub(crate) fn denied(id: u32, call: &Call, privilege: Privilege, link: &LinkSecurity) {
    log::warn!(
        target: "control",
        "Request denied: weak link security (id={} op={} needs={:?} enc={} auth={})",
        id,
        op_name(call),
        privilege,
        link.encrypted,
        link.authenticated
    );
}

#[cold]
#[inline(never)]
pub(crate) fn unknown_held(id: u32, call: &Call, link: &LinkSecurity) {
    // The token of a page is never logged.
    let service = match call {
        Call::ReadStream(r) => r.service_id,
        Call::CloseStream(r) => r.service_id,
        Call::ReadResultPage(r) => r.service_id,
        Call::CloseResultPage(r) => r.service_id,
        _ => 0,
    };
    log::debug!(
        target: "control",
        "Request refers to an unknown stream or snapshot (id={} op={} service={} session={})",
        id,
        op_name(call),
        service,
        link.session
    );
}

#[cold]
#[inline(never)]
pub(crate) fn response_fallback(id: u32) {
    log::warn!(
        target: "control",
        "Response does not fit the buffer, sent internal error (id={})",
        id
    );
}

#[cold]
#[inline(never)]
pub(crate) fn response_lost(id: u32) {
    log::warn!(
        target: "control",
        "Response dropped: even an error does not fit (id={})",
        id
    );
}

#[cold]
#[inline(never)]
pub(crate) fn link_lost(streams: usize) {
    log::info!(target: "control", "Link lost, streams and snapshots released (count={})", streams);
}

#[cold]
#[inline(never)]
pub(crate) fn config_failed(op: &'static str, section: u64, e: ConfigError) {
    log::warn!(
        target: "control",
        "Config operation failed (op={} section={} err={:?})",
        op,
        section,
        e
    );
}

#[cold]
#[inline(never)]
pub(crate) fn action_failed(op: &'static str, service: u32, e: ActionError) {
    log::warn!(
        target: "control",
        "Action operation failed (op={} service={} err={:?})",
        op,
        service,
        e
    );
}

#[cold]
#[inline(never)]
pub(crate) fn status_failed(service: u32) {
    log::warn!(
        target: "control",
        "Service status request failed: unknown service (service={})",
        service
    );
}

#[cold]
#[inline(never)]
pub(crate) fn invalid_values(op: &'static str, id: u64, count: usize) {
    log::warn!(
        target: "control",
        "Request rejected: invalid or excess values (op={} id={} count={})",
        op,
        id,
        count
    );
}

#[inline(never)]
pub(crate) fn config_applied(op: &'static str, section: u64, dry_run: bool, restart: bool) {
    if dry_run {
        log::debug!(
            target: "control",
            "Config change validated by dry run (op={} section={})",
            op,
            section
        );
    } else {
        log::info!(
            target: "control",
            "Config change applied (op={} section={} restart_required={})",
            op,
            section,
            restart
        );
    }
}

#[inline(never)]
// A page action is polled (`rohi logs -f` reads the log tail once a second), so
// it is reported at trace, which the persistent log never keeps: following the
// tail must not feed it.
pub(crate) fn invoking(service: u32, action: u32, args: usize, quiet: bool) {
    let level = if quiet {
        log::Level::Trace
    } else {
        log::Level::Debug
    };
    log::log!(
        target: "control",
        level,
        "Invoking action (service={} action={} args_count={})",
        service,
        action,
        args
    );
}

#[inline(never)]
pub(crate) fn invoked(service: u32, action: u32, privilege: Privilege, what: &'static str) {
    if privilege == Privilege::Provision {
        log::info!(
            target: "control",
            "Provisioning action completed (service={} action={} result={})",
            service,
            action,
            what
        );
    } else {
        let level = if what == "feed" {
            log::Level::Trace
        } else {
            log::Level::Debug
        };
        log::log!(
            target: "control",
            level,
            "Action completed (service={} action={} result={})",
            service,
            action,
            what
        );
    }
}

#[inline(never)]
pub(crate) fn stream_opened(service: u32, stream: u32, session: u64, files: usize) {
    log::debug!(
        target: "control",
        "Stream opened (service={} stream={} session={} files={})",
        service,
        stream,
        session,
        files
    );
}

#[cold]
#[inline(never)]
pub(crate) fn held_evicted(service: u32, id: u32, stream: bool) {
    log::warn!(
        target: "control",
        "Table full, closing the oldest stream or snapshot (service={} id={} stream={})",
        service,
        id,
        stream
    );
}

#[inline(never)]
pub(crate) fn stream_read(
    service: u32,
    stream: u32,
    file: u32,
    offset: u64,
    len: usize,
    eof: bool,
) {
    log::trace!(
        target: "control",
        "Stream chunk read (service={} stream={} file={} offset_b={} size_b={} eof={})",
        service,
        stream,
        file,
        offset,
        len,
        eof
    );
}

#[cold]
#[inline(never)]
pub(crate) fn stream_read_oversized(service: u32, stream: u32, len: usize) {
    log::warn!(
        target: "control",
        "Stream read returned more than a chunk (service={} stream={} size_b={})",
        service,
        stream,
        len
    );
}

#[inline(never)]
pub(crate) fn stream_closed(service: u32, stream: u32, result: Result<(), ActionError>) {
    log::debug!(
        target: "control",
        "Stream closed (service={} stream={} result={:?})",
        service,
        stream,
        result
    );
}

#[inline(never)]
pub(crate) fn stream_gone(service: u32, stream: u32) {
    log::debug!(
        target: "control",
        "Stream closed or timed out in the service (service={} stream={})",
        service,
        stream
    );
}

#[cold]
#[inline(never)]
pub(crate) fn session_held_closed(
    session: u64,
    service: u32,
    id: u32,
    stream: bool,
    result: Result<(), ActionError>,
) {
    log::info!(
        target: "control",
        "Session ended, stream or snapshot closed (session={} service={} id={} stream={} result={:?})",
        session,
        service,
        id,
        stream,
        result
    );
}

#[inline(never)]
pub(crate) fn snapshot_opened(service: u32, snapshot: u32, session: u64, total: u16) {
    log::debug!(
        target: "control",
        "Snapshot opened (service={} snapshot={} session={} records={})",
        service,
        snapshot,
        session,
        total
    );
}

#[inline(never)]
pub(crate) fn snapshot_closed(service: u32, snapshot: u32, result: Result<(), ActionError>) {
    log::debug!(
        target: "control",
        "Snapshot closed (service={} snapshot={} result={:?})",
        service,
        snapshot,
        result
    );
}

#[inline(never)]
pub(crate) fn snapshot_gone(service: u32, snapshot: u32) {
    log::debug!(
        target: "control",
        "Snapshot closed or expired in the service (service={} snapshot={})",
        service,
        snapshot
    );
}

#[inline(never)]
pub(crate) fn page_read(service: u32, snapshot: u32, from: u16, count: usize) {
    log::trace!(
        target: "control",
        "Result page read (service={} snapshot={} from={} records={})",
        service,
        snapshot,
        from,
        count
    );
}

#[cold]
#[inline(never)]
pub(crate) fn page_overflow(service: u32, action: u32, fitting: usize, written: usize) {
    log::warn!(
        target: "control",
        "Result page rejected: no progress or too large (service={} action={} fitting={} written={})",
        service,
        action,
        fitting,
        written
    );
}

#[inline(never)]
pub(crate) fn held_closed(service: u32, id: u32, stream: bool, result: Result<(), ActionError>) {
    log::debug!(
        target: "control",
        "Stream or snapshot closed (service={} id={} stream={} result={:?})",
        service,
        id,
        stream,
        result
    );
}
