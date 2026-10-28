use rohi_service::Config;

#[derive(Clone, Config)]
#[config(name = "demo", overrides = DemoOverrides, version = 1)]
struct Demo {
    #[config(id = 1, secret)]
    token: u32,
}

impl Default for Demo {
    fn default() -> Self {
        Self { token: 0 }
    }
}

fn main() {}
