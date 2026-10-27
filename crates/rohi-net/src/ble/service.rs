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
//! The `ble` service: the state of the BLE peripheral.

use heapless::Vec;
use rohi_service::{
    FieldDescriptor, Issue, IssueSeverity, MAX_ISSUES, ManagedService, ServiceHealth, ServiceState,
    ServiceSummary, StatusWriter, ValueKind, WriteError,
};

use super::{BleError, BleName, BleState};

/// What the [`BleService`] reads from the platform. The transport itself is
/// owned by the control link, so the platform hands out a separate view of
/// the same peripheral.
pub trait BleProbe {
    /// Current state of the peripheral.
    fn state(&self) -> BleState;

    /// Largest chunk the current link carries.
    fn max_payload(&self) -> usize;

    /// The name the peripheral advertises, once it is known.
    fn name(&self) -> Option<BleName>;
}

/// Field ids of the `ble` service status. Only ever added.
pub mod status_field {
    /// `disabled`, `advertising`, `connected` or `error`.
    pub const STATE: u32 = 1;
    /// Advertised name.
    pub const NAME: u32 = 2;
    /// The link is encrypted.
    pub const ENCRYPTED: u32 = 3;
    /// The link is encrypted with an authenticated key.
    pub const AUTHENTICATED: u32 = 4;
    /// Largest chunk of the link.
    pub const MAX_PAYLOAD: u32 = 5;
}

/// Variants of the `state` field.
pub const STATES: &[&str] = &["disabled", "advertising", "connected", "error"];

/// The BLE peripheral as a `ble` service. It has no actions; it is
/// configured through the `ble` section.
pub struct BleService<P>(pub P);

impl<P: BleProbe> ManagedService for BleService<P> {
    const NAME: &'static str = "ble";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::STATE, "state", ValueKind::Enum)
            .describe("State of the peripheral")
            .variants(STATES),
        FieldDescriptor::new(status_field::NAME, "name", ValueKind::String)
            .describe("Advertised name"),
        FieldDescriptor::new(status_field::ENCRYPTED, "encrypted", ValueKind::Bool)
            .describe("The link is encrypted"),
        FieldDescriptor::new(
            status_field::AUTHENTICATED,
            "authenticated",
            ValueKind::Bool,
        )
        .describe("The link key is authenticated"),
        FieldDescriptor::new(status_field::MAX_PAYLOAD, "max_payload", ValueKind::U64)
            .describe("Largest chunk of the link")
            .unit("B"),
    ];

    async fn summary(&self) -> ServiceSummary {
        match self.0.state() {
            BleState::Disabled => {
                ServiceSummary::new(ServiceState::Disabled, ServiceHealth::Unknown, false)
            }
            BleState::Advertising | BleState::Connected(_) => {
                ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
            }
            BleState::Error(_) => {
                ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false)
            }
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if let BleState::Error(error) = self.0.state() {
            let (code, message) = match error {
                BleError::Disconnected => ("disconnected", "The central disconnected"),
                BleError::BufferTooSmall => ("buffer_too_small", "A BLE chunk did not fit"),
                BleError::Busy => ("busy", "The BLE transport is busy"),
                BleError::DriverError => ("driver_error", "The BLE driver failed"),
            };
            let _ = out.push(Issue {
                severity: IssueSeverity::Error,
                code,
                message,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let state = self.0.state();
        let name = match state {
            BleState::Disabled => "disabled",
            BleState::Advertising => "advertising",
            BleState::Connected(_) => "connected",
            BleState::Error(_) => "error",
        };
        out.variant(status_field::STATE, name)?;
        if let Some(name) = self.0.name() {
            out.text(status_field::NAME, name.as_str())?;
        }
        if let BleState::Connected(info) = state {
            out.bool(status_field::ENCRYPTED, info.encrypted)?;
            out.bool(status_field::AUTHENTICATED, info.authenticated)?;
            out.u64(status_field::MAX_PAYLOAD, self.0.max_payload() as u64)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::Cell;

    use embassy_futures::block_on;
    use rohi_service::{MAX_FIELDS, Value};

    use super::*;
    use crate::BleConnectionInfo;

    struct Fake {
        state: Cell<BleState>,
    }

    impl BleProbe for &Fake {
        fn state(&self) -> BleState {
            self.state.get()
        }

        fn max_payload(&self) -> usize {
            185
        }

        fn name(&self) -> Option<BleName> {
            BleName::new("urban-A1B2")
        }
    }

    #[derive(Default)]
    struct Collect(std::vec::Vec<(u32, Value)>);

    impl StatusWriter for Collect {
        fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
            self.0.push((id, value));
            Ok(())
        }
    }

    impl Collect {
        fn get(&self, id: u32) -> Option<&Value> {
            self.0
                .iter()
                .find(|(field, _)| *field == id)
                .map(|(_, v)| v)
        }
    }

    #[test]
    fn the_schema_is_well_formed() {
        let schema = <BleService<&Fake> as ManagedService>::STATUS_SCHEMA;
        assert!(schema.len() <= MAX_FIELDS);
        let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        for field in schema {
            assert!(field.name.len() <= 24, "{}", field.name);
            assert!(field.description.len() <= 40, "{}", field.name);
        }
        assert!(<BleService<&Fake> as ManagedService>::ACTIONS.is_empty());
    }

    #[test]
    fn the_state_is_mapped_to_the_summary_and_the_status() {
        let fake = Fake {
            state: Cell::new(BleState::Disabled),
        };
        let service = BleService(&fake);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Disabled);
        assert!(!summary.ready);

        fake.state.set(BleState::Advertising);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Running);
        assert!(summary.ready);
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(status_field::STATE),
            Value::variant("advertising").as_ref()
        );
        assert_eq!(
            out.get(status_field::NAME),
            Value::text("urban-A1B2").as_ref()
        );
        assert!(out.get(status_field::ENCRYPTED).is_none());

        fake.state.set(BleState::Connected(BleConnectionInfo {
            encrypted: true,
            authenticated: false,
        }));
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.get(status_field::ENCRYPTED), Some(&Value::Bool(true)));
        assert_eq!(
            out.get(status_field::AUTHENTICATED),
            Some(&Value::Bool(false))
        );
        assert_eq!(out.get(status_field::MAX_PAYLOAD), Some(&Value::U64(185)));
    }

    #[test]
    fn a_failed_stack_is_an_issue() {
        let fake = Fake {
            state: Cell::new(BleState::Error(BleError::DriverError)),
        };
        let service = BleService(&fake);
        assert_eq!(block_on(service.summary()).state, ServiceState::Failed);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "driver_error");
    }
}
