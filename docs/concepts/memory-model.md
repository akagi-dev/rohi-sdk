# Memory model

> **Application and runtime code never allocates.** Every queue, buffer and table has a compile-time bound. The only
> heap is a fixed region used by the vendor radio drivers.

## Static bounds

- **Bounded types.** Collections are `heapless` types or fixed arrays; strings and scan results have maximum lengths
  (`MAX_SSID_LEN`, `MAX_WIFI_SCAN_RESULTS`, ...). Section payloads are limited by `MAX_SECTION_PAYLOAD`.
- **Bounded queues and channels.** Every channel and request slot has a fixed depth; "full" has a defined behaviour
  (`Busy`, oldest event dropped, backpressure) documented with the limit. Nothing grows.
- **Static state.** Shared state (`WifiShared`, `MeasurementService`, `EVENTS`, `NetResources`, `BleResources`) is placed in
  `static` items, so its size is visible in `.bss` and checked at link time. Task futures are statically allocated by
  Embassy.
- **No heap in protocol code.** The control protocol uses bounded generated types (`micropb`, no heap) and fixed
  framing buffers.

## Where heap exists

The Espressif Wi-Fi and BLE controllers allocate dynamically. The ESP backend gives them **one fixed-size wireless heap**
(`esp-alloc`, no growth) shared by Wi-Fi and BLE:

- It is created by the firmware with `wireless_heap!(SIZE)` and handed to `EspWireless::new`, which is its only owner.
  The allocator implementation stays in `rohi-net-esp`; the **size** is a product decision.
- Only the radio drivers allocate from it. Heap-backed vendor results (for example the scan `Vec`) are converted into
  bounded ROHI types inside the backend and dropped there.
- `wireless_heap()` reports free bytes and, with the `heap-stats` feature, the minimum free bytes; verify it on hardware
  with both radios active before changing the size.

## The product owns the budget

Platform crates provide mechanisms with const generics; the **firmware** states the numbers in one file —
`firmwares/altruist-urban/src/resources.rs` — which is the authoritative RAM budget of the product:

| Resource | Type / constant | Consumed by |
|----------|-----------------|-------------|
| Wireless heap | `WIRELESS_HEAP_SIZE` → `wireless_heap!` | Wi-Fi and BLE controllers |
| IP stack sockets, TCP connections and buffers | `NETWORK_SOCKETS`, `TCP_CONNECTIONS`, `TCP_RX_BUFFER`, `TCP_TX_BUFFER` → `NetResources` | DHCP, DNS, TCP |
| BLE queues, connections, L2CAP channels, attributes, controller slots | `BLE_*` → `BleResources` | BLE peripheral |

Each constant documents what consumes it, why it has its value, what happens when it is exhausted and what has been
measured; the values are summarised in [Resource limits](../reference/resource-limits.md). Invariants that would
otherwise abort at runtime are `const` assertions (for example the IP stack needs
`SOCKETS ≥ TCP connections + DHCP + DNS`).

Adding a new static resource: put its type in the platform crate with const generics, its size in `resources.rs` with
the four-point documentation, and a line in [Resource limits](../reference/resource-limits.md).

## Exhaustion behaviour

Exhaustion never panics the firmware and never allocates: it is a typed error or backpressure
(`NetError::NoSocketAvailable`, `WifiError::Busy`, a delayed ATT response, a dropped oldest sensor event counted by
`EVENTS.dropped()`). The one exception is an attribute table too small for the GATT service, which panics once at BLE
start and is therefore caught on the first boot.

## Measured sizes

Flash and RAM measurements are recorded in [Resource budget](../validation/resource-budget.md).
