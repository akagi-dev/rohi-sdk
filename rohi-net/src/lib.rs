///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <admin@akagi.dev>
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

//! Network layer for Robonomics Open Hardware Initiative.
//!
//! This crate aims to support general IoT related network services. Provides
//! abstraction for Robonomics Hardware collection, makes firmware creation
//! developer friendly.
//!
//! Same as other ROHI SDK crates this is **async-only**. It based on [embassy-net](https://crates.io/crates/embassy-net)
//! as low level networking and uses [edge-http](https://crates.io/crates/edge-http) for HTTP.

/// The [esp-wifi](https://crates.io/crates/esp-wifi) based Wifi AP/Client support.
pub mod wifi;

/// HTTP server and client support.
pub mod http;

/// Entry point for networking.
pub mod network;
pub use network::Network;
