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

use core::cell::{Cell, RefCell};
use core::pin::pin;

use embassy_futures::select::{Either, Either3, Either4, select, select3, select4};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embedded_hal_async::delay::DelayNs;
use log::{info, warn};

use super::backoff::Backoff;
use crate::{
    NetworkInfo, WIFI_COMMAND_QUEUE_DEPTH, WifiBackend, WifiConfig, WifiError, WifiScanResults,
    WifiState, WifiStatus,
};

/// Number of tasks that can wait for status changes at the same time.
pub const STATUS_RECEIVERS: usize = 3;

/// Transient, imperative requests. Persistent configuration is never a
/// command: it is published as desired state, see
/// [`WifiHandle::set_desired_config`].
#[derive(Clone, Copy)]
enum Command {
    Reconnect,
    Disconnect,
    Scan,
}

/// Latest desired configuration. Every publication bumps `generation`, so the
/// manager notices updates it has not seen yet even when several of them were
/// coalesced into the last one.
struct Desired {
    generation: u32,
    config: Option<WifiConfig>,
}

/// State shared between the [`WifiManager`] task and its [`WifiHandle`]s.
///
/// Place it in a `static`; all storage is inline and bounded.
pub struct WifiShared<M: RawMutex> {
    desired: BlockingMutex<M, RefCell<Desired>>,
    desired_changed: Signal<M, ()>,
    commands: Channel<M, Command, WIFI_COMMAND_QUEUE_DEPTH>,
    status: Watch<M, WifiStatus, STATUS_RECEIVERS>,
    info: BlockingMutex<M, RefCell<Option<NetworkInfo>>>,
    scan: Signal<M, Result<WifiScanResults, WifiError>>,
    scan_pending: BlockingMutex<M, Cell<bool>>,
}

impl<M: RawMutex> WifiShared<M> {
    pub const fn new() -> Self {
        Self {
            desired: BlockingMutex::new(RefCell::new(Desired {
                generation: 0,
                config: None,
            })),
            desired_changed: Signal::new(),
            commands: Channel::new(),
            status: Watch::new_with(WifiStatus::new(WifiState::NotConfigured)),
            info: BlockingMutex::new(RefCell::new(None)),
            scan: Signal::new(),
            scan_pending: BlockingMutex::new(Cell::new(false)),
        }
    }

    /// Handle used by other tasks to command the manager and read its state.
    pub const fn handle(&self) -> WifiHandle<'_, M> {
        WifiHandle { shared: self }
    }

    fn generation(&self) -> u32 {
        self.desired.lock(|desired| desired.borrow().generation)
    }
}

impl<M: RawMutex> Default for WifiShared<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// Cheap, copyable interface to the Wi-Fi manager: the Wi-Fi control plane.
///
/// The persistent configuration is published as desired state
/// ([`set_desired_config`](Self::set_desired_config)); transient actions
/// (scan, reconnect, disconnect) are bounded commands. No handle method
/// touches the platform driver. IP traffic (DNS, TCP) does not go through
/// this handle, see [`NetworkHandle`](crate::NetworkHandle).
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

    /// Publishes the desired configuration: `Some` to use a network, `None`
    /// when no network is configured (forgotten).
    ///
    /// Synchronous on purpose: there is no `.await`, so a caller that
    /// persisted the configuration cannot be cancelled before the manager
    /// learns about it. The latest value wins; updates published before the
    /// manager ran are coalesced and only the newest one is applied.
    /// Publishing the configuration the manager already uses changes nothing
    /// (use [`reconnect`](Self::reconnect) to force a new attempt).
    ///
    /// The manager validates the value again. Completion does not mean the
    /// access point was reached; follow [`status`](Self::status) for that.
    pub fn set_desired_config(&self, config: Option<WifiConfig>) {
        self.shared.desired.lock(|desired| {
            let mut desired = desired.borrow_mut();
            desired.generation = desired.generation.wrapping_add(1);
            desired.config = config;
        });
        self.shared.desired_changed.signal(());
    }

    /// `true` if the latest desired configuration is `Some`.
    pub fn is_configured(&self) -> bool {
        self.shared
            .desired
            .lock(|desired| desired.borrow().config.is_some())
    }

    /// Drops the current connection and connects again immediately.
    ///
    /// Fails with [`WifiError::Busy`] when the bounded command queue is full.
    pub fn reconnect(&self) -> Result<(), WifiError> {
        self.send(Command::Reconnect)
    }

    /// Disconnects and stays idle until the next reconnect or a new
    /// configuration.
    ///
    /// Fails with [`WifiError::Busy`] when the bounded command queue is full.
    pub fn disconnect(&self) -> Result<(), WifiError> {
        self.send(Command::Disconnect)
    }

    /// Scans for networks. Only one scan can be in flight.
    ///
    /// The caller should bound the wait with its own timeout.
    ///
    /// Cancellation: dropping the future abandons the request. A scan already
    /// handed to the manager still runs and other callers get
    /// [`WifiError::Busy`] until it finishes; its result is discarded and is
    /// never delivered to a later caller.
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
        self.shared.info.lock(|info| info.borrow().clone())
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

/// What interrupted a wait of the manager.
enum Interrupt {
    Command(Command),
    Desired,
}

/// Single owner of the Wi-Fi backend.
///
/// Reconciles the radio towards the latest desired configuration: connect
/// when configured, publish state, reconnect with bounded backoff after
/// failures and link loss, and switch networks without a reboot. Transient
/// commands (scan, reconnect, disconnect) are served in between. Nothing here
/// panics or reboots on failure.
///
/// | Desired update | Behaviour |
/// |----------------|-----------|
/// | `None` | disconnect, `NotConfigured`, no automatic reconnect |
/// | new `Some(config)` | abort attempt or backoff, disconnect, connect with the new config and a fresh backoff |
/// | same config again | nothing: the attempt, backoff timer, idle state or link continue |
/// | invalid config | disconnect, `NotConfigured` with [`WifiError::InvalidConfig`] |
///
/// An update published during a scan is applied once the scan finished.
pub struct WifiManager<'a, B, D, M: RawMutex> {
    backend: B,
    delay: D,
    shared: &'a WifiShared<M>,
    settings: ManagerConfig,
    config: Option<WifiConfig>,
    /// Generation of the last desired configuration taken into account.
    seen: u32,
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
            seen: 0,
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
        let shared = self.shared;
        if !desired_differs(shared, &mut self.seen, &self.config) {
            match select(
                shared.commands.receive(),
                desired_changed(shared, self.seen),
            )
            .await
            {
                Either::First(command) => return self.on_command(command).await,
                Either::Second(()) => {}
            }
        }
        self.reconcile().await;
    }

    async fn attempt(&mut self) {
        let Some(config) = self.config.clone() else {
            self.phase = Phase::NoConfig;
            return;
        };
        self.set_info(None);
        self.publish(WifiStatus::new(WifiState::Connecting));

        let shared = self.shared;
        let seen = &mut self.seen;
        let mut scan_requested = false;
        let outcome = {
            let mut connect = pin!(self.backend.connect(&config));
            loop {
                if desired_differs(shared, seen, &self.config) {
                    break Err(Interrupt::Desired);
                }
                match select3(
                    connect.as_mut(),
                    shared.commands.receive(),
                    desired_changed(shared, *seen),
                )
                .await
                {
                    Either3::First(result) => break Ok(result),
                    // The radio is busy connecting: serve the scan afterwards
                    // instead of aborting the attempt, so periodic scans
                    // cannot starve the connection.
                    Either3::Second(Command::Scan) => scan_requested = true,
                    Either3::Second(command) => break Err(Interrupt::Command(command)),
                    // Same configuration republished: keep connecting.
                    Either3::Third(()) => {}
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
            Err(interrupt) => {
                // The interrupted attempt may have left the driver half-connected.
                let _ = self.backend.disconnect().await;
                self.on_interrupt(interrupt).await;
            }
        }

        if scan_requested {
            self.scan().await;
        }
    }

    async fn retry(&mut self) {
        let wait = self.backoff.next_delay_ms();
        let shared = self.shared;
        let interrupt = {
            // The timer keeps running while scans are served, so scans
            // neither postpone the retry nor consume backoff steps.
            let mut delay = pin!(self.delay.delay_ms(wait));
            loop {
                if desired_differs(shared, &mut self.seen, &self.config) {
                    break Interrupt::Desired;
                }
                match select3(
                    delay.as_mut(),
                    shared.commands.receive(),
                    desired_changed(shared, self.seen),
                )
                .await
                {
                    Either3::First(()) => {
                        self.phase = Phase::Attempt;
                        return;
                    }
                    Either3::Second(Command::Scan) => {
                        scan(&mut self.backend, shared, &mut self.status).await;
                    }
                    Either3::Second(command) => break Interrupt::Command(command),
                    Either3::Third(()) => {}
                }
            }
        };
        self.on_interrupt(interrupt).await;
    }

    async fn online(&mut self) {
        let shared = self.shared;
        let interrupt = {
            let mut refresh = pin!(self.delay.delay_ms(self.settings.info_refresh_ms));
            loop {
                if desired_differs(shared, &mut self.seen, &self.config) {
                    break Interrupt::Desired;
                }
                let event = select4(
                    self.backend.wait_link_lost(),
                    shared.commands.receive(),
                    refresh.as_mut(),
                    desired_changed(shared, self.seen),
                )
                .await;
                match event {
                    Either4::First(error) => {
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
                    Either4::Second(Command::Scan) => {
                        scan(&mut self.backend, shared, &mut self.status).await;
                    }
                    Either4::Second(command) => break Interrupt::Command(command),
                    Either4::Third(()) => {
                        // The link survived a full interval: it is stable.
                        self.backoff.reset();
                        if let Some(info) = self.backend.network_info() {
                            set_info(shared, Some(info));
                        }
                        return;
                    }
                    Either4::Fourth(()) => {}
                }
            }
        };
        self.on_interrupt(interrupt).await;
    }

    async fn on_interrupt(&mut self, interrupt: Interrupt) {
        match interrupt {
            Interrupt::Command(command) => self.on_command(command).await,
            Interrupt::Desired => self.reconcile().await,
        }
    }

    /// Takes the latest desired configuration and moves towards it.
    async fn reconcile(&mut self) {
        let (generation, desired) = self.shared.desired.lock(|desired| {
            let desired = desired.borrow();
            (desired.generation, desired.config.clone())
        });
        self.seen = generation;
        match desired {
            None => {
                if self.config.is_some() {
                    let _ = self.backend.disconnect().await;
                }
                self.forget();
            }
            Some(config) => {
                if let Err(error) = config.validate() {
                    let _ = self.backend.disconnect().await;
                    self.forget();
                    self.publish(WifiStatus::failed(WifiState::NotConfigured, error));
                } else if self.config.as_ref() != Some(&config) {
                    let _ = self.backend.disconnect().await;
                    // Retry state belongs to the previous network.
                    self.config = Some(config);
                    self.backoff.reset();
                    self.set_info(None);
                    self.phase = Phase::Attempt;
                }
            }
        }
    }

    async fn on_command(&mut self, command: Command) {
        match command {
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
// pinned timer or connect future borrows the delay or the backend.

fn publish<M: RawMutex>(shared: &WifiShared<M>, current: &mut WifiStatus, status: WifiStatus) {
    if *current != status {
        info!("Wi-Fi: {:?} {:?}", status.state, status.error);
        *current = status;
        shared.status.sender().send(status);
    }
}

fn set_info<M: RawMutex>(shared: &WifiShared<M>, info: Option<NetworkInfo>) {
    shared.info.lock(|cell| *cell.borrow_mut() = info);
}

/// `true` if an unseen desired configuration differs from `current`. An
/// unseen but identical one is marked as seen, so republishing the same
/// configuration never interrupts anything.
fn desired_differs<M: RawMutex>(
    shared: &WifiShared<M>,
    seen: &mut u32,
    current: &Option<WifiConfig>,
) -> bool {
    shared.desired.lock(|desired| {
        let desired = desired.borrow();
        if desired.generation == *seen {
            return false;
        }
        if desired.config == *current {
            *seen = desired.generation;
            return false;
        }
        true
    })
}

/// Completes once a desired configuration newer than `seen` is published.
/// Cancel-safe: the generation is checked on every wake-up.
async fn desired_changed<M: RawMutex>(shared: &WifiShared<M>, seen: u32) {
    while shared.generation() == seen {
        shared.desired_changed.wait().await;
    }
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
