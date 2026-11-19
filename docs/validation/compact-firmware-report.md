# Compact firmware report (#73)

Before and after of the change that made the Altruist Urban firmware smaller at compile time: optional LZ4 log compression
and a 128 KiB log on the ESP32-C3, compile-time route and telemetry capacities, a signed packet built in one buffer, and
transport-owned TCP security without signed control requests.

**How to read the numbers.** *Measured* means taken from a built image or a host test run with the commands below.
*Static* means derived from the source and the schema without a device. *Not measured* means that nothing here backs it.
**No number on this page comes from hardware**: nothing was flashed, no stack high-water mark was read and no flash was
written by a device. The host tests run the log, the secure channel and the control plane on emulated media and sockets.

## What was compared

| | |
|---|---|
| Baseline | `5f445be` (the head of PR #72, `feat: auth based control`), built in a clean worktree |
| After | this branch (`feat/compact-firmware`) |
| Toolchain, flags | rustc 1.99.1, the `firmware` profile on the ESP32-C3 (`riscv32imc`) and `release` on the ESP32-C6 (`riscv32imac`), `-C force-frame-pointers`, the same features (`esp32c3` / `esp32c6`) |
| Image sections | `scripts/measure.sh <chip> [features]` (GNU `size -A` of the production ELF) |
| Task pools, frames, main stack | `scripts/memcheck.sh images <chip> out && scripts/memcheck.sh analyze <chip> out --base <baseline json>` |
| Variants | LZ4 off (default) and on (`--features lz4`), packet export on (default) and off |

`flash` is what the image occupies in flash: `.text`, `.rodata`, `.rwtext`, `.trap`, the application descriptor, the Wi-Fi
sections and the initial values of `.data`. `RAM (static)` is `.data` + `.bss`, which includes the 112 KiB wireless heap.

## Image size, default build (LZ4 off, packet export on)

| Metric | C3 before | C3 after | Δ bytes | Δ % | C6 before | C6 after | Δ bytes | Δ % |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `.text` | 1 276 856 | 1 259 632 | −17 224 | −1.35 | 1 806 988 | 1 776 564 | −30 424 | −1.68 |
| `.rodata` | 184 152 | 175 352 | −8 800 | −4.78 | 217 592 | 208 776 | −8 816 | −4.05 |
| `.data` | 31 144 | 30 056 | −1 088 | −3.49 | 34 956 | 33 884 | −1 072 | −3.07 |
| `.bss` | 199 264 | 193 912 | −5 352 | −2.69 | 203 376 | 198 016 | −5 360 | −2.64 |
| flash image | 1 592 860 | 1 565 752 | −27 108 | −1.70 | 2 195 680 | 2 155 368 | −40 312 | −1.84 |
| RAM (static) | 230 408 | 223 968 | −6 440 | −2.80 | 238 332 | 231 900 | −6 432 | −2.70 |

Measured. The ESP32-C3 partition for the application is 0x3C0000 B (3 932 160 B) after this change; the image uses 39.8 %.

## Runtime RAM: task pools, frames, main stack

| Metric | C3 before | C3 after | Δ | C6 before | C6 after | Δ |
|---|---:|---:|---:|---:|---:|---:|
| Embassy task pools (total) | 45 008 | 41 136 | −3 872 (−8.6 %) | 44 984 | 41 112 | −3 872 (−8.6 %) |
| `connectivity_task` pool | 5 840 | 3 736 | −2 104 | 5 840 | 3 736 | −2 104 |
| `control_task` pool | 15 128 | 13 840 | −1 288 | 15 128 | 13 840 | −1 288 |
| `logging_task` pool | 2 304 | 1 824 | −480 | 2 304 | 1 824 | −480 |
| largest function frame | 13 120 | 12 256 | −864 (−6.6 %) | 9 744 | 8 400 | −1 344 (−13.8 %) |
| main stack allocation | 26 232 | 32 672 | +6 440 (+24.6 %) | 125 528 | 131 960 | +6 432 (+5.1 %) |

Measured (statically, from the images). The main stack is what is left of the RAM after the statics, so it grows by what the
statics and task pools gave back. **The high-water mark of the stack was not measured on hardware.** The deepest call chain under
the largest frames is unknown: the numbers say that the largest poll frame shrank, not how deep the stack gets. A smaller wire
message is not a proof of a smaller stack; the frame table is the evidence that the stack did not grow.

The memcheck budgets of `firmwares/altruist-urban/memcheck-budgets.toml` were **lowered** to the new measurements (ESP32-C3:
`max_data` 31 200 → 30 100, `max_bss` 210 800 → 194 000, `max_task_ram` 45 100 → 41 200, `max_frame` 13 200 → 12 300,
`min_main_stack` 15 800 → 32 600; ESP32-C6 analogously). No limit and no approved frame growth was raised. While measuring, the
`control_task` poll frame and `config::start` showed frame growth against the baseline only because the compiler had inlined
helpers differently; `publish_section`, `publish_boot` and `publish_factory_reset` are `#[inline(never)]` again and the controller's
`dispatch_call` goes through `rohi_service::isolated`, which restored the layout instead of widening an approval.

## Variants

| Variant | C3 flash | Δ vs default | C3 RAM (static) | C6 flash | Δ vs default | C6 RAM (static) |
|---|---:|---:|---:|---:|---:|---:|
| default (LZ4 off, export on) | 1 565 752 | | 223 968 | 2 155 368 | | 231 900 |
| `--features lz4` | 1 570 018 | +4 266 (+0.27 %) | 223 968 | 2 163 022 | +7 654 (+0.36 %) | 231 892 |
| packet export off | 1 566 552 | +800 (+0.05 %) | 223 312 (−656) | 2 148 022 | −7 346 (−0.34 %) | 231 324 (−576) |

Measured. The image without LZ4 has no LZ4 symbol (`scripts/measure.sh` fails the build otherwise); the codec costs 4.3 KB (C3)
and 7.7 KB (C6) of flash when it is compiled in.

### Packet export (`connectivity.packet_get`)

The export pool and its action cost 656 B of RAM on the ESP32-C3 (576 B on the ESP32-C6) and no flash on the ESP32-C3 (the image
without it is 800 B larger there, a layout effect), 7.3 KB of flash on the ESP32-C6. It is **kept on by default**: removing it
loses the ability to fetch a freshly sealed packet for a profile without a route (`rohi connectivity packet_get`), which is the way
to check a profile and a recipient by hand, and the gain is below 0.3 % of the RAM. It is a feature (`connectivity-packet-export` of
`rohi-sdk`, listed in the feature list of `altruist-urban`; remove it from that list to build without it, and
`cargo test -p rohi-connectivity --no-default-features` covers the build without it).

## Log (ESP32-C3: 128 KiB at 0x20000)

Layout, right after the configuration region (this deliberately differs from the issue, which kept the log at the end of the flash):

| Partition | Offset | Size |
|---|---:|---:|
| `rohi-cfg` | `0x10000` | `0x4000` |
| `rohi-log` | `0x20000` | `0x20000` (128 KiB, 32 sectors: 30 for the ring, 2 for the format header) |
| `factory` | `0x40000` | `0x3C0000` (ends the flash, both ends 64 KiB aligned) |

The ESP32-C6 is unchanged (`factory` at `0x20000`, `rohi-log` the last 1 MiB). The ring has 15 segments of 8 KiB (120 KiB). A
device flashed with an earlier layout must be reflashed with the new table (the app moves) and the first start **formats the
`rohi-log` region only**: nothing is migrated and the old archive is discarded (host tests: a format cut at every erase and write
leaves the neighbours, here the application and the configuration, byte for byte as they were).

**Retention (measured on an emulated 128 KiB medium, plain text, `the_esp32c3_log_retention_is_measured`).** Text lines of 80 to
160 bytes, written until three times the ring has passed, batch of up to 1 KiB per flush. Every cell is records / payload bytes of
text kept, 15 segments retained in all cases. The theoretical ceiling is 122 880 B (15 × 8 KiB, before segment and batch overhead).

| Line | flush after 1 record | after 6 | after 20 | after 64 |
|---|---:|---:|---:|---:|
| 80 B | 1 207 / 103 802 | 1 260 / 107 100 | 1 320 / 110 880 | 1 256 / 105 504 |
| 100 B | 993 / 104 868 | 1 056 / 110 880 | 991 / 103 064 | 1 025 / 106 600 |
| 120 B | 807 / 100 875 | 882 / 110 250 | 880 / 109 120 | 960 / 119 040 |
| 160 B | 708 / 104 784 | 732 / 108 084 | 754 / 110 838 | 747 / 109 638 |

That is 98 to 116 KiB of text kept (82 % to 97 % of the 120 KiB ring), not the ~119 KiB estimate in the issue, which no flush pattern
reaches because a segment is sealed once a batch would not fit. The batch was reduced from 2 KiB to 1 KiB of text for this: with
2 KiB, 85 to 107 KiB (87 462 to 109 988 B) were kept, and the `Batch` in the logging static shrank by 1 KiB. A reboot keeps the same records (tested for every
cell). Reboot and corruption recovery are covered by the cut-at-every-write tests across format, clear, version mismatch and
recovery, for both codecs. **Not measured:** the time of a format or of a download on the device, flash wear.

## Telemetry packet

| | Before | After |
|---|---:|---:|
| `MAX_SENSORS_PER_MESSAGE` | 9 | 8 (one per measurement the encoder supports) |
| `MAX_PRIVATE_SECTIONS` | 2 | 1 (one recipient per profile) |
| `MAX_PRIVATE_PLAINTEXT_LEN` | 128 B | 115 B (the sum of the largest entries of the 8 supported measurements) |
| `MAX_ENCRYPTED_LEN` | 232 B | 207 B (as this SDK writes it) |
| `MAX_MESSAGE_LEN` | 780 B | 352 B |
| `MAX_ENVELOPE_LEN` = packet buffer | 920 B | 473 B (−447 B per pool slot; three slots, the pool and one export slot, −1 341 B in the pool and export statics) |
| typical packet (2 public, 1 private section of 2), signed | not measured before | 284 B (measured) |
| largest packet (8 public, 8 private) | not measured before | 419 B, message 298 B (measured); the limit is exactly the largest message that the compiled capabilities allow (352 B message, 473 B envelope), checked by a test that builds it from the schema |

The bounds follow the *compiled capabilities*, not the schema alone: the schema permits a list of eight GPS entries (510 B as a
message), the encoder never writes more than one entry per supported measurement. A section that would exceed them fails with `TooLarge`;
nothing is truncated.

**One buffer instead of five.** `RobonomicsFormat::seal` used to hold the sensors, the plaintext (128 B), the ciphertext (144 B), an
`Encrypted` value, an encoded `Message` (780 B), a separate Ed25519 preimage (828 B) and a `SignedEnvelope` (944 B) at the same time. Now the
message is encoded to its final place in the caller's packet buffer, `sensor_id || nonce` is staged right in front of it (that is the
exact preimage), the signature is computed over those bytes and the envelope fields are written over the staged bytes. The private
section is encrypted in place in one 131 B buffer and its `Encrypted` message is written from the parts. **The signed bytes, the
Ed25519 prehash (none: the preimage is signed whole), the XChaCha20-Poly1305 semantics, the order of the entropy draws and the byte
output are unchanged**: tests decode every packet, re-encode the envelope, the message and the private section with the generated
types and compare byte for byte, and verify the signature over the preimage for message sizes on both sides of the length-prefix
boundaries.

**Snapshot.** `SNAPSHOT_OBSERVATIONS` was `CATALOG.len()` (18); it is the number of measurements the compiled encoder supports (8),
and a capture stores only those (`capture_accepting`): a source that also reports an unsupported measurement (the SCD4x, the gas
sensors) is not discarded for it. The diagnostics array holds 2 sources (the sensors, the location) instead of 8. Per-source
atomicity, freshness, validation and the distinction between capture time and observation time are unchanged; the local sensors and
their status do not go through the snapshot. A snapshot of 8 observations and 2 reports is about 0.4 KB smaller than one of 18 and 8 (static, 32 B per observation).

## Connectivity and telemetry capacities

`ROUTES = 2`, `FALLBACKS = 1`, `PROFILES = 2`, `RECIPIENTS = 1` are const generics through `Routes`, `ConnectivitySection`,
`ConnectivityState`, `Router`, `Publisher`, `ConnectivityService` (and the store trait), `TelemetryConfig`, `TelemetrySection`,
`TelemetryState`, `TelemetryService`. The storage is the capacity (a third route has no room), the status schema is written
out for the route count (`Slots<2>`; any count outside 1 to 3 does not compile) with stable ids, names and kinds, and a stored
section over capacity does not decode: it is refused, not truncated (tests with three routes, two fallbacks, three profiles and two
recipients). The section identities did not change. Retry and backoff, fair scheduling, generation and stamp checks, outbox
handling and profile invalidation keep their tests unchanged in meaning (they run with 3 routes and 2 fallbacks, the SDK-side
tests cover more than a device keeps).

## TCP security

`rohi-control-tcp` owns the security of the TCP endpoint; `rohi-control` has no cryptography and consumes only the
`encrypted`/`authenticated` flags that a trusted transport sets (a request field cannot raise them). A restricted endpoint (the
default) serves only clients that complete the handshake (32-byte pre-shared key, HKDF-SHA256, random nonces, mutual proofs bound to
the transcript, directional keys, XChaCha20-Poly1305 with a counter and direction nonce, versioned authenticated record framing,
no processing before the handshake verified, no fallback to plaintext). The key is provisioned over an encrypted (paired) BLE
link or over the TCP connection of the current key (`secure_link` actions), never over an unauthenticated link, and is replaced the
same way; there is **no forward secrecy**. Signed requests, the challenge and the administrator key are gone from the protocol,
the control core, the services, the firmware and the `rohi` CLI (`rohi admin` became `rohi tcp-key`; `--admin-key` became
`--psk-file`). The `ControlHub` request slot is 128 B smaller (896 → 768) because it no longer has to hold a signed wrapper.

Negative tests (host): tampered (every bit of header, data and tag), replayed, reordered, dropped and truncated records, a record of
another connection and of the other direction, a counter that is used up (stops for good), a wrong key on either side, a client
that sends plaintext or a bad final message (ends in silence, counted, never processed), no key or no random source (serves
nobody), a key that was replaced while connected (ends the connection), and cancellation (the session is released).
**Not covered:** TCP is a host (std) transport; there is no TCP endpoint on the ESP firmware, so none of this ran on a device, and
the handshake has no deadline in this crate (a platform that needs one wraps `serve_next` in a timeout).

## Functionality intentionally removed or reduced

- Signed control requests, `GetAuthChallenge`, the administrator key and `rohi admin` (replaced by the pre-shared key and the TCP channel).
- LZ4 log compression from the default build (the `lz4` feature brings it back) and every reader of older log formats: a log of
  another format version is discarded, not read.
- The ESP32-C3 log shrank from 512 KiB to 128 KiB (120 KiB ring) and moved in front of the application.
- A third route, a second fallback, a third profile, a second private recipient and a second private section per message; the
  Robonomics format is the only compiled telemetry format; unsupported measurements are no longer stored in a snapshot.
- A TCP client can no longer connect without the key unless the endpoint is explicitly open (plaintext, loopback development).

## Not measured, not verified

- Everything on hardware: boot time, the time of a log format and a download, stack high-water marks, flash wear, BLE pairing and the
  link security of a real central, the 128 KiB layout flashed to a board and the move of the application.
- Throughput or latency of the secure channel (the host tests do not time it).
- The cost of the removed signed-request code in flash separately from the other changes of this branch (the totals above are
  for the whole change; the per-feature variants are the LZ4 and packet export rows).

## Reproduce

```bash
git worktree add /tmp/base 5f445be && (cd /tmp/base && scripts/memcheck.sh images esp32c3 /tmp/base-out && scripts/memcheck.sh analyze esp32c3 /tmp/base-out)
scripts/memcheck.sh images esp32c3 out && scripts/memcheck.sh analyze esp32c3 out --base /tmp/base-out/task-memory-esp32c3.json
scripts/measure.sh esp32c3            # the image sections; again with `lz4` as the second argument
cargo test -p rohi-logging retention -- --nocapture   # the retention table
cargo test -p rohi-telemetry-robonomics --test seal -- --nocapture   # the packet lengths
```
