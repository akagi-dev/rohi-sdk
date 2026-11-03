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

//! The simulator management interface and the scenario timeline.
//!
//! This is the only control surface of the simulation itself: fault
//! injection and scenario control. It is a line-based text protocol on a
//! second localhost port, completely separate from the ROHI device control
//! protocol, and nothing here is part of `rohi-control-proto`. Ordinary device
//! operations (config, status, actions) always go through the control
//! transport.
//!
//! ```text
//! > set co2 1200
//! ok
//! > fail scd4x
//! ok
//! > status
//! sensor bmx280 fault=None interval=1000ms
//! ...
//! ok
//! ```
//!
//! Every command is answered by `ok` or `error: <reason>`; the commands are
//! documented in [`crate::command`]. The scenario timeline executes the same
//! commands at their uptime.

use std::net::{SocketAddr, TcpListener, TcpStream};

use async_io::Async;
use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};
use log::{debug, info, warn};

use altruist_urban::tasks::store;
use rohi_sdk::hal::{Measurement, MeasurementsSnapshot};

use crate::command::Command;
use crate::diagnostics::SHUTDOWN;
use crate::io::{read_some, write_all};
use crate::storage::FileFlash;
use crate::world::world;

/// The longest command line.
const MAX_LINE: usize = 512;

fn measurement<T: core::fmt::Debug + Copy>(
    out: &mut String,
    name: &str,
    measurement: &Measurement<T>,
) {
    out.push_str(&format!(
        "measurement {name} state={:?} value={:?}\n",
        measurement.state, measurement.value
    ));
}

/// One line per sensor and per measurement of the store snapshot.
fn snapshot_text(snapshot: &MeasurementsSnapshot) -> String {
    let mut out = String::new();
    for status in snapshot.sensor_statuses.iter() {
        out.push_str(&format!(
            "sensor {:?} state={:?} errors={} last_error={:?}\n",
            status.kind, status.state, status.error_count, status.last_error
        ));
    }
    let m = &snapshot.measurements;
    if let Some(bmx) = &m.bmx280 {
        measurement(&mut out, "bmx280.temperature", &bmx.temperature);
        if let Some(humidity) = &bmx.humidity {
            measurement(&mut out, "bmx280.humidity", humidity);
        }
        measurement(&mut out, "bmx280.pressure", &bmx.pressure);
    }
    if let Some(scd) = &m.scd4x {
        measurement(&mut out, "scd4x.co2", &scd.co2);
        measurement(&mut out, "scd4x.temperature", &scd.temperature);
        measurement(&mut out, "scd4x.humidity", &scd.humidity);
    }
    if let Some(sds) = &m.sds011 {
        measurement(&mut out, "sds011.pm25", &sds.pm25);
        measurement(&mut out, "sds011.pm10", &sds.pm10);
    }
    if let Some(mic) = &m.microphone {
        measurement(&mut out, "noise.average", &mic.average);
        measurement(&mut out, "noise.maximum", &mic.maximum);
    }
    out
}

/// Executes `command`; the reply is the text to show on success.
///
/// # Errors
///
/// The reason a command cannot be executed.
pub async fn execute(command: &Command, storage: &FileFlash) -> Result<String, String> {
    match command {
        Command::Storage(fault) => {
            storage.inject(*fault)?;
            Ok(String::new())
        }
        Command::Status => {
            let mut text = world().summary();
            text.push_str(&match storage.path() {
                Some(path) => format!("storage {}\n", path.display()),
                None => "storage ephemeral\n".to_owned(),
            });
            Ok(text)
        }
        Command::Measurements => match store::handle().snapshot().await {
            Ok(snapshot) => Ok(snapshot_text(&snapshot)),
            Err(_) => Err("the measurement store did not answer".into()),
        },
        Command::Shutdown => {
            SHUTDOWN.signal(());
            Ok(String::new())
        }
        other => {
            world().apply(other);
            info!("simulation: {other:?}");
            Ok(String::new())
        }
    }
}

async fn serve(stream: Async<TcpStream>, storage: FileFlash) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let reply = match Command::parse(line) {
                Ok(command) => match execute(&command, &storage).await {
                    Ok(text) => format!("{text}ok\n"),
                    Err(reason) => format!("error: {reason}\n"),
                },
                Err(error) => format!("error: {error}\n"),
            };
            if write_all(&stream, reply.as_bytes()).await.is_err() {
                return;
            }
        }
        if pending.len() > MAX_LINE {
            let _ = write_all(&stream, b"error: command too long\n").await;
            return;
        }
        match read_some(&stream, &mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => pending.extend_from_slice(&chunk[..read]),
        }
    }
}

/// Simultaneous management connections.
const SESSIONS: usize = 4;

#[embassy_executor::task(pool_size = 4)]
async fn session_task(stream: Async<TcpStream>, storage: FileFlash) {
    serve(stream, storage).await;
    debug!("management: client disconnected");
}

#[embassy_executor::task]
async fn listen_task(listener: Async<TcpListener>, storage: FileFlash, spawner: Spawner) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                debug!("management: client {peer} connected");
                let task = session_task(stream, storage.clone());
                match task {
                    Ok(task) => spawner.spawn(task),
                    Err(_) => warn!("management: more than {SESSIONS} clients, {peer} refused"),
                }
            }
            Err(e) => {
                warn!("management: accept failed: {e}");
                Timer::after_millis(100).await;
            }
        }
    }
}

/// Starts the management listener on `listener`.
///
/// # Errors
///
/// The listener cannot be registered with the reactor.
pub fn start(
    spawner: Spawner,
    listener: TcpListener,
    storage: FileFlash,
) -> std::io::Result<SocketAddr> {
    let address = listener.local_addr()?;
    spawner.spawn(listen_task(Async::new(listener)?, storage, spawner).unwrap());
    Ok(address)
}

#[embassy_executor::task]
async fn timeline_task(events: Vec<crate::scenario::TimedCommand>, storage: FileFlash) {
    for event in events {
        Timer::at(Instant::from_millis(event.at_ms)).await;
        if let Err(reason) = execute(&event.command, &storage).await {
            warn!("scenario: {:?} failed: {reason}", event.command);
        }
    }
}

/// Starts the timeline of the scenario, if it has events.
pub fn start_timeline(
    spawner: Spawner,
    mut events: Vec<crate::scenario::TimedCommand>,
    storage: FileFlash,
) {
    if events.is_empty() {
        return;
    }
    events.sort_by_key(|event| event.at_ms);
    spawner.spawn(timeline_task(events, storage).unwrap());
}
