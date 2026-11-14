//! Host-side test helpers.

use core::future::Future;

pub fn block_on<F: Future>(f: F) -> F::Output {
    embassy_futures::block_on(f)
}
