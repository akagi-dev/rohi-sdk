# Resource limits

The canonical list of limits and static resource sizes. Other pages link here instead of repeating the numbers. Values
marked **product** are chosen by the firmware in `firmwares/altruist-urban/src/resources.rs`, which documents, for each
constant, what consumes it, why it has its value, what happens when it is exhausted and what was measured. Measured
sizes are in [Resource budget](../validation/resource-budget.md); the approach is in
[Memory model](../concepts/memory-model.md).

## Product resource budget (Altruist Urban)

| Constant | Value | Consumed by | When exhausted |
|----------|-------|-------------|----------------|
| `WIRELESS_HEAP_SIZE` | 128 KiB | Wi-Fi and BLE controllers (one shared fixed heap) | radio driver allocation fails; the driver reports an error, the sensors keep running |
| `TCP_CONNECTIONS` | 2 | `NetworkHandle::connect` | `NetError::NoSocketAvailable` immediately (no waiting) |
| `TCP_RX_BUFFER` / `TCP_TX_BUFFER` | 1024 B / 1024 B per connection | TCP connections | TCP flow control; nothing is lost |
| `UDP_SOCKETS` | 1 | `NetworkHandle::udp_socket` (NTP time source, one exchange at a time) | `NetError::NoSocketAvailable` immediately; the time source records a failed attempt and retries with backoff |
| `UDP_BUFFER` | 128 B per direction, `UDP_PACKETS` = 2 datagrams | each UDP socket | an oversized datagram is dropped by the stack |
| `NETWORK_SOCKETS` | `TCP_CONNECTIONS + UDP_SOCKETS + 2` (DHCP, DNS) = 5 | `embassy-net` socket table | cannot happen: checked at compile time |
| `BLE_RX_DEPTH` / `BLE_TX_DEPTH` | 4 / 4 chunks of ≤ 244 B | BLE RX / TX queues | RX: the ATT response is delayed (backpressure); TX: `send` waits |
| `BLE_CONNECTIONS` | 1 | BLE host stack | further centrals cannot connect |
| `BLE_L2CAP_CHANNELS` | 2 | BLE host stack | connection setup fails, advertising restarts |
| `BLE_ATTRIBUTES` | 24 | GATT attribute table | panics once when the BLE task starts |
| `BLE_CONTROLLER_SLOTS` | 4 | HCI command slots | commands wait for a slot |
| `CONNECTIVITY_OUTBOX_DEPTH` | 4 frames of ≤ 544 B | telemetry outbox (`rohi-connectivity`) | no new frame is created (`skipped_outbox_full`); queued frames are never evicted. See [Telemetry](../sdk/connectivity.md) |

Heap history: 72 KiB (Wi-Fi only) → 96 KiB left 11 KB minimum free with Wi-Fi + BLE → 128 KiB. The high-water mark with
the final value is **not yet measured** ([Hardware testing](../validation/hardware-testing.md)).

## Wi-Fi

| Constant | Value |
|----------|-------|
| `MAX_SSID_LEN` | 32 |
| `MAX_HOST_LEN` (telemetry host, control protocol) | 64 |
| `MAX_WIFI_PASSWORD_LEN` / `MIN_WIFI_PASSWORD_LEN` | 64 / 8 (open networks have no password) |
| `MAX_WIFI_SCAN_RESULTS` | 16 (strongest first; the backend converts at most 48 raw entries per scan) |
| `WIFI_COMMAND_QUEUE_DEPTH` | 4 (scan, reconnect, disconnect only; `Busy` when full) |
| `STATUS_RECEIVERS` | 3 tasks waiting for status changes |
| Backoff (`network` section: `backoff_initial`, `backoff_max`, `link_refresh`) | default 1 s doubling to 60 s; reset after the link stayed up for 10 s |
| Timeouts (`network` section) | scan 20 s, connect 30 s, DHCP 20 s by default |

The desired-configuration cell holds one value (latest wins); it is not a queue.

## IP data plane

Capacity is the product's `TCP_CONNECTIONS` above. `rohi-net` itself imposes no socket limit; the backend's pool is
bounded (at most 32 connections and 32 UDP sockets) and `NetResources` checks `SOCKETS ≥ TCP + UDP + 2` at compile time. DNS returns IPv4 only.

## BLE

| Constant | Value |
|----------|-------|
| `MAX_BLE_PAYLOAD` | 244 B (MTU 247 − 3) |
| `DEFAULT_BLE_PAYLOAD` | 20 B (MTU 23) |
| Control framing | 6 B header, at most 255 fragments, see the control protocol limits below |

Queue depths, connections and attribute count are product constants (table above).

## Control protocol

Protocol limits (`MAX_REQUEST_LEN` 256 B, `MAX_RESPONSE_LEN` 2048 B, `MAX_SENSORS` 8, `MAX_MEASUREMENTS_PER_SENSOR` 4,
text/SSID/password lengths, ATT payload) are defined once in `rohi-control-proto/build.rs` and tabulated in
[Control plane › Limits](../sdk/control.md#limits).

## Configuration store

| Item | Size |
|------|------|
| Configuration flash region | 16 KiB (`rohi-cfg`) |
| Slot size | 8 KiB (commit word included); at most `MAX_SLOT_SIZE` = 64 KiB |
| Envelope / payload overhead | about 10 B + 8 B plus 2–4 B per record |
| Maximum section payload (`MAX_SECTION_PAYLOAD`) | 1 024 B |
| Maximum sections (`MAX_SECTIONS`) | 16 |
| `ConfigStore` RAM | a few words of state |
| `get` stack workspace | 1 KiB (one `MAX_SECTION_PAYLOAD` buffer, only during the call) |
| Commit stack workspace | < 0.5 KiB (64 B write buffer, 64 B copy chunk, 128 B ID set) |
| Request path (firmware `ConfigHandle`) | one request slot, one response slot, callers serialised |

## Sensor runtime

| Item | Value |
|------|-------|
| Event queue (`EVENT_QUEUE_CAPACITY`) | 16 events; the oldest is dropped when full and counted (`EVENTS.dropped()`) |
| Snapshot request path (`MeasurementService`) | one request slot, one response slot, callers serialised |
| Noise buffers | 256 samples (1 KiB) on the stack + 4 KiB I²S DMA ring (`I2S_DMA_BUFFER_BYTES`, 64 ms of audio: the noise task must drain it in time) |
| Runner defaults (`RunnerConfig::DEFAULT`) | reprobe every 10 s, `Error` after 3 failures, re-init after 3, retry delay 1 s up to 30 s |
| I²C transaction timeout (firmware) | 200 ms |

Task stack usage is not measured yet.

## Changing a limit

Change the constant where it is defined, update this page and the documentation next to the constant, and re-measure
what is affected. A limit that is part of a wire or storage format (protocol sizes, `MAX_SECTION_PAYLOAD`) is an ABI
decision: see [Versioning and ABI](../concepts/versioning-and-abi.md).
