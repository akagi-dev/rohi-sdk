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
//! High-level typed device API.
//! High-level typed device API.
//!
//! Every method is generic: sections, services and actions are named by
//! strings the device lists, so a new firmware module needs no change here.
//! The wire addresses them by number; the device is asked for the numbers once
//! (discovery) and the answers are cached for the life of the connection, as
//! are the schemas that name enum values and issues.

use std::collections::HashMap;
use std::time::Duration;

use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};

use crate::convert;
use crate::error::{Error, Result};
use crate::model::*;
use crate::session::{ClientTransport, Session};

/// Wi-Fi scans and similar actions run on the device and take a while.
const ACTION_TIMEOUT: Duration = Duration::from_secs(40);

/// How often the compact and the detailed status of a service are read again
/// when the service changes between the two.
const STATUS_ATTEMPTS: usize = 3;

/// What a name that the device does not list is answered with.
fn not_found() -> Error {
    Error::Device(ErrorCode::NotFound)
}

/// What a service that exists, but lacks the requested capability, is answered
/// with: it reports status or offers actions, not both.
fn not_supported() -> Error {
    Error::Device(ErrorCode::NotSupported)
}

/// A connected ROHI device.
///
/// Discovery and schemas are fetched on demand and cached for the life of
/// the connection.
pub struct Device<T> {
    session: Session<T>,
    sections: Option<Vec<SectionInfo>>,
    services: Option<Vec<ServiceInfo>>,
    actions: Option<Vec<ActionInfo>>,
    config_schemas: HashMap<String, ConfigSchema>,
    service_schemas: HashMap<String, ServiceSchema>,
    action_schemas: HashMap<(String, String), ActionSchema>,
}

impl<T: ClientTransport> Device<T> {
    /// A device behind `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            session: Session::new(transport),
            sections: None,
            services: None,
            actions: None,
            config_schemas: HashMap::new(),
            service_schemas: HashMap::new(),
            action_schemas: HashMap::new(),
        }
    }

    /// Changes the response timeout of normal requests.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.session.set_timeout(timeout);
    }

    /// Releases the transport, for example to close the connection.
    pub fn into_transport(self) -> T {
        self.session.into_transport()
    }

    async fn call(&mut self, call: Call) -> Result<Reply> {
        self.session.call(call).await
    }

    /// Device description.
    pub async fn info(&mut self) -> Result<DeviceInfo> {
        match self.call(Call::GetInfo(pb::GetInfoRequest {})).await? {
            Reply::GetInfo(info) => Ok(convert::info(info)),
            other => Err(unexpected(&other)),
        }
    }

    // -- config ---------------------------------------------------------------

    /// The config sections of the device.
    pub async fn config_sections(&mut self) -> Result<Vec<SectionInfo>> {
        let call = Call::ListConfigSections(pb::ListConfigSectionsRequest {});
        match self.call(call).await? {
            Reply::ListConfigSections(list) => {
                let sections: Vec<_> = list.sections.iter().map(convert::section).collect();
                self.sections = Some(sections.clone());
                Ok(sections)
            }
            other => Err(unexpected(&other)),
        }
    }

    async fn section_id(&mut self, section: &str) -> Result<u64> {
        if self.sections.is_none() {
            self.config_sections().await?;
        }
        self.sections
            .iter()
            .flatten()
            .find(|info| info.name == section)
            .map(|info| info.id)
            .ok_or_else(not_found)
    }

    /// Schema of a section; cached.
    pub async fn config_schema(&mut self, section: &str) -> Result<ConfigSchema> {
        if let Some(schema) = self.config_schemas.get(section) {
            return Ok(schema.clone());
        }
        let section_id = self.section_id(section).await?;
        let call = Call::GetConfigSchema(pb::GetConfigSchemaRequest { section_id });
        let schema = match self.call(call).await? {
            Reply::GetConfigSchema(response) => convert::config_schema(&response)
                .ok_or_else(|| Error::Protocol("empty config schema".into()))?,
            other => return Err(unexpected(&other)),
        };
        self.config_schemas
            .insert(section.to_owned(), schema.clone());
        Ok(schema)
    }

    /// Readable content of a section. Secrets are only reported as set.
    pub async fn config_get(&mut self, section: &str) -> Result<ConfigValues> {
        let schema = self.config_schema(section).await?;
        let call = Call::GetConfigSection(pb::GetConfigSectionRequest {
            section_id: schema.section.id,
        });
        match self.call(call).await? {
            Reply::GetConfigSection(response) => Ok(convert::config_values(&response, &schema)),
            other => Err(unexpected(&other)),
        }
    }

    /// Applies `mutations` to a section atomically; with `dry_run` the
    /// device only validates.
    pub async fn config_patch(
        &mut self,
        section: &str,
        mutations: &[Mutation],
        dry_run: bool,
    ) -> Result<Applied> {
        let schema = self.config_schema(section).await?;
        let mut list = heapless::Vec::new();
        for mutation in mutations {
            let wire = convert::mutation(mutation, &schema)
                .ok_or_else(|| Error::InvalidInput("a value does not fit the protocol".into()))?;
            list.push(wire).map_err(|_| Error::TooLarge)?;
        }
        let request = pb::PatchConfigSectionRequest {
            section_id: schema.section.id,
            mutations: list,
            dry_run,
        };
        match self.call(Call::PatchConfigSection(request)).await? {
            Reply::PatchConfigSection(done) => Ok(Applied {
                restart_required: done.restart_required,
            }),
            other => Err(unexpected(&other)),
        }
    }

    /// Removes the persisted section and restores the defaults.
    pub async fn config_reset(&mut self, section: &str, dry_run: bool) -> Result<Applied> {
        let request = pb::ResetConfigSectionRequest {
            section_id: self.section_id(section).await?,
            dry_run,
        };
        let call = Call::ResetConfigSection(request);
        match self.call(call).await? {
            Reply::ResetConfigSection(done) => Ok(Applied {
                restart_required: done.restart_required,
            }),
            other => Err(unexpected(&other)),
        }
    }

    // -- status ---------------------------------------------------------------

    /// The services of the device, as discovered by [`Self::services_status`].
    pub async fn services(&mut self) -> Result<Vec<ServiceInfo>> {
        Ok(self
            .services_status()
            .await?
            .services
            .iter()
            .map(|summary| ServiceInfo {
                id: summary.id,
                name: summary.service.clone(),
                version: summary.version,
            })
            .collect())
    }

    /// Discovery and compact status of every service and the device health,
    /// in one request.
    pub async fn services_status(&mut self) -> Result<ServicesStatus> {
        let call = Call::GetServicesStatus(pb::GetServicesStatusRequest {});
        match self.call(call).await? {
            Reply::GetServicesStatus(response) => {
                let status = ServicesStatus {
                    uptime_ms: response.uptime_ms,
                    health: DeviceHealth::from_proto(response.health),
                    services: response.services.iter().map(convert::summary).collect(),
                };
                self.services = Some(
                    status
                        .services
                        .iter()
                        .map(|summary| ServiceInfo {
                            id: summary.id,
                            name: summary.service.clone(),
                            version: summary.version,
                        })
                        .collect(),
                );
                Ok(status)
            }
            other => Err(unexpected(&other)),
        }
    }

    async fn service_id(&mut self, service: &str) -> Result<u32> {
        if self.services.is_none() {
            self.services_status().await?;
        }
        let found = self
            .services
            .iter()
            .flatten()
            .find(|info| info.name == service)
            .map(|info| info.id);
        if let Some(id) = found {
            return Ok(id);
        }
        // An action-only service has no status.
        if self
            .actions()
            .await?
            .iter()
            .any(|info| info.service == service)
        {
            return Err(not_supported());
        }
        Err(not_found())
    }

    /// Status schema of a service; cached.
    pub async fn service_schema(&mut self, service: &str) -> Result<ServiceSchema> {
        if let Some(schema) = self.service_schemas.get(service) {
            return Ok(schema.clone());
        }
        let service_id = self.service_id(service).await?;
        let call = Call::GetServiceSchema(pb::GetServiceSchemaRequest { service_id });
        let schema = match self.call(call).await? {
            Reply::GetServiceSchema(response) => ServiceSchema {
                service: ServiceInfo {
                    id: response.service_id,
                    name: response.name.as_str().to_owned(),
                    version: response.version,
                },
                fields: response
                    .fields
                    .iter()
                    .map(|field| convert::field(field, &response.enums))
                    .collect(),
                issues: response
                    .issues
                    .iter()
                    .map(|issue| IssueInfo {
                        code: issue.code,
                        name: issue.name.as_str().to_owned(),
                    })
                    .collect(),
            },
            other => return Err(unexpected(&other)),
        };
        self.service_schemas
            .insert(service.to_owned(), schema.clone());
        Ok(schema)
    }

    /// Detailed status of a service: its compact status, values and active
    /// issues. The compact status is a separate request, the detailed one does
    /// not repeat it, so the two are paired by revision: when the service
    /// changed between them the pair is read again, a few times at most.
    pub async fn service_status(&mut self, service: &str) -> Result<ServiceStatus> {
        self.read_status(service, None).await
    }

    /// [`Device::service_status`] that starts from a compact status the caller
    /// has just read with [`Device::services_status`], saving that request
    /// when the service has not changed since.
    pub async fn service_status_after(
        &mut self,
        service: &str,
        known: &ServiceSummary,
    ) -> Result<ServiceStatus> {
        self.read_status(service, Some(known.clone())).await
    }

    async fn read_status(
        &mut self,
        service: &str,
        mut known: Option<ServiceSummary>,
    ) -> Result<ServiceStatus> {
        let schema = self.service_schema(service).await?;
        for _ in 0..STATUS_ATTEMPTS {
            let summary = match known.take() {
                Some(summary) if summary.id == schema.service.id => summary,
                _ => self
                    .services_status()
                    .await?
                    .services
                    .into_iter()
                    .find(|summary| summary.id == schema.service.id)
                    .ok_or_else(not_found)?,
            };
            let call = Call::GetServiceStatus(pb::GetServiceStatusRequest {
                service_id: schema.service.id,
            });
            match self.call(call).await? {
                Reply::GetServiceStatus(response) if response.revision == summary.revision => {
                    return Ok(ServiceStatus {
                        summary,
                        fields: response
                            .fields
                            .iter()
                            .filter_map(|field| convert::field_value(field, &schema.fields))
                            .collect(),
                        issues: response
                            .issues
                            .iter()
                            .map(|issue| convert::issue(issue, &schema.issues))
                            .collect(),
                    });
                }
                Reply::GetServiceStatus(_) => {}
                other => return Err(unexpected(&other)),
            }
        }
        Err(Error::StatusChanging(service.to_owned()))
    }

    // -- actions --------------------------------------------------------------

    /// The actions of all services.
    pub async fn actions(&mut self) -> Result<Vec<ActionInfo>> {
        let mut actions: Vec<ActionInfo> = Vec::new();
        loop {
            let call = Call::ListActions(pb::ListActionsRequest {
                offset: actions.len() as u32,
            });
            match self.call(call).await? {
                Reply::ListActions(list) => {
                    actions.extend(list.actions.iter().map(convert::action_info));
                    if list.actions.is_empty() || actions.len() >= list.total as usize {
                        break;
                    }
                }
                other => return Err(unexpected(&other)),
            }
        }
        self.actions = Some(actions.clone());
        Ok(actions)
    }

    /// The entries of the catalog a bitset field selects from, in pages of the
    /// catalog action: `bit_index` (field 1) and `name` (field 2).
    ///
    /// # Errors
    ///
    /// [`Error::Protocol`] when the catalog is not a read-only action with
    /// one offset argument and such records.
    pub async fn catalog(&mut self, selection: &Selection) -> Result<Vec<CatalogEntry>> {
        if self.actions.is_none() {
            self.actions().await?;
        }
        let info = self
            .actions
            .iter()
            .flatten()
            .find(|info| {
                info.service_id == selection.catalog_service_id
                    && info.id == selection.catalog_action_id
            })
            .cloned()
            .ok_or_else(not_found)?;
        let schema = self.action_schema(&info.service, &info.name).await?;
        let [offset] = schema.args.as_slice() else {
            return Err(Error::Protocol(format!(
                "the catalog {} {} must take one offset",
                info.service, info.name
            )));
        };
        let offset_id = offset.id;
        let mut entries = Vec::new();
        loop {
            let args = [FieldValue {
                field_id: offset_id,
                value: Value::U64(entries.len() as u64),
            }];
            let records = self.invoke(&info.service, &info.name, &args).await?;
            if records.is_empty() {
                return Ok(entries);
            }
            for record in records {
                let bit = record.iter().find_map(|f| match (f.field_id, &f.value) {
                    (1, Value::U64(bit)) => Some(*bit),
                    _ => None,
                });
                let name = record.iter().find_map(|f| match (f.field_id, &f.value) {
                    (2, Value::String(name)) => Some(name.clone()),
                    _ => None,
                });
                let (Some(bit), Some(name)) = (bit, name) else {
                    return Err(Error::Protocol(
                        "a catalog record without bit_index and name".into(),
                    ));
                };
                entries.push(CatalogEntry { bit, name });
            }
        }
    }

    async fn action_ids(&mut self, service: &str, action: &str) -> Result<(u32, u32)> {
        if self.actions.is_none() {
            self.actions().await?;
        }
        let found = self
            .actions
            .iter()
            .flatten()
            .find(|info| info.service == service && info.name == action)
            .map(|info| (info.service_id, info.id));
        if let Some(ids) = found {
            return Ok(ids);
        }
        // A status-only service has no actions.
        if self
            .actions
            .iter()
            .flatten()
            .all(|info| info.service != service)
            && self.service_id(service).await.is_ok()
        {
            return Err(not_supported());
        }
        Err(not_found())
    }

    /// Schema of an action; cached.
    pub async fn action_schema(&mut self, service: &str, action: &str) -> Result<ActionSchema> {
        let key = (service.to_owned(), action.to_owned());
        if let Some(schema) = self.action_schemas.get(&key) {
            return Ok(schema.clone());
        }
        let (service_id, action_id) = self.action_ids(service, action).await?;
        let call = Call::GetActionSchema(pb::GetActionSchemaRequest {
            service_id,
            action_id,
        });
        let schema =
            match self.call(call).await? {
                Reply::GetActionSchema(response) => ActionSchema {
                    action: convert::action_info(response.action().ok_or_else(|| {
                        Error::Protocol("action schema without an action".into())
                    })?),
                    args: response
                        .args
                        .iter()
                        .map(|field| {
                            convert::action_field(field, &response.enums, &response.selections)
                        })
                        .collect(),
                    results: response
                        .results
                        .iter()
                        .map(|field| convert::field(field, &response.enums))
                        .collect(),
                },
                other => return Err(unexpected(&other)),
            };
        self.action_schemas.insert(key, schema.clone());
        Ok(schema)
    }

    /// Runs an action with the given arguments (all required) and returns its
    /// result records.
    ///
    /// # Errors
    ///
    /// [`Error::Protocol`] when the action returns a stream: use
    /// [`invoke_outcome`](Self::invoke_outcome) for those.
    pub async fn invoke(
        &mut self,
        service: &str,
        action: &str,
        args: &[FieldValue],
    ) -> Result<Vec<Record>> {
        match self.invoke_outcome(service, action, args).await? {
            ActionOutcome::Records(records) => Ok(records),
            ActionOutcome::Stream(_) => Err(Error::Protocol(format!(
                "{service} {action} returns a stream, which this call cannot show"
            ))),
        }
    }

    /// Runs an action and returns what it returned: records, or a stream the
    /// caller reads with [`read_stream`](Self::read_stream) and must
    /// [`close_stream`](Self::close_stream).
    pub async fn invoke_outcome(
        &mut self,
        service: &str,
        action: &str,
        args: &[FieldValue],
    ) -> Result<ActionOutcome> {
        let schema = self.action_schema(service, action).await?;
        let mut list = heapless::Vec::new();
        for arg in args {
            let wire = convert::to_action_arg(arg, &schema.args)
                .ok_or_else(|| Error::InvalidInput("a value does not fit the protocol".into()))?;
            list.push(wire).map_err(|_| Error::TooLarge)?;
        }
        let request = pb::InvokeActionRequest {
            service_id: schema.action.service_id,
            action_id: schema.action.id,
            args: list,
        };
        // Scans and similar actions take a while on the device.
        let normal = self.session.timeout();
        self.session.set_timeout(normal.max(ACTION_TIMEOUT));
        let reply = self.call(Call::InvokeAction(request)).await;
        self.session.set_timeout(normal);
        match reply? {
            Reply::ActionStream(info) => Ok(ActionOutcome::Stream(Stream {
                service_id: info.service_id,
                id: info.id,
                files: info
                    .files
                    .iter()
                    .map(|file| StreamFile {
                        id: file.id,
                        name: file.name.as_str().to_owned(),
                        size_bytes: file.size_bytes,
                    })
                    .collect(),
            })),
            Reply::InvokeAction(done) => Ok(ActionOutcome::Records(
                done.records
                    .iter()
                    .map(|record| {
                        record
                            .fields
                            .iter()
                            .filter_map(|field| convert::field_value(field, &schema.results))
                            .collect()
                    })
                    .collect(),
            )),
            other => Err(unexpected(&other)),
        }
    }

    /// Reads the chunk of a stream file at `offset`. A read can be repeated
    /// with the same offset: it returns the same bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Device`] with [`ErrorCode::NotFound`] when the stream was
    /// closed or timed out on the device.
    pub async fn read_stream(
        &mut self,
        stream: &Stream,
        file: u32,
        offset: u64,
    ) -> Result<StreamChunk> {
        let call = Call::ReadStream(pb::ReadStreamRequest {
            service_id: stream.service_id,
            stream_id: stream.id,
            file_id: file,
            offset,
        });
        match self.call(call).await? {
            Reply::ReadStream(chunk) => {
                if chunk.stream_id != stream.id || chunk.file_id != file || chunk.offset != offset {
                    return Err(Error::Protocol("a chunk of another read".into()));
                }
                Ok(StreamChunk {
                    data: chunk.data.to_vec(),
                    eof: chunk.eof,
                })
            }
            other => Err(unexpected(&other)),
        }
    }

    /// Closes a stream: the device releases what it holds for it.
    pub async fn close_stream(&mut self, stream: &Stream) -> Result<()> {
        let call = Call::CloseStream(pb::CloseStreamRequest {
            service_id: stream.service_id,
            stream_id: stream.id,
        });
        match self.call(call).await? {
            Reply::CloseStream(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }
}

fn unexpected(reply: &Reply) -> Error {
    let kind = match reply {
        Reply::Error(_) => "error",
        Reply::GetInfo(_) => "info",
        _ => "another response",
    };
    Error::Protocol(format!("unexpected response: {kind}"))
}
