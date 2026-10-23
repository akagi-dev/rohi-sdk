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

extern crate std;

use core::future::{pending, poll_fn};
use core::net::Ipv4Addr;
use core::pin::pin;
use core::task::Poll;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_futures::join::join;
use embassy_futures::poll_once;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::delay::DelayNs;

use super::*;
use crate::{ScanCollector, WifiNetwork, WifiPassword, WifiSecurity, WifiSsid};

type Shared = WifiShared<NoopRawMutex>;

enum Connect {
    Ok,
    Err(WifiError),
    Hang,
    /// Pends until `connect_released` is set, then succeeds.
    Held,
}

#[derive(Default)]
struct MockState {
    connects: VecDeque<Connect>,
    link_lost: VecDeque<WifiError>,
    scan_results: Option<Result<WifiScanResults, WifiError>>,
    connect_configs: Vec<WifiConfig>,
    disconnects: usize,
    delays: Vec<u32>,
    /// Delays pend while set; every started delay is still recorded once.
    hold_delays: bool,
    connect_released: bool,
    /// Scans pend while set.
    hold_scan: bool,
}

#[derive(Clone, Default)]
struct Mock(Rc<RefCell<MockState>>);

impl Mock {
    fn script(&self, connects: impl IntoIterator<Item = Connect>) {
        self.0.borrow_mut().connects.extend(connects);
    }
}

fn info_for(config: &WifiConfig) -> NetworkInfo {
    NetworkInfo {
        ssid: config.ssid.clone(),
        rssi_dbm: -50,
        ipv4: Ipv4Addr::new(192, 168, 1, 41),
        gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
        dns: None,
    }
}

impl WifiBackend for Mock {
    async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
        let state = self.0.clone();
        poll_fn(|_| {
            if state.borrow().hold_scan {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        self.0
            .borrow_mut()
            .scan_results
            .clone()
            .unwrap_or(Err(WifiError::ScanFailed))
    }

    async fn connect(&mut self, config: &WifiConfig) -> Result<NetworkInfo, WifiError> {
        let next = {
            let mut state = self.0.borrow_mut();
            state.connect_configs.push(config.clone());
            state.connects.pop_front()
        };
        match next {
            Some(Connect::Ok) => Ok(info_for(config)),
            Some(Connect::Err(error)) => Err(error),
            Some(Connect::Held) => {
                let state = self.0.clone();
                poll_fn(|_| {
                    if state.borrow().connect_released {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                Ok(info_for(config))
            }
            Some(Connect::Hang) | None => pending().await,
        }
    }

    async fn disconnect(&mut self) -> Result<(), WifiError> {
        self.0.borrow_mut().disconnects += 1;
        Ok(())
    }

    async fn wait_link_lost(&mut self) -> WifiError {
        let lost = self.0.borrow_mut().link_lost.pop_front();
        match lost {
            Some(error) => error,
            None => pending().await,
        }
    }

    fn network_info(&mut self) -> Option<NetworkInfo> {
        None
    }
}

struct MockDelay(Mock);

impl DelayNs for MockDelay {
    async fn delay_ns(&mut self, _ns: u32) {}

    async fn delay_ms(&mut self, ms: u32) {
        self.0.0.borrow_mut().delays.push(ms);
        let state = self.0.0.clone();
        poll_fn(|_| {
            if state.borrow().hold_delays {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }
}

fn config(ssid: &str) -> WifiConfig {
    WifiConfig::new(
        WifiSsid::from_bytes(ssid.as_bytes()).unwrap(),
        Some(WifiPassword::from_bytes(b"hunter2hunter2").unwrap()),
    )
}

struct Fixture<'a> {
    mock: Mock,
    handle: WifiHandle<'a, NoopRawMutex>,
    manager: WifiManager<'a, Mock, MockDelay, NoopRawMutex>,
}

fn fixture(shared: &Shared) -> Fixture<'_> {
    let mock = Mock::default();
    Fixture {
        handle: shared.handle(),
        manager: WifiManager::new(
            mock.clone(),
            MockDelay(mock.clone()),
            shared,
            ManagerConfig::DEFAULT,
        ),
        mock,
    }
}

impl Fixture<'_> {
    fn steps(&mut self, count: usize) {
        for _ in 0..count {
            block_on(self.manager.step());
        }
    }

    fn state(&self) -> WifiState {
        self.handle.status().state
    }
}

#[test]
fn not_configured_does_not_retry() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    assert_eq!(f.state(), WifiState::NotConfigured);
    // Nothing to do: the manager just waits for a command.
    assert!(poll_once(f.manager.step()).is_pending());
    assert!(f.mock.0.borrow().connect_configs.is_empty());
    assert!(f.mock.0.borrow().delays.is_empty());
}

#[test]
fn configured_connects_after_dhcp() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok]);
    let cfg = config("office");

    f.handle.set_desired_config(Some(cfg.clone()));
    f.steps(2);

    assert_eq!(f.state(), WifiState::Connected);
    assert!(f.handle.status().is_network_ready());
    let info = f.handle.network_info().unwrap();
    assert_eq!(info.ssid, cfg.ssid);
    assert_eq!(info.ipv4, Ipv4Addr::new(192, 168, 1, 41));
}

#[test]
fn connecting_is_not_network_ready() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Hang]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(1);
    // The attempt hangs (association without DHCP): never Connected.
    assert!(poll_once(f.manager.step()).is_pending());
    assert_eq!(f.state(), WifiState::Connecting);
    assert!(f.handle.network_info().is_none());
}

#[test]
fn failures_back_off_and_success_resets() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([
        Connect::Err(WifiError::AuthenticationFailed),
        Connect::Err(WifiError::DhcpFailed),
        Connect::Err(WifiError::NetworkNotFound),
        Connect::Ok,
    ]);
    f.handle.set_desired_config(Some(config("office")));

    f.steps(2); // command, first attempt
    assert_eq!(
        f.handle.status(),
        WifiStatus::failed(WifiState::Error, WifiError::AuthenticationFailed)
    );
    f.steps(2); // backoff, attempt
    assert_eq!(f.handle.status().error, Some(WifiError::DhcpFailed));
    f.steps(2);
    assert_eq!(f.handle.status().error, Some(WifiError::NetworkNotFound));
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.mock.0.borrow().delays, [1_000, 2_000, 4_000]);

    // Once the link survived one refresh interval, the backoff restarts
    // from the initial delay.
    f.steps(1);
    f.mock
        .0
        .borrow_mut()
        .link_lost
        .push_back(WifiError::Disconnected);
    f.mock.script([Connect::Ok]);
    f.steps(1);
    assert_eq!(
        f.handle.status(),
        WifiStatus::failed(WifiState::Disconnected, WifiError::Disconnected)
    );
    assert!(f.handle.network_info().is_none());
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(
        f.mock.0.borrow().delays,
        [1_000, 2_000, 4_000, 10_000, 1_000]
    );
}

#[test]
fn flapping_link_keeps_backing_off() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok, Connect::Ok, Connect::Ok]);
    f.mock
        .0
        .borrow_mut()
        .link_lost
        .extend([WifiError::Disconnected, WifiError::Disconnected]);
    f.handle.set_desired_config(Some(config("office")));

    f.steps(2); // command, connect
    f.steps(1); // link lost immediately
    f.steps(2); // backoff, connect
    f.steps(1); // link lost immediately again
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    // A connection that drops right away does not reset the backoff.
    assert_eq!(f.mock.0.borrow().delays, [1_000, 2_000]);
}

#[test]
fn scan_during_connect_does_not_abort_attempt() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Held]);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));
    f.handle.set_desired_config(Some(config("office")));
    f.steps(1);
    let disconnects = f.mock.0.borrow().disconnects;
    let handle = f.handle;
    let mock = f.mock.clone();

    let result = block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        let mut scan = pin!(handle.scan());
        assert!(poll_once(scan.as_mut()).is_pending());
        // The scan request is deferred; the attempt keeps running.
        assert!(poll_once(step.as_mut()).is_pending());
        assert_eq!(handle.status().state, WifiState::Connecting);

        mock.0.borrow_mut().connect_released = true;
        let (result, ()) = join(scan, step).await;
        result
    });

    assert!(result.is_ok());
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.mock.0.borrow().connect_configs.len(), 1);
    assert_eq!(f.mock.0.borrow().disconnects, disconnects);
}

#[test]
fn scan_during_backoff_keeps_the_timer() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock
        .script([Connect::Err(WifiError::NetworkNotFound), Connect::Ok]);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);
    assert_eq!(f.state(), WifiState::Error);
    f.mock.0.borrow_mut().hold_delays = true;
    let handle = f.handle;
    let mock = f.mock.clone();

    block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        let mut scan = pin!(handle.scan());
        assert!(poll_once(scan.as_mut()).is_pending());
        // Served without leaving the backoff wait.
        assert!(poll_once(step.as_mut()).is_pending());
        assert!(scan.await.is_ok());
        assert_eq!(
            handle.status(),
            WifiStatus::failed(WifiState::Error, WifiError::NetworkNotFound)
        );
        mock.0.borrow_mut().hold_delays = false;
        step.await;
    });
    f.steps(1);

    assert_eq!(f.state(), WifiState::Connected);
    // One timer, one backoff step: the scan neither restarted nor advanced it.
    assert_eq!(f.mock.0.borrow().delays, [1_000]);
}

#[test]
fn backoff_is_bounded() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock
        .script((0..12).map(|_| Connect::Err(WifiError::NetworkNotFound)));
    f.handle.set_desired_config(Some(config("office")));
    f.steps(1 + 2 * 11);
    let max = ManagerConfig::DEFAULT.backoff_max_ms;
    let delays = f.mock.0.borrow().delays.clone();
    assert_eq!(delays.len(), 11);
    assert!(delays.iter().all(|delay| *delay <= max));
    assert_eq!(*delays.last().unwrap(), max);
}

#[test]
fn config_change_reconnects_with_new_config() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok, Connect::Ok]);
    f.handle.set_desired_config(Some(config("old")));
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    let disconnects = f.mock.0.borrow().disconnects;

    f.handle.set_desired_config(Some(config("new")));
    f.steps(2);

    assert!(f.mock.0.borrow().disconnects > disconnects);
    assert_eq!(
        f.mock.0.borrow().connect_configs.last(),
        Some(&config("new"))
    );
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.handle.network_info().unwrap().ssid, config("new").ssid);
}

#[test]
fn config_change_interrupts_pending_attempt() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Hang, Connect::Ok]);
    f.handle.set_desired_config(Some(config("old")));
    f.steps(1);
    assert!(poll_once(f.manager.step()).is_pending()); // attempt hangs
    f.handle.set_desired_config(Some(config("new")));
    f.steps(2); // interrupted attempt reconciles, then reconnects
    assert_eq!(
        f.mock.0.borrow().connect_configs.last(),
        Some(&config("new"))
    );
    assert_eq!(f.state(), WifiState::Connected);
}

#[test]
fn config_removed_disconnects_and_is_not_configured() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);

    f.handle.set_desired_config(None);
    f.steps(1);

    assert_eq!(f.state(), WifiState::NotConfigured);
    assert!(f.handle.network_info().is_none());
    assert!(poll_once(f.manager.step()).is_pending());
}

#[test]
fn invalid_config_is_rejected_without_connecting() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    let invalid = WifiConfig::new(WifiSsid::empty(), None);
    f.handle.set_desired_config(Some(invalid));
    f.steps(1);
    assert_eq!(
        f.handle.status(),
        WifiStatus::failed(WifiState::NotConfigured, WifiError::InvalidConfig)
    );
    assert!(f.mock.0.borrow().connect_configs.is_empty());
}

#[test]
fn disconnect_goes_idle_and_reconnect_resumes() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok, Connect::Ok]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);

    f.handle.disconnect().unwrap();
    f.steps(1);
    assert!(poll_once(f.manager.step()).is_pending());
    assert_eq!(f.state(), WifiState::Idle);
    assert!(f.handle.network_info().is_none());

    f.handle.reconnect().unwrap();
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
}

#[test]
fn reconnect_without_config_reports_not_configured() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.handle.reconnect().unwrap();
    f.steps(1);
    assert_eq!(
        f.handle.status(),
        WifiStatus::failed(WifiState::NotConfigured, WifiError::NotConfigured)
    );
}

#[test]
fn command_queue_is_bounded() {
    let shared = Shared::new();
    let f = fixture(&shared);
    for _ in 0..WIFI_COMMAND_QUEUE_DEPTH {
        f.handle.reconnect().unwrap();
    }
    assert_eq!(f.handle.reconnect(), Err(WifiError::Busy));
    assert_eq!(f.handle.disconnect(), Err(WifiError::Busy));
}

#[test]
fn desired_config_is_published_even_with_a_full_command_queue() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok]);
    for _ in 0..WIFI_COMMAND_QUEUE_DEPTH {
        f.handle.reconnect().unwrap();
    }
    assert_eq!(f.handle.reconnect(), Err(WifiError::Busy));

    // Synchronous: no queue room is needed.
    f.handle.set_desired_config(Some(config("office")));
    assert!(f.handle.is_configured());

    // The desired configuration is reconciled before queued commands.
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.mock.0.borrow().connect_configs, [config("office")]);
}

#[test]
fn desired_config_accepts_without_association() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.handle.set_desired_config(Some(config("office")));
    assert_eq!(f.state(), WifiState::NotConfigured);
    assert!(f.mock.0.borrow().connect_configs.is_empty());
    f.steps(1);
    assert_ne!(f.state(), WifiState::Connected);
    assert!(f.handle.network_info().is_none());
}

#[test]
fn rapid_updates_coalesce_to_the_latest() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok, Connect::Ok, Connect::Ok]);
    f.handle.set_desired_config(Some(config("a")));
    f.handle.set_desired_config(Some(config("b")));
    f.handle.set_desired_config(Some(config("c")));
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    // A and B never reached the radio: no connect/disconnect cycle for them.
    assert_eq!(f.mock.0.borrow().connect_configs, [config("c")]);
}

#[test]
fn updates_while_connecting_coalesce_to_the_latest() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Hang, Connect::Ok]);
    f.handle.set_desired_config(Some(config("a")));
    f.steps(1);
    assert!(poll_once(f.manager.step()).is_pending());
    f.handle.set_desired_config(Some(config("b")));
    f.handle.set_desired_config(Some(config("c")));
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(
        f.mock.0.borrow().connect_configs,
        [config("a"), config("c")]
    );
}

#[test]
fn same_config_while_connected_keeps_the_link() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);
    let disconnects = f.mock.0.borrow().disconnects;

    f.handle.set_desired_config(Some(config("office")));
    // Only the refresh timer completes the step: the link is untouched.
    f.steps(1);
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.mock.0.borrow().disconnects, disconnects);
    assert_eq!(f.mock.0.borrow().connect_configs.len(), 1);
}

#[test]
fn same_config_while_connecting_keeps_the_attempt() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Held]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(1);
    let disconnects = f.mock.0.borrow().disconnects;
    let mock = f.mock.clone();
    let handle = f.handle;
    block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        handle.set_desired_config(Some(config("office")));
        assert!(poll_once(step.as_mut()).is_pending());
        mock.0.borrow_mut().connect_released = true;
        step.await;
    });
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.mock.0.borrow().connect_configs.len(), 1);
    assert_eq!(f.mock.0.borrow().disconnects, disconnects);
}

#[test]
fn same_config_during_backoff_keeps_the_timer() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock
        .script([Connect::Err(WifiError::NetworkNotFound), Connect::Ok]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);
    f.mock.0.borrow_mut().hold_delays = true;
    let mock = f.mock.clone();
    let handle = f.handle;
    block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        handle.set_desired_config(Some(config("office")));
        assert!(poll_once(step.as_mut()).is_pending());
        mock.0.borrow_mut().hold_delays = false;
        step.await;
    });
    f.steps(1);
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(f.mock.0.borrow().delays, [1_000]);
}

#[test]
fn same_config_while_idle_stays_idle() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);
    f.handle.disconnect().unwrap();
    f.steps(1);
    assert_eq!(f.state(), WifiState::Idle);

    f.handle.set_desired_config(Some(config("office")));
    assert!(poll_once(f.manager.step()).is_pending());
    assert_eq!(f.state(), WifiState::Idle);
    assert_eq!(f.mock.0.borrow().connect_configs.len(), 1);
}

#[test]
fn new_config_does_not_inherit_backoff() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    // AP A: wrong password, backing off further and further.
    f.mock.script([
        Connect::Err(WifiError::AuthenticationFailed),
        Connect::Err(WifiError::AuthenticationFailed),
        Connect::Err(WifiError::AuthenticationFailed),
        Connect::Err(WifiError::AuthenticationFailed),
    ]);
    f.handle.set_desired_config(Some(config("a")));
    f.steps(1 + 2 * 3 + 1);
    assert_eq!(f.state(), WifiState::Error);
    assert_eq!(f.mock.0.borrow().delays, [1_000, 2_000, 4_000]);

    // Switch to AP B in the middle of A's 8 s backoff wait.
    f.mock.0.borrow_mut().hold_delays = true;
    f.mock
        .script([Connect::Err(WifiError::NetworkNotFound), Connect::Ok]);
    let handle = f.handle;
    let mock = f.mock.clone();
    block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        handle.set_desired_config(Some(config("b")));
        // The backoff wait is abandoned right away.
        assert!(poll_once(step.as_mut()).is_ready());
        mock.0.borrow_mut().hold_delays = false;
    });
    f.steps(1); // B, first attempt, no delay in between
    assert_eq!(f.mock.0.borrow().connect_configs.last(), Some(&config("b")));
    f.steps(2); // B's own backoff starts from the initial delay
    assert_eq!(f.state(), WifiState::Connected);
    assert_eq!(
        f.mock.0.borrow().delays,
        [1_000, 2_000, 4_000, 8_000, 1_000]
    );
}

#[test]
fn forget_during_backoff_stops_retrying() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Err(WifiError::NetworkNotFound)]);
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);
    f.mock.0.borrow_mut().hold_delays = true;
    let handle = f.handle;
    block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        handle.set_desired_config(None);
        assert!(poll_once(step.as_mut()).is_ready());
    });
    assert_eq!(f.state(), WifiState::NotConfigured);
    assert!(!f.handle.is_configured());
    f.mock.0.borrow_mut().hold_delays = false;
    // No automatic reconnect: the manager waits for a new configuration.
    assert!(poll_once(f.manager.step()).is_pending());
    assert_eq!(f.mock.0.borrow().connect_configs.len(), 1);
}

#[test]
fn config_change_during_scan_applies_after_the_scan() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok, Connect::Ok]);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));
    f.handle.set_desired_config(Some(config("old")));
    f.steps(2);
    f.mock.0.borrow_mut().hold_scan = true;
    f.mock.0.borrow_mut().hold_delays = true;
    let handle = f.handle;
    let mock = f.mock.clone();

    let result = block_on(async {
        let mut step = pin!(f.manager.step());
        assert!(poll_once(step.as_mut()).is_pending());
        let mut scan = pin!(handle.scan());
        assert!(poll_once(scan.as_mut()).is_pending());
        // The manager is scanning now; the update waits for the scan.
        assert!(poll_once(step.as_mut()).is_pending());
        handle.set_desired_config(Some(config("new")));
        assert!(poll_once(step.as_mut()).is_pending());
        assert_eq!(mock.0.borrow().connect_configs.len(), 1);

        mock.0.borrow_mut().hold_scan = false;
        mock.0.borrow_mut().hold_delays = false;
        let (result, ()) = join(scan, step).await;
        result
    });
    assert!(result.is_ok());
    f.steps(1);
    assert_eq!(
        f.mock.0.borrow().connect_configs.last(),
        Some(&config("new"))
    );
    assert_eq!(f.state(), WifiState::Connected);
}

#[test]
fn dropped_scan_does_not_leak_into_the_next_one() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.0.borrow_mut().scan_results = Some(Err(WifiError::ScanFailed));
    let handle = f.handle;
    {
        let mut abandoned = pin!(handle.scan());
        assert!(poll_once(abandoned.as_mut()).is_pending());
    }
    // The manager still serves the abandoned request; its result is dropped.
    f.steps(1);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));
    let (result, ()) = block_on(join(handle.scan(), f.manager.step()));
    assert_eq!(result.unwrap().len(), 1);
    // The command queue capacity is intact.
    for _ in 0..WIFI_COMMAND_QUEUE_DEPTH {
        handle.reconnect().unwrap();
    }
}

fn scan_result() -> WifiScanResults {
    let mut collector = ScanCollector::new();
    collector.insert(WifiNetwork {
        ssid: WifiSsid::from_bytes(b"office").unwrap(),
        rssi_dbm: -42,
        security: WifiSecurity::Wpa2,
        channel: 6,
    });
    collector.finish()
}

#[test]
fn scan_returns_bounded_results_and_restores_state() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));

    let (result, ()) = block_on(join(f.handle.scan(), f.manager.step()));

    assert_eq!(result.unwrap().len(), 1);
    assert_eq!(f.state(), WifiState::NotConfigured);
}

#[test]
fn scan_while_connected_keeps_connected() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.script([Connect::Ok]);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));
    f.handle.set_desired_config(Some(config("office")));
    f.steps(2);

    let (result, ()) = block_on(join(f.handle.scan(), f.manager.step()));

    assert!(result.is_ok());
    assert_eq!(f.state(), WifiState::Connected);
    assert!(f.handle.network_info().is_some());
}

#[test]
fn scan_failure_is_reported() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.0.borrow_mut().scan_results = Some(Err(WifiError::ScanFailed));
    let (result, ()) = block_on(join(f.handle.scan(), f.manager.step()));
    assert_eq!(result, Err(WifiError::ScanFailed));
}

#[test]
fn only_one_scan_in_flight() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock.0.borrow_mut().scan_results = Some(Ok(scan_result()));
    let handle = f.handle;

    let (first, second) = block_on(async {
        let mut first = pin!(handle.scan());
        assert!(poll_once(&mut first).is_pending());
        let second = handle.scan().await;
        f.manager.step().await;
        (first.await, second)
    });

    assert!(first.is_ok());
    assert_eq!(second, Err(WifiError::Busy));
    // The slot is free again afterwards.
    let (again, ()) = block_on(join(handle.scan(), f.manager.step()));
    assert!(again.is_ok());
}
