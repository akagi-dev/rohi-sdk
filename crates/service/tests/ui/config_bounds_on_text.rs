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

use rohi_config::{Codec, FieldError, Value, ValueKind};
use rohi_service::Config;

struct Text;

impl Codec<String> for Text {
    const KIND: ValueKind = ValueKind::String;

    fn encode(value: &String) -> Result<Option<Value>, FieldError> {
        Ok(Value::text(value))
    }

    fn decode(value: &Value) -> Result<String, FieldError> {
        value.as_str().map(str::to_owned).ok_or(FieldError::WrongKind)
    }
}

#[derive(Clone, Default, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1)]
struct Demo {
    #[config(id = 1, with = Text, min = 1, max = 2)]
    host: String,
}

fn main() {
    // The bounds need a numeric field: the schema is evaluated with the type.
    let _ = <Demo as Config>::FIELDS;
}
