use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{
    FieldDescriptor, Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusSet,
    StatusWriter, WriteError,
};

#[derive(Service)]
struct WifiService;

#[derive(Service)]
#[service(name = "wifi")]
struct WirelessService;

macro_rules! status {
    ($ty:ty) => {
        impl Status for $ty {
            const VERSION: u16 = 1;
            const STATUS_SCHEMA: &'static [FieldDescriptor] = &[];

            async fn summary(&self) -> ServiceSummary {
                ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
            }

            async fn status(&self, _: &mut impl StatusWriter) -> Result<(), WriteError> {
                Ok(())
            }
        }
    };
}

status!(WifiService);
status!(WirelessService);

// `Statuses::new` fails the build with the same flag after monomorphization.
const _: () = assert!(<(&WifiService, &WirelessService) as StatusSet<NoopRawMutex>>::IDS_UNIQUE);

fn main() {}
