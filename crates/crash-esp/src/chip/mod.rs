//! The chip side: everything here needs `esp-hal`.

mod panic;
mod reset;
mod retained;
mod watchdog;

pub use panic::on_panic;
pub use reset::reset_reason;
pub use retained::{acknowledge, init, pending, report};
