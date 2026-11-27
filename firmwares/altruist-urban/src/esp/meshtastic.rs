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
///////////////////////////////////////////////////////////////////////////////
//! The Meshtastic radio on the ESP: UART0 of the board and the session task.
//!
//! The board HAL sets the UART up (pins, 8N1, no flow control); this module wraps it for the
//! session of `rohi-meshtastic`, which is the only reader and writer of the radio. A UART that
//! could not be set up is reported by the `meshtastic` service as `uart_unavailable`; sensors and
//! the control plane do not depend on it.

use embassy_executor::Spawner;
use embassy_time::Delay;
use esp_hal::Async;
use esp_hal::uart::{Config, Uart};
use log::{error, info};
use rohi_sdk::meshtastic::uart::{BaudError, SetBaudRate, UartSession};
use rohi_sdk::time::EmbassyClock;

use crate::meshtastic::link;

/// The UART of the radio for the session: reads, writes and changes the baud rate.
pub struct MeshtasticUart(Uart<'static, Async>);

impl embedded_io_async::ErrorType for MeshtasticUart {
    type Error = <Uart<'static, Async> as embedded_io_async::ErrorType>::Error;
}

impl embedded_io_async::Read for MeshtasticUart {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Read::read(&mut self.0, buf).await
    }
}

impl embedded_io_async::Write for MeshtasticUart {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Write::write(&mut self.0, buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io_async::Write::flush(&mut self.0).await
    }
}

impl SetBaudRate for MeshtasticUart {
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), BaudError> {
        self.0
            .apply_config(&Config::default().with_baudrate(baud))
            .map_err(|_| BaudError)
    }
}

/// Starts the session task on `uart`, or reports that there is no UART.
pub fn start(spawner: Spawner, uart: Option<Uart<'static, Async>>) {
    match uart {
        Some(uart) => {
            let mut seed = [0u8; 4];
            super::connectivity::esp_entropy(&mut seed);
            let seed = u32::from_le_bytes(seed);
            // Packet IDs of this run do not repeat those of the previous one.
            link().seed_ids(seed);
            crate::tasks::spawn(
                spawner,
                "meshtastic_task",
                meshtastic_task(MeshtasticUart(uart), seed),
            );
            info!(target: "meshtastic", "Radio session started");
        }
        None => {
            link().set_uart_failed(true);
            error!(target: "meshtastic", "No UART for the radio");
        }
    }
}

/// The only owner of the radio: handshake, receive, send and recover.
#[embassy_executor::task]
pub async fn meshtastic_task(uart: MeshtasticUart, nonce_seed: u32) {
    UartSession::new(link(), uart, Delay, EmbassyClock, nonce_seed)
        .run()
        .await
}
