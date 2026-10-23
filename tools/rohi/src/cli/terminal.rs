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
//! Terminal interaction. stdout carries only the requested result; prompts,
//! progress, warnings and diagnostics go to stderr. Everything that reads or
//! writes goes through [`Io`], so tests can script a session.

use std::collections::VecDeque;
use std::io::{self, BufRead, IsTerminal, Write};

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use super::args::OutputFormat;
use super::diagnostic::{CliError, describe};
use crate::DiscoveredDevice;

enum Input {
    /// The process stdin, read by a helper thread started on first use so
    /// that waiting for the user never blocks Ctrl-C handling.
    Stdin(Option<mpsc::UnboundedReceiver<io::Result<String>>>),
    /// Prepared answers, for tests.
    Script(VecDeque<String>),
}

/// The streams of one `rohi` invocation.
pub struct Io {
    stdout: Box<dyn Write>,
    stderr: Box<dyn Write>,
    input: Input,
    interactive: bool,
}

impl Io {
    /// The process streams. Interactive when both stdin and stderr are
    /// terminals, so prompts have somebody to talk to.
    pub fn stdio() -> Self {
        Self {
            stdout: Box::new(io::stdout()),
            stderr: Box::new(io::stderr()),
            input: Input::Stdin(None),
            interactive: io::stdin().is_terminal() && io::stderr().is_terminal(),
        }
    }

    /// Streams with prepared `input` lines; they answer prompts, hidden
    /// prompts and reads of stdin alike, in order.
    pub fn scripted(
        stdout: impl Write + 'static,
        stderr: impl Write + 'static,
        interactive: bool,
        input: &[&str],
    ) -> Self {
        Self {
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            input: Input::Script(input.iter().map(|line| (*line).to_owned()).collect()),
            interactive,
        }
    }

    /// Whether prompts can be shown and answered.
    pub fn interactive(&self) -> bool {
        self.interactive
    }

    /// Writes the command result to stdout.
    pub fn result(&mut self, text: &str) -> Result<(), CliError> {
        self.stdout.write_all(text.as_bytes())?;
        self.stdout.flush()?;
        Ok(())
    }

    /// Writes the command result: `value` as JSON, or the text of `human`.
    pub fn emit<T: Serialize>(
        &mut self,
        format: OutputFormat,
        value: &T,
        human: impl FnOnce() -> String,
    ) -> Result<(), CliError> {
        match format {
            OutputFormat::Human => self.result(&human()),
            OutputFormat::Json => {
                let mut json = serde_json::to_string_pretty(value)
                    .map_err(|error| CliError::Output(io::Error::other(error)))?;
                json.push('\n');
                self.result(&json)
            }
        }
    }

    /// Progress, warnings and prompts, on stderr. Failing to show them is not
    /// worth failing the command.
    pub fn note(&mut self, text: &str) {
        let _ = self.stderr.write_all(text.as_bytes());
        let _ = self.stderr.flush();
    }

    /// The next line of input; `None` at the end of input.
    pub async fn line(&mut self) -> Result<Option<String>, CliError> {
        match &mut self.input {
            Input::Script(lines) => Ok(lines.pop_front()),
            Input::Stdin(receiver) => {
                let receiver = receiver.get_or_insert_with(|| {
                    let (sender, receiver) = mpsc::unbounded_channel();
                    std::thread::spawn(move || {
                        let mut stdin = io::stdin().lock();
                        loop {
                            let mut line = String::new();
                            match stdin.read_line(&mut line) {
                                Ok(0) => break,
                                Ok(_) => {
                                    if sender.send(Ok(line)).is_err() {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    let _ = sender.send(Err(error));
                                    break;
                                }
                            }
                        }
                    });
                    receiver
                });
                match receiver.recv().await {
                    Some(line) => Ok(Some(line?.trim_end_matches(['\n', '\r']).to_owned())),
                    None => Ok(None),
                }
            }
        }
    }

    /// Asks for a secret without echoing it. Only for interactive sessions.
    pub async fn hidden(&mut self, prompt: &str) -> Result<Zeroizing<String>, CliError> {
        self.note(prompt);
        match &mut self.input {
            Input::Script(lines) => {
                let line = lines.pop_front().ok_or(CliError::Cancelled)?;
                self.note("\n");
                Ok(Zeroizing::new(line))
            }
            Input::Stdin(_) => {
                let (sender, receiver) = oneshot::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(rpassword::read_password());
                });
                let password = receiver
                    .await
                    .map_err(|_| CliError::Cancelled)?
                    .map_err(CliError::Output)?;
                Ok(Zeroizing::new(password))
            }
        }
    }

    /// First line of stdin as a secret, for `--password-stdin`.
    pub async fn secret_from_stdin(&mut self, what: &str) -> Result<Zeroizing<String>, CliError> {
        match self.line().await? {
            Some(line) if !line.is_empty() => Ok(Zeroizing::new(line)),
            _ => Err(CliError::PasswordRequired {
                what: what.to_owned(),
                hint: "stdin was empty; pipe the password in".to_owned(),
            }),
        }
    }

    /// Asks `question` unless `yes` is set. Without a terminal the answer
    /// cannot be given: that is an error, never an implicit yes.
    pub async fn confirm(&mut self, question: &str, yes: bool) -> Result<(), CliError> {
        if yes {
            return Ok(());
        }
        if !self.interactive {
            return Err(CliError::ConfirmationRequired);
        }
        self.note(&format!("{question} [y/N] "));
        match self.line().await? {
            Some(answer) if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") => Ok(()),
            _ => Err(CliError::Cancelled),
        }
    }

    /// Lets the user pick one of `devices`; returns its index.
    pub async fn select_device(&mut self, devices: &[DiscoveredDevice]) -> Result<usize, CliError> {
        let mut menu = String::from("Multiple ROHI devices found:\n\n");
        for (index, device) in devices.iter().enumerate() {
            menu.push_str(&format!("  {}  {}\n", index + 1, describe(device)));
        }
        self.note(&menu);
        loop {
            self.note(&format!("\nSelect device [1-{}]: ", devices.len()));
            let Some(answer) = self.line().await? else {
                return Err(CliError::Cancelled);
            };
            let answer = answer.trim();
            if answer.is_empty() {
                return Err(CliError::Cancelled);
            }
            match answer.parse::<usize>() {
                Ok(number) if (1..=devices.len()).contains(&number) => return Ok(number - 1),
                _ => self.note("Not a number from the list.\n"),
            }
        }
    }
}
