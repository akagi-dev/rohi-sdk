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
//! Embassy time driver for ESP32-C3.
//!
//! Replaces an RTOS-provided time base: `embassy-time` ticks at 1 MHz from
//! [`esp_hal::time::Instant`] and timers are woken by one SYSTIMER alarm
//! interrupt. Call [`init`] once, before any timer is used.

use core::cell::RefCell;
use core::task::Waker;

use critical_section::Mutex;
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;
use esp_hal::Blocking;
use esp_hal::interrupt::{InterruptHandler, Priority};
use esp_hal::peripherals::SYSTIMER;
use esp_hal::time::{Duration, Instant};
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::{Error as TimerError, OneShotTimer};

/// The SYSTIMER counter is 52 bits wide.
const MAX_TIMEOUT_US: u64 = (1 << 52) - 1;

struct State {
    queue: Queue,
    timer: Option<OneShotTimer<'static, Blocking>>,
}

struct EspTimeDriver {
    state: Mutex<RefCell<State>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: EspTimeDriver = EspTimeDriver {
    state: Mutex::new(RefCell::new(State {
        queue: Queue::new(),
        timer: None,
    })),
});

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

impl State {
    /// Arm the alarm for `at`. Returns `false` if `at` is already reached.
    fn arm(&mut self, at: u64) -> bool {
        let Some(timer) = self.timer.as_mut() else {
            // Not initialized yet: `init` arms the alarm for queued timers.
            return true;
        };
        if at == u64::MAX {
            timer.stop();
            return true;
        }
        let now = now_us();
        if at <= now {
            return false;
        }

        let mut timeout = (at - now).min(MAX_TIMEOUT_US);
        loop {
            match timer.schedule(Duration::from_micros(timeout)) {
                Ok(()) => break,
                // Waking up early is harmless: the handler re-arms the alarm.
                Err(TimerError::InvalidTimeout) if timeout != 0 => timeout /= 2,
                Err(e) => panic!("failed to schedule the time driver alarm: {:?}", e),
            }
        }
        // The deadline may have passed while arming; the caller re-evaluates.
        now_us() < at
    }

    /// Wake expired timers and arm the alarm for the next one.
    fn reschedule(&mut self) {
        loop {
            let next = self.queue.next_expiration(now_us());
            if self.arm(next) {
                break;
            }
        }
    }
}

impl Driver for EspTimeDriver {
    fn now(&self) -> u64 {
        now_us()
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            let mut state = self.state.borrow_ref_mut(cs);
            if state.queue.schedule_wake(at, waker) {
                state.reschedule();
            }
        });
    }
}

#[esp_hal::ram]
extern "C" fn alarm_interrupt() {
    critical_section::with(|cs| {
        let mut state = DRIVER.state.borrow_ref_mut(cs);
        if let Some(timer) = state.timer.as_mut() {
            timer.clear_interrupt();
        }
        state.reschedule();
    });
}

/// Start the time driver on SYSTIMER alarm 0.
pub fn init(systimer: SYSTIMER<'static>) {
    let mut timer = OneShotTimer::new(SystemTimer::new(systimer).alarm0);
    timer.set_interrupt_handler(InterruptHandler::new(alarm_interrupt, Priority::Priority1));
    timer.listen();

    critical_section::with(|cs| {
        let mut state = DRIVER.state.borrow_ref_mut(cs);
        state.timer = Some(timer);
        state.reschedule();
    });
}
