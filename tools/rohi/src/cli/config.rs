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
//! `rohi config`: generic configuration, driven by the schemas of the device.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json};

use super::args::ConfigAction;
use super::diagnostic::{CliError, PartialApply};
use super::{Ctx, dto, values};
use crate::format::{self, Paint};
use crate::model::*;
use crate::{ClientTransport, Device, Error};

/// What `rohi config ...` asks for besides the fixed commands.
pub struct Request {
    pub section: Option<String>,
    pub assignments: Vec<String>,
    pub dry_run: bool,
    pub help: bool,
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: Option<ConfigAction>,
    request: Request,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        Some(ConfigAction::Reset {
            section,
            yes,
            dry_run,
        }) => {
            // Validate the name before asking anybody anything.
            device.config_schema(&section).await?;
            if !dry_run {
                ctx.io
                    .confirm(
                        &format!(
                            "Every override of the `{section}` section will be removed: it will follow the defaults again.\n\nContinue?"
                        ),
                        yes,
                    )
                    .await?;
            }
            let applied = device.config_reset(&section, dry_run).await?;
            report(ctx, &section, &applied, dry_run, "reset to the defaults")
        }
        Some(ConfigAction::Export { file }) => export(device, file, ctx).await,
        Some(ConfigAction::Diff { file }) => diff(device, &file, ctx).await,
        Some(ConfigAction::Apply { file, dry_run, yes }) => {
            apply(device, &file, dry_run, yes, ctx).await
        }
        None => match request.section {
            None => {
                if !request.assignments.is_empty() || request.dry_run {
                    return Err(CliError::Usage("name the section to change".into()));
                }
                let sections = device.config_sections().await?;
                ctx.io.emit(
                    ctx.format,
                    &dto::Versioned::new(dto::sections(&sections)),
                    || format::config_sections(ctx.paint, &sections),
                )
            }
            Some(section) if request.help => {
                let schema = device.config_schema(&section).await?;
                ctx.io.emit(
                    ctx.format,
                    &dto::Versioned::new(dto::config_schema(&schema)),
                    || format::config_help(ctx.paint, &schema),
                )
            }
            Some(section) if request.assignments.is_empty() => {
                if request.dry_run {
                    return Err(CliError::Usage(
                        "--dry-run needs FIELD=VALUE to validate".into(),
                    ));
                }
                show(device, &section, false, ctx).await
            }
            Some(section) => {
                set(device, &section, &request.assignments, request.dry_run, ctx).await
            }
        },
    }
}

/// Reads the section again and prints what the device reports: the effective
/// configuration. After a change it also says whether a restart is needed.
async fn show<T: ClientTransport>(
    device: &mut Device<T>,
    section: &str,
    restart_required: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.config_schema(section).await?;
    let current = device.config_get(section).await?;
    let mut body = dto::config_values(&current, &schema);
    if restart_required {
        body["restart_required"] = Json::Bool(true);
    }
    ctx.io.emit(ctx.format, &dto::Versioned::new(body), || {
        let mut text = format::config_values(ctx.paint, &current, &schema);
        if restart_required {
            text.push_str("\nA restart is required for the change to take effect\n");
        }
        text
    })
}

fn report(
    ctx: &mut Ctx,
    section: &str,
    applied: &Applied,
    dry_run: bool,
    done: &str,
) -> Result<(), CliError> {
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::applied(section, applied, dry_run)),
        || {
            let mut text = if dry_run {
                format!("`{section}` is valid; nothing was changed\n")
            } else {
                format!("`{section}` {done}\n")
            };
            if applied.restart_required && !dry_run {
                text.push_str("A restart is required for the change to take effect\n");
            }
            text
        },
    )
}

fn writable<'a>(
    schema: &'a ConfigSchema,
    section: &str,
    name: &str,
) -> Result<&'a ConfigField, CliError> {
    let field = values::lookup(
        schema.fields.iter().map(|field| &field.field),
        section,
        name,
    )?;
    let config = schema
        .fields
        .iter()
        .find(|config| config.field.id == field.id)
        .expect("the field comes from the schema");
    if !config.writable {
        return Err(CliError::Usage(format!("{section}.{name} is read-only")));
    }
    Ok(config)
}

/// `FIELD=VALUE` sets an override, `FIELD=none` overrides an optional field
/// with no value and `FIELD=null` removes the override. Afterwards the section
/// is read again and printed as the device reports it.
async fn set<T: ClientTransport>(
    device: &mut Device<T>,
    section: &str,
    assignments: &[String],
    dry_run: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.config_schema(section).await?;
    let mut mutations: Vec<Mutation> = Vec::new();
    for assignment in assignments {
        let (name, text) = values::assignment(assignment)?;
        let field = writable(&schema, section, name)?;
        if mutations
            .iter()
            .any(|mutation| mutation.field_id == field.field.id)
        {
            return Err(CliError::Usage(format!("{name} is given twice")));
        }
        let op = if text == "null" {
            MutationOp::Clear
        } else if text == "none" {
            if !field.optional {
                return Err(CliError::Usage(format!(
                    "{name} is not nullable: it always has a value; use {name}=null to follow the default"
                )));
            }
            MutationOp::None
        } else {
            if field.secret && text != "-" {
                return Err(CliError::Usage(format!(
                    "{name} is a secret: do not put it on the command line, use {name}=- to read it from stdin"
                )));
            }
            MutationOp::Set(values::argument(ctx, &field.field, text).await?)
        };
        mutations.push(Mutation {
            field_id: field.field.id,
            op,
        });
    }
    let applied = device.config_patch(section, &mutations, dry_run).await?;
    if dry_run {
        return report(ctx, section, &applied, true, "");
    }
    show(device, section, applied.restart_required, ctx).await
}

// -- files ----------------------------------------------------------------------

/// The explicit overrides of every section that has some, without secrets and
/// read-only fields. A field that follows the defaults is not part of it: the
/// file says what the user chose, not what the firmware defaults are today. An
/// explicit "no value" is `{"none": true}`, distinct from `null`, which clears
/// an override.
async fn snapshot<T: ClientTransport>(
    device: &mut Device<T>,
) -> Result<BTreeMap<String, Map<String, Json>>, CliError> {
    let mut out = BTreeMap::new();
    for info in device.config_sections().await? {
        let schema = device.config_schema(&info.name).await?;
        let current = device.config_get(&info.name).await?;
        let mut fields = Map::new();
        for field in schema.fields.iter().filter(|f| f.writable && !f.secret) {
            if !current.is_overridden(field.field.id) {
                continue;
            }
            let entry = current
                .entries
                .iter()
                .find(|entry| entry.field_id == field.field.id);
            match entry {
                Some(ConfigEntry {
                    state: EntryState::Value(value),
                    ..
                }) => {
                    fields.insert(
                        field.field.name.clone(),
                        serde_json::to_value(value).unwrap_or(Json::Null),
                    );
                }
                // Overridden without a value: an explicit none.
                None if field.optional => {
                    fields.insert(field.field.name.clone(), none_json());
                }
                _ => {}
            }
        }
        // A section with only secret overrides has nothing to say here.
        if !fields.is_empty() {
            out.insert(info.name, fields);
        }
    }
    Ok(out)
}

/// The file representation of an explicit none.
fn none_json() -> Json {
    serde_json::json!({ "none": true })
}

/// Whether `json` is the explicit none of a file.
fn is_none_json(json: &Json) -> bool {
    json == &none_json()
}

async fn export<T: ClientTransport>(
    device: &mut Device<T>,
    file: Option<PathBuf>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let document = snapshot(device).await?;
    let mut text = serde_json::to_string_pretty(&document)
        .map_err(|error| CliError::File(error.to_string()))?;
    text.push('\n');
    match file.as_deref() {
        None => ctx.io.result(&text),
        Some(path) if path == Path::new("-") => ctx.io.result(&text),
        Some(path) => {
            std::fs::write(path, &text).map_err(|error| {
                CliError::File(format!("cannot write {}: {error}", path.display()))
            })?;
            let count = document.len();
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(serde_json::json!({
                    "file": path.display().to_string(),
                    "sections": count,
                })),
                || format!("Exported {count} section(s) to {}\n", path.display()),
            )
        }
    }
}

/// The state of a field before a file is applied.
#[derive(PartialEq)]
enum Was {
    /// Not overridden: the firmware default applies.
    Default,
    /// Explicitly overridden with no value.
    None,
    /// Explicitly overridden with a value.
    Value(Value),
}

/// One difference of a section.
struct Change {
    marker: char,
    name: String,
    text: String,
}

/// What applying the file does to one section.
struct SectionPlan {
    section: String,
    reset: bool,
    mutations: Vec<Mutation>,
    changes: Vec<Change>,
}

fn read_document(path: &Path) -> Result<Map<String, Json>, CliError> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .map_err(|error| CliError::File(format!("cannot read stdin: {error}")))?;
        text
    } else {
        std::fs::read_to_string(path)
            .map_err(|error| CliError::File(format!("cannot read {}: {error}", path.display())))?
    };
    match serde_json::from_str::<Json>(&text) {
        Ok(Json::Object(map)) => Ok(map),
        Ok(_) => Err(CliError::File(
            "the file must be a JSON object that maps section names to their fields".into(),
        )),
        Err(error) => Err(CliError::File(format!("not valid JSON: {error}"))),
    }
}

/// Compares the file with the device. Everything that can be checked
/// without the device changing is checked here, before anything is stored.
async fn plan<T: ClientTransport>(
    device: &mut Device<T>,
    document: &Map<String, Json>,
) -> Result<Vec<SectionPlan>, CliError> {
    let known: Vec<String> = device
        .config_sections()
        .await?
        .into_iter()
        .map(|section| section.name)
        .collect();
    let mut plans = Vec::new();
    for (section, body) in document {
        if !known.contains(section) {
            return Err(CliError::File(format!(
                "the device has no section `{section}`; its sections are: {}",
                known.join(", ")
            )));
        }
        let schema = device.config_schema(section).await?;
        let current = device.config_get(section).await?;
        let mut plan = SectionPlan {
            section: section.clone(),
            reset: false,
            mutations: Vec::new(),
            changes: Vec::new(),
        };
        match body {
            Json::Null => {
                if !current.overridden.is_empty() {
                    plan.reset = true;
                    plan.changes.push(Change {
                        marker: '-',
                        name: String::new(),
                        text: "reset section".into(),
                    });
                }
            }
            Json::Object(fields) => {
                for (name, json) in fields {
                    let config = schema
                        .fields
                        .iter()
                        .find(|config| &config.field.name == name)
                        .ok_or_else(|| {
                            let names: Vec<&str> = schema
                                .fields
                                .iter()
                                .map(|f| f.field.name.as_str())
                                .collect();
                            CliError::File(format!(
                                "{section} has no field `{name}`; its fields are: {}",
                                names.join(", ")
                            ))
                        })?;
                    if !config.writable {
                        return Err(CliError::File(format!("{section}.{name} is read-only")));
                    }
                    if config.secret {
                        return Err(CliError::File(format!(
                            "{section}.{name} is a secret and cannot be part of a file; \
                             use `rohi config {section} {name}=-`"
                        )));
                    }
                    // The explicit override of the field, if there is one. The
                    // file is compared with what the user chose, not with the
                    // effective value: a value that equals the default is
                    // still a new override, and an explicit none is not the
                    // default.
                    let was = if !current.is_overridden(config.field.id) {
                        Was::Default
                    } else {
                        match current
                            .entries
                            .iter()
                            .find(|entry| entry.field_id == config.field.id)
                            .map(|entry| &entry.state)
                        {
                            Some(EntryState::Value(value)) => Was::Value(value.clone()),
                            _ => Was::None,
                        }
                    };
                    let describe = |was: &Was| match was {
                        Was::Default => "(default)".to_owned(),
                        Was::None => "(none)".to_owned(),
                        Was::Value(value) => format::value(value, Some(&config.field)),
                    };
                    let (op, now) = if json.is_null() {
                        (MutationOp::Clear, Was::Default)
                    } else if is_none_json(json) {
                        if !config.optional {
                            return Err(CliError::File(format!(
                                "{section}.{name} is not nullable: it always has a value"
                            )));
                        }
                        (MutationOp::None, Was::None)
                    } else {
                        let new = values::from_json(&config.field, json)?;
                        (MutationOp::Set(new.clone()), Was::Value(new))
                    };
                    if was != now {
                        plan.changes.push(Change {
                            marker: match (&was, &now) {
                                (_, Was::Default) => '-',
                                (Was::Default, _) => '+',
                                _ => '~',
                            },
                            name: name.clone(),
                            text: match &was {
                                Was::Default => describe(&now),
                                _ => format!("{} -> {}", describe(&was), describe(&now)),
                            },
                        });
                        plan.mutations.push(Mutation {
                            field_id: config.field.id,
                            op,
                        });
                    }
                }
            }
            _ => {
                return Err(CliError::File(format!(
                    "{section} must be an object of fields or null"
                )));
            }
        }
        plans.push(plan);
    }
    Ok(plans)
}

fn diff_text(paint: Paint, plans: &[SectionPlan]) -> String {
    let mut out = String::new();
    for plan in plans.iter().filter(|plan| !plan.changes.is_empty()) {
        out.push_str(&format!(
            "{}\n",
            paint.strong(&format!("{}:", plan.section))
        ));
        for change in &plan.changes {
            if change.name.is_empty() {
                out.push_str(&format!("  {}\n", change.text));
            } else {
                out.push_str(&format::diff_line(
                    paint,
                    change.marker,
                    &change.name,
                    &change.text,
                ));
            }
        }
    }
    if out.is_empty() {
        out.push_str("No differences\n");
    }
    out
}

fn diff_json(plans: &[SectionPlan]) -> Json {
    let sections: Vec<Json> = plans
        .iter()
        .filter(|plan| !plan.changes.is_empty())
        .map(|plan| {
            serde_json::json!({
                "section": plan.section,
                "reset": plan.reset,
                "changes": plan.changes.iter().filter(|c| !c.name.is_empty()).map(|change| {
                    serde_json::json!({ "field": change.name, "change": change.text })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "differences": sections })
}

async fn diff<T: ClientTransport>(
    device: &mut Device<T>,
    file: &Path,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let document = read_document(file)?;
    let plans = plan(device, &document).await?;
    ctx.io
        .emit(ctx.format, &dto::Versioned::new(diff_json(&plans)), || {
            diff_text(ctx.paint, &plans)
        })
}

async fn apply<T: ClientTransport>(
    device: &mut Device<T>,
    file: &Path,
    dry_run: bool,
    yes: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let document = read_document(file)?;
    let plans: Vec<SectionPlan> = plan(device, &document)
        .await?
        .into_iter()
        .filter(|plan| !plan.changes.is_empty())
        .collect();
    if plans.is_empty() {
        return ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(serde_json::json!({ "applied": [], "dry_run": dry_run })),
            || "Nothing to change\n".to_owned(),
        );
    }

    // The device validates the complete result of every section before the
    // first one is stored.
    for plan in &plans {
        if plan.reset {
            device.config_reset(&plan.section, true).await?;
        } else {
            device
                .config_patch(&plan.section, &plan.mutations, true)
                .await?;
        }
    }
    if dry_run {
        return ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(serde_json::json!({
                "applied": [],
                "dry_run": true,
                "valid": plans.iter().map(|plan| plan.section.clone()).collect::<Vec<_>>(),
            })),
            || {
                format!(
                    "The file is valid; nothing was changed\n\n{}",
                    diff_text(ctx.paint, &plans)
                )
            },
        );
    }

    ctx.io
        .confirm(
            &format!(
                "This changes the device:\n\n{}\nContinue?",
                diff_text(Paint::plain(), &plans)
            ),
            yes,
        )
        .await?;
    let mut applied: Vec<String> = Vec::new();
    let mut restart = false;
    for (index, plan) in plans.iter().enumerate() {
        let result = if plan.reset {
            device.config_reset(&plan.section, false).await
        } else {
            device
                .config_patch(&plan.section, &plan.mutations, false)
                .await
        };
        match result {
            Ok(done) => {
                restart |= done.restart_required;
                applied.push(plan.section.clone());
            }
            Err(source) => return Err(partial(applied, plan, &plans[index + 1..], source)),
        }
    }
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(serde_json::json!({
            "applied": applied,
            "dry_run": false,
            "restart_required": restart,
        })),
        || {
            let mut text = format!("Applied: {}\n", applied.join(", "));
            if restart {
                text.push_str("A restart is required for the changes to take effect\n");
            }
            text
        },
    )
}

fn partial(
    applied: Vec<String>,
    failed: &SectionPlan,
    pending: &[SectionPlan],
    source: Error,
) -> CliError {
    CliError::PartialApply(Box::new(PartialApply {
        applied,
        failed: failed.section.clone(),
        pending: pending.iter().map(|plan| plan.section.clone()).collect(),
        source,
    }))
}
