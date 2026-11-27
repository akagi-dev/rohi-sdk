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
//! The local Meshtastic radio on the board UART: the shared link, the `meshtastic` service and
//! the binding of its configuration.
//!
//! Compiled with the `meshtastic-uart` feature. The portable part (this module) is the state
//! and the policy glue; on ESP [`crate::esp::meshtastic`] owns the UART and runs the session.
//! The link is built on first use so that it stays in `.bss` (it holds non-zero initial values).

use rohi_sdk::connectivity::Mux;
use rohi_sdk::connectivity_http::HttpPolicy;
use rohi_sdk::connectivity_meshtastic::MeshtasticPolicy;
use rohi_sdk::meshtastic::service::{MeshtasticConfig, MeshtasticService};
use rohi_sdk::meshtastic::{LinkRadio, MeshtasticLink};
use rohi_sdk::time::EmbassyClock;

use crate::bindings::{Binding, ConfigRuntime};
use crate::tasks::{OnDemand, RawMutex};

/// The link of the radio: its state, queues and diagnostic inbox.
pub type Link = MeshtasticLink<RawMutex>;

/// The `meshtastic` service of the device.
pub type UrbanMeshtastic = MeshtasticService<'static, RawMutex, EmbassyClock>;

/// The endpoint schemes this build delivers: HTTP and Meshtastic over the UART.
pub type Policy = Mux<HttpPolicy, MeshtasticPolicy>;

/// See [`Policy`].
pub const POLICY: Policy = Mux(HttpPolicy, MeshtasticPolicy::UART);

static LINK: OnDemand<Link> = OnDemand::new();

/// The link of the radio.
pub fn link() -> &'static Link {
    LINK.get(Link::new)
}

/// The transport's view of the radio.
pub fn radio() -> LinkRadio<'static, RawMutex> {
    LinkRadio::new(link())
}

/// The `meshtastic` service over [`link`].
pub fn service() -> UrbanMeshtastic {
    MeshtasticService::new(link(), EmbassyClock)
}

/// `meshtastic`: the baud rate applies by starting a new session with it, no restart.
pub struct MeshtasticBinding;

impl Binding for MeshtasticBinding {
    type Config = MeshtasticConfig;

    fn publish(_: &ConfigRuntime, effective: &MeshtasticConfig) {
        link().set_baud(effective.baud_rate.bps());
    }
}

#[cfg(test)]
mod tests {
    use rohi_sdk::connectivity::{EndpointError, EndpointPolicy};
    use rohi_sdk::meshtastic::service::BaudRate;

    use super::*;

    #[test]
    fn the_build_delivers_http_and_meshtastic_over_the_uart_only() {
        assert!(POLICY.validate("http://ingress.example").is_ok());
        assert!(POLICY.validate("meshtastic+uart://!deadbeef").is_ok());
        assert_eq!(
            POLICY.validate("meshtastic+ble://!deadbeef"),
            Err(EndpointError::UnsupportedScheme)
        );
        assert_eq!(
            POLICY.validate("meshtastic+uart://!deadbeef?device=1"),
            Err(EndpointError::Invalid)
        );
    }

    #[test]
    fn a_committed_baud_rate_reaches_the_link_and_asks_for_a_new_session() {
        let config = MeshtasticConfig {
            baud_rate: BaudRate::B57600,
        };
        let runtime = crate::bindings::ConfigRuntime::new(crate::net::handle(), |_| {});
        MeshtasticBinding::publish(&runtime, &config);
        assert_eq!(link().baud(), 57_600);
        assert!(link().reconnect_pending());
    }

    #[test]
    fn the_service_has_the_documented_identity() {
        use rohi_sdk::service::Service;
        assert_eq!(<UrbanMeshtastic as Service>::NAME, "meshtastic");
    }
}
