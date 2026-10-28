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

//! Time service of the Altruist Urban: synchronized wall-clock time.
//!
//! ```text
//! network ready -> NtpTimeSource -> Synchronizer (ntp_task) -> TimeService
//!                                                                  |
//!                       control plane, future logging/datalog ----+  now()
//! ```
//!
//! The service starts unsynchronized and nothing waits for it: sensing, the
//! control plane and Wi-Fi run without wall-clock time. [`ntp_task`] samples
//! NTP in the background once the network is ready and keeps resynchronizing;
//! failures are retried with bounded backoff and only recorded in the status.
//! The servers are the `time` configuration section (the public servers,
//! the firmware overrides and the stored overrides) and follow its changes, see
//! [`publish`].
//! Components read the time through [`service`] and never touch NTP.
//!
//! Wall-clock time may step when a new sample corrects it; durations and
//! deadlines use the monotonic clock. NTP is unauthenticated, so the time is
//! not input for security decisions.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Delay;
use log::{info, warn};
use rohi_config::materialize;
use rohi_hal_esp::EmbassyClock;
use rohi_net::NetworkHandle;
use rohi_service::ManagedConfig;
use rohi_time::{SyncSchedule, Synchronizer, TimeConfig, TimeService, UnixTime};
use rohi_time_ntp::{NtpConfig, NtpServersConfig, NtpServersOverrides, NtpTimeSource};

use crate::config::ConfigHandle;
use crate::policy;
use crate::resources::Ip;
use crate::tasks::RawMutex;

/// The firmware's time service.
pub type Time = TimeService<EmbassyClock, CriticalSectionRawMutex>;

/// `BUILD_TIMESTAMP` (Unix seconds), set by `build.rs`. A device cannot be running
/// before its firmware was built, so no earlier time is plausible; it is also
/// the NTP era pivot until the first synchronization.
const BUILD_TIME: UnixTime = UnixTime::from_secs(parse_secs(env!("BUILD_TIMESTAMP")));

// A host without a clock yields 0 in `build.rs`; refuse to build a firmware whose
// plausibility window would exclude the present.
const _: () = assert!(
    BUILD_TIME.as_secs() >= 1_767_225_600,
    "the build time must be 2026-01-01 or later"
);

const fn parse_secs(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut value = 0u64;
    let mut index = 0;
    while index < bytes.len() {
        assert!(bytes[index].is_ascii_digit(), "invalid build time");
        value = value * 10 + (bytes[index] - b'0') as u64;
        index += 1;
    }
    value
}

/// Acceptance and staleness policy: plausible from the build time on, stale
/// after `DEFAULT_STALE_AFTER` (six hours, several missed hourly resyncs).
const CONFIG: TimeConfig = TimeConfig::new(BUILD_TIME);

static TIME: Time = TimeService::new(EmbassyClock, CONFIG);

/// The time service for consumers: `service().now()` is `None` until the first
/// valid sample.
pub fn service() -> &'static Time {
    &TIME
}

/// Servers the next synchronization rounds use, see [`publish`].
static UPDATES: Signal<RawMutex, NtpServersConfig> = Signal::new();

/// Hands the effective `NtpServersConfig` (the defaults, the firmware
/// overrides and the committed user overrides) to the running
/// [`ntp_task`]. Synchronous, so the configuration owner can call it in the
/// commit step; only the latest value matters.
pub fn publish(servers: NtpServersConfig) {
    UPDATES.signal(servers);
}

/// The effective servers, or the firmware policy over the defaults when they
/// are unusable.
fn resolve(servers: NtpServersConfig) -> NtpServersConfig {
    if servers.validate().is_ok() {
        servers
    } else {
        warn!("Time: NTP servers are invalid, using the default servers");
        materialize(&policy::time(), &NtpServersOverrides::default())
    }
}

/// Keeps the time service synchronized over NTP; never returns.
///
/// Waits for the network without counting failures, then samples hourly
/// ([`SyncSchedule::DEFAULT`]); after a failure it backs off from 15 s up to
/// 10 minutes. Uses one UDP socket per exchange, see
/// [`crate::resources::UDP_SOCKETS`].
///
/// The servers come from the `time` configuration section. Every commit or
/// removal of it restarts the synchronization with the new servers, which
/// samples at once.
#[embassy_executor::task]
pub async fn ntp_task(network: NetworkHandle<'static, Ip>, config: ConfigHandle) {
    let user = match config.get::<NtpServersOverrides>().await {
        Ok(Some(overrides)) => overrides,
        Ok(None) => NtpServersOverrides::default(),
        Err(error) => {
            warn!("Time: stored NTP servers unreadable: {}", error);
            NtpServersOverrides::default()
        }
    };
    let mut servers = resolve(materialize(&policy::time(), &user));
    loop {
        let published = {
            let hosts = servers.servers();
            info!("Time: NTP servers {:?}", hosts.as_slice());
            let source =
                NtpTimeSource::new(network, Delay, NtpConfig::new(&hosts), || TIME.pivot());
            let mut synchronizer = Synchronizer::new(&TIME, source, Delay, SyncSchedule::DEFAULT);
            match select(synchronizer.run(), UPDATES.wait()).await {
                Either::First(never) => match never {},
                Either::Second(published) => published,
            }
        };
        info!("Time: NTP servers changed");
        servers = resolve(published);
    }
}
