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
//! `rohi logs`: the device log, as the actions of the `logging` service.
//!
//! This module is a convenience over the generic action interface and knows
//! nothing the device does not tell it: `tail` returns the latest records from the
//! RAM of the device as a stream of text lines, `download` a stream of the
//! retained segment files, `clear` nothing. `rohi action logging ...` reaches
//! the same actions.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rohi_logging::segment::text::{parse_line, write_line};
use rohi_logging::{LogRecord, parse_tail_file_name};
use serde::Serialize;

use super::args::LogsAction;
use super::diagnostic::CliError;
use super::{Ctx, OutputFormat, dto};
use crate::model::{ActionOutcome, ErrorCode, FieldValue, Stream, Value};
use crate::{ClientTransport, Device, Error};

const SERVICE: &str = "logging";
/// Records asked for in one `tail`; the device clamps it to its RAM tail.
const PAGE: u64 = u64::MAX;
/// A tail stream can be replaced by newer records between the call and the
/// read; the call is then repeated.
const ATTEMPTS: usize = 3;

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: Option<LogsAction>,
    lines: usize,
    follow: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        None => show(device, lines, follow, ctx).await,
        Some(LogsAction::Download { directory, force }) => {
            download(device, &directory, force, ctx).await
        }
        Some(LogsAction::Clear { yes }) => clear(device, yes, ctx).await,
    }
}

// -- reading ------------------------------------------------------------------

#[derive(Serialize)]
struct RecordDto {
    sequence: u64,
    uptime_ms: u64,
    time: Option<String>,
    level: &'static str,
    target: String,
    message: String,
    truncated: bool,
}

#[derive(Serialize)]
struct Records {
    /// Identifies the boot of the device the sequences belong to.
    session: u32,
    records: Vec<RecordDto>,
}

fn dto_of(record: &LogRecord) -> RecordDto {
    RecordDto {
        sequence: record.sequence,
        uptime_ms: record.monotonic_ms,
        time: record.wall_time.and_then(|time| {
            let millis = i64::try_from(time.as_millis()).ok()?;
            Some(
                chrono::DateTime::from_timestamp_millis(millis)?
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            )
        }),
        level: record.level.name(),
        target: record.target.as_str().to_owned(),
        message: record.message.as_str().to_owned(),
        truncated: record.truncated,
    }
}

fn text_of(records: &[LogRecord]) -> String {
    let mut text = String::new();
    for record in records {
        // Writing to a String cannot fail.
        let _ = write_line(&mut text, record);
    }
    text
}

/// What one `tail` returned.
struct Tailed {
    /// Identifies the boot of the device the sequences belong to.
    session: u32,
    /// The request did not belong to this boot: `records` are a fresh
    /// snapshot and the cursor starts over.
    reset: bool,
    /// Oldest first, by tail sequence.
    records: Vec<LogRecord>,
}

fn arg(
    schema: &crate::model::ActionSchema,
    name: &str,
    value: u64,
) -> Result<FieldValue, CliError> {
    let field = schema
        .args
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| Error::Protocol(format!("logging.tail has no {name} argument")))?;
    Ok(FieldValue {
        field_id: field.id,
        value: Value::U64(value),
    })
}

/// One `logging.tail`: up to `limit` records after `after` of the boot
/// `session`, oldest first; the latest ones when `session` or `after` is 0.
async fn tail<T: ClientTransport>(
    device: &mut Device<T>,
    session: u32,
    after: u64,
    limit: u64,
) -> Result<Tailed, CliError> {
    let schema = device.action_schema(SERVICE, "tail").await?;
    let args = [
        arg(&schema, "session", u64::from(session))?,
        arg(&schema, "after", after)?,
        arg(&schema, "limit", limit)?,
    ];
    for attempt in 1..=ATTEMPTS {
        match device.invoke_outcome(SERVICE, "tail", &args).await? {
            ActionOutcome::Records(records) => {
                let result = |name: &str| {
                    let id = schema.results.iter().find(|field| field.name == name)?.id;
                    records
                        .first()?
                        .iter()
                        .find(|field| field.field_id == id)
                        .map(|field| field.value.clone())
                };
                let session = match result("session") {
                    Some(Value::U64(session)) => u32::try_from(session).ok(),
                    _ => None,
                }
                .ok_or_else(|| Error::Protocol("logging.tail returned no session".into()))?;
                return Ok(Tailed {
                    session,
                    reset: result("reset") == Some(Value::Bool(true)),
                    records: Vec::new(),
                });
            }
            ActionOutcome::Stream(stream) => {
                let name = stream.files.first().map(|file| file.name.as_str());
                let (session, reset) = name.and_then(parse_tail_file_name).ok_or_else(|| {
                    Error::Protocol("logging.tail returned an unexpected file".into())
                })?;
                let data = read_all(device, &stream).await;
                // Whatever happened, release the stream; its content is already here.
                let _ = device.close_stream(&stream).await;
                match data {
                    Ok(data) => {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        // A line that is not a record is skipped, never trusted.
                        return Ok(Tailed {
                            session,
                            reset,
                            records: text.lines().filter_map(parse_line).collect(),
                        });
                    }
                    // New records replaced the window before it was read.
                    Err(CliError::Device(Error::Device(ErrorCode::NotFound)))
                        if attempt < ATTEMPTS => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Err(Error::Device(ErrorCode::NotFound).into())
}

/// The first file of a stream, whole.
async fn read_all<T: ClientTransport>(
    device: &mut Device<T>,
    stream: &Stream,
) -> Result<Vec<u8>, CliError> {
    let Some(file) = stream.files.first() else {
        return Ok(Vec::new());
    };
    let mut data = Vec::new();
    loop {
        let chunk = device
            .read_stream(stream, file.id, data.len() as u64)
            .await?;
        data.extend_from_slice(&chunk.data);
        if chunk.eof {
            return Ok(data);
        }
        if chunk.data.is_empty() {
            return Err(Error::Protocol("the device returned an empty chunk".into()).into());
        }
    }
}

fn print(session: u32, records: &[LogRecord], ctx: &mut Ctx, follow: bool) -> Result<(), CliError> {
    if records.is_empty() {
        return Ok(());
    }
    match ctx.format {
        OutputFormat::Human => ctx.io.result(&text_of(records)),
        // A followed log is one JSON document per line.
        OutputFormat::Json if follow => {
            for record in records {
                let mut line = serde_json::to_string(&dto::Versioned::new(Records {
                    session,
                    records: vec![dto_of(record)],
                }))
                .map_err(|error| CliError::Output(std::io::Error::other(error)))?;
                line.push('\n');
                ctx.io.result(&line)?;
            }
            Ok(())
        }
        OutputFormat::Json => unreachable!("a one-shot JSON document is emitted whole"),
    }
}

async fn show<T: ClientTransport>(
    device: &mut Device<T>,
    lines: usize,
    follow: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    // With `-n 0` one record is asked for, only to learn where to continue.
    let first = tail(device, 0, 0, (lines as u64).max(1)).await?;
    let session = first.session;
    let mut records = first.records;
    let mut cursor = records.last().map_or(0, |record| record.sequence);
    if lines == 0 {
        records.clear();
    }
    if !follow {
        if records.is_empty() {
            ctx.io.note("No log records\n");
        }
        return ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(Records {
                session,
                records: records.iter().map(dto_of).collect(),
            }),
            || text_of(&records),
        );
    }
    print(session, &records, ctx, true)?;
    follow_tail(device, session, &mut cursor, ctx).await
}

// Asks for what is new, forever (or `watch_limit` times in tests). The
// session and the cursor of the device are the only state.
async fn follow_tail<T: ClientTransport>(
    device: &mut Device<T>,
    mut session: u32,
    cursor: &mut u64,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let mut polls = 0;
    loop {
        if ctx.watch_limit.is_some_and(|limit| polls >= limit) {
            return Ok(());
        }
        tokio::time::sleep(ctx.follow_interval).await;
        // The device answers at most what its RAM holds; ask again until
        // nothing is new.
        loop {
            let page = tail(device, session, *cursor, PAGE).await?;
            if page.reset {
                ctx.io
                    .note("The device restarted: the log it keeps in RAM starts over\n");
                *cursor = 0;
            }
            session = page.session;
            let Some(first) = page.records.first().map(|record| record.sequence) else {
                break;
            };
            if !page.reset && *cursor > 0 && first > *cursor + 1 {
                ctx.io.note(&format!(
                    "{} log record(s) were overwritten on the device before they could be read\n",
                    first - *cursor - 1
                ));
            }
            if let Some(last) = page.records.last() {
                *cursor = last.sequence;
            }
            print(session, &page.records, ctx, true)?;
        }
        polls += 1;
    }
}

// -- download -----------------------------------------------------------------

#[derive(Serialize)]
struct SavedFile {
    name: String,
    size_bytes: u64,
    path: String,
}

#[derive(Serialize)]
struct Downloaded {
    directory: String,
    files: Vec<SavedFile>,
}

/// A file name that stays inside the directory: the device chooses it.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.ends_with(".part")
}

async fn download<T: ClientTransport>(
    device: &mut Device<T>,
    directory: &Path,
    force: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let stream = match device.invoke_outcome(SERVICE, "download", &[]).await? {
        ActionOutcome::Stream(stream) => stream,
        ActionOutcome::Records(_) => {
            return Err(Error::Protocol("logging.download returned records".into()).into());
        }
    };
    let saved = save(device, &stream, directory, force, ctx).await;
    // The device keeps the files pinned until the stream is closed; a failure
    // here only means it is already gone.
    let _ = device.close_stream(&stream).await;
    let saved = saved?;
    let total: u64 = saved.iter().map(|file| file.size_bytes).sum();
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(Downloaded {
            directory: directory.display().to_string(),
            files: saved
                .iter()
                .map(|file| SavedFile {
                    name: file.name.clone(),
                    size_bytes: file.size_bytes,
                    path: file.path.clone(),
                })
                .collect(),
        }),
        || {
            if saved.is_empty() {
                return "The device has no log files\n".to_owned();
            }
            let mut text = String::new();
            for file in &saved {
                let _ = writeln!(text, "{}  {} bytes", file.path, file.size_bytes);
            }
            let _ = writeln!(
                text,
                "{} file(s), {total} bytes, saved to {}",
                saved.len(),
                directory.display()
            );
            text
        },
    )
}

struct Saved {
    name: String,
    size_bytes: u64,
    path: String,
}

async fn save<T: ClientTransport>(
    device: &mut Device<T>,
    stream: &Stream,
    directory: &Path,
    force: bool,
    ctx: &mut Ctx,
) -> Result<Vec<Saved>, CliError> {
    if let Some(file) = stream.files.iter().find(|file| !safe_name(&file.name)) {
        return Err(Error::Protocol(format!("unsafe file name {:?}", file.name)).into());
    }
    if !force {
        let existing: Vec<&str> = stream
            .files
            .iter()
            .filter(|file| directory.join(&file.name).exists())
            .map(|file| file.name.as_str())
            .collect();
        if !existing.is_empty() {
            return Err(CliError::Usage(format!(
                "{} already exist(s) in {}: use another directory with -d, or --force to replace",
                existing.join(", "),
                directory.display()
            )));
        }
    }
    if !stream.files.is_empty() {
        std::fs::create_dir_all(directory)?;
    }
    let mut saved = Vec::new();
    for file in &stream.files {
        let target: PathBuf = directory.join(&file.name);
        let partial = directory.join(format!("{}.part", file.name));
        let written = copy(device, stream, file.id, file.size_bytes, &partial).await;
        if let Err(error) = written {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        if force {
            let _ = std::fs::remove_file(&target);
        }
        std::fs::rename(&partial, &target)?;
        ctx.io
            .note(&format!("{}  {} bytes\n", file.name, file.size_bytes));
        saved.push(Saved {
            name: file.name.clone(),
            size_bytes: file.size_bytes,
            path: target.display().to_string(),
        });
    }
    Ok(saved)
}

/// One file of the stream, written unchanged and checked against its size.
async fn copy<T: ClientTransport>(
    device: &mut Device<T>,
    stream: &Stream,
    file: u32,
    size: u64,
    to: &Path,
) -> Result<(), CliError> {
    use std::io::Write as _;
    let mut out = std::fs::File::create(to)?;
    let mut offset = 0u64;
    loop {
        let chunk = device.read_stream(stream, file, offset).await?;
        out.write_all(&chunk.data)?;
        offset += chunk.data.len() as u64;
        if chunk.eof {
            break;
        }
        if chunk.data.is_empty() {
            return Err(Error::Protocol("the device returned an empty chunk".into()).into());
        }
    }
    out.flush()?;
    if offset != size {
        return Err(Error::Protocol(format!(
            "a file has {offset} bytes, the device announced {size}"
        ))
        .into());
    }
    Ok(())
}

// -- clear --------------------------------------------------------------------

async fn clear<T: ClientTransport>(
    device: &mut Device<T>,
    yes: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    ctx.io
        .confirm(
            "The log history of the device will be removed:\n\n  \
             every retained record of the active log storage;\n  \
             the configuration and other storage are not touched.\n\nContinue?",
            yes,
        )
        .await?;
    device.invoke(SERVICE, "clear", &[]).await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(serde_json::json!({ "cleared": true })),
        || "The log history was cleared\n".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn only_plain_file_names_are_accepted_from_the_device() {
        for good in ["rohi1.log.lz4", "rohi24.log", "rohi4294967295.log.lz4"] {
            assert!(safe_name(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "../rohi1.log",
            "a/b.log",
            "a\\b.log",
            "/etc/passwd",
            "nul\0.log",
            "rohi1.log.part",
        ] {
            assert!(!safe_name(bad), "{bad:?}");
        }
    }
}
