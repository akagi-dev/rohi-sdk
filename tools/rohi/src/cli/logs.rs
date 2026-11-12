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
//! nothing the device does not tell it: `read` returns a stream of text lines
//! after a sequence number, `download` a stream of the retained segment
//! files, `clear` nothing. `rohi action logging ...` reaches the same
//! actions.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rohi_logging::segment::text::{parse_line, write_line};
use rohi_logging::{LogRecord, history_of_read_file};
use serde::Serialize;

use super::args::LogsAction;
use super::diagnostic::CliError;
use super::{Ctx, OutputFormat, dto};
use crate::model::{
    ActionOutcome, ErrorCode, FieldValue, ServiceSchema, ServiceStatus, Stream, Value,
};
use crate::{ClientTransport, Device, Error};

const SERVICE: &str = "logging";
/// Records asked for in one `read`; the device returns fewer when its answer
/// is full and the next page continues after the last record.
const PAGE: u64 = 200;

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

fn field_id(schema: &ServiceSchema, name: &str) -> Option<u32> {
    schema
        .fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.id)
}

fn status_u64(schema: &ServiceSchema, status: &ServiceStatus, name: &str) -> Option<u64> {
    let id = field_id(schema, name)?;
    status
        .fields
        .iter()
        .find(|field| field.field_id == id)
        .and_then(|field| match field.value {
            Value::U64(value) => Some(value),
            _ => None,
        })
}

/// What a read returned: the history the records belong to, when the device
/// says, and the records.
struct Page {
    history: Option<u32>,
    records: Vec<LogRecord>,
}

/// One `logging.read`: up to `limit` records after `after`, oldest first.
async fn read_page<T: ClientTransport>(
    device: &mut Device<T>,
    after: u64,
    limit: u64,
) -> Result<Page, CliError> {
    for attempt in 0..2 {
        let schema = device.action_schema(SERVICE, "read").await?;
        let mut args = Vec::new();
        for (name, value) in [("after", after), ("limit", limit)] {
            let field = schema
                .args
                .iter()
                .find(|field| field.name == name)
                .ok_or_else(|| Error::Protocol(format!("logging.read has no {name} argument")))?;
            args.push(FieldValue {
                field_id: field.id,
                value: Value::U64(value),
            });
        }
        let stream = match device.invoke_outcome(SERVICE, "read", &args).await? {
            ActionOutcome::Stream(stream) => stream,
            ActionOutcome::Records(_) => {
                return Err(Error::Protocol("logging.read returned records".into()).into());
            }
        };
        let history = stream
            .files
            .first()
            .and_then(|file| history_of_read_file(&file.name));
        let data = read_all(device, &stream).await;
        // Whatever happened, release the stream; its content is already here.
        let _ = device.close_stream(&stream).await;
        let data = match data {
            Ok(data) => data,
            // A concurrent clear can revoke the stream after `read` returned it.
            Err(CliError::Device(Error::Device(ErrorCode::NotFound))) if attempt == 0 => continue,
            Err(error) => return Err(error),
        };
        let text = String::from_utf8_lossy(&data).into_owned();
        // A line that is not a record is skipped, never trusted.
        return Ok(Page {
            history,
            records: text.lines().filter_map(parse_line).collect(),
        });
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

/// Every record after `after`, a page at a time, and the history they are
/// from. A history that changes while reading (the log was cleared) restarts
/// the read from the beginning of the new one.
async fn read_after<T: ClientTransport>(
    device: &mut Device<T>,
    mut after: u64,
) -> Result<(Option<u32>, Vec<LogRecord>), CliError> {
    let mut all = Vec::new();
    let mut history = None;
    loop {
        let page = read_page(device, after, PAGE).await?;
        if page.history.is_some() && history.is_some() && page.history != history {
            history = page.history;
            all.clear();
            after = 0;
            continue;
        }
        history = history.or(page.history);
        let Some(last) = page.records.last().map(|record| record.sequence) else {
            return Ok((history, all));
        };
        if last <= after {
            return Err(Error::Protocol("the log did not move forward".into()).into());
        }
        after = last;
        all.extend(page.records);
    }
}

/// The latest `count` records, the sequence to continue after and the history
/// they belong to.
///
/// The device keeps sequence numbers, not counts, so the window is placed by
/// the newest sequence the status reports and widened while it holds fewer
/// than `count` records and the oldest retained record is not reached.
async fn latest<T: ClientTransport>(
    device: &mut Device<T>,
    count: usize,
) -> Result<(Vec<LogRecord>, u64, Option<u32>), CliError> {
    // The device writes records in batches: what was logged a moment ago may
    // still be waiting. A read commits it, and this one asks for nothing
    // (no record has a sequence above the maximum), so that the status below
    // describes everything the device has accepted.
    read_page(device, u64::MAX, 1).await?;
    let schema = device.service_schema(SERVICE).await?;
    let status = device.service_status(SERVICE).await?;
    let history = status_u64(&schema, &status, "history").and_then(|id| u32::try_from(id).ok());
    let Some(newest) = status_u64(&schema, &status, "newest_sequence") else {
        return Ok((Vec::new(), 0, history));
    };
    if count == 0 {
        return Ok((Vec::new(), newest, history));
    }
    let oldest = status_u64(&schema, &status, "oldest_sequence").unwrap_or(1);
    let mut want = count as u64;
    loop {
        let after = newest.saturating_sub(want);
        let (read_history, mut records) = read_after(device, after).await?;
        if records.len() >= count || after < oldest || after == 0 {
            let cursor = records.last().map_or(newest, |record| record.sequence);
            let skip = records.len().saturating_sub(count);
            records.drain(..skip);
            return Ok((records, cursor, read_history.or(history)));
        }
        want = want.saturating_mul(2);
    }
}

fn print(records: &[LogRecord], ctx: &mut Ctx, follow: bool) -> Result<(), CliError> {
    if records.is_empty() {
        return Ok(());
    }
    match ctx.format {
        OutputFormat::Human => ctx.io.result(&text_of(records)),
        // A followed log is one JSON document per line.
        OutputFormat::Json if follow => {
            for record in records {
                let mut line = serde_json::to_string(&dto::Versioned::new(Records {
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
    let (records, mut cursor, mut history) = latest(device, lines).await?;
    if follow {
        print(&records, ctx, true)?;
    } else {
        if records.is_empty() {
            ctx.io.note("No log records\n");
        }
        return ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(Records {
                records: records.iter().map(dto_of).collect(),
            }),
            || text_of(&records),
        );
    }
    let mut polls = 0;
    loop {
        if ctx.watch_limit.is_some_and(|limit| polls >= limit) {
            return Ok(());
        }
        tokio::time::sleep(ctx.follow_interval).await;
        let (now, mut new) = read_after(device, cursor).await?;
        if history.is_some() && now.is_some() && now != history {
            // Cleared or lost on the device: the cursor means nothing in the
            // new history, so it is read from its start.
            ctx.io
                .note("The log history was cleared or reset on the device\n");
            history = now;
            new = read_after(device, 0).await?.1;
        } else {
            history = history.or(now);
        }
        if let Some(last) = new.last() {
            cursor = last.sequence;
        }
        print(&new, ctx, true)?;
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
