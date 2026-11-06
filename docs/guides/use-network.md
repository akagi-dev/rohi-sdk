# Use IP networking

Open DNS and TCP connections through the network handle of `rohi-net`. Never use `embassy-net` or any vendor type
directly: code written against the standard `embedded-nal-async` and `embedded-io-async` traits is portable, testable on
the host and independent of the stack. The API, its errors and its limits are in the
[`rohi-net` rustdoc](https://docs.rs/rohi-net).

## Write portable code

Make your layer generic over the standard traits (see the example in the rustdoc) and handle the typed network errors:

- **network unavailable**: Wi-Fi is not connected or DHCP is incomplete; wait for the connection and retry. This is not
  a server problem.
- **DNS or connect failure**: the network is up but the name or endpoint failed; apply your own retry policy.
- **no socket available**: all TCP slots of the product are in use; the call does not wait. Close a connection first.
- **reset or timeout**: the connection is gone; reconnect.

Drop connections you no longer need and bound waits with your own timeout; dropping a pending connect is safe
([cancellation](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html)).

## Test on the host

`rohi-net` ships an in-memory fake network (the `testing` feature) that scripts DNS answers, refused connections,
readiness and slot exhaustion and lets the test play the peer. Use it from your crate's tests.

## Try it against real services

The [Altruist Urban on std](altruist-urban-std.md) implements the same network handle on the IP stack of your computer (`rohi-net-std`), so code
that uses `NetworkHandle` can talk to `127.0.0.1`, containers and local test servers without hardware.

## On the device

The firmware creates the handle when it starts Wi-Fi and hands it to the component that needs it. Capacity is a product
decision in `firmwares/altruist-urban/src/esp/resources.rs` (see the
[memory model](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html)). There is no TLS yet: do not send secrets in
clear text.
