///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <admin@akagi.dev>
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
//! Embedded networking for Robonomics Open Hardware.

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use edge_dhcp::{
    io::{self, DEFAULT_SERVER_PORT},
    server::{Server, ServerOptions},
};
use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_executor::Spawner;
use embassy_net::{Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_time::Timer;
use esp_hal::rng::Rng;
use esp_wifi::wifi::{WifiApDevice, WifiController, WifiDevice, WifiError};
use heapless::String;
use log::{info, warn};

use crate::wifi::{self, Wifi};

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

/// General network service interface.
pub struct Network {
    rng: Rng,
    wifi_ap: Option<WifiApConfig>,
}

struct WifiApConfig {
    runner: Runner<'static, WifiDevice<'static, WifiApDevice>>,
    controller: WifiController<'static>,
    stack: Stack<'static>,
    ssid: String<32>,
    ip: Ipv4Addr,
}

impl Network {
    pub fn new(rng: Rng) -> Self {
        Self { rng, wifi_ap: None }
    }

    /// Create WiFi AP with DHCPv4 server on given IP
    pub fn with_wifi_ap(
        mut self,
        wifi: Wifi,
        ssid: String<32>,
        address: Ipv4Cidr,
    ) -> Result<Self, WifiError> {
        let (device, controller) = wifi.into_ap()?;

        let ip = address.address();
        let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
            address,
            gateway: None,
            dns_servers: Default::default(),
        });
        let seed = (self.rng.random() as u64) << 32 | self.rng.random() as u64;

        let (stack, runner) = embassy_net::new(
            device,
            config,
            mk_static!(StackResources<5>, StackResources::<5>::new()),
            seed,
        );

        self.wifi_ap = Some(WifiApConfig {
            runner,
            controller,
            stack,
            ssid,
            ip,
        });
        Ok(self)
    }

    /// Launch background network services like dhcp, wifi, etc.
    pub fn start(self, spawner: &Spawner) {
        if let Some(WifiApConfig {
            runner,
            controller,
            stack,
            ssid,
            ip,
        }) = self.wifi_ap
        {
            info!(
                "[Network] > Start WiFi AP with config: SSID({}) IP({})",
                ssid, ip
            );
            spawner.spawn(wifi::ap_setup_task(controller, ssid)).ok();
            spawner.spawn(ap_network_task(runner)).ok();
            spawner.spawn(dhcp_server_task(stack, ip)).ok();
        }
    }
}

#[embassy_executor::task]
pub async fn ap_network_task(mut runner: Runner<'static, WifiDevice<'static, WifiApDevice>>) {
    info!("[Network] > Wifi AP network task started");
    runner.run().await
}

#[embassy_executor::task]
pub async fn dhcp_server_task(stack: Stack<'static>, ip: Ipv4Addr) {
    info!("[Network] > DHCP server task started");
    let mut buf = [0u8; 1500];
    let mut gw_buf = [Ipv4Addr::UNSPECIFIED];
    let buffers = UdpBuffers::<3, 1024, 1024, 10>::new();
    let unbound_socket = Udp::new(stack, &buffers);
    let mut bound_socket = unbound_socket
        .bind(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            DEFAULT_SERVER_PORT,
        )))
        .await
        .unwrap();

    loop {
        _ = io::server::run(
            &mut Server::<_, 64>::new_with_et(ip),
            &ServerOptions::new(ip, Some(&mut gw_buf)),
            &mut bound_socket,
            &mut buf,
        )
        .await
        .inspect_err(|_| warn!("[Network] > DHCP server error"));
        Timer::after_secs(3).await;
    }
}
