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

//! The derive macros resolve the runtime crates through `rohi-sdk` when a
//! consumer depends on the umbrella crate only.
#![cfg(feature = "service")]

use minicbor::{CborLen, Decode, Encode};
use rohi_sdk::config::{ConfigSection, SectionId};
use rohi_sdk::service::Service;

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
struct Settings {
    #[n(0)]
    value: u32,
}

#[derive(Service)]
struct DemoService;

#[test]
fn derives_resolve_through_the_umbrella() {
    assert_eq!(Settings::ID, SectionId::from_name("Settings"));
    assert_eq!(Settings::VERSION, 1);
    assert_eq!(DemoService::NAME, "demo");
}

#[cfg(feature = "system")]
#[test]
fn the_system_service_is_reexported() {
    use rohi_sdk::service::Action;
    use rohi_sdk::system::{SystemPlatform, SystemService};

    fn actions<P: SystemPlatform>() -> usize {
        <SystemService<P> as Action>::ACTIONS.len()
    }
    struct Unsupported;
    impl SystemPlatform for Unsupported {
        fn schedule_reboot(&self) -> Result<(), rohi_sdk::system::SystemError> {
            Err(rohi_sdk::system::SystemError::Unavailable)
        }

        async fn factory_reset(
            &self,
        ) -> Result<rohi_sdk::service::FactoryReset, rohi_sdk::system::SystemError> {
            Err(rohi_sdk::system::SystemError::Unavailable)
        }
    }
    assert_eq!(actions::<Unsupported>(), 2);
}
