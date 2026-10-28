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
