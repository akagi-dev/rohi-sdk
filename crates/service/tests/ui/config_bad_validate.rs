use rohi_service::Config;

fn check(_: &Demo) -> bool {
    true
}

#[derive(Clone, Default, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1, validate = check)]
struct Demo {
    #[config(id = 1)]
    a: u8,
}

fn main() {}
