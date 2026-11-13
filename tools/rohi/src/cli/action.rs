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
//! The actions of the services: `rohi action ...` (arguments as
//! `NAME=VALUE`) and `rohi SERVICE ACTION [ARG]...` (positional arguments).
//!
//! Both forms are two ways to write the same call. They share the discovery of
//! the action, the schema, the typed conversion of the values, the handling of
//! secrets, the invocation and the rendering of the result; only the step that
//! says which word is the value of which argument differs ([`named` and
//! `positional`](Syntax)). No service, action or argument is known by name.

use super::diagnostic::CliError;
use super::positional::Words;
use super::{Ctx, dto, values};
use crate::format::Syntax;
use crate::model::{
    ActionInfo, ActionOutcome, ActionSchema, ErrorCode, FieldDescriptor, FieldValue, Stream, Value,
};
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
        (Some(service), None) => list(device, &service, Syntax::Named, ctx).await,
        (Some(service), Some(action)) if help => {
            describe(device, &service, &action, Syntax::Named, ctx).await
        }
        (Some(service), Some(action)) => {
            let schema = device.action_schema(&service, &action).await?;
            let given = named(&schema, &service, &action, &args)?;
            invoke(device, &schema, given, Syntax::Named, ctx).await
        }
    }
}

/// `rohi SERVICE [ACTION [ARG]...]`.
pub async fn run_positional<T: ClientTransport>(
    device: &mut Device<T>,
    words: Words,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let Words {
        service,
        action,
        args,
        help,
    } = words;
    let actions = device.actions().await?;
    let Some(action) = action else {
        if !args.is_empty() {
            return Err(CliError::Usage(format!(
                "{service} needs an action before its arguments"
            )));
        }
        known_service(&actions, &service)?;
        return list(device, &service, Syntax::Positional, ctx).await;
    };
    known_action(&actions, &service, &action)?;
    if help {
        return describe(device, &service, &action, Syntax::Positional, ctx).await;
    }
    let schema = device.action_schema(&service, &action).await?;
    let given = positional(&schema, &service, &action, &args)?;
    invoke(device, &schema, given, Syntax::Positional, ctx).await
}

/// The service exists and has actions; otherwise says what is known.
fn known_service(actions: &[ActionInfo], service: &str) -> Result<(), CliError> {
    if actions.iter().any(|action| action.service == service) {
        return Ok(());
    }
    let mut services: Vec<&str> = actions
        .iter()
        .map(|action| action.service.as_str())
        .collect();
    services.dedup();
    Err(CliError::Usage(if services.is_empty() {
        format!("`{service}` is not a command, and the device offers no actions")
    } else {
        format!(
            "`{service}` is neither a command nor a service of the device with actions; the services with actions are: {}",
            services.join(", ")
        )
    }))
}

/// The action exists; otherwise says what the service offers.
fn known_action(actions: &[ActionInfo], service: &str, action: &str) -> Result<(), CliError> {
    known_service(actions, service)?;
    if actions
        .iter()
        .any(|info| info.service == service && info.name == action)
    {
        return Ok(());
    }
    let names: Vec<&str> = actions
        .iter()
        .filter(|info| info.service == service)
        .map(|info| info.name.as_str())
        .collect();
    Err(CliError::Usage(format!(
        "{service} has no action `{action}`; its actions are: {}",
        names.join(", ")
    )))
}

/// The actions of one service with their arguments.
async fn list<T: ClientTransport>(
    device: &mut Device<T>,
    service: &str,
    syntax: Syntax,
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
        || format::service_actions(ctx.paint, service, &schemas, syntax),
    )
}

/// The help of one action, generated from its schema.
async fn describe<T: ClientTransport>(
    device: &mut Device<T>,
    service: &str,
    action: &str,
    syntax: Syntax,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.action_schema(service, action).await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::action_schema(&schema)),
        || format::action_help(ctx.paint, &schema, syntax),
    )
}

/// One argument as it was given: the field it is for and its text.
struct Given<'a> {
    field: &'a FieldDescriptor,
    text: &'a str,
}

/// `NAME=VALUE` pairs in any order.
fn named<'a>(
    schema: &'a ActionSchema,
    service: &str,
    action: &str,
    args: &'a [String],
) -> Result<Vec<Given<'a>>, CliError> {
    let mut given: Vec<Given<'a>> = Vec::new();
    for arg in args {
        let (name, text) = values::assignment(arg)?;
        let field = values::lookup(schema.args.iter(), &format!("{service} {action}"), name)?;
        if given.iter().any(|given| given.field.id == field.id) {
            return Err(CliError::Usage(format!("{name} is given twice")));
        }
        given.push(Given { field, text });
    }
    let missing: Vec<&str> = schema
        .args
        .iter()
        .filter(|field| !given.iter().any(|given| given.field.id == field.id))
        .map(|field| field.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(CliError::Usage(format!(
            "{service} {action} needs: {}",
            missing.join(", ")
        )));
    }
    Ok(given)
}

/// One word per argument, in the order of `schema.args` as the device lists
/// them. The numeric ids of the fields play no part. Nothing a user typed is
/// repeated in the errors: the words can be secrets.
fn positional<'a>(
    schema: &'a ActionSchema,
    service: &str,
    action: &str,
    args: &'a [String],
) -> Result<Vec<Given<'a>>, CliError> {
    let names = |fields: &[FieldDescriptor]| {
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let count = |n: usize| match n {
        1 => "1 argument".to_owned(),
        n => format!("{n} arguments"),
    };
    let wanted = schema.args.len();
    let secrets = if schema.action.secret_args {
        "; secrets are read with `-`"
    } else {
        ""
    };
    match args.len().cmp(&wanted) {
        std::cmp::Ordering::Less => Err(CliError::Usage(format!(
            "{service} {action} needs {} ({}), {} given: missing {}{secrets}",
            count(wanted),
            names(&schema.args),
            args.len(),
            names(&schema.args[args.len()..]),
        ))),
        std::cmp::Ordering::Greater if wanted == 0 => Err(CliError::Usage(format!(
            "{service} {action} takes no arguments, {} given",
            args.len()
        ))),
        std::cmp::Ordering::Greater => Err(CliError::Usage(format!(
            "{service} {action} takes {} ({}), {} given",
            count(wanted),
            names(&schema.args),
            args.len()
        ))),
        std::cmp::Ordering::Equal => Ok(schema
            .args
            .iter()
            .zip(args)
            .map(|(field, text)| Given { field, text })
            .collect()),
    }
}

/// Converts the words to typed values and runs the action. The same for both
/// syntaxes.
async fn invoke<T: ClientTransport>(
    device: &mut Device<T>,
    schema: &ActionSchema,
    given: Vec<Given<'_>>,
    syntax: Syntax,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let (service, action) = (&schema.action.service, &schema.action.name);
    let mut arguments = Arguments::default();
    for Given { field, text } in given {
        if schema.action.secret_args && text != "-" {
            let how = match syntax {
                Syntax::Named => format!("use {}=- to read it from stdin", field.name),
                Syntax::Positional => "use - to read it from stdin".to_owned(),
            };
            return Err(CliError::Usage(format!(
                "{service} {action} takes secrets: do not put {} on the command line, {how}",
                field.name
            )));
        }
        let value = values::argument(ctx, field, text).await?;
        arguments.0.push(FieldValue {
            field_id: field.id,
            value,
        });
    }
    match device.invoke_outcome(service, action, &arguments.0).await? {
        ActionOutcome::Records(records) => ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(dto::records(&records, schema)),
            || format::records(ctx.paint, &records, schema),
        ),
        ActionOutcome::Stream(stream) => show_stream(device, &stream, ctx).await,
    }
}

/// Writes every file of a stream to stdout, closing the stream even when a read
/// or write fails. File names and sizes are reported on stderr.
async fn show_stream<T: ClientTransport>(
    device: &mut Device<T>,
    stream: &Stream,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let result = async {
        for file in &stream.files {
            ctx.io
                .note(&format!("{:?}  {} bytes\n", file.name, file.size_bytes));
            let mut offset = 0u64;
            loop {
                let chunk = device.read_stream(stream, file.id, offset).await?;
                let next = offset
                    .checked_add(chunk.data.len() as u64)
                    .ok_or_else(|| Error::Protocol("a stream file is too large".into()))?;
                if next > file.size_bytes {
                    return Err(
                        Error::Protocol("a stream file is larger than announced".into()).into(),
                    );
                }
                ctx.io.write_bytes(&chunk.data)?;
                offset = next;
                if chunk.eof {
                    if offset != file.size_bytes {
                        return Err(Error::Protocol(
                            "a stream file is smaller than announced".into(),
                        )
                        .into());
                    }
                    break;
                }
                if chunk.data.is_empty() {
                    return Err(Error::Protocol("the device returned an empty chunk".into()).into());
                }
            }
        }
        Ok(())
    }
    .await;
    let _ = device.close_stream(stream).await;
    result
}

/// The arguments of an action. They can be secrets, so they are overwritten
/// when dropped, whether the call succeeded, failed or was cancelled.
#[derive(Default)]
struct Arguments(Vec<FieldValue>);

impl Drop for Arguments {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

fn wipe(arguments: &mut [FieldValue]) {
    for argument in arguments {
        match &mut argument.value {
            Value::String(text) | Value::Enum(text) => text.zeroize(),
            Value::Bytes(bytes) => bytes.zeroize(),
            Value::Bool(_) | Value::I64(_) | Value::U64(_) | Value::F32(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_argument_buffers_are_overwritten() {
        let mut arguments = vec![
            FieldValue {
                field_id: 1,
                value: Value::Bytes(vec![1, 2, 3]),
            },
            FieldValue {
                field_id: 2,
                value: Value::String("hunter2".into()),
            },
        ];
        wipe(&mut arguments);
        assert_eq!(arguments[0].value, Value::Bytes(Vec::new()));
        assert_eq!(arguments[1].value, Value::String(String::new()));
    }
}
