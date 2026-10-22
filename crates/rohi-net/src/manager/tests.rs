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

use core::future::pending;
use core::net::Ipv4Addr;
use core::pin::pin;
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
}

#[derive(Default)]
struct MockState {
    connects: VecDeque<Connect>,
    link_lost: VecDeque<WifiError>,
    scan_results: Option<Result<WifiScanResults, WifiError>>,
    connect_configs: Vec<WifiConfig>,
    disconnects: usize,
    delays: Vec<u32>,
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
        ssid: config.ssid,
        rssi_dbm: -50,
        ipv4: Ipv4Addr::new(192, 168, 1, 41),
        gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
        dns: None,
    }
}

impl WifiBackend for Mock {
    async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
        self.0
            .borrow_mut()
            .scan_results
            .clone()
            .unwrap_or(Err(WifiError::ScanFailed))
    }

    async fn connect(&mut self, config: &WifiConfig) -> Result<NetworkInfo, WifiError> {
        let next = {
            let mut state = self.0.borrow_mut();
            state.connect_configs.push(*config);
            state.connects.pop_front()
        };
        match next {
            Some(Connect::Ok) => Ok(info_for(config)),
            Some(Connect::Err(error)) => Err(error),
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

    f.handle.apply_config(Some(cfg)).unwrap();
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
    f.handle.apply_config(Some(config("office"))).unwrap();
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
    f.handle.apply_config(Some(config("office"))).unwrap();

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

    // Link loss after a success restarts from the initial delay.
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
    assert_eq!(f.mock.0.borrow().delays, [1_000, 2_000, 4_000, 1_000]);
}

#[test]
fn backoff_is_bounded() {
    let shared = Shared::new();
    let mut f = fixture(&shared);
    f.mock
        .script((0..12).map(|_| Connect::Err(WifiError::NetworkNotFound)));
    f.handle.apply_config(Some(config("office"))).unwrap();
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
    f.handle.apply_config(Some(config("old"))).unwrap();
    f.steps(2);
    assert_eq!(f.state(), WifiState::Connected);
    let disconnects = f.mock.0.borrow().disconnects;

    f.handle.apply_config(Some(config("new"))).unwrap();
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
    f.handle.apply_config(Some(config("old"))).unwrap();
    f.steps(1);
    f.handle.apply_config(Some(config("new"))).unwrap();
    f.steps(2); // interrupted attempt handles the command, then reconnects
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
    f.handle.apply_config(Some(config("office"))).unwrap();
    f.steps(2);

    f.handle.apply_config(None).unwrap();
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
    f.handle.apply_config(Some(invalid)).unwrap();
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
    f.handle.apply_config(Some(config("office"))).unwrap();
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
    assert_eq!(f.handle.apply_config(None), Err(WifiError::Busy));
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
    f.handle.apply_config(Some(config("office"))).unwrap();
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
