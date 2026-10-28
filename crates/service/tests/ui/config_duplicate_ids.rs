use rohi_service::Config;

#[derive(Clone, Default, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1)]
struct Demo {
    #[config(id = 1)]
    a: u8,
    #[config(id = 1)]
    b: u8,
}

fn main() {}
