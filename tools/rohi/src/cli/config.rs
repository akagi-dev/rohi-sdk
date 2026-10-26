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

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: ConfigAction,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        ConfigAction::List => {
            let sections = device.config_sections().await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::sections(&sections)),
                || format::config_sections(ctx.paint, &sections),
            )
        }
        ConfigAction::Describe { section } => {
            let schema = device.config_schema(&section).await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::config_schema(&schema)),
                || format::config_schema(ctx.paint, &schema),
            )
        }
        ConfigAction::Get { section, field } => get(device, &section, field.as_deref(), ctx).await,
        ConfigAction::Set {
            section,
            assignments,
            dry_run,
        } => set(device, &section, &assignments, dry_run, ctx).await,
        ConfigAction::Unset {
            section,
            fields,
            dry_run,
        } => unset(device, &section, &fields, dry_run, ctx).await,
        ConfigAction::Reset {
            section,
            yes,
            dry_run,
        } => {
            // Validate the name before asking anybody anything.
            device.config_schema(&section).await?;
            if !dry_run {
                ctx.io
                    .confirm(
                        &format!(
                            "The `{section}` section will be removed and its defaults restored.\n\nContinue?"
                        ),
                        yes,
                    )
                    .await?;
            }
            let applied = device.config_reset(&section, dry_run).await?;
            report(ctx, &section, &applied, dry_run)
        }
        ConfigAction::Export { file } => export(device, file, ctx).await,
        ConfigAction::Diff { file } => diff(device, &file, ctx).await,
        ConfigAction::Apply { file, dry_run, yes } => apply(device, &file, dry_run, yes, ctx).await,
    }
}

async fn get<T: ClientTransport>(
    device: &mut Device<T>,
    section: &str,
    only: Option<&str>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.config_schema(section).await?;
    if let Some(name) = only {
        values::lookup(
            schema.fields.iter().map(|field| &field.field),
            section,
            name,
        )?;
    }
    let current = device.config_get(section).await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::config_values(&current, &schema, only)),
        || format::config_values(ctx.paint, &current, &schema, only),
    )
}

fn report(ctx: &mut Ctx, section: &str, applied: &Applied, dry_run: bool) -> Result<(), CliError> {
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::applied(section, applied, dry_run)),
        || {
            let mut text = if dry_run {
                format!("`{section}` is valid; nothing was changed\n")
            } else {
                format!("`{section}` stored (revision {})\n", applied.revision)
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

async fn set<T: ClientTransport>(
    device: &mut Device<T>,
    section: &str,
    assignments: &[String],
    dry_run: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.config_schema(section).await?;
    let mut mutations = Vec::new();
    for assignment in assignments {
        let (name, text) = values::assignment(assignment)?;
        let field = writable(&schema, section, name)?;
        if field.secret && text != "-" {
            return Err(CliError::Usage(format!(
                "{name} is a secret: do not put it on the command line, use {name}=- to read it from stdin"
            )));
        }
        let value = values::argument(ctx, &field.field, text).await?;
        mutations.push(Mutation {
            field_id: field.field.id,
            op: MutationOp::Set(value),
        });
    }
    let applied = device
        .config_patch(section, &mutations, None, dry_run)
        .await?;
    report(ctx, section, &applied, dry_run)
}

async fn unset<T: ClientTransport>(
    device: &mut Device<T>,
    section: &str,
    fields: &[String],
    dry_run: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let schema = device.config_schema(section).await?;
    let mut mutations = Vec::new();
    for name in fields {
        let field = writable(&schema, section, name)?;
        if !field.optional {
            return Err(CliError::Usage(format!(
                "{section}.{name} is required and cannot be unset"
            )));
        }
        mutations.push(Mutation {
            field_id: field.field.id,
            op: MutationOp::Unset,
        });
    }
    let applied = device
        .config_patch(section, &mutations, None, dry_run)
        .await?;
    report(ctx, section, &applied, dry_run)
}

// -- files ----------------------------------------------------------------------

/// The configuration of every configured section, without secrets and
/// read-only fields.
async fn snapshot<T: ClientTransport>(
    device: &mut Device<T>,
) -> Result<BTreeMap<String, Map<String, Json>>, CliError> {
    let mut out = BTreeMap::new();
    for info in device.config_sections().await? {
        let schema = device.config_schema(&info.name).await?;
        let current = device.config_get(&info.name).await?;
        if !current.configured {
            continue;
        }
        let mut fields = Map::new();
        for field in schema.fields.iter().filter(|f| f.writable && !f.secret) {
            let entry = current
                .entries
                .iter()
                .find(|entry| entry.field_id == field.field.id);
            if let Some(ConfigEntry {
                state: EntryState::Value(value),
                ..
            }) = entry
            {
                fields.insert(
                    field.field.name.clone(),
                    serde_json::to_value(value).unwrap_or(Json::Null),
                );
            }
        }
        out.insert(info.name, fields);
    }
    Ok(out)
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
                if current.configured {
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
                             use `rohi config set {section} {name}=-`"
                        )));
                    }
                    let old = current
                        .entries
                        .iter()
                        .find(|entry| entry.field_id == config.field.id)
                        .and_then(|entry| match &entry.state {
                            EntryState::Value(value) => Some(value.clone()),
                            EntryState::Redacted => None,
                        });
                    if json.is_null() {
                        if !config.optional {
                            return Err(CliError::File(format!(
                                "{section}.{name} is required and cannot be null"
                            )));
                        }
                        if let Some(old) = old {
                            plan.changes.push(Change {
                                marker: '-',
                                name: name.clone(),
                                text: format!(
                                    "{} -> (unset)",
                                    format::value(&old, Some(&config.field))
                                ),
                            });
                            plan.mutations.push(Mutation {
                                field_id: config.field.id,
                                op: MutationOp::Unset,
                            });
                        }
                    } else {
                        let new = values::from_json(&config.field, json)?;
                        if old.as_ref() != Some(&new) {
                            let shown = format::value(&new, Some(&config.field));
                            plan.changes.push(match &old {
                                Some(old) => Change {
                                    marker: '~',
                                    name: name.clone(),
                                    text: format!(
                                        "{} -> {shown}",
                                        format::value(old, Some(&config.field))
                                    ),
                                },
                                None => Change {
                                    marker: '+',
                                    name: name.clone(),
                                    text: shown,
                                },
                            });
                            plan.mutations.push(Mutation {
                                field_id: config.field.id,
                                op: MutationOp::Set(new),
                            });
                        }
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
                .config_patch(&plan.section, &plan.mutations, None, true)
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
                .config_patch(&plan.section, &plan.mutations, None, false)
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
