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

//! NTP synchronization on the ESP: the portable task of [`crate::time`] over
//! the ESP IP stack.

use embassy_executor::Spawner;
use rohi_sdk::net::NetworkHandle;

use super::resources::Ip;
use crate::time;

/// Starts the NTP synchronization of the time service, see [`time::run`].
pub fn start(spawner: Spawner, network: NetworkHandle<'static, Ip>) {
    spawner.spawn(ntp_task(network).unwrap());
}

/// Keeps the time service synchronized over NTP.
///
/// Uses one UDP socket per exchange, see [`super::resources::UDP_SOCKETS`].
#[embassy_executor::task]
pub async fn ntp_task(network: NetworkHandle<'static, Ip>) {
    time::run(network).await
}
