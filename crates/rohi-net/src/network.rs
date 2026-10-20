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
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::{
    Controller,
    wifi::{
        AccessPointConfig, Interfaces, ModeConfig, WifiApState, WifiController, WifiDevice,
        WifiEvent,
    },
};
use heapless::String;
use log::{info, warn};

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
    wifi_controller: WifiController<'static>,
    wifi_interfaces: Interfaces<'static>,
}

/// WiFi interface configuration.
pub enum WifiConfig {
    /// Access point with given SSID and IP.
    Ap { ssid: String<32>, ip: Ipv4Cidr },
}

impl Network {
    /// New network instance.
    pub fn new(wifi: WIFI<'static>) -> Self {
        let esp_ctrl = &*mk_static!(Controller<'static>, esp_radio::init().unwrap());
        let (wifi_controller, wifi_interfaces) =
            esp_radio::wifi::new(esp_ctrl, wifi, Default::default()).unwrap();
        Self {
            wifi_controller,
            wifi_interfaces,
        }
    }

    /// Spawn background network services like dhcp, wifi, etc.
    pub fn start_wifi(self, config: WifiConfig, spawner: &Spawner) {
        match config {
            WifiConfig::Ap { ssid, ip } => {
                info!(
                    "[Network] > Start WiFi AP with config: SSID({}) IP({})",
                    ssid, ip
                );

                let rng = Rng::new();
                let ip_config = embassy_net::Config::ipv4_static(StaticConfigV4 {
                    address: ip,
                    gateway: None,
                    dns_servers: Default::default(),
                });
                let seed = (rng.random() as u64) << 32 | rng.random() as u64;

                let (stack, runner) = embassy_net::new(
                    self.wifi_interfaces.ap,
                    ip_config,
                    mk_static!(StackResources<5>, StackResources::<5>::new()),
                    seed,
                );

                spawner
                    .spawn(ap_setup_task(self.wifi_controller, ssid))
                    .ok();
                spawner.spawn(ap_network_task(runner)).ok();
                spawner.spawn(dhcp_server_task(stack, ip.address())).ok();
            }
        }
    }
}

#[embassy_executor::task]
pub async fn ap_setup_task(mut controller: WifiController<'static>, ssid: String<32>) {
    info!("[Network] > Wifi AP setup task started");
    let config =
        ModeConfig::AccessPoint(AccessPointConfig::default().with_ssid(ssid.as_str().into()));
    loop {
        if esp_radio::wifi::ap_state() == WifiApState::Started {
            controller.wait_for_event(WifiEvent::ApStop).await;
            Timer::after_secs(5).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            controller.set_config(&config).unwrap();
            controller.start_async().await.unwrap();
            info!("[Network] > Wifi started!");
        }
    }
}

#[embassy_executor::task]
pub async fn ap_network_task(mut runner: Runner<'static, WifiDevice<'static>>) {
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
