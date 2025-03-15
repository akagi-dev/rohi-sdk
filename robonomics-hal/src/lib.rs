///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <research@akagi.dev>
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

//! Robonomics Open Hardware abstraction layer.
///
/// This crate introduce Rust support for devices designed and assembled
/// as part of Robonomics Open Hardware Initiative (ROHI). Details available at
/// https://robonomics.network/devices/
///

/// Air Quality sensor Altruist
/// Details: https://robonomics.network/devices/altruist/
pub mod altruist;

/// Robonomics Hardware sensors
pub mod sensors;
