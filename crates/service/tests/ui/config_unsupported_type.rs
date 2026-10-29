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

use rohi_service::Config;

// No codec: a domain type is never guessed.
#[derive(Clone, Debug, Default, PartialEq, minicbor::Encode, minicbor::Decode, minicbor::CborLen)]
#[cbor(transparent)]
struct Hostname(#[n(0)] u8);

#[derive(Clone, Default, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1)]
struct Demo {
    #[config(id = 1)]
    host: Hostname,
}

fn main() {}
