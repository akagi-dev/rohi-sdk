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
//! The `ble` service: the state of the BLE peripheral.

use heapless::Vec;
use rohi_service::{
    EnumDescriptor, EnumVariant, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity,
    MAX_FIELD_VALUE_LEN, MAX_ISSUES, Service, ServiceHealth, ServiceState, ServiceSummary, Status,
    StatusWriter, Unit, ValueKind, WriteError,
};

use super::{BleError, BleName, BleState};

// The name is a status value, which is never cut.
const _: () = assert!(super::MAX_BLE_NAME_LEN <= MAX_FIELD_VALUE_LEN);

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

/// Issue codes of the `ble` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// The central disconnected.
    pub const DISCONNECTED: u32 = BASE;
    /// A BLE chunk did not fit.
    pub const BUFFER_TOO_SMALL: u32 = BASE + 1;
    /// The BLE transport is busy.
    pub const BUSY: u32 = BASE + 2;
    /// The BLE driver failed.
    pub const DRIVER_ERROR: u32 = BASE + 3;
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

/// Values of the `state` field.
pub mod state {
    /// The peripheral is off.
    pub const DISABLED: u32 = 0;
    /// Waiting for a central.
    pub const ADVERTISING: u32 = 1;
    /// A central is connected.
    pub const CONNECTED: u32 = 2;
    /// The peripheral failed.
    pub const ERROR: u32 = 3;
}

/// The enum of the `state` field.
pub const STATES: &[EnumDescriptor] = &[EnumDescriptor::new(
    1,
    &[
        EnumVariant::new(state::DISABLED, "disabled"),
        EnumVariant::new(state::ADVERTISING, "advertising"),
        EnumVariant::new(state::CONNECTED, "connected"),
        EnumVariant::new(state::ERROR, "error"),
    ],
)];

/// The BLE peripheral as a `ble` service. It has no actions; it is
/// configured through the `ble` section.
#[derive(Service)]
pub struct BleService<P>(pub P);

impl<P: BleProbe> Status for BleService<P> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::STATE, "state", ValueKind::Enum).with_enum(1),
        FieldDescriptor::new(status_field::NAME, "name", ValueKind::String),
        FieldDescriptor::new(status_field::ENCRYPTED, "encrypted", ValueKind::Bool),
        FieldDescriptor::new(
            status_field::AUTHENTICATED,
            "authenticated",
            ValueKind::Bool,
        ),
        FieldDescriptor::new(status_field::MAX_PAYLOAD, "max_payload", ValueKind::U64)
            .unit(Unit::Bytes),
    ];
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::DISCONNECTED, "disconnected"),
        IssueDescriptor::new(issue::BUFFER_TOO_SMALL, "buffer_too_small"),
        IssueDescriptor::new(issue::BUSY, "busy"),
        IssueDescriptor::new(issue::DRIVER_ERROR, "driver_error"),
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
            let code = match error {
                BleError::Disconnected => issue::DISCONNECTED,
                BleError::BufferTooSmall => issue::BUFFER_TOO_SMALL,
                BleError::Busy => issue::BUSY,
                BleError::DriverError => issue::DRIVER_ERROR,
            };
            let _ = out.push(Issue {
                severity: IssueSeverity::Error,
                code,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let state = self.0.state();
        let value = match state {
            BleState::Disabled => state::DISABLED,
            BleState::Advertising => state::ADVERTISING,
            BleState::Connected(_) => state::CONNECTED,
            BleState::Error(_) => state::ERROR,
        };
        out.variant(status_field::STATE, value)?;
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
        let schema = <BleService<&Fake> as Status>::STATUS_SCHEMA;
        assert!(schema.len() <= MAX_FIELDS);
        let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), schema.len());
        for field in schema {
            assert!(field.name.len() <= 24, "{}", field.name);
        }
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
            Some(&Value::Enum(state::ADVERTISING))
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
        assert_eq!(issues[0].code, issue::DRIVER_ERROR);
    }

    #[test]
    fn the_service_identity_is_part_of_the_wire_abi() {
        assert_eq!(<BleService<&Fake> as Service>::NAME, "ble");
        assert_eq!(
            <BleService<&Fake> as Service>::ID,
            rohi_service::service_id("ble")
        );
    }
}
