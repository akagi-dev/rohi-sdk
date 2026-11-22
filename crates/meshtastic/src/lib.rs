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
#![no_std]
//! The `meshtastic` service: a local Meshtastic radio on a UART.
//!
//! This crate owns the radio and everything about talking to it, and nothing about
//! telemetry. The delivery of sealed telemetry packets over it is
//! [`rohi-connectivity-meshtastic`](https://docs.rs/rohi-connectivity-meshtastic), which depends
//! on this crate.
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`proto`] | the small part of the Meshtastic client protobufs (`ToRadio`, `FromRadio`) that is spoken |
//! | [`uart`] | serial stream framing and the PhoneAPI session over the halves of a UART |
//! | [`link`] | [`MeshtasticLink`]: the state, counters, queues and diagnostic inbox shared by the session, the transport and the service |
//! | [`radio`] | [`MeshtasticRadio`], the contract a transport uses to reach the radio |
//! | [`service`] | the `meshtastic` config section, status and the `send`, `inbox` and `reconnect` actions |
//!
//! # One owner of the radio
//!
//! A single [`uart::UartSession`] owns UART RX and TX and parses every `FromRadio` message once.
//! It fans them out through the link: routing results of telemetry packets to the queue of the
//! transport, text messages and the acknowledgements of diagnostic sends to a six-entry inbox.
//! The service never touches the UART, and acknowledgement handling of the transport works
//! whether or not the inbox is ever read.
//!
//! The UART peripheral, its pins and 8N1 parameters are chosen by the firmware (the board HAL);
//! the only setting a user has is the baud rate.

#[cfg(test)]
extern crate std;

pub mod link;
pub mod node;
pub mod proto;
pub mod radio;
pub mod service;
pub mod uart;

pub use link::{
    Diagnostics, InboxEntry, InboxKind, LastError, LinkRadio, LinkSnapshot, MeshtasticLink,
};
pub use node::NodeId;
pub use radio::{MeshtasticRadio, Outgoing, RadioError, RoutingResult, routing};
