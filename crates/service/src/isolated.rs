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
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use pin_project_lite::pin_project;

pin_project! {
    /// A future that is polled through a function of its own.
    ///
    /// A backend call is an `async fn` over a generic registry, and its poll
    /// is inlined into the caller together with every temporary it has: the
    /// bounded collections of a response or of a section are a few KiB each,
    /// so a caller that awaits many different calls reserves the sum of all
    /// of them in its stack frame, although only one runs at a time. Behind
    /// this wrapper each call keeps its temporaries in its own frame, which
    /// exists while it runs.
    pub struct Isolated<F> {
        #[pin]
        inner: F,
    }
}

/// Wraps `future` so that it is polled through a function of its own, see [`Isolated`].
pub fn isolated<F: Future>(future: F) -> Isolated<F> {
    Isolated { inner: future }
}

impl<F: Future> Future for Isolated<F> {
    type Output = F::Output;

    #[inline(never)]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        self.project().inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_futures::{block_on, yield_now};

    #[test]
    fn an_isolated_future_gives_the_output_of_the_inner_one() {
        let value = block_on(isolated(async {
            yield_now().await;
            7
        }));
        assert_eq!(value, 7);
    }
}
