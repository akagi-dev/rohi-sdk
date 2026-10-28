use rohi_service::Config;

#[derive(Clone, Default, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1)]
struct Demo<T> {
    #[config(id = 1)]
    a: T,
}

fn main() {}
