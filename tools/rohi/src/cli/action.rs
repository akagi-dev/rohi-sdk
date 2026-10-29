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
//! `rohi action`: the generic operations of the services.

use super::diagnostic::CliError;
use super::{Ctx, dto, values};
use crate::model::{ErrorCode, FieldValue, Value};
use crate::{ClientTransport, Device, Error, format};
use zeroize::Zeroize;

/// What `rohi action ...` asks for.
pub struct Request {
    pub service: Option<String>,
    pub action: Option<String>,
    pub args: Vec<String>,
    pub help: bool,
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    request: Request,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let Request {
        service,
        action,
        args,
        help,
    } = request;
    match (service, action) {
        (None, _) => {
            let actions = device.actions().await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::actions(&actions)),
                || format::actions(ctx.paint, &actions),
            )
        }
        (Some(service), None) => list(device, &service, ctx).await,
        (Some(service), Some(action)) if help => {
            let schema = device.action_schema(&service, &action).await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::action_schema(&schema)),
                || format::action_help(ctx.paint, &schema),
            )
        }
        (Some(service), Some(action)) => run_action(device, &service, &action, &args, ctx).await,
    }
}

/// The actions of one service with their arguments.
async fn list<T: ClientTransport>(
    device: &mut Device<T>,
    service: &str,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let actions: Vec<_> = device
        .actions()
        .await?
        .into_iter()
        .filter(|action| action.service == service)
        .collect();
    if actions.is_empty() {
        return Err(CliError::Device(Error::Device(ErrorCode::NotFound)));
    }
    let mut schemas = Vec::new();
    for action in &actions {
        schemas.push(device.action_schema(service, &action.name).await?);
    }
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::actions(&actions)),
        || format::service_actions(ctx.paint, &schemas),
    )
}

async fn run_action<T: ClientTransport>(
    device: &mut Device<T>,
    service: &str,
    action: &str,
    args: &[String],
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.action_schema(service, action).await?;
    let secret = schema.action.secret_args;
    let mut given = Arguments::default();
    for arg in args {
        let (name, text) = values::assignment(arg)?;
        let field = values::lookup(schema.args.iter(), &format!("{service} {action}"), name)?;
        if given.0.iter().any(|given| given.field_id == field.id) {
            return Err(CliError::Usage(format!("{name} is given twice")));
        }
        if secret && text != "-" {
            return Err(CliError::Usage(format!(
                "{service} {action} takes secrets: do not put {name} on the command line, use {name}=- to read it from stdin"
            )));
        }
        let value = values::argument(ctx, field, text).await?;
        given.0.push(FieldValue {
            field_id: field.id,
            value,
        });
    }
    let missing: Vec<&str> = schema
        .args
        .iter()
        .filter(|field| !given.0.iter().any(|given| given.field_id == field.id))
        .map(|field| field.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(CliError::Usage(format!(
            "{service} {action} needs: {}",
            missing.join(", ")
        )));
    }
    let records = device.invoke(service, action, &given.0).await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::records(&records, &schema)),
        || format::records(ctx.paint, &records, &schema),
    )
}

/// The arguments of an action. They can be secrets, so they are overwritten
/// when dropped, whether the call succeeded, failed or was cancelled.
#[derive(Default)]
struct Arguments(Vec<FieldValue>);

impl Drop for Arguments {
    fn drop(&mut self) {
        for argument in &mut self.0 {
            match &mut argument.value {
                Value::String(text) | Value::Enum(text) => text.zeroize(),
                Value::Bytes(bytes) => bytes.zeroize(),
                Value::Bool(_) | Value::I64(_) | Value::U64(_) | Value::F32(_) => {}
            }
        }
    }
}
