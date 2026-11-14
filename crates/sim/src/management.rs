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

//! The management interface of the simulation and the scenario timeline.
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

use core::future::Future;
use core::net::SocketAddr;
use core::pin::Pin;
use std::net::TcpListener as StdListener;

use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};
use log::{debug, info, warn};
use rohi_net_std::runtime::{handle, within};
use rohi_sensors::{Measurement, MeasurementsSnapshot};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::command::Command;
use crate::storage::FileFlash;
use crate::world::world;

/// The future of a measurement snapshot, `None` when its owner does not answer.
pub type SnapshotFuture = Pin<Box<dyn Future<Output = Option<MeasurementsSnapshot>>>>;

/// What the application provides to the management interface.
///
/// The interface knows nothing about the application it manages: the
/// measurement store and the end of the process are reached through these
/// hooks.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Reads the snapshot of the measurement store.
    pub snapshot: fn() -> SnapshotFuture,
    /// Ends the process.
    pub shutdown: fn(),
    /// Whether simulated sensors run. Without them the commands that set a
    /// value or fail a sensor are refused instead of silently doing nothing.
    pub sensors: bool,
}

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
pub async fn execute(
    command: &Command,
    storage: &FileFlash,
    hooks: &Hooks,
) -> Result<String, String> {
    match command {
        Command::Set(..) | Command::Fail(..) | Command::Recover(_) if !hooks.sensors => {
            Err("sensor simulation is not enabled in this build".into())
        }
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
        Command::Measurements => match (hooks.snapshot)().await {
            Some(snapshot) => Ok(snapshot_text(&snapshot)),
            None => Err("the measurement store did not answer".into()),
        },
        Command::Shutdown => {
            (hooks.shutdown)();
            Ok(String::new())
        }
        other => {
            world().apply(other);
            info!("simulation: {other:?}");
            Ok(String::new())
        }
    }
}

async fn serve(mut stream: TcpStream, storage: FileFlash, hooks: Hooks) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        loop {
            let line = match take_line(&mut pending) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(()) => {
                    let _ = within(stream.write_all(b"error: command too long\n")).await;
                    return;
                }
            };
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let reply = match Command::parse(line) {
                Ok(command) => match execute(&command, &storage, &hooks).await {
                    Ok(text) => format!("{text}ok\n"),
                    Err(reason) => format!("error: {reason}\n"),
                },
                Err(error) => format!("error: {error}\n"),
            };
            if within(stream.write_all(reply.as_bytes())).await.is_err() {
                return;
            }
        }
        match within(stream.read(&mut chunk)).await {
            Ok(0) | Err(_) => return,
            Ok(read) => pending.extend_from_slice(&chunk[..read]),
        }
    }
}

fn take_line(pending: &mut Vec<u8>) -> Result<Option<Vec<u8>>, ()> {
    let Some(end) = pending.iter().position(|byte| *byte == b'\n') else {
        return (pending.len() <= MAX_LINE).then_some(None).ok_or(());
    };
    if end > MAX_LINE {
        return Err(());
    }
    Ok(Some(pending.drain(..=end).collect()))
}

/// Simultaneous management connections.
const SESSIONS: usize = 4;

#[embassy_executor::task(pool_size = 4)]
async fn session_task(stream: TcpStream, storage: FileFlash, hooks: Hooks) {
    serve(stream, storage, hooks).await;
    debug!("management: client disconnected");
}

#[embassy_executor::task]
async fn listen_task(listener: TcpListener, storage: FileFlash, hooks: Hooks, spawner: Spawner) {
    loop {
        match within(listener.accept()).await {
            Ok((stream, peer)) => {
                debug!("management: client {peer} connected");
                match session_task(stream, storage.clone(), hooks) {
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
/// The interface has no access control: bind it to the loopback address.
///
/// # Errors
///
/// The listener cannot be registered with the reactor.
pub fn start(
    spawner: Spawner,
    listener: StdListener,
    storage: FileFlash,
    hooks: Hooks,
) -> std::io::Result<SocketAddr> {
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let listener = {
        let _entered = handle().enter();
        TcpListener::from_std(listener)?
    };
    spawner.spawn(listen_task(listener, storage, hooks, spawner).unwrap());
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_overlong_lines_before_extracting_them() {
        let mut pending = vec![b'x'; MAX_LINE + 1];
        pending.push(b'\n');
        pending.extend_from_slice(b"status\n");
        assert!(take_line(&mut pending).is_err());
        assert_eq!(pending.len(), MAX_LINE + 1 + 1 + 7);
    }

    #[test]
    fn accepts_bounded_lines_and_checks_each_batched_line() {
        let mut pending = b"status\n".to_vec();
        pending.extend(std::iter::repeat_n(b'x', MAX_LINE + 1));
        pending.extend_from_slice(b"\nstatus\n");

        assert_eq!(take_line(&mut pending).unwrap(), Some(b"status\n".to_vec()));
        assert!(take_line(&mut pending).is_err());
        assert!(pending.ends_with(b"\nstatus\n"));

        let mut bounded = vec![b'x'; MAX_LINE];
        bounded.push(b'\n');
        assert_eq!(
            take_line(&mut bounded).unwrap().unwrap().len(),
            MAX_LINE + 1
        );
    }

    #[test]
    fn rejects_overlong_unterminated_input() {
        let mut pending = vec![b'x'; MAX_LINE + 1];
        assert!(take_line(&mut pending).is_err());
    }
}

#[embassy_executor::task]
async fn timeline_task(
    events: Vec<crate::scenario::TimedCommand>,
    storage: FileFlash,
    hooks: Hooks,
) {
    for event in events {
        Timer::at(Instant::from_millis(event.at_ms)).await;
        if let Err(reason) = execute(&event.command, &storage, &hooks).await {
            warn!("scenario: {:?} failed: {reason}", event.command);
        }
    }
}

/// Starts the timeline of the scenario, if it has events.
pub fn start_timeline(
    spawner: Spawner,
    mut events: Vec<crate::scenario::TimedCommand>,
    storage: FileFlash,
    hooks: Hooks,
) {
    if events.is_empty() {
        return;
    }
    events.sort_by_key(|event| event.at_ms);
    spawner.spawn(timeline_task(events, storage, hooks).unwrap());
}
