use rohi_service::{Codec, Config, FieldError, Value, ValueKind};

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
