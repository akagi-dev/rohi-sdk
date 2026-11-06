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

//! Process harness of the end-to-end tests: starts a device on free ports,
//! waits for its READY line (no sleeps), talks to it through the real `rohi`
//! client library and stops it without leaving a process behind.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rohi::model::{Mutation, MutationOp, ServiceHealth, ServiceState, Value};
use rohi::{Device, TcpLink};

/// How long a device may take to become ready.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// What to start.
#[derive(Clone, Default)]
pub struct Launch {
    pub storage: Option<PathBuf>,
    pub scenario: Option<PathBuf>,
    pub extra: Vec<String>,
}

impl Launch {
    /// Ephemeral storage and the default scenario.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn storage(mut self, path: &Path) -> Self {
        self.storage = Some(path.to_owned());
        self
    }

    pub fn scenario(mut self, path: &Path) -> Self {
        self.scenario = Some(path.to_owned());
        self
    }

    pub fn arg(mut self, arg: &str) -> Self {
        self.extra.push(arg.to_owned());
        self
    }
}

/// A running simulator. It is killed when dropped.
pub struct Sim {
    child: Child,
    pub control: SocketAddr,
    pub manage: SocketAddr,
    stderr: Arc<Mutex<String>>,
}

impl Sim {
    /// Starts a device on free ports and waits until it is ready.
    pub fn start(launch: &Launch) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_altruist-urban"));
        command
            .args(["--control-port", "0", "--log-level", "debug"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        match &launch.storage {
            Some(path) => command.arg("--storage").arg(path),
            None => command.arg("--ephemeral"),
        };
        if let Some(scenario) = &launch.scenario {
            command.arg("--scenario").arg(scenario);
        }
        command.args(&launch.extra);
        let mut child = command.spawn().expect("the device starts");

        let stderr = Arc::new(Mutex::new(String::new()));
        let log = stderr.clone();
        let mut err = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = err.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                log.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buffer[..read]));
            }
        });

        let (lines, ready) = mpsc::channel();
        let out = child.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });

        let mut sim = Self {
            child,
            control: ([127, 0, 0, 1], 0).into(),
            manage: ([127, 0, 0, 1], 0).into(),
            stderr,
        };
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match ready.recv_timeout(remaining) {
                Ok(line) if line.starts_with("READY ") => {
                    for word in line.split_whitespace().skip(1) {
                        match word.split_once('=') {
                            Some(("control", address)) => sim.control = address.parse().unwrap(),
                            Some(("manage", address)) => sim.manage = address.parse().unwrap(),
                            _ => {}
                        }
                    }
                    return sim;
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => panic!(
                    "the device was not ready in {READY_TIMEOUT:?}\n{}",
                    sim.log()
                ),
                Err(RecvTimeoutError::Disconnected) => {
                    let status = sim.child.wait().ok();
                    panic!(
                        "the device ended before it was ready ({status:?})\n{}",
                        sim.log()
                    );
                }
            }
        }
    }

    /// What the device logged so far.
    pub fn log(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Sends one management command and returns its reply, which ends with
    /// `ok` or is an `error:` line.
    pub fn manage(&self, line: &str) -> String {
        let mut stream = TcpStream::connect(self.manage).expect("management connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{line}").unwrap();
        let mut reply = String::new();
        let mut reader = BufReader::new(stream);
        loop {
            let mut next = String::new();
            let read = reader.read_line(&mut next).expect("management answers");
            assert!(
                read > 0,
                "management closed before it answered `{line}`: {reply}"
            );
            reply.push_str(&next);
            if next == "ok\n" || next.starts_with("error:") {
                return reply;
            }
        }
    }

    /// A management command that must succeed.
    pub fn manage_ok(&self, line: &str) -> String {
        let reply = self.manage(line);
        assert!(reply.ends_with("ok\n"), "`{line}` failed: {reply}");
        reply
    }

    /// A control connection through the `rohi` client library.
    pub async fn device(&self) -> Device<TcpLink> {
        let link = TcpLink::connect(&self.control.to_string(), Duration::from_secs(10))
            .await
            .unwrap_or_else(|e| panic!("cannot connect: {e}\n{}", self.log()));
        let mut device = Device::new(link);
        device.set_timeout(Duration::from_secs(10));
        device
    }

    /// Waits for the process to end on its own.
    pub fn wait_exit(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// Stops the device cleanly and returns its exit status.
    pub fn stop(mut self) -> ExitStatus {
        let _ = self.manage("shutdown");
        match self.wait_exit(Duration::from_secs(10)) {
            Some(status) => status,
            None => panic!("the device did not stop\n{}", self.log()),
        }
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("--- device log ---\n{}", self.log());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Polls `check` until it returns a value; the only waiting the tests do.
pub async fn eventually<T>(
    what: &str,
    within: Duration,
    mut check: impl AsyncFnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The status fields of `service` by name.
pub async fn fields(device: &mut Device<TcpLink>, service: &str) -> HashMap<String, Value> {
    let schema = device.service_schema(service).await.unwrap();
    let status = device.service_status(service).await.unwrap();
    status
        .fields
        .into_iter()
        .filter_map(|field| {
            let name = schema.fields.iter().find(|f| f.id == field.field_id)?;
            Some((name.name.clone(), field.value))
        })
        .collect()
}

/// State, health and issue names of `service`.
pub async fn summary(
    device: &mut Device<TcpLink>,
    service: &str,
) -> (ServiceState, ServiceHealth, bool, Vec<String>) {
    let status = device.service_status(service).await.unwrap();
    (
        status.summary.state,
        status.summary.health,
        status.summary.ready,
        status.issues.into_iter().map(|issue| issue.code).collect(),
    )
}

pub fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

pub fn set(field_id: u32, value: Value) -> Mutation {
    Mutation {
        field_id,
        op: MutationOp::Set(value),
    }
}

/// The effective text of a field of a config section.
pub async fn config_text(
    device: &mut Device<TcpLink>,
    section: &str,
    field_id: u32,
) -> Option<String> {
    let values = device.config_get(section).await.unwrap();
    values
        .entries
        .into_iter()
        .find(|entry| entry.field_id == field_id)
        .and_then(|entry| match entry.state {
            rohi::model::EntryState::Value(Value::String(text)) => Some(text),
            _ => None,
        })
}

/// Field ids of the `wifi` section.
pub mod wifi {
    pub const SSID: u32 = 1;
    pub const PASSWORD: u32 = 2;
    pub const BACKOFF_INITIAL: u32 = 5;
    pub const BACKOFF_MAX: u32 = 6;
}

pub fn f32_of(value: Option<&Value>) -> Option<f32> {
    match value {
        Some(Value::F32(value)) => Some(*value),
        _ => None,
    }
}
