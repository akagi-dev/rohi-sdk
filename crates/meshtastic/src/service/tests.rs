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
use core::cell::Cell;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_config::{ConfigSection, Override};
use rohi_service::{
    ActionRegistry, ActionResult, Config, ServiceHealth, StatusRegistry, Statuses, Value,
};
use rohi_time::MonotonicTimestamp;

use super::*;
use crate::radio::RoutingResult;

type Link = MeshtasticLink<NoopRawMutex>;

struct Time<'a>(&'a Cell<u64>);

impl rohi_time::Clock for Time<'_> {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.get())
    }
}

const SERVICE_ID: u32 = rohi_service::service_id("meshtastic");

fn text(value: &str) -> Value {
    Value::text(value).unwrap()
}

fn send(
    service: &MeshtasticService<'_, NoopRawMutex, Time<'_>>,
    to: &str,
    body: &str,
) -> Result<ActionResult, ActionError> {
    let actions = (service,);
    block_on(actions.invoke(
        SERVICE_ID,
        action::SEND,
        &[
            (action_field::TO, text(to)),
            (action_field::TEXT, text(body)),
        ],
    ))
}

fn status_of(
    service: &MeshtasticService<'_, NoopRawMutex, Time<'_>>,
) -> rohi_service::ServiceStatus {
    let registry = Statuses::<NoopRawMutex, _>::new((service,));
    block_on(registry.status(SERVICE_ID, 0)).unwrap()
}

fn field(status: &rohi_service::ServiceStatus, id: u32) -> Option<Value> {
    status
        .fields
        .iter()
        .find(|(field, _)| *field == id)
        .map(|(_, value)| value.clone())
}

// ---- config ---------------------------------------------------------------------------------------

#[test]
fn the_config_is_one_baud_rate_with_a_115200_default() {
    assert_eq!(<MeshtasticConfig as Config>::NAME, "meshtastic");
    let fields = <MeshtasticConfig as Config>::FIELDS;
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].field.name, "baud_rate");
    assert_eq!(MeshtasticConfig::default().baud_rate.bps(), 115_200);
    let rates: std::vec::Vec<u32> = [
        BaudRate::B9600,
        BaudRate::B19200,
        BaudRate::B38400,
        BaudRate::B57600,
        BaudRate::B115200,
        BaudRate::B230400,
    ]
    .iter()
    .map(|rate| rate.bps())
    .collect();
    assert_eq!(rates, [9_600, 19_200, 38_400, 57_600, 115_200, 230_400]);
}

#[test]
fn only_the_listed_rates_exist_and_the_variant_numbers_are_stable() {
    for variant in 0..6 {
        let rate = BaudRate::from_variant(variant).unwrap();
        assert_eq!(rate.variant(), variant);
    }
    assert_eq!(BaudRate::from_variant(6), None);
    assert_eq!(BaudRate::from_variant(u32::MAX), None);
    assert_eq!(CONFIG_ENUMS[0].variants.len(), 6);
}

#[test]
fn the_stored_overrides_are_stable() {
    // The name of the section type is part of the persistent ABI.
    assert_eq!(
        MeshtasticOverrides::ID,
        rohi_config::SectionId::from_name("MeshtasticConfig")
    );
    assert_eq!(<MeshtasticOverrides as ConfigSection>::VERSION, 1);
    let overrides = MeshtasticOverrides {
        baud_rate: Override::Value(BaudRate::B57600),
    };
    let mut buffer = [0u8; 16];
    let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
    // map(1): key 1, the variant number as an unsigned integer.
    assert_eq!(&buffer[..used], [0xa1, 0x01, 0x03]);
    let decoded: MeshtasticOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
    assert_eq!(
        rohi_config::materialize::<MeshtasticConfig>(&Default::default(), &decoded).baud_rate,
        BaudRate::B57600
    );
    // Nothing overridden: nothing stored, the default applies.
    let open = MeshtasticOverrides::default();
    let used = rohi_config::encode_to_slice(&open, &mut buffer).unwrap();
    assert_eq!(&buffer[..used], [0xa0]);
    // An unknown variant does not decode into a rate.
    assert!(rohi_config::decode_exact::<MeshtasticOverrides>(&[0xa1, 0x01, 0x09]).is_err());
}

// ---- status ----------------------------------------------------------------------------------------

#[test]
fn the_status_schema_has_the_eleven_stable_fields_and_five_issues() {
    type S<'a> = MeshtasticService<'a, NoopRawMutex, Time<'a>>;
    let schema = <S as Status>::STATUS_SCHEMA;
    let names: std::vec::Vec<(u32, &str)> = schema.iter().map(|f| (f.id, f.name)).collect();
    assert_eq!(
        names,
        [
            (1, "node_id"),
            (2, "firmware_version"),
            (3, "hardware_model"),
            (4, "last_rx_age_s"),
            (5, "tx_frames"),
            (6, "rx_frames"),
            (7, "framing_errors"),
            (8, "decode_errors"),
            (9, "reconnects"),
            (10, "last_error"),
            (11, "session_age_s"),
        ]
    );
    assert_eq!(<S as Status>::ISSUES.len(), 5);
    assert_eq!(<S as Status>::ENUMS[0].variants.len(), LastError::ALL.len());
    for (variant, error) in <S as Status>::ENUMS[0].variants.iter().zip(LastError::ALL) {
        assert_eq!((variant.value, variant.name), (error.code(), error.name()));
    }
    const { assert!(!<S as Status>::ESSENTIAL) };
    assert_eq!(<S as Service>::NAME, "meshtastic");
}

#[test]
fn a_service_that_has_not_synchronized_is_unknown_and_omits_what_it_does_not_know() {
    let now = Cell::new(10_000);
    let link = Link::new();
    let service = MeshtasticService::new(&link, Time(&now));
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Unknown);
    assert!(status.issues.is_empty());
    for omitted in [1, 2, 3, 4, 11] {
        assert_eq!(field(&status, omitted), None, "field {omitted}");
    }
    for counter in [5, 6, 7, 8, 9] {
        assert_eq!(field(&status, counter), Some(Value::U64(0)));
    }
    assert_eq!(field(&status, 10), Some(Value::Enum(0)));
}

#[test]
fn an_established_session_is_ok_and_reports_the_radio() {
    let now = Cell::new(10_000);
    let link = Link::new();
    link.session_begin();
    link.set_node(0xdead_beef);
    link.set_metadata("2.5.9.abcdef", 43);
    link.session_up(10_000);
    link.note_rx(10_000);
    link.note_tx();
    now.set(17_400);
    let service = MeshtasticService::new(&link, Time(&now));
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Ok);
    assert!(status.issues.is_empty());
    assert_eq!(field(&status, 1), Some(Value::text("!deadbeef").unwrap()));
    assert_eq!(
        field(&status, 2),
        Some(Value::text("2.5.9.abcdef").unwrap())
    );
    assert_eq!(field(&status, 3), Some(Value::text("heltec_v3").unwrap()));
    assert_eq!(field(&status, 4), Some(Value::U64(7)));
    assert_eq!(field(&status, 11), Some(Value::U64(7)));
    assert_eq!(
        (field(&status, 5), field(&status, 6)),
        (Some(Value::U64(1)), Some(Value::U64(1)))
    );
}

#[test]
fn an_unknown_hardware_model_is_a_number_not_a_guess() {
    let now = Cell::new(0);
    let link = Link::new();
    link.set_metadata("2.5.9", 9999);
    let service = MeshtasticService::new(&link, Time(&now));
    assert_eq!(
        field(&status_of(&service), 3),
        Some(Value::text("model_9999").unwrap())
    );
}

#[test]
fn the_first_failed_handshake_is_already_a_recovery_with_its_issue() {
    let now = Cell::new(0);
    let link = Link::new();
    let service = MeshtasticService::new(&link, Time(&now));
    link.session_begin();
    link.session_down(Some(LastError::HandshakeTimeout), Some(false));
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Degraded);
    let codes: std::vec::Vec<u32> = status.issues.iter().map(|issue| issue.code).collect();
    assert_eq!(
        codes,
        [issue::RADIO_RECONNECTING],
        "degraded always says why"
    );
}

#[test]
fn health_and_issues_follow_the_policy() {
    let now = Cell::new(0);
    let link = Link::new();
    let service = MeshtasticService::new(&link, Time(&now));
    let codes = |status: &rohi_service::ServiceStatus| -> std::vec::Vec<u32> {
        status.issues.iter().map(|issue| issue.code).collect()
    };

    // The UART cannot be set up.
    link.set_uart_failed(true);
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Error);
    assert_eq!(codes(&status), [issue::UART_UNAVAILABLE]);
    link.set_uart_failed(false);

    // A first failed handshake is recovery, not yet an error.
    link.session_begin();
    link.session_down(Some(LastError::HandshakeTimeout), Some(false));
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Degraded);
    assert_eq!(codes(&status), [issue::RADIO_RECONNECTING]);
    assert_eq!(
        field(&status, 10),
        Some(Value::Enum(LastError::HandshakeTimeout.code()))
    );

    // Repeated failures: the radio does not answer.
    for _ in 0..2 {
        link.session_begin();
        link.session_down(Some(LastError::HandshakeTimeout), Some(false));
    }
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Error);
    assert_eq!(
        codes(&status),
        [issue::RADIO_UNRESPONSIVE, issue::RADIO_RECONNECTING]
    );

    // A radio that talks, but not the PhoneAPI.
    link.session_begin();
    link.session_down(Some(LastError::HandshakeTimeout), Some(true));
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Error);
    assert_eq!(codes(&status)[0], issue::RADIO_NOT_CONFIGURED);
    assert!(!codes(&status).contains(&issue::RADIO_UNRESPONSIVE));

    // Up again: everything clears, a stream of bad frames only degrades.
    link.session_begin();
    link.session_up(0);
    assert_eq!(status_of(&service).summary.health, ServiceHealth::Ok);
    for _ in 0..PROTOCOL_ERROR_STREAK {
        link.note_framing_error();
    }
    let status = status_of(&service);
    assert_eq!(status.summary.health, ServiceHealth::Degraded);
    assert_eq!(codes(&status), [issue::PROTOCOL_ERROR]);
    link.note_rx(1);
    assert_eq!(status_of(&service).summary.health, ServiceHealth::Ok);
}

// ---- actions ---------------------------------------------------------------------------------------

#[test]
fn the_actions_are_exactly_send_inbox_and_reconnect_with_their_access() {
    type S<'a> = MeshtasticService<'a, NoopRawMutex, Time<'a>>;
    let actions: std::vec::Vec<_> = <S as Action>::ACTIONS
        .iter()
        .map(|a| (a.name, a.access, a.format))
        .collect();
    assert_eq!(
        actions,
        [
            ("send", ActionAccess::Mutate, ResultFormat::Inline),
            ("inbox", ActionAccess::Read, ResultFormat::Records),
            ("reconnect", ActionAccess::Mutate, ResultFormat::Empty),
        ]
    );
}

fn connected_link() -> Link {
    let link = Link::new();
    link.session_begin();
    link.session_up(0);
    link
}

#[test]
fn send_submits_a_unicast_text_with_an_ack_request_and_returns_the_local_packet_id() {
    let now = Cell::new(0);
    let link = connected_link();
    let service = MeshtasticService::new(&link, Time(&now));
    let result = send(&service, "!DEADbeef", "Hello ROHI").unwrap();
    let ActionResult::Inline(fields) = result else {
        panic!("{result:?}")
    };
    let Some((action_field::SEND_PACKET_ID, Value::U64(id))) = fields.first().cloned() else {
        panic!("{fields:?}")
    };
    // What went to the radio: the encoded packet with this id.
    let message = block_on(link.next_outgoing());
    let mut expected = [0u8; 128];
    let len = crate::proto::encode_packet(
        &mut expected,
        &Outgoing {
            destination: 0xdead_beef,
            id: id as u32,
            port: 1,
            payload: b"Hello ROHI",
            want_ack: true,
            pki: false,
        },
    )
    .unwrap();
    assert_eq!(message.as_slice(), &expected[..len]);
    // The packet takes its id from the counter shared with the transport.
    assert_eq!(link.reserve_ids(1), id as u32 + 1);
    // A success is local submission: nothing is in the inbox until the ack arrives.
    assert!(link.inbox().is_empty());
    link.on_routing(
        0xdead_beef,
        RoutingResult {
            request_id: id as u32,
            error: 0,
        },
    );
    assert_eq!(link.inbox()[0].packet_id, id as u32);
}

#[test]
fn send_refuses_bad_input_without_transmitting_and_an_unready_radio_is_unavailable() {
    let now = Cell::new(0);
    let link = connected_link();
    let service = MeshtasticService::new(&link, Time(&now));
    let long = "x".repeat(INBOX_TEXT_MAX);
    assert!(send(&service, "!deadbeef", &long).is_ok(), "64 bytes fit");
    let _ = block_on(link.next_outgoing());
    for to in [
        "deadbeef",
        "!deadbee",
        "!deadbeeg",
        "!deadbeef0",
        "!ffffffff",
        "",
    ] {
        assert_eq!(
            send(&service, to, "hi"),
            Err(ActionError::Rejected("invalid_destination")),
            "{to:?}"
        );
    }
    assert_eq!(
        send(&service, "!deadbeef", ""),
        Err(ActionError::Rejected("invalid_text"))
    );
    // Nothing was queued by the refusals.
    assert!(!link.has_outgoing());
    assert!(link.inbox().is_empty());

    link.session_down(None, None);
    assert_eq!(
        send(&service, "!deadbeef", "hi"),
        Err(ActionError::Unavailable)
    );
    assert!(!link.has_outgoing());
}

#[test]
fn the_text_limit_is_64_encoded_bytes_not_characters() {
    // The control plane cannot carry a text value above 64 bytes, so a longer text never reaches
    // the action; 32 two-byte characters are exactly 64 bytes and are accepted.
    assert!(Value::text(&"é".repeat(33)).is_none());
    let now = Cell::new(0);
    let link = connected_link();
    let service = MeshtasticService::new(&link, Time(&now));
    assert!(send(&service, "!deadbeef", &"é".repeat(32)).is_ok());
}

#[test]
fn the_inbox_lists_newest_first_without_consuming_and_an_empty_one_is_an_empty_collection() {
    let now = Cell::new(0);
    let link = connected_link();
    let service = MeshtasticService::new(&link, Time(&now));
    let actions = (&service,);
    let empty = block_on(actions.invoke(SERVICE_ID, action::INBOX, &[])).unwrap();
    assert!(empty.records().is_empty());

    link.on_text(0xaa, 1, b"first");
    link.note_diagnostic_send(2);
    link.on_routing(
        0xaa,
        RoutingResult {
            request_id: 2,
            error: 0,
        },
    );
    let read = || block_on(actions.invoke(SERVICE_ID, action::INBOX, &[])).unwrap();
    let result = read();
    let records = result.records();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0],
        [
            (action_field::KIND, Value::Enum(1)),
            (action_field::FROM, Value::text("!000000aa").unwrap()),
            (action_field::PACKET_ID, Value::U64(2)),
            (action_field::RECORD_TEXT, Value::text("ok").unwrap()),
        ]
    );
    assert_eq!(records[1][0], (action_field::KIND, Value::Enum(0)));
    assert_eq!(
        records[1][3],
        (action_field::RECORD_TEXT, Value::text("first").unwrap())
    );
    // Reading again returns the same: the inbox is not cleared.
    assert_eq!(read().records().len(), 2);
}

#[test]
fn a_full_inbox_returns_six_records_within_the_page_limit() {
    let now = Cell::new(0);
    let link = connected_link();
    let service = MeshtasticService::new(&link, Time(&now));
    for id in 1..=9 {
        link.on_text(1, id, b"x");
    }
    let actions = (&service,);
    let result = block_on(actions.invoke(SERVICE_ID, action::INBOX, &[])).unwrap();
    let ids: std::vec::Vec<_> = result
        .records()
        .iter()
        .map(|record| record[2].1.clone())
        .collect();
    assert_eq!(
        ids,
        (4..=9).rev().map(Value::U64).collect::<std::vec::Vec<_>>()
    );
}

#[test]
fn reconnect_requests_a_new_session_and_returns_nothing() {
    let now = Cell::new(0);
    let link = connected_link();
    let service = MeshtasticService::new(&link, Time(&now));
    let actions = (&service,);
    assert!(!link.reconnect_pending());
    assert_eq!(
        block_on(actions.invoke(SERVICE_ID, action::RECONNECT, &[])),
        Ok(ActionResult::Empty)
    );
    assert!(link.reconnect_pending());
    assert_eq!(
        block_on(actions.invoke(SERVICE_ID, 99, &[])),
        Err(ActionError::UnknownAction)
    );
}
