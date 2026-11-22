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
//! The `meshtastic` service: configuration, status and diagnostic actions of the local radio.
//!
//! The service owns no I/O. The [`UartSession`](crate::uart::UartSession) task owns the UART and
//! the PhoneAPI session; the service reads and drives it through the shared
//! [`MeshtasticLink`].
//!
//! # Config
//!
//! Exactly one field, `baud_rate` ([`BaudRate`], default `115200`). The UART peripheral and its
//! pins are chosen at compile time by the board HAL, the frame format is 8N1 without flow
//! control, and there is no `enabled` switch: the service runs whenever the UART backend is
//! compiled in. A new baud rate is applied by starting a new session with it; it does not
//! reboot the device (a platform whose UART cannot be reconfigured live fails with
//! `last_error = baud` and the service reports `radio_reconnecting` until the old rate is
//! restored by a reboot).
//!
//! # Status
//!
//! | Id | Field | Meaning |
//! |---:|-------|---------|
//! | 1 | `node_id` | `!xxxxxxxx` of the local radio, once known |
//! | 2 | `firmware_version` | Meshtastic firmware version, once known |
//! | 3 | `hardware_model` | hardware model of the radio, once known |
//! | 4 | `last_rx_age_s` | seconds since the last valid `FromRadio`, after the first |
//! | 5 | `tx_frames` | `ToRadio` messages written to the UART |
//! | 6 | `rx_frames` | valid `FromRadio` messages |
//! | 7 | `framing_errors` | invalid serial frame headers |
//! | 8 | `decode_errors` | frames that did not decode |
//! | 9 | `reconnects` | session attempts after the first |
//! | 10 | `last_error` | enum, see [`LastError`] |
//! | 11 | `session_age_s` | seconds since the current session was established |
//!
//! Fields that are not known yet are **omitted**. Counters live in RAM, restart at zero with the
//! device and saturate at `u64::MAX` instead of wrapping. The configured baud rate is not a status
//! field (read the config). Health is `unknown` until the first synchronization finished or
//! failed, `ok` while the session is established, `degraded` while it is being recovered or the
//! stream is full of bad frames, and `error` when the UART cannot be used, the radio does not
//! speak the serial PhoneAPI or stays silent after repeated attempts. The service is not essential:
//! a missing optional radio does not make the device unhealthy.
//!
//! # Issues
//!
//! At most four are active; the most actionable come first.
//!
//! | Issue | Severity | Condition |
//! |-------|----------|-----------|
//! | `uart_unavailable` | error | the UART could not be set up |
//! | `radio_not_configured` | error | the radio sends data but no serial PhoneAPI frames (set the Serial module to `PROTO`) |
//! | `radio_unresponsive` | error | the handshake failed [`UNRESPONSIVE_AFTER`] times in a row |
//! | `protocol_error` | warning | [`PROTOCOL_ERROR_STREAK`] bad frames in a row |
//! | `radio_reconnecting` | warning | a session is being recovered |
//!
//! # Actions
//!
//! * `send(to, text)` (mutate, inline `packet_id`): a text message (`TEXT_MESSAGE_APP`) as unicast on
//!   channel 0 with `want_ack`. `to` is `!xxxxxxxx`, not the broadcast address; `text` is 1 to
//!   64 bytes. The result means the radio accepted the packet, **not** that the mesh delivered it:
//!   the routing result comes later and appears in the inbox. `unavailable` while the radio is
//!   not ready; invalid input is refused without sending anything.
//! * `inbox` (read, records): up to six entries in RAM, newest first, `kind` (`text` or `ack`),
//!   `from`, `packet_id` and `text`. Reading does not clear it; a longer text is skipped, never cut;
//!   telemetry traffic never appears.
//! * `reconnect` (mutate, empty): ends the current session and starts a new one; whatever is
//!   waiting for the old session fails at once as unavailable.

use core::fmt::Write as _;

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::{String, Vec};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{Variant, VariantValue};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, Config, EnumDescriptor,
    EnumVariant, FIRST_SERVICE_ISSUE, FieldDescriptor, FieldError, Issue, IssueDescriptor,
    IssueSeverity, MAX_ISSUES, ResultFormat, Service, ServiceHealth, ServiceSummary, Status,
    StatusWriter, Unit, Value, ValueKind, WriteError,
};
use rohi_time::Clock;

use crate::link::{
    Diagnostics, INBOX_TEXT_MAX, InboxKind, LastError, MeshtasticLink, PROTOCOL_ERROR_STREAK,
    UNRESPONSIVE_AFTER,
};
use crate::node::NodeId;
use crate::proto::PORT_TEXT_MESSAGE_APP;
use crate::radio::{Outgoing, RadioError};

/// Id of the enum of the `baud_rate` field.
pub const BAUD_ENUM: u32 = 1;

/// The baud rates the UART link supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum BaudRate {
    /// 9600 baud.
    #[n(0)]
    B9600,
    /// 19200 baud.
    #[n(1)]
    B19200,
    /// 38400 baud.
    #[n(2)]
    B38400,
    /// 57600 baud.
    #[n(3)]
    B57600,
    /// 115200 baud, the default of the Meshtastic serial module.
    #[default]
    #[n(4)]
    B115200,
    /// 230400 baud.
    #[n(5)]
    B230400,
}

impl BaudRate {
    /// Bits per second.
    pub const fn bps(self) -> u32 {
        match self {
            Self::B9600 => 9_600,
            Self::B19200 => 19_200,
            Self::B38400 => 38_400,
            Self::B57600 => 57_600,
            Self::B115200 => 115_200,
            Self::B230400 => 230_400,
        }
    }
}

impl VariantValue for BaudRate {
    fn variant(&self) -> u32 {
        match self {
            Self::B9600 => 0,
            Self::B19200 => 1,
            Self::B38400 => 2,
            Self::B57600 => 3,
            Self::B115200 => 4,
            Self::B230400 => 5,
        }
    }

    fn from_variant(variant: u32) -> Option<Self> {
        match variant {
            0 => Some(Self::B9600),
            1 => Some(Self::B19200),
            2 => Some(Self::B38400),
            3 => Some(Self::B57600),
            4 => Some(Self::B115200),
            5 => Some(Self::B230400),
            _ => None,
        }
    }
}

const BAUD_VARIANTS: EnumDescriptor = EnumDescriptor::new(
    BAUD_ENUM,
    &[
        EnumVariant::new(0, "9600"),
        EnumVariant::new(1, "19200"),
        EnumVariant::new(2, "38400"),
        EnumVariant::new(3, "57600"),
        EnumVariant::new(4, "115200"),
        EnumVariant::new(5, "230400"),
    ],
);

/// The enums of the `meshtastic` section.
pub const CONFIG_ENUMS: &[EnumDescriptor] = &[BAUD_VARIANTS];

/// The effective `meshtastic` config section: the baud rate of the UART link.
///
/// ```
/// use rohi_meshtastic::service::{BaudRate, MeshtasticConfig};
///
/// assert_eq!(MeshtasticConfig::default().baud_rate, BaudRate::B115200);
/// assert_eq!(BaudRate::B115200.bps(), 115_200);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Config)]
#[config(
    name = "meshtastic",
    overrides = MeshtasticOverrides,
    version = 1,
    validate = validate_section,
    enums = CONFIG_ENUMS
)]
pub struct MeshtasticConfig {
    /// UART baud rate used to talk to the local radio.
    #[config(id = 1, with = Variant, enum_id = BAUD_ENUM)]
    pub baud_rate: BaudRate,
}

// Every value of the enum is a valid rate.
fn validate_section(_config: &MeshtasticConfig) -> Result<(), FieldError> {
    Ok(())
}

/// Ids of the status fields.
pub mod status_field {
    /// `node_id`.
    pub const NODE_ID: u32 = 1;
    /// `firmware_version`.
    pub const FIRMWARE_VERSION: u32 = 2;
    /// `hardware_model`.
    pub const HARDWARE_MODEL: u32 = 3;
    /// `last_rx_age_s`.
    pub const LAST_RX_AGE_S: u32 = 4;
    /// `tx_frames`.
    pub const TX_FRAMES: u32 = 5;
    /// `rx_frames`.
    pub const RX_FRAMES: u32 = 6;
    /// `framing_errors`.
    pub const FRAMING_ERRORS: u32 = 7;
    /// `decode_errors`.
    pub const DECODE_ERRORS: u32 = 8;
    /// `reconnects`.
    pub const RECONNECTS: u32 = 9;
    /// `last_error`.
    pub const LAST_ERROR: u32 = 10;
    /// `session_age_s`.
    pub const SESSION_AGE_S: u32 = 11;
}

/// Codes of the issues.
pub mod issue {
    use super::FIRST_SERVICE_ISSUE;

    /// The UART could not be set up.
    pub const UART_UNAVAILABLE: u32 = FIRST_SERVICE_ISSUE;
    /// The radio does not answer.
    pub const RADIO_UNRESPONSIVE: u32 = FIRST_SERVICE_ISSUE + 1;
    /// Repeated framing or decoding errors.
    pub const PROTOCOL_ERROR: u32 = FIRST_SERVICE_ISSUE + 2;
    /// A session is being recovered.
    pub const RADIO_RECONNECTING: u32 = FIRST_SERVICE_ISSUE + 3;
    /// The radio is not in the serial PhoneAPI mode.
    pub const RADIO_NOT_CONFIGURED: u32 = FIRST_SERVICE_ISSUE + 4;
}

/// Ids of the actions.
pub mod action {
    /// `send`.
    pub const SEND: u32 = 1;
    /// `inbox`.
    pub const INBOX: u32 = 2;
    /// `reconnect`.
    pub const RECONNECT: u32 = 3;
}

/// Ids of the fields of the actions.
pub mod action_field {
    /// Argument `to` of `send`.
    pub const TO: u32 = 1;
    /// Argument `text` of `send`.
    pub const TEXT: u32 = 2;
    /// Result `packet_id` of `send`.
    pub const SEND_PACKET_ID: u32 = 1;
    /// Result `kind` of a record of `inbox`.
    pub const KIND: u32 = 1;
    /// Result `from` of a record of `inbox`.
    pub const FROM: u32 = 2;
    /// Result `packet_id` of a record of `inbox`.
    pub const PACKET_ID: u32 = 3;
    /// Result `text` of a record of `inbox`.
    pub const RECORD_TEXT: u32 = 4;
}

const STATUS_ENUM: u32 = 1;
const KIND_ENUM: u32 = 1;

/// The `meshtastic` service over a [`MeshtasticLink`].
pub struct MeshtasticService<'a, M: RawMutex, C> {
    link: &'a MeshtasticLink<M>,
    clock: C,
}

impl<'a, M: RawMutex, C: Clock> MeshtasticService<'a, M, C> {
    /// The service of `link`; `clock` measures the ages of the status.
    pub const fn new(link: &'a MeshtasticLink<M>, clock: C) -> Self {
        Self { link, clock }
    }
}

impl<M: RawMutex, C> Service for MeshtasticService<'_, M, C> {
    const ID: u32 = rohi_service::service_id("meshtastic");
    const NAME: &'static str = "meshtastic";
}

fn unresponsive(d: &Diagnostics) -> bool {
    !d.link.connected && d.handshake_failures >= UNRESPONSIVE_AFTER && !d.not_configured
}

fn health(d: &Diagnostics) -> ServiceHealth {
    if d.uart_failed || d.not_configured || unresponsive(d) {
        ServiceHealth::Error
    } else if d.link.connected {
        if d.bad_streak >= PROTOCOL_ERROR_STREAK {
            ServiceHealth::Degraded
        } else {
            ServiceHealth::Ok
        }
    } else if d.link.sessions == 0 && d.handshake_failures == 0 && d.last_error == LastError::None {
        ServiceHealth::Unknown
    } else {
        ServiceHealth::Degraded
    }
}

/// The name of a `HardwareModel` of the Meshtastic protocol, for the models likely next to an ESP32.
fn hardware_name(model: u32) -> Option<&'static str> {
    Some(match model {
        4 => "tbeam",
        7 => "t_echo",
        9 => "rak4631",
        13 => "rak11200",
        43 => "heltec_v3",
        44 => "heltec_wsl_v3",
        48 => "heltec_wireless_tracker",
        50 => "t_deck",
        69 => "heltec_mesh_node_t114",
        110 => "heltec_v4",
        255 => "private_hw",
        _ => return None,
    })
}

fn seconds_since(now_ms: u64, then_ms: u64) -> u64 {
    now_ms.saturating_sub(then_ms) / 1_000
}

impl<M: RawMutex, C: Clock> Status for MeshtasticService<'_, M, C> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::NODE_ID, "node_id", ValueKind::String),
        FieldDescriptor::new(
            status_field::FIRMWARE_VERSION,
            "firmware_version",
            ValueKind::String,
        ),
        FieldDescriptor::new(
            status_field::HARDWARE_MODEL,
            "hardware_model",
            ValueKind::String,
        ),
        FieldDescriptor::new(status_field::LAST_RX_AGE_S, "last_rx_age_s", ValueKind::U64)
            .unit(Unit::S),
        FieldDescriptor::new(status_field::TX_FRAMES, "tx_frames", ValueKind::U64),
        FieldDescriptor::new(status_field::RX_FRAMES, "rx_frames", ValueKind::U64),
        FieldDescriptor::new(
            status_field::FRAMING_ERRORS,
            "framing_errors",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::DECODE_ERRORS, "decode_errors", ValueKind::U64),
        FieldDescriptor::new(status_field::RECONNECTS, "reconnects", ValueKind::U64),
        FieldDescriptor::new(status_field::LAST_ERROR, "last_error", ValueKind::Enum)
            .with_enum(STATUS_ENUM),
        FieldDescriptor::new(status_field::SESSION_AGE_S, "session_age_s", ValueKind::U64)
            .unit(Unit::S),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        STATUS_ENUM,
        &[
            EnumVariant::new(0, "none"),
            EnumVariant::new(1, "uart_io"),
            EnumVariant::new(2, "uart_closed"),
            EnumVariant::new(3, "handshake_failed"),
            EnumVariant::new(4, "radio_rebooted"),
            EnumVariant::new(5, "framing"),
            EnumVariant::new(6, "decode"),
            EnumVariant::new(7, "requested"),
            EnumVariant::new(8, "baud"),
        ],
    )];
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::UART_UNAVAILABLE, "uart_unavailable"),
        IssueDescriptor::new(issue::RADIO_UNRESPONSIVE, "radio_unresponsive"),
        IssueDescriptor::new(issue::PROTOCOL_ERROR, "protocol_error"),
        IssueDescriptor::new(issue::RADIO_RECONNECTING, "radio_reconnecting"),
        IssueDescriptor::new(issue::RADIO_NOT_CONFIGURED, "radio_not_configured"),
    ];
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(health(&self.link.diagnostics()))
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let d = self.link.diagnostics();
        let mut push = |severity, code| {
            let _ = out.push(Issue {
                severity,
                code,
                age_ms: None,
            });
        };
        if d.uart_failed {
            push(IssueSeverity::Error, issue::UART_UNAVAILABLE);
        }
        if d.not_configured {
            push(IssueSeverity::Error, issue::RADIO_NOT_CONFIGURED);
        }
        if unresponsive(&d) {
            push(IssueSeverity::Error, issue::RADIO_UNRESPONSIVE);
        }
        if d.bad_streak >= PROTOCOL_ERROR_STREAK {
            push(IssueSeverity::Warning, issue::PROTOCOL_ERROR);
        }
        if d.recovering && !d.link.connected && !d.uart_failed {
            push(IssueSeverity::Warning, issue::RADIO_RECONNECTING);
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let d = self.link.diagnostics();
        let now = self.clock.now().millis;
        if let Some(node) = d.link.node {
            let mut text: String<9> = String::new();
            let _ = write!(text, "{}", NodeId(node));
            out.text(status_field::NODE_ID, &text)?;
        }
        if !d.firmware.is_empty() {
            out.text(status_field::FIRMWARE_VERSION, &d.firmware)?;
        }
        if let Some(model) = d.hardware {
            match hardware_name(model) {
                Some(name) => out.text(status_field::HARDWARE_MODEL, name)?,
                None => {
                    let mut text: String<16> = String::new();
                    let _ = write!(text, "model_{model}");
                    out.text(status_field::HARDWARE_MODEL, &text)?;
                }
            }
        }
        if let Some(last) = d.last_rx_ms {
            out.u64(status_field::LAST_RX_AGE_S, seconds_since(now, last))?;
        }
        out.u64(status_field::TX_FRAMES, d.tx_frames)?;
        out.u64(status_field::RX_FRAMES, d.rx_frames)?;
        out.u64(status_field::FRAMING_ERRORS, d.framing_errors)?;
        out.u64(status_field::DECODE_ERRORS, d.decode_errors)?;
        out.u64(status_field::RECONNECTS, d.reconnects)?;
        out.variant(status_field::LAST_ERROR, d.last_error.code())?;
        if let Some(started) = d.session_ms {
            out.u64(status_field::SESSION_AGE_S, seconds_since(now, started))?;
        }
        Ok(())
    }
}

const SEND_ARGS: &[FieldDescriptor] = &[
    FieldDescriptor::new(action_field::TO, "to", ValueKind::String),
    FieldDescriptor::new(action_field::TEXT, "text", ValueKind::String),
];
const SEND_RESULT: &[FieldDescriptor] = &[FieldDescriptor::new(
    action_field::SEND_PACKET_ID,
    "packet_id",
    ValueKind::U64,
)];
const INBOX_RESULT: &[FieldDescriptor] = &[
    FieldDescriptor::new(action_field::KIND, "kind", ValueKind::Enum).with_enum(KIND_ENUM),
    FieldDescriptor::new(action_field::FROM, "from", ValueKind::String),
    FieldDescriptor::new(action_field::PACKET_ID, "packet_id", ValueKind::U64),
    FieldDescriptor::new(action_field::RECORD_TEXT, "text", ValueKind::String),
];
const KIND_ENUMS: &[EnumDescriptor] = &[EnumDescriptor::new(
    KIND_ENUM,
    &[EnumVariant::new(0, "text"), EnumVariant::new(1, "ack")],
)];

fn write_error(_: WriteError) -> ActionError {
    ActionError::Internal
}

fn text_arg(args: &[(u32, Value)], id: u32) -> Result<&str, ActionError> {
    args.iter()
        .find(|(field, _)| *field == id)
        .and_then(|(_, value)| value.as_str())
        .ok_or(ActionError::InvalidArgument)
}

impl<M: RawMutex, C: Clock> Action for MeshtasticService<'_, M, C> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::SEND, "send", ActionAccess::Mutate)
            .args(SEND_ARGS)
            .format(ResultFormat::Inline)
            .results(SEND_RESULT),
        ActionDescriptor::new(action::INBOX, "inbox", ActionAccess::Read)
            .format(ResultFormat::Records)
            .results(INBOX_RESULT)
            .enums(KIND_ENUMS),
        ActionDescriptor::new(action::RECONNECT, "reconnect", ActionAccess::Mutate),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::SEND => {
                let to = text_arg(args, action_field::TO)?;
                let text = text_arg(args, action_field::TEXT)?;
                // Everything is checked before anything is sent.
                let destination = NodeId::parse(to)
                    .filter(|node| *node != NodeId::BROADCAST)
                    .ok_or(ActionError::Rejected("invalid_destination"))?;
                if text.is_empty() || text.len() > INBOX_TEXT_MAX {
                    return Err(ActionError::Rejected("invalid_text"));
                }
                if !self.link.snapshot().connected {
                    return Err(ActionError::Unavailable);
                }
                let id = self.link.reserve_ids(1);
                // Registered first: the acknowledgement may beat the end of this call.
                self.link.note_diagnostic_send(id);
                let packet = Outgoing {
                    destination: destination.0,
                    id,
                    port: PORT_TEXT_MESSAGE_APP,
                    payload: text.as_bytes(),
                    want_ack: true,
                    pki: false,
                };
                if let Err(error) = self.link.submit(&packet).await {
                    self.link.cancel_diagnostic_send(id);
                    return Err(match error {
                        RadioError::NotConnected | RadioError::Disconnected => {
                            ActionError::Unavailable
                        }
                        RadioError::Io => ActionError::Internal,
                    });
                }
                out.u64(action_field::SEND_PACKET_ID, u64::from(id))
                    .map_err(write_error)
            }
            action::INBOX => {
                let entries = self.link.inbox();
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 {
                        out.next_record().map_err(write_error)?;
                    }
                    out.variant(
                        action_field::KIND,
                        match entry.kind {
                            InboxKind::Text => 0,
                            InboxKind::Ack => 1,
                        },
                    )
                    .map_err(write_error)?;
                    if let Some(from) = entry.from {
                        let mut text: String<9> = String::new();
                        let _ = write!(text, "{}", NodeId(from));
                        out.text(action_field::FROM, &text).map_err(write_error)?;
                    }
                    out.u64(action_field::PACKET_ID, u64::from(entry.packet_id))
                        .map_err(write_error)?;
                    out.text(action_field::RECORD_TEXT, &entry.text)
                        .map_err(write_error)?;
                }
                Ok(())
            }
            action::RECONNECT => {
                self.link.request_reconnect();
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests;
