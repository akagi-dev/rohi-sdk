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

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{
    FieldDescriptor, Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusSet,
    StatusWriter, WriteError,
};

#[derive(Service)]
struct WifiService;

#[derive(Service)]
#[service(name = "wifi")]
struct WirelessService;

macro_rules! status {
    ($ty:ty) => {
        impl Status for $ty {
            const VERSION: u16 = 1;
            const STATUS_SCHEMA: &'static [FieldDescriptor] = &[];

            async fn summary(&self) -> ServiceSummary {
                ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
            }

            async fn status(&self, _: &mut impl StatusWriter) -> Result<(), WriteError> {
                Ok(())
            }
        }
    };
}

status!(WifiService);
status!(WirelessService);

// `Statuses::new` fails the build with the same flag after monomorphization.
const _: () = assert!(<(&WifiService, &WirelessService) as StatusSet<NoopRawMutex>>::IDS_UNIQUE);

fn main() {}
