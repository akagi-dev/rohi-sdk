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
//! High-level typed device API.
//!
//! Every method is generic: sections, services and actions are named by
//! strings the device lists, so a new firmware module needs no change here.

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

fn name<const N: usize>(value: &str) -> Result<heapless::String<N>> {
    heapless::String::try_from(value)
        .map_err(|_| Error::InvalidInput(format!("the name {value:?} is too long")))
}

/// A connected ROHI device.
///
/// Schemas are fetched on demand and cached for the life of the connection.
pub struct Device<T> {
    session: Session<T>,
    config_schemas: HashMap<String, ConfigSchema>,
    service_schemas: HashMap<String, ServiceSchema>,
    action_schemas: HashMap<(String, String), ActionSchema>,
}

impl<T: ClientTransport> Device<T> {
    /// A device behind `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            session: Session::new(transport),
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
                Ok(list.sections.iter().map(convert::section).collect())
            }
            other => Err(unexpected(&other)),
        }
    }

    /// Schema of a section; cached.
    pub async fn config_schema(&mut self, section: &str) -> Result<ConfigSchema> {
        if let Some(schema) = self.config_schemas.get(section) {
            return Ok(schema.clone());
        }
        // Schemas come in pages that keep every response small.
        let mut schema: Option<ConfigSchema> = None;
        loop {
            let offset = schema.as_ref().map_or(0, |schema| schema.fields.len());
            let call = Call::GetConfigSchema(pb::GetConfigSchemaRequest {
                section: name(section)?,
                offset: offset as u32,
            });
            let (page, total) = match self.call(call).await? {
                Reply::GetConfigSchema(response) => (
                    response
                        .schema()
                        .and_then(convert::config_schema)
                        .ok_or_else(|| Error::Protocol("empty config schema".into()))?,
                    response.total as usize,
                ),
                other => return Err(unexpected(&other)),
            };
            let advanced = !page.fields.is_empty();
            let merged = match schema.take() {
                None => page,
                Some(mut schema) => {
                    schema.fields.extend(page.fields);
                    schema
                }
            };
            let done = merged.fields.len() >= total || !advanced;
            schema = Some(merged);
            if done {
                break;
            }
        }
        let schema = schema.expect("the loop ran at least once");
        self.config_schemas
            .insert(section.to_owned(), schema.clone());
        Ok(schema)
    }

    /// Readable content of a section. Secrets are only reported as set.
    pub async fn config_get(&mut self, section: &str) -> Result<ConfigValues> {
        let call = Call::GetConfigSection(pb::GetConfigSectionRequest {
            section: name(section)?,
        });
        match self.call(call).await? {
            Reply::GetConfigSection(response) => Ok(convert::config_values(&response)),
            other => Err(unexpected(&other)),
        }
    }

    /// Applies `mutations` to a section atomically; with `dry_run` the
    /// device only validates.
    pub async fn config_patch(
        &mut self,
        section: &str,
        mutations: &[Mutation],
        expected_revision: Option<u32>,
        dry_run: bool,
    ) -> Result<Applied> {
        let mut list = heapless::Vec::new();
        for mutation in mutations {
            let wire = convert::mutation(mutation)
                .ok_or_else(|| Error::InvalidInput("a value does not fit the protocol".into()))?;
            list.push(wire).map_err(|_| Error::TooLarge)?;
        }
        let mut request = pb::PatchConfigSectionRequest {
            section: name(section)?,
            mutations: list,
            dry_run,
            ..Default::default()
        };
        if let Some(revision) = expected_revision {
            request.set_expected_revision(revision);
        }
        match self.call(Call::PatchConfigSection(request)).await? {
            Reply::PatchConfigSection(done) => Ok(Applied {
                revision: done.revision,
                restart_required: done.restart_required,
            }),
            other => Err(unexpected(&other)),
        }
    }

    /// Removes the persisted section and restores the defaults.
    pub async fn config_reset(&mut self, section: &str, dry_run: bool) -> Result<Applied> {
        let call = Call::ResetConfigSection(pb::ResetConfigSectionRequest {
            section: name(section)?,
            dry_run,
        });
        match self.call(call).await? {
            Reply::ResetConfigSection(done) => Ok(Applied {
                revision: done.revision,
                restart_required: done.restart_required,
            }),
            other => Err(unexpected(&other)),
        }
    }

    // -- status ---------------------------------------------------------------

    /// The services of the device.
    pub async fn services(&mut self) -> Result<Vec<ServiceInfo>> {
        match self
            .call(Call::ListServices(pb::ListServicesRequest {}))
            .await?
        {
            Reply::ListServices(list) => Ok(list
                .services
                .iter()
                .map(|info| ServiceInfo {
                    name: info.name.as_str().to_owned(),
                    version: info.version,
                })
                .collect()),
            other => Err(unexpected(&other)),
        }
    }

    /// Compact status of every service and the device health, in one request.
    pub async fn services_status(&mut self) -> Result<ServicesStatus> {
        let call = Call::GetServicesStatus(pb::GetServicesStatusRequest {});
        match self.call(call).await? {
            Reply::GetServicesStatus(response) => Ok(ServicesStatus {
                uptime_ms: response.uptime_ms,
                health: DeviceHealth::from_proto(response.health),
                services: response.services.iter().map(convert::summary).collect(),
            }),
            other => Err(unexpected(&other)),
        }
    }

    /// Status schema of a service; cached.
    pub async fn service_schema(&mut self, service: &str) -> Result<ServiceSchema> {
        if let Some(schema) = self.service_schemas.get(service) {
            return Ok(schema.clone());
        }
        let mut schema: Option<ServiceSchema> = None;
        loop {
            let offset = schema.as_ref().map_or(0, |schema| schema.fields.len());
            let call = Call::GetServiceSchema(pb::GetServiceSchemaRequest {
                service: name(service)?,
                offset: offset as u32,
            });
            let (page, total) = match self.call(call).await? {
                Reply::GetServiceSchema(response) => (
                    ServiceSchema {
                        service: ServiceInfo {
                            name: response.service.as_str().to_owned(),
                            version: response.version,
                        },
                        fields: response.fields.iter().map(convert::field).collect(),
                    },
                    response.total as usize,
                ),
                other => return Err(unexpected(&other)),
            };
            let advanced = !page.fields.is_empty();
            let merged = match schema.take() {
                None => page,
                Some(mut schema) => {
                    schema.fields.extend(page.fields);
                    schema
                }
            };
            let done = merged.fields.len() >= total || !advanced;
            schema = Some(merged);
            if done {
                break;
            }
        }
        let schema = schema.expect("the loop ran at least once");
        self.service_schemas
            .insert(service.to_owned(), schema.clone());
        Ok(schema)
    }

    /// Detailed status of a service.
    pub async fn service_status(&mut self, service: &str) -> Result<ServiceStatus> {
        // Values come in pages; the summary and the issues are in every page,
        // the last one is the freshest.
        let mut status: Option<ServiceStatus> = None;
        loop {
            let offset = status.as_ref().map_or(0, |status| status.fields.len());
            let call = Call::GetServiceStatus(pb::GetServiceStatusRequest {
                service: name(service)?,
                offset: offset as u32,
            });
            let (page, total) = match self.call(call).await? {
                Reply::GetServiceStatus(response) => (
                    ServiceStatus {
                        summary: response
                            .summary()
                            .map(convert::summary)
                            .ok_or_else(|| Error::Protocol("status without a summary".into()))?,
                        fields: response
                            .fields
                            .iter()
                            .filter_map(convert::field_value)
                            .collect(),
                        issues: response.issues.iter().map(convert::issue).collect(),
                    },
                    response.total as usize,
                ),
                other => return Err(unexpected(&other)),
            };
            let advanced = !page.fields.is_empty();
            let merged = match status.take() {
                None => page,
                Some(mut status) => {
                    status.fields.extend(page.fields);
                    ServiceStatus {
                        fields: status.fields,
                        ..page
                    }
                }
            };
            let done = merged.fields.len() >= total || !advanced;
            status = Some(merged);
            if done {
                break;
            }
        }
        Ok(status.expect("the loop ran at least once"))
    }

    // -- actions --------------------------------------------------------------

    /// The actions of all services.
    pub async fn actions(&mut self) -> Result<Vec<ActionInfo>> {
        let mut actions: Vec<ActionInfo> = Vec::new();
        loop {
            let call = Call::ListActions(pb::ListActionsRequest {
                offset: actions.len() as u32,
            });
            let (page, total) = match self.call(call).await? {
                Reply::ListActions(list) => (
                    list.actions
                        .iter()
                        .map(convert::action_info)
                        .collect::<Vec<_>>(),
                    list.total as usize,
                ),
                other => return Err(unexpected(&other)),
            };
            let advanced = !page.is_empty();
            actions.extend(page);
            if actions.len() >= total || !advanced {
                return Ok(actions);
            }
        }
    }

    /// Schema of an action; cached.
    pub async fn action_schema(&mut self, service: &str, action: &str) -> Result<ActionSchema> {
        let key = (service.to_owned(), action.to_owned());
        if let Some(schema) = self.action_schemas.get(&key) {
            return Ok(schema.clone());
        }
        let call = Call::GetActionSchema(pb::GetActionSchemaRequest {
            service: name(service)?,
            action: name(action)?,
        });
        let schema = match self.call(call).await? {
            Reply::GetActionSchema(response) => ActionSchema {
                action: response
                    .action()
                    .map(convert::action_info)
                    .ok_or_else(|| Error::Protocol("action schema without an action".into()))?,
                args: response.args.iter().map(convert::field).collect(),
                results: response.results.iter().map(convert::field).collect(),
            },
            other => return Err(unexpected(&other)),
        };
        self.action_schemas.insert(key, schema.clone());
        Ok(schema)
    }

    /// Runs an action with the given arguments (all required) and returns
    /// its result records.
    pub async fn invoke(
        &mut self,
        service: &str,
        action: &str,
        args: &[FieldValue],
    ) -> Result<Vec<Record>> {
        let mut list = heapless::Vec::new();
        for arg in args {
            let mut wire = pb::FieldValue {
                field_id: arg.field_id,
                ..Default::default()
            };
            wire.set_value(
                convert::to_proto(&arg.value).ok_or_else(|| {
                    Error::InvalidInput("a value does not fit the protocol".into())
                })?,
            );
            list.push(wire).map_err(|_| Error::TooLarge)?;
        }
        let request = pb::InvokeActionRequest {
            service: name(service)?,
            action: name(action)?,
            args: list,
        };
        // Scans and similar actions take a while on the device.
        let normal = self.session.timeout();
        self.session.set_timeout(normal.max(ACTION_TIMEOUT));
        let reply = self.call(Call::InvokeAction(request)).await;
        self.session.set_timeout(normal);
        match reply? {
            Reply::InvokeAction(done) => Ok(done
                .records
                .iter()
                .map(|record| {
                    record
                        .fields
                        .iter()
                        .filter_map(convert::field_value)
                        .collect()
                })
                .collect()),
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
