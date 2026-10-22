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

use core::cell::Cell;
use core::pin::pin;

use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embedded_hal_async::delay::DelayNs;
use log::{info, warn};

use crate::backoff::Backoff;
use crate::{
    NetworkInfo, WIFI_COMMAND_QUEUE_DEPTH, WifiBackend, WifiConfig, WifiError, WifiScanResults,
    WifiState, WifiStatus,
};

/// Number of tasks that can wait for status changes at the same time.
pub const STATUS_RECEIVERS: usize = 3;

#[derive(Clone, Copy)]
enum Command {
    ApplyConfig(Option<WifiConfig>),
    Reconnect,
    Disconnect,
    Scan,
}

/// State shared between the [`WifiManager`] task and its [`WifiHandle`]s.
///
/// Place it in a `static`; all storage is inline and bounded.
pub struct WifiShared<M: RawMutex> {
    commands: Channel<M, Command, WIFI_COMMAND_QUEUE_DEPTH>,
    status: Watch<M, WifiStatus, STATUS_RECEIVERS>,
    info: BlockingMutex<M, Cell<Option<NetworkInfo>>>,
    scan: Signal<M, Result<WifiScanResults, WifiError>>,
    scan_pending: BlockingMutex<M, Cell<bool>>,
}

impl<M: RawMutex> WifiShared<M> {
    pub const fn new() -> Self {
        Self {
            commands: Channel::new(),
            status: Watch::new_with(WifiStatus::new(WifiState::NotConfigured)),
            info: BlockingMutex::new(Cell::new(None)),
            scan: Signal::new(),
            scan_pending: BlockingMutex::new(Cell::new(false)),
        }
    }

    /// Handle used by other tasks to command the manager and read its state.
    pub const fn handle(&self) -> WifiHandle<'_, M> {
        WifiHandle { shared: self }
    }
}

impl<M: RawMutex> Default for WifiShared<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// Cheap, copyable interface to the Wi-Fi manager.
///
/// Commands are queued to the manager task; no handle method touches the
/// platform driver.
pub struct WifiHandle<'a, M: RawMutex> {
    shared: &'a WifiShared<M>,
}

impl<M: RawMutex> Clone for WifiHandle<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: RawMutex> Copy for WifiHandle<'_, M> {}

impl<M: RawMutex> WifiHandle<'_, M> {
    fn send(&self, command: Command) -> Result<(), WifiError> {
        self.shared
            .commands
            .try_send(command)
            .map_err(|_| WifiError::Busy)
    }

    /// Applies a new configuration (`Some`) or forgets the network (`None`).
    /// The configuration must already be persisted by the caller; it is
    /// validated again here.
    pub fn apply_config(&self, config: Option<WifiConfig>) -> Result<(), WifiError> {
        self.send(Command::ApplyConfig(config))
    }

    /// Drops the current connection and connects again immediately.
    pub fn reconnect(&self) -> Result<(), WifiError> {
        self.send(Command::Reconnect)
    }

    /// Disconnects and stays idle until the next reconnect or configuration.
    pub fn disconnect(&self) -> Result<(), WifiError> {
        self.send(Command::Disconnect)
    }

    /// Scans for networks. Only one scan can be in flight.
    ///
    /// The caller should bound the wait with its own timeout.
    pub async fn scan(&self) -> Result<WifiScanResults, WifiError> {
        if self
            .shared
            .scan_pending
            .lock(|pending| pending.replace(true))
        {
            return Err(WifiError::Busy);
        }
        self.shared.scan.reset();
        if let Err(error) = self.send(Command::Scan) {
            self.shared.scan_pending.lock(|pending| pending.set(false));
            return Err(error);
        }
        self.shared.scan.wait().await
    }

    pub fn status(&self) -> WifiStatus {
        self.shared
            .status
            .try_get()
            .unwrap_or(WifiStatus::new(WifiState::NotConfigured))
    }

    /// Current connection details; `None` unless the network is ready.
    pub fn network_info(&self) -> Option<NetworkInfo> {
        self.shared.info.lock(Cell::get)
    }

    /// Receiver for status changes, `None` if all [`STATUS_RECEIVERS`] are taken.
    pub fn status_receiver(&self) -> Option<Receiver<'_, M, WifiStatus, STATUS_RECEIVERS>> {
        self.shared.status.receiver()
    }
}

/// Reconnect and refresh timing of the [`WifiManager`].
#[derive(Clone, Copy, Debug)]
pub struct ManagerConfig {
    pub backoff_initial_ms: u32,
    pub backoff_max_ms: u32,
    /// How often connection details (RSSI) are refreshed while connected.
    ///
    /// The reconnect backoff is reset only after the link has stayed up for
    /// one such interval, so a link that drops right after connecting keeps
    /// backing off.
    pub info_refresh_ms: u32,
}

impl ManagerConfig {
    pub const DEFAULT: Self = Self {
        backoff_initial_ms: 1_000,
        backoff_max_ms: 60_000,
        info_refresh_ms: 10_000,
    };
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    NoConfig,
    Idle,
    Attempt,
    Retry,
    Online,
}

/// Single owner of the Wi-Fi backend.
///
/// Runs the connection state machine: connect on configuration, publish
/// state, reconnect with bounded backoff after failures and link loss, and
/// apply configuration changes without a reboot. Nothing here panics or
/// reboots on failure.
pub struct WifiManager<'a, B, D, M: RawMutex> {
    backend: B,
    delay: D,
    shared: &'a WifiShared<M>,
    settings: ManagerConfig,
    config: Option<WifiConfig>,
    phase: Phase,
    status: WifiStatus,
    backoff: Backoff,
}

impl<'a, B: WifiBackend, D: DelayNs, M: RawMutex> WifiManager<'a, B, D, M> {
    pub fn new(backend: B, delay: D, shared: &'a WifiShared<M>, settings: ManagerConfig) -> Self {
        Self {
            backend,
            delay,
            shared,
            settings,
            config: None,
            phase: Phase::NoConfig,
            status: WifiStatus::new(WifiState::NotConfigured),
            backoff: Backoff::new(settings.backoff_initial_ms, settings.backoff_max_ms),
        }
    }

    /// Runs the manager forever. Spawn it as the only task using the backend.
    pub async fn run(&mut self) -> ! {
        loop {
            self.step().await;
        }
    }

    /// Performs one state machine transition, waiting for its trigger.
    pub async fn step(&mut self) {
        match self.phase {
            Phase::NoConfig => self.offline(WifiState::NotConfigured).await,
            Phase::Idle => self.offline(WifiState::Idle).await,
            Phase::Attempt => self.attempt().await,
            Phase::Retry => self.retry().await,
            Phase::Online => self.online().await,
        }
    }

    fn publish(&mut self, status: WifiStatus) {
        publish(self.shared, &mut self.status, status);
    }

    fn set_info(&self, info: Option<NetworkInfo>) {
        set_info(self.shared, info);
    }

    async fn offline(&mut self, state: WifiState) {
        if self.status.state != state {
            self.publish(WifiStatus::new(state));
        }
        let command = self.shared.commands.receive().await;
        self.on_command(command).await;
    }

    async fn attempt(&mut self) {
        let Some(config) = self.config else {
            self.phase = Phase::NoConfig;
            return;
        };
        self.set_info(None);
        self.publish(WifiStatus::new(WifiState::Connecting));

        let shared = self.shared;
        let mut scan_requested = false;
        let outcome = {
            let mut connect = pin!(self.backend.connect(&config));
            loop {
                match select(connect.as_mut(), shared.commands.receive()).await {
                    Either::First(result) => break Ok(result),
                    // The radio is busy connecting: serve the scan afterwards
                    // instead of aborting the attempt, so periodic scans
                    // cannot starve the connection.
                    Either::Second(Command::Scan) => scan_requested = true,
                    Either::Second(command) => break Err(command),
                }
            }
        };

        match outcome {
            Ok(Ok(info)) => {
                // The backoff is reset only once the link proved stable, see `online`.
                self.set_info(Some(info));
                self.phase = Phase::Online;
                self.publish(WifiStatus::new(WifiState::Connected));
            }
            Ok(Err(error)) => {
                warn!("Wi-Fi connect failed: {}", error);
                self.phase = Phase::Retry;
                self.publish(WifiStatus::failed(WifiState::Error, error));
            }
            Err(command) => {
                // The interrupted attempt may have left the driver half-connected.
                let _ = self.backend.disconnect().await;
                self.on_command(command).await;
            }
        }

        if scan_requested {
            self.scan().await;
        }
    }

    async fn retry(&mut self) {
        let wait = self.backoff.next_delay_ms();
        let shared = self.shared;
        let command = {
            // The timer keeps running while scans are served, so scans
            // neither postpone the retry nor consume backoff steps.
            let mut delay = pin!(self.delay.delay_ms(wait));
            loop {
                match select(delay.as_mut(), shared.commands.receive()).await {
                    Either::First(()) => {
                        self.phase = Phase::Attempt;
                        return;
                    }
                    Either::Second(Command::Scan) => {
                        scan(&mut self.backend, shared, &mut self.status).await;
                    }
                    Either::Second(command) => break command,
                }
            }
        };
        self.on_command(command).await;
    }

    async fn online(&mut self) {
        let shared = self.shared;
        let command = {
            let mut refresh = pin!(self.delay.delay_ms(self.settings.info_refresh_ms));
            loop {
                let event = select3(
                    self.backend.wait_link_lost(),
                    shared.commands.receive(),
                    refresh.as_mut(),
                )
                .await;
                match event {
                    Either3::First(error) => {
                        warn!("Wi-Fi link lost: {}", error);
                        set_info(shared, None);
                        self.phase = Phase::Retry;
                        publish(
                            shared,
                            &mut self.status,
                            WifiStatus::failed(WifiState::Disconnected, error),
                        );
                        return;
                    }
                    Either3::Second(Command::Scan) => {
                        scan(&mut self.backend, shared, &mut self.status).await;
                    }
                    Either3::Second(command) => break command,
                    Either3::Third(()) => {
                        // The link survived a full interval: it is stable.
                        self.backoff.reset();
                        if let Some(info) = self.backend.network_info() {
                            set_info(shared, Some(info));
                        }
                        return;
                    }
                }
            }
        };
        self.on_command(command).await;
    }

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::ApplyConfig(None) => {
                let _ = self.backend.disconnect().await;
                self.forget();
            }
            Command::ApplyConfig(Some(config)) => {
                let _ = self.backend.disconnect().await;
                if let Err(error) = config.validate() {
                    self.forget();
                    self.publish(WifiStatus::failed(WifiState::NotConfigured, error));
                } else {
                    self.config = Some(config);
                    self.backoff.reset();
                    self.set_info(None);
                    self.phase = Phase::Attempt;
                }
            }
            Command::Reconnect => {
                if self.config.is_none() {
                    self.publish(WifiStatus::failed(
                        WifiState::NotConfigured,
                        WifiError::NotConfigured,
                    ));
                    return;
                }
                let _ = self.backend.disconnect().await;
                self.backoff.reset();
                self.set_info(None);
                self.phase = Phase::Attempt;
            }
            Command::Disconnect => {
                let _ = self.backend.disconnect().await;
                self.set_info(None);
                self.backoff.reset();
                let (phase, state) = if self.config.is_some() {
                    (Phase::Idle, WifiState::Idle)
                } else {
                    (Phase::NoConfig, WifiState::NotConfigured)
                };
                self.phase = phase;
                self.publish(WifiStatus::new(state));
            }
            Command::Scan => self.scan().await,
        }
    }

    fn forget(&mut self) {
        self.config = None;
        self.backoff.reset();
        self.set_info(None);
        self.phase = Phase::NoConfig;
        self.publish(WifiStatus::new(WifiState::NotConfigured));
    }

    async fn scan(&mut self) {
        scan(&mut self.backend, self.shared, &mut self.status).await;
    }
}

// Free functions over disjoint manager fields, so they can run while a
// pinned timer future borrows the delay.

fn publish<M: RawMutex>(shared: &WifiShared<M>, current: &mut WifiStatus, status: WifiStatus) {
    if *current != status {
        info!("Wi-Fi: {:?} {:?}", status.state, status.error);
        *current = status;
        shared.status.sender().send(status);
    }
}

fn set_info<M: RawMutex>(shared: &WifiShared<M>, info: Option<NetworkInfo>) {
    shared.info.lock(|cell| cell.set(info));
}

async fn scan<B: WifiBackend, M: RawMutex>(
    backend: &mut B,
    shared: &WifiShared<M>,
    current: &mut WifiStatus,
) {
    let previous = *current;
    if previous.state != WifiState::Connected {
        publish(shared, current, WifiStatus::new(WifiState::Scanning));
    }
    let result = backend.scan().await;
    publish(shared, current, previous);
    shared.scan_pending.lock(|pending| pending.set(false));
    shared.scan.signal(result);
}

#[cfg(test)]
mod tests;
