# Memory model

> **Application and runtime code never allocates.** Every queue, buffer and table has a compile-time bound. The only
> heap is a fixed region used by the vendor radio drivers.

Collections are `heapless` types or fixed arrays, channels and request slots have a fixed depth with a defined "full"
behaviour, shared state lives in `static` items, and the control protocol uses bounded generated types. The Espressif
Wi-Fi and BLE controllers do allocate: the ESP backend gives them one fixed-size wireless heap, shared by both radios,
that never grows.

The **product owns the budget**. Platform crates provide mechanisms with const generics; the firmware states the numbers
in one file, `firmwares/altruist-urban/src/resources.rs`. Each constant documents what consumes it, why it has its value,
what happens when it is exhausted and what has been measured. Exhaustion never panics and never allocates: it is a typed
error or backpressure.

| Topic | Where |
|-------|-------|
| Resource sizes and limits | [Resource limits](../reference/resource-limits.md) |
| Measured flash and RAM | [Resource budget](../validation/resource-budget.md) |
| Wireless heap on the ESP | [Wireless](../platform/wireless.md) |
| Rules, exhaustion behaviour, adding a resource | rustdoc, below |

## Reference

- [`rohi_sdk::memory`](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html) and
  [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk).
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
