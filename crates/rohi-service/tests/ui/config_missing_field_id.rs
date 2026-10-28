use rohi_service::Config;

#[derive(Clone, Default, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1)]
struct Demo {
    a: u8,
}

fn main() {}
