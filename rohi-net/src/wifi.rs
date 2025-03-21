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
//! Wifi access point and client implementation.

use embassy_time::Timer as EmbassyTimer;
use esp_hal::peripherals::{RADIO_CLK, TIMG0, WIFI};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::{Timer, TimerGroup};
use esp_wifi::{
    EspWifiController, init,
    wifi::{
        AccessPointConfiguration, Configuration, WifiApDevice, WifiController, WifiDevice,
        WifiEvent, WifiState, new_with_mode,
    },
};
use heapless::String;
use log::info;

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

/// WiFi hardware configuration.
pub struct Wifi {
    wifi: WIFI,
    radio_clk: RADIO_CLK,
    timer: Timer,
    rng: Rng,
}

impl Wifi {
    pub fn new(wifi: WIFI, timg0: TIMG0, radio_clk: RADIO_CLK, rng: Rng) -> Self {
        let timer = TimerGroup::new(timg0).timer0;
        Self {
            wifi,
            radio_clk,
            timer,
            rng,
        }
    }

    /// Create Access Point from WiFi configuration.
    pub fn into_ap(
        self,
    ) -> Result<
        (WifiDevice<'static, WifiApDevice>, WifiController<'static>),
        esp_wifi::wifi::WifiError,
    > {
        let esp_wifi_ctrl = &*mk_static!(
            EspWifiController<'static>,
            init(self.timer, self.rng, self.radio_clk).unwrap()
        );

        new_with_mode(&esp_wifi_ctrl, self.wifi, WifiApDevice)
    }
}

#[embassy_executor::task]
pub async fn ap_setup_task(mut controller: WifiController<'static>, ssid: String<32>) {
    info!("[Network] > Wifi AP setup task started");
    let config = Configuration::AccessPoint(AccessPointConfiguration {
        ssid,
        ..Default::default()
    });
    loop {
        match esp_wifi::wifi::wifi_state() {
            WifiState::ApStarted => {
                controller.wait_for_event(WifiEvent::ApStop).await;
                EmbassyTimer::after_secs(5).await
            }
            _ => {}
        }
        if !matches!(controller.is_started(), Ok(true)) {
            controller.set_configuration(&config).unwrap();
            controller.start_async().await.unwrap();
            info!("[Network] > Wifi started!");
        }
    }
}
