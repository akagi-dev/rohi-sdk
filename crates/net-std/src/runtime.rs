///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////

//! The reactor of the `std` platform.
//!
//! Embassy stays the runtime of a ROHI application: its tasks are polled by the
//! executor, not by an async runtime of the host. Host sockets still need a
//! reactor, and this module is the only one: a single `tokio` runtime on one
//! dedicated thread (`rohi-io`) that does nothing but wait for I/O readiness and
//! wake the task that waits for it. The other `std` platform parts (the
//! BlueZ peripheral of the `ble` feature, the development interfaces of
//! applications) use the same runtime, so a process has one reactor.
//!
//! Host I/O objects are created and polled through [`within`], which polls a
//! future with the reactor entered. The future itself stays in the Embassy task:
//! dropping it cancels the operation, there is no detached background task.

use core::future::{Future, poll_fn};
use core::pin::pin;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle};

static HANDLE: OnceLock<Handle> = OnceLock::new();

/// The handle of the reactor, which is started by the first call.
///
/// The reactor runs for the rest of the process.
///
/// # Panics
///
/// When the operating system cannot start the thread or the reactor; a process
/// without them cannot do any network I/O.
pub fn handle() -> &'static Handle {
    HANDLE.get_or_init(|| {
        let runtime = Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("cannot start the I/O reactor");
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("rohi-io".into())
            .spawn(move || runtime.block_on(core::future::pending::<()>()))
            .expect("cannot start the I/O thread");
        handle
    })
}

/// Runs `future` with the reactor entered, so that it can create and use host
/// sockets and timers from any executor.
///
/// Cancellation follows `future`: dropping the result drops it.
pub async fn within<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    poll_fn(|cx| {
        let _entered = handle().enter();
        future.as_mut().poll(cx)
    })
    .await
}

/// Runs a blocking call on the thread pool of the reactor.
///
/// Dropping the future does not stop the call; its result is discarded.
pub async fn blocking<T, F>(call: F) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    handle().spawn_blocking(call).await
}
